//! tonic gRPC server over Unix domain socket and optional TCP

use anyhow::Result;
#[cfg(unix)]
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::transport::Server;
use tracing::{info, warn};

use crate::cred_store::SharedCredStore;

use base64::Engine;
use secrecy::ExposeSecret;
use tcfs_core::config::{
    sanitize_http_endpoint_for_display, RegisteredRootConfig, RegisteredRootPolicy, TcfsConfig,
};
use tcfs_core::proto::{
    tcfs_daemon_server::{TcfsDaemon, TcfsDaemonServer},
    *,
};
use tcfs_sync::state::StateCacheBackend;

const FILE_PROVIDER_VERSION_MISMATCH_PREFIX: &str = "file-provider version mismatch:";
const EXACT_PULL_CHUNK_SIZE: usize = 1024 * 1024;

fn file_provider_version_mismatch_status(requested: &str, current: &str) -> tonic::Status {
    tonic::Status::failed_precondition(format!(
        "{FILE_PROVIDER_VERSION_MISMATCH_PREFIX} requested {requested}, current {current}"
    ))
}

async fn authoritative_watch_event(
    operator: &opendal::Operator,
    storage_prefix: &str,
    rel_path: &str,
    requested_event_type: &str,
    timestamp: i64,
    device_id: String,
) -> std::result::Result<WatchEvent, tonic::Status> {
    use tcfs_sync::index_entry::ExactIndexPathState;

    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    let filename = Path::new(rel_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let state =
        tcfs_sync::index_entry::read_exact_index_path_state(operator, storage_prefix, rel_path)
            .await
            .map_err(|error| {
                tonic::Status::internal(format!(
                    "read authoritative Watch index state for {rel_path}: {error:#}"
                ))
            })?;

    match state {
        ExactIndexPathState::Deleted => Ok(WatchEvent {
            path: rel_path.to_string(),
            event_type: "deleted".into(),
            timestamp,
            filename,
            device_id,
            ..Default::default()
        }),
        ExactIndexPathState::Missing => Err(tonic::Status::failed_precondition(format!(
            "Watch authority is missing (not tombstoned) for {rel_path}; full refresh required"
        ))),
        ExactIndexPathState::Live => {
            let snapshot = tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(
                operator,
                rel_path,
                storage_prefix,
            )
            .await
            .map_err(|error| {
                tonic::Status::internal(format!(
                    "resolve authoritative Watch manifest for {rel_path}: {error:#}"
                ))
            })?
            .ok_or_else(|| {
                tonic::Status::aborted(format!(
                    "Watch authority changed while resolving live path {rel_path}; full refresh required"
                ))
            })?;
            let event_type = match requested_event_type {
                "created" => "created",
                "renamed" => "renamed",
                _ => "modified",
            };
            Ok(WatchEvent {
                path: rel_path.to_string(),
                event_type: event_type.into(),
                timestamp,
                filename,
                size: snapshot.size(),
                blake3: snapshot.content_hash().to_string(),
                is_directory: false,
                device_id,
                version_token: snapshot.manifest_object_id().to_string(),
            })
        }
    }
}

fn legacy_push_rejection_reason(
    skipped: bool,
    outcome: Option<&tcfs_sync::conflict::SyncOutcome>,
) -> Option<String> {
    match outcome {
        Some(tcfs_sync::conflict::SyncOutcome::RemoteNewer) => {
            Some("push rejected: the remote version is newer".into())
        }
        Some(tcfs_sync::conflict::SyncOutcome::Conflict(info)) => Some(format!(
            "push rejected: concurrent update conflict between {} and {}",
            info.local_device, info.remote_device
        )),
        _ if skipped => Some("push rejected: sync engine skipped publication".into()),
        _ => None,
    }
}

fn repo_keep_both_mode(resolution: &str) -> Option<tcfs_sync::conflict_git::GitKeepBothMode> {
    match resolution {
        "git_keep_both_dry_run" => Some(tcfs_sync::conflict_git::GitKeepBothMode::DryRun),
        "git_keep_both_execute" => Some(tcfs_sync::conflict_git::GitKeepBothMode::Execute),
        _ => None,
    }
}

/// Canonical, daemon-selected routing record for one non-primary root.
#[derive(Debug, Clone)]
struct RegisteredRootRoute {
    root_id: String,
    local_root: PathBuf,
    remote_prefix: String,
    state_path: PathBuf,
    policy: RegisteredRootPolicy,
}

fn validate_root_id(root_id: &str) -> std::result::Result<(), String> {
    let valid = !root_id.is_empty()
        && root_id.len() <= 64
        && !root_id.eq_ignore_ascii_case("primary")
        && root_id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && root_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid registered root id '{root_id}': use 1-64 lowercase ASCII letters, digits, '.', '_' or '-' (reserved: primary)"
        ))
    }
}

fn validate_remote_prefix(prefix: &str) -> std::result::Result<(), String> {
    let valid = !prefix.is_empty()
        && !prefix.starts_with('/')
        && !prefix.ends_with('/')
        && !prefix.contains('\\')
        && prefix
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid registered root remote_prefix '{prefix}': expected a non-empty relative object-key prefix without '.', '..', empty, or backslash segments"
        ))
    }
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn local_roots_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn canonicalize_if_present(
    path: &Path,
    description: &str,
) -> std::result::Result<Option<PathBuf>, String> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "canonicalizing {description} {}: {error}",
            path.display()
        )),
    }
}

fn lexically_normalize_absolute(path: &Path) -> std::result::Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("expected an absolute path, got {}", path.display()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "path escapes its filesystem root during lexical normalization: {}",
                        path.display()
                    ));
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    if !normalized.is_absolute() {
        return Err(format!(
            "path lost its absolute root during normalization: {}",
            path.display()
        ));
    }
    Ok(normalized)
}

/// Resolve symlinks in the longest existing prefix, then append and normalize
/// any missing tail. Conflict resolution must also fence deleted files and
/// not-yet-created paths; `canonicalize(path)` alone would let those bypass a
/// registered-root boundary.
fn canonicalize_with_missing_tail(path: &Path) -> std::result::Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("expected an absolute path, got {}", path.display()));
    }

    // `canonicalize()` reports a dangling symlink as NotFound. Do not then
    // treat that symlink name as an ordinary missing tail: a later target could
    // redirect the mutation outside the authorized root.
    let mut probe = PathBuf::new();
    for component in path.components() {
        probe.push(component.as_os_str());
        match std::fs::symlink_metadata(&probe) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::canonicalize(&probe).map_err(|error| {
                    format!(
                        "refusing unresolved symlink component {} in {}: {error}",
                        probe.display(),
                        path.display()
                    )
                })?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "inspecting path component {} for {}: {error}",
                    probe.display(),
                    path.display()
                ));
            }
        }
    }

    let components = path.components().collect::<Vec<_>>();
    for split in (1..=components.len()).rev() {
        let mut prefix = PathBuf::new();
        for component in &components[..split] {
            prefix.push(component.as_os_str());
        }
        match std::fs::canonicalize(&prefix) {
            Ok(mut resolved) => {
                for component in &components[split..] {
                    resolved.push(component.as_os_str());
                }
                return lexically_normalize_absolute(&resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "resolving path prefix {} for {}: {error}",
                    prefix.display(),
                    path.display()
                ));
            }
        }
    }

    Err(format!(
        "no existing ancestor could be resolved for {}",
        path.display()
    ))
}

fn rootless_path_may_belong_to_registered_root(config: &TcfsConfig, path: &Path) -> bool {
    if config.sync.roots.is_empty() {
        return false;
    }
    if !path.is_absolute() {
        return true;
    }

    let Ok(request_lexical) = lexically_normalize_absolute(path) else {
        return true;
    };
    let Ok(request_resolved) = canonicalize_with_missing_tail(path) else {
        return true;
    };
    for root in config.sync.roots.values() {
        let root_path = tcfs_core::config::expand_tilde(&root.local_root);
        let Ok(root_lexical) = lexically_normalize_absolute(&root_path) else {
            return true;
        };
        if request_lexical == root_lexical || request_lexical.starts_with(&root_lexical) {
            return true;
        }

        let Ok(root_resolved) = canonicalize_with_missing_tail(&root_path) else {
            return true;
        };
        if request_resolved == root_resolved || request_resolved.starts_with(&root_resolved) {
            return true;
        }
    }
    false
}

fn rootless_path_is_within_primary_sync_root(config: &TcfsConfig, path: &Path) -> bool {
    let Some(root) = config.sync.sync_root.as_deref() else {
        return false;
    };
    let root = tcfs_core::config::expand_tilde(root);
    if !path.is_absolute() || !root.is_absolute() {
        return false;
    }
    let (Ok(path_lexical), Ok(root_lexical)) = (
        lexically_normalize_absolute(path),
        lexically_normalize_absolute(&root),
    ) else {
        return false;
    };
    if path_lexical != root_lexical && !path_lexical.starts_with(&root_lexical) {
        return false;
    }
    let (Ok(path_resolved), Ok(root_resolved)) = (
        canonicalize_with_missing_tail(path),
        canonicalize_with_missing_tail(&root),
    ) else {
        return false;
    };
    path_resolved == root_resolved || path_resolved.starts_with(root_resolved)
}

/// Bind a local mutation target to the daemon's configured primary root and
/// return its canonical slash-separated remote-index path. This resolves every
/// existing symlink component before deriving the relative path, so a stub
/// cannot use a lexical in-root name to select or mutate an out-of-root file.
fn primary_sync_root_target(
    config: &TcfsConfig,
    target: &Path,
    operation: &str,
) -> std::result::Result<(PathBuf, String), String> {
    let configured_root = config
        .sync
        .sync_root
        .as_deref()
        .ok_or_else(|| format!("{operation} requires a configured sync.sync_root"))?;
    let configured_root = tcfs_core::config::expand_tilde(configured_root);
    if !configured_root.is_absolute() {
        return Err(format!(
            "configured sync.sync_root must be absolute: {}",
            configured_root.display()
        ));
    }
    if !target.is_absolute() {
        return Err(format!(
            "{operation} target must be absolute: {}",
            target.display()
        ));
    }

    let canonical_root = std::fs::canonicalize(&configured_root).map_err(|error| {
        format!(
            "canonicalizing configured sync.sync_root {}: {error}",
            configured_root.display()
        )
    })?;
    if !canonical_root.is_dir() {
        return Err(format!(
            "configured sync.sync_root is not a directory: {}",
            canonical_root.display()
        ));
    }
    let canonical_target = canonicalize_with_missing_tail(target)?;
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        return Err(format!(
            "{operation} target is outside configured sync.sync_root: {}",
            target.display()
        ));
    }

    let relative = canonical_target
        .strip_prefix(&canonical_root)
        .map_err(|_| format!("{operation} target lost configured root prefix"))?;
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(format!(
                "{operation} target has a non-canonical relative component: {}",
                target.display()
            ));
        };
        components.push(
            component
                .to_str()
                .ok_or_else(|| format!("{operation} target is not valid UTF-8"))?,
        );
    }
    let rel_path = components.join("/");
    tcfs_sync::index_entry::validate_canonical_rel_path(&rel_path)
        .map_err(|error| format!("invalid {operation} relative path: {error}"))?;

    Ok((canonical_target, rel_path))
}

fn validate_fixed_ingress_path(path: &Path, description: &str) -> std::result::Result<(), String> {
    let fixed = tcfs_sync::blacklist::Blacklist::default();
    if let Some(reason) = fixed.check_fixed_ingress_path_components(path) {
        return Err(format!(
            "{description} {} is blocked by the fixed security deny-set: {reason}",
            path.display()
        ));
    }
    Ok(())
}

fn validate_pull_destination(
    config: &TcfsConfig,
    destination: &Path,
    logical_path: &str,
) -> std::result::Result<(), String> {
    validate_fixed_ingress_path(Path::new(logical_path), "pull logical path")?;
    validate_fixed_ingress_path(destination, "pull destination")?;
    tcfs_core::config::validate_sync_selection_excludes_master_key(config, destination)
}

fn remote_prefixes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn registered_root_state_dir(config: &TcfsConfig) -> std::result::Result<PathBuf, String> {
    if let Some(root_state_dir) = config.sync.root_state_dir.as_deref() {
        let root_state_dir = tcfs_core::config::expand_tilde(root_state_dir);
        if !root_state_dir.is_absolute() || has_parent_component(&root_state_dir) {
            return Err(format!(
                "sync.root_state_dir must be an absolute path without '..': {}",
                root_state_dir.display()
            ));
        }
        return Ok(root_state_dir);
    }

    let socket = tcfs_core::config::expand_tilde(&config.daemon.socket);
    let parent = socket
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            format!(
                "daemon socket {} has no parent for registered-root state fencing",
                socket.display()
            )
        })?;
    let state_dir = parent.join("reconcile");
    if !state_dir.is_absolute() || has_parent_component(&state_dir) {
        return Err(format!(
            "daemon-derived registered-root state directory must be absolute without '..': {}",
            state_dir.display()
        ));
    }
    Ok(state_dir)
}

fn normalized_registered_state_path(path: &Path) -> PathBuf {
    let path = tcfs_core::config::expand_tilde(path);
    if path.extension().is_some_and(|extension| extension == "db") {
        path.with_extension("json")
    } else {
        path
    }
}

/// Validate the trusted mapping without touching the filesystem.
fn validate_registered_root_definition(
    config: &TcfsConfig,
    root_id: &str,
    root: &RegisteredRootConfig,
) -> std::result::Result<(PathBuf, PathBuf), String> {
    validate_root_id(root_id)?;
    validate_remote_prefix(&root.remote_prefix)?;

    let local_root = tcfs_core::config::expand_tilde(&root.local_root);
    if !local_root.is_absolute() || has_parent_component(&local_root) {
        return Err(format!(
            "registered root '{root_id}' local_root must be an absolute path without '..': {}",
            local_root.display()
        ));
    }

    let state_path = normalized_registered_state_path(&root.state_path);
    if !state_path.is_absolute() || has_parent_component(&state_path) {
        return Err(format!(
            "registered root '{root_id}' state_path must be an absolute path without '..': {}",
            state_path.display()
        ));
    }
    if state_path
        .extension()
        .is_none_or(|extension| extension != "json")
    {
        return Err(format!(
            "registered root '{root_id}' state_path must resolve to a .json cache: {}",
            state_path.display()
        ));
    }

    let expected_dir = registered_root_state_dir(config)?;
    let expected_name = format!("{root_id}.json");
    if state_path.parent() != Some(expected_dir.as_path())
        || state_path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
    {
        return Err(format!(
            "registered root '{root_id}' state_path must be {} (daemon-owned root-state fence), got {}",
            expected_dir.join(expected_name).display(),
            state_path.display()
        ));
    }

    Ok((local_root, state_path))
}

/// Startup-time validation for every configured mapping and the master-key
/// isolation boundary. Runtime root selection repeats the mapping checks and
/// adds stricter ownership/existence validation.
pub(crate) fn validate_registered_roots_config(config: &TcfsConfig) -> anyhow::Result<()> {
    if let Some(master_key_path) = config.crypto.master_key_file.as_deref() {
        tcfs_core::config::validate_master_key_outside_sync_roots(config, master_key_path)
            .map_err(anyhow::Error::msg)?;
    }

    if config.sync.roots.is_empty() {
        return Ok(());
    }

    let primary_local_root = if let Some(path) = config.sync.sync_root.as_deref() {
        let path = tcfs_core::config::expand_tilde(path);
        if !path.is_absolute() || has_parent_component(&path) {
            anyhow::bail!(
                "primary sync.sync_root must be an absolute path without '..' when registered roots are configured: {}",
                path.display()
            );
        }
        Some(path)
    } else {
        None
    };
    let primary_prefix = config.storage.resolved_prefix();
    validate_remote_prefix(primary_prefix).map_err(|error| {
        anyhow::anyhow!(
            "primary storage prefix is invalid while registered roots are configured: {error}"
        )
    })?;
    let root_state_dir = registered_root_state_dir(config).map_err(anyhow::Error::msg)?;
    if let Some(primary) = primary_local_root.as_deref() {
        if local_roots_overlap(&root_state_dir, primary) {
            anyhow::bail!(
                "registered-root state directory {} overlaps primary sync_root {}",
                root_state_dir.display(),
                primary.display()
            );
        }
    }

    let mut prefixes: Vec<(&str, &str)> = Vec::new();
    let mut local_roots: Vec<(&str, PathBuf)> = Vec::new();
    for (root_id, root) in &config.sync.roots {
        let (local_root, _) = validate_registered_root_definition(config, root_id, root)
            .map_err(anyhow::Error::msg)?;
        if local_roots_overlap(&root_state_dir, &local_root) {
            anyhow::bail!(
                "registered-root state directory {} overlaps registered root '{root_id}' local_root {}",
                root_state_dir.display(),
                local_root.display()
            );
        }
        if let Some(primary) = primary_local_root.as_deref() {
            if local_roots_overlap(&local_root, primary) {
                anyhow::bail!(
                    "registered root '{root_id}' local_root {} overlaps primary sync_root {}",
                    local_root.display(),
                    primary.display()
                );
            }
        }
        if remote_prefixes_overlap(&root.remote_prefix, primary_prefix) {
            anyhow::bail!(
                "registered root '{root_id}' remote_prefix '{}' overlaps primary storage prefix '{}'",
                root.remote_prefix,
                primary_prefix
            );
        }
        for (other_id, other_root) in &local_roots {
            if local_roots_overlap(&local_root, other_root) {
                anyhow::bail!(
                    "registered roots '{root_id}' and '{other_id}' have overlapping local roots ({} and {})",
                    local_root.display(),
                    other_root.display()
                );
            }
        }
        for (other_id, other_prefix) in &prefixes {
            if remote_prefixes_overlap(&root.remote_prefix, other_prefix) {
                anyhow::bail!(
                    "registered roots '{root_id}' and '{other_id}' have overlapping remote prefixes ('{}' and '{}')",
                    root.remote_prefix,
                    other_prefix
                );
            }
        }
        local_roots.push((root_id, local_root));
        prefixes.push((root_id, &root.remote_prefix));
    }
    Ok(())
}

fn validate_canonical_local_root_isolation(
    config: &TcfsConfig,
    root_id: &str,
    local_root: &Path,
) -> std::result::Result<(), String> {
    if let Some(primary) = config.sync.sync_root.as_deref() {
        // The primary root may be configured before it is hydrated on this
        // host. As with named peers, re-evaluate it on every selection so a
        // symlink alias is fenced as soon as the path appears.
        let primary_path = tcfs_core::config::expand_tilde(primary);
        if let Some(primary) = canonicalize_if_present(&primary_path, "primary sync_root")? {
            if local_roots_overlap(local_root, &primary) {
                return Err(format!(
                    "registered root '{root_id}' overlaps primary sync_root after canonicalization ({} and {})",
                    local_root.display(),
                    primary.display()
                ));
            }
        }
    }

    for (other_id, other) in &config.sync.roots {
        if other_id == root_id {
            continue;
        }
        // A peer may be enrolled before it is hydrated on this host. Ignore it
        // while unavailable, but re-evaluate it on every selected-root request
        // so an alias becomes fenced as soon as the peer path appears.
        let other_path = tcfs_core::config::expand_tilde(&other.local_root);
        let Some(other_root) = canonicalize_if_present(
            &other_path,
            &format!("registered root '{other_id}' local_root"),
        )?
        else {
            continue;
        };
        if local_roots_overlap(local_root, &other_root) {
            return Err(format!(
                "registered roots '{root_id}' and '{other_id}' overlap after canonicalization ({} and {})",
                local_root.display(),
                other_root.display()
            ));
        }
    }
    Ok(())
}

fn validate_canonical_state_dir_isolation(
    config: &TcfsConfig,
    state_dir: &Path,
) -> std::result::Result<(), String> {
    if let Some(primary) = config.sync.sync_root.as_deref() {
        let primary_path = tcfs_core::config::expand_tilde(primary);
        if let Some(primary) = canonicalize_if_present(&primary_path, "primary sync_root")? {
            if local_roots_overlap(state_dir, &primary) {
                return Err(format!(
                    "registered-root state directory {} overlaps primary sync_root after canonicalization ({})",
                    state_dir.display(),
                    primary.display()
                ));
            }
        }
    }

    for (other_id, other) in &config.sync.roots {
        let other_path = tcfs_core::config::expand_tilde(&other.local_root);
        let Some(other_root) = canonicalize_if_present(
            &other_path,
            &format!("registered root '{other_id}' local_root"),
        )?
        else {
            continue;
        };
        if local_roots_overlap(state_dir, &other_root) {
            return Err(format!(
                "registered-root state directory {} overlaps registered root '{other_id}' local_root after canonicalization ({})",
                state_dir.display(),
                other_root.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_registered_state_inode_isolation(
    config: &TcfsConfig,
    root_id: &str,
    state_path: &Path,
    metadata: &std::fs::Metadata,
) -> std::result::Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if metadata.nlink() != 1 {
        return Err(format!(
            "registered root '{root_id}' state cache must have exactly one hard link, got {}: {}",
            metadata.nlink(),
            state_path.display()
        ));
    }

    let primary_state =
        tcfs_core::config::expand_tilde(&config.sync.state_db).with_extension("json");
    let mut candidates = vec![("primary state cache".to_string(), primary_state)];
    candidates.extend(
        config
            .sync
            .roots
            .iter()
            .filter(|(other_id, _)| other_id.as_str() != root_id)
            .map(|(other_id, other)| {
                (
                    format!("registered root '{other_id}' state cache"),
                    normalized_registered_state_path(&other.state_path),
                )
            }),
    );

    for (description, candidate) in candidates {
        let candidate_metadata = match std::fs::metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "reading {description} inode metadata {} while selecting registered root '{root_id}': {error}",
                    candidate.display()
                ));
            }
        };
        if candidate_metadata.dev() == metadata.dev() && candidate_metadata.ino() == metadata.ino()
        {
            return Err(format!(
                "registered root '{root_id}' state cache aliases {description} by inode ({} and {})",
                state_path.display(),
                candidate.display()
            ));
        }
    }
    Ok(())
}

fn canonical_registered_root(
    config: &TcfsConfig,
    root_id: &str,
    root: &RegisteredRootConfig,
) -> std::result::Result<RegisteredRootRoute, String> {
    let (local_root, state_path) = validate_registered_root_definition(config, root_id, root)?;
    tcfs_sync::conflict_git::validate_trusted_configured_path(&local_root).map_err(|error| {
        format!(
            "registered root '{root_id}' configured local_root has an untrusted original path chain: {error:#}"
        )
    })?;
    let local_root = std::fs::canonicalize(&local_root).map_err(|error| {
        format!(
            "registered root '{root_id}' local_root {} is unavailable: {error}",
            local_root.display()
        )
    })?;
    if !local_root.is_dir() {
        return Err(format!(
            "registered root '{root_id}' local_root is not a directory: {}",
            local_root.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let local_metadata = std::fs::symlink_metadata(&local_root).map_err(|error| {
            format!(
                "reading registered root '{root_id}' local_root metadata {}: {error}",
                local_root.display()
            )
        })?;
        // SAFETY: `geteuid` has no preconditions and only reads process identity.
        let effective_uid = unsafe { libc::geteuid() };
        if local_metadata.uid() != effective_uid {
            return Err(format!(
                "registered root '{root_id}' local_root must be owned by tcfsd effective uid {effective_uid}, got uid {}: {}",
                local_metadata.uid(),
                local_root.display(),
            ));
        }
        if local_metadata.mode() & 0o022 != 0 {
            return Err(format!(
                "registered root '{root_id}' local_root must not be group/world writable: {}",
                local_root.display()
            ));
        }
    }
    tcfs_sync::path_acl::reject_write_grant_acl(&local_root).map_err(|error| {
        format!("registered root '{root_id}' local_root has an untrusted extended ACL: {error:#}")
    })?;
    tcfs_sync::conflict_git::validate_trusted_ancestor_chain(&local_root).map_err(|error| {
        format!("registered root '{root_id}' local_root has an untrusted ancestor chain: {error:#}")
    })?;
    validate_canonical_local_root_isolation(config, root_id, &local_root)?;

    let metadata = std::fs::symlink_metadata(&state_path).map_err(|error| {
        format!(
            "registered root '{root_id}' state cache {} is unavailable: {error}",
            state_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "registered root '{root_id}' state cache must be a regular, non-symlink file: {}",
            state_path.display()
        ));
    }
    tcfs_sync::path_acl::reject_write_grant_acl(&state_path).map_err(|error| {
        format!("registered root '{root_id}' state cache has an untrusted extended ACL: {error:#}")
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        validate_registered_state_inode_isolation(config, root_id, &state_path, &metadata)?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "registered root '{root_id}' state cache must not be group/world accessible (mode 0600 or stricter): {}",
                state_path.display()
            ));
        }
    }

    let state_dir = registered_root_state_dir(config)?;
    tcfs_sync::conflict_git::validate_trusted_configured_path(&state_dir).map_err(|error| {
        format!(
            "registered root '{root_id}' configured state directory has an untrusted original path chain: {error:#}"
        )
    })?;
    let canonical_state_dir = std::fs::canonicalize(&state_dir).map_err(|error| {
        format!(
            "registered-root state directory {} is unavailable: {error}",
            state_dir.display()
        )
    })?;
    tcfs_sync::conflict_git::validate_trusted_ancestor_chain(&canonical_state_dir).map_err(
        |error| {
            format!(
                "registered root '{root_id}' state directory has an untrusted ancestor chain: {error:#}"
            )
        },
    )?;
    validate_canonical_state_dir_isolation(config, &canonical_state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory_metadata = std::fs::metadata(&canonical_state_dir).map_err(|error| {
            format!(
                "reading registered-root state directory metadata {}: {error}",
                canonical_state_dir.display()
            )
        })?;
        // SAFETY: `geteuid` has no preconditions and only reads process identity.
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.uid() != effective_uid || directory_metadata.uid() != effective_uid {
            return Err(format!(
                "registered root '{root_id}' state directory and cache must be owned by tcfsd effective uid {effective_uid} (directory uid {}, cache uid {}): {}",
                directory_metadata.uid(),
                metadata.uid(),
                state_path.display(),
            ));
        }
        if directory_metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "registered-root state directory must not be group/world writable: {}",
                canonical_state_dir.display()
            ));
        }
    }
    tcfs_sync::path_acl::reject_write_grant_acl(&canonical_state_dir).map_err(|error| {
        format!(
            "registered root '{root_id}' state directory has an untrusted extended ACL: {error:#}"
        )
    })?;
    let state_path = std::fs::canonicalize(&state_path).map_err(|error| {
        format!("canonicalizing registered root '{root_id}' state cache: {error}")
    })?;
    if state_path.parent() != Some(canonical_state_dir.as_path()) {
        return Err(format!(
            "registered root '{root_id}' state cache escapes daemon-owned directory after canonicalization: {}",
            state_path.display()
        ));
    }
    if state_path.starts_with(&local_root) {
        return Err(format!(
            "registered root '{root_id}' state cache must remain outside local_root: {}",
            state_path.display()
        ));
    }

    Ok(RegisteredRootRoute {
        root_id: root_id.to_string(),
        local_root,
        remote_prefix: root.remote_prefix.clone(),
        state_path,
        policy: root.policy,
    })
}

fn object_key_is_within_prefix(key: &str, prefix: &str) -> bool {
    let Some(suffix) = key
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('/'))
    else {
        return false;
    };
    !suffix.is_empty()
        && !suffix.contains('\\')
        && suffix
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn primary_conflict_entry_matches_prefix(
    entry: &tcfs_sync::state::SyncState,
    prefix: &str,
) -> bool {
    if entry.status != tcfs_sync::state::FileSyncStatus::Conflict
        || !object_key_is_within_prefix(&entry.remote_path, prefix)
    {
        return false;
    }
    let Some(conflict) = entry.conflict.as_ref() else {
        return false;
    };
    let rel_path = Path::new(&conflict.rel_path);
    if conflict.rel_path.is_empty()
        || conflict.rel_path.contains('\\')
        || rel_path.is_absolute()
        || has_parent_component(rel_path)
    {
        return false;
    }
    conflict
        .remote_manifest_key
        .as_deref()
        .is_none_or(|key| object_key_is_within_prefix(key, prefix))
}

fn check_registered_prefix_permission(
    session: &tcfs_auth::Session,
    prefix: &str,
) -> std::result::Result<(), tonic::Status> {
    if registered_prefix_allowed(session, prefix) {
        Ok(())
    } else {
        Err(tonic::Status::permission_denied(format!(
            "device {} is not authorized for the requested storage scope",
            session.device_id
        )))
    }
}

fn registered_prefix_allowed(session: &tcfs_auth::Session, prefix: &str) -> bool {
    session.permissions.allowed_prefixes.is_empty()
        || session.permissions.allowed_prefixes.iter().any(|allowed| {
            let allowed = allowed.trim_matches('/');
            !allowed.is_empty()
                && (prefix == allowed
                    || prefix
                        .strip_prefix(allowed)
                        .is_some_and(|suffix| suffix.starts_with('/')))
        })
}

fn validate_conflict_cache_route(
    route: &RegisteredRootRoute,
    state: &tcfs_sync::state::StateCache,
) -> std::result::Result<(), String> {
    for (cache_key, entry) in state.conflicts() {
        let cache_path = Path::new(cache_key);
        if cache_key.contains('\\')
            || !cache_path.is_absolute()
            || has_parent_component(cache_path)
            || !cache_path.starts_with(&route.local_root)
        {
            return Err(format!(
                "registered root '{}' cache contains conflict outside local_root: {}",
                route.root_id, cache_key
            ));
        }
        if let Some(conflict) = entry.conflict.as_ref() {
            let rel_path = Path::new(&conflict.rel_path);
            if conflict.rel_path.is_empty()
                || conflict.rel_path.contains('\\')
                || rel_path.is_absolute()
                || has_parent_component(rel_path)
            {
                return Err(format!(
                    "registered root '{}' cache contains an unsafe conflict rel_path: {}",
                    route.root_id, conflict.rel_path
                ));
            }
        }
        if !object_key_is_within_prefix(&entry.remote_path, &route.remote_prefix) {
            return Err(format!(
                "registered root '{}' cache entry escapes remote_prefix '{}': {}",
                route.root_id, route.remote_prefix, entry.remote_path
            ));
        }
        if let Some(remote_manifest_key) = entry
            .conflict
            .as_ref()
            .and_then(|conflict| conflict.remote_manifest_key.as_deref())
        {
            if !object_key_is_within_prefix(remote_manifest_key, &route.remote_prefix) {
                return Err(format!(
                    "registered root '{}' conflict manifest escapes remote_prefix '{}': {}",
                    route.root_id, route.remote_prefix, remote_manifest_key
                ));
            }
        }
    }
    Ok(())
}

fn validate_standalone_git_root(route: &RegisteredRootRoute) -> std::result::Result<(), String> {
    tcfs_sync::conflict_git::validate_standalone_repo_topology(&route.local_root).map_err(|error| {
        format!(
            "registered root '{}' is not a standalone git repository: {error:#}",
            route.root_id
        )
    })
}

fn conflict_records(state: &tcfs_sync::state::StateCache) -> Vec<ConflictRecord> {
    let mut records = state
        .conflicts()
        .into_iter()
        .filter_map(|(cache_key, entry)| {
            let conflict = entry.conflict.as_ref()?;
            Some(ConflictRecord {
                cache_key: cache_key.to_string(),
                rel_path: conflict.rel_path.clone(),
                local_device: conflict.local_device.clone(),
                remote_device: conflict.remote_device.clone(),
                detected_at: conflict.detected_at,
                times_recorded: conflict.times_recorded,
            })
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.rel_path
            .cmp(&right.rel_path)
            .then_with(|| left.cache_key.cmp(&right.cache_key))
    });
    records
}

/// Machine-local directory for the keep-both undo bundle. It MUST be outside any
/// sync root, so anchor it to the daemon state DB's directory (the same
/// machine-local location the state cache uses; see `worker.rs`/`daemon.rs`).
/// Expands a leading `~/` and falls back to the platform data dir (BLOCKING 2).
fn undo_bundle_state_dir(config: &TcfsConfig) -> PathBuf {
    config
        .sync
        .state_db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(expand_home_prefix)
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_default().join("tcfsd"))
}

/// Expand a leading `~/` against the current user's home directory. Leaves any
/// other path untouched.
fn expand_home_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

/// Build an `EncryptionContext` honoring `crypto.wrap_mode` (TIN-1417).
///
/// - `Master` (default): legacy shared-master wrap. Byte-identical to the prior
///   default; returns `EncryptionContext::new(master_key)` verbatim.
/// - `Dual` (EXPAND): emit BOTH the master wrap and per-device wraps. Requires a
///   real age recipient set; if none are available we log and fall back to
///   master (never produce content this device cannot read back).
/// - `PerDevice` (CONTRACT): drop the master wrap. Gated behind a roll-call
///   probe — we refuse PerDevice and **fall back to Dual + warn loudly** unless
///   EVERY active (non-revoked) device carries a real age recipient. We also
///   fall back if the local device secret is unreadable.
///
/// Whenever the requested mode cannot be honored safely we degrade to the most
/// conservative mode that still keeps every device able to read (Dual, then
/// Master), and log why — we never silently drop the master fallback.
pub(crate) fn build_encryption_context(
    config: &TcfsConfig,
    device_id: &str,
    master_key: &tcfs_crypto::MasterKey,
) -> tcfs_sync::engine::EncryptionContext {
    use tcfs_core::config::WrapMode;
    use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext};

    let base = EncryptionContext::new(master_key.clone());
    let requested = config.crypto.wrap_mode;
    if requested == WrapMode::Master {
        return base;
    }

    let registry_path = config
        .sync
        .device_identity
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);
    // TIN-1417 B4: the recipient set MUST come from a signature-VERIFIED registry.
    // A tampered/unsigned registry must never source per-device recipients —
    // fall back to the shared master wrap (and warn) instead of wrapping to an
    // unverified, possibly-hostile recipient.
    let registry = match tcfs_secrets::device::DeviceRegistry::load_verified(
        &registry_path,
        master_key.as_bytes(),
    ) {
        Ok((r, tcfs_secrets::device::RegistryTrust::Signed)) => r,
        Ok((_, tcfs_secrets::device::RegistryTrust::UnsignedLegacy)) => {
            tracing::warn!(
                "wrap_mode={requested:?}: device registry is UNSIGNED (legacy); refusing to \
                 build a per-device recipient set from an unverified registry — using master \
                 wrap. Re-save the registry with a master-key command to sign it."
            );
            return base;
        }
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: device registry FAILED signature verification ({e}); \
                 refusing per-device recipients — using master wrap (fail-closed)"
            );
            return base;
        }
    };

    let recipients: Vec<tcfs_crypto::AgeFileKeyRecipient> = registry
        .active_devices()
        .filter(|d| tcfs_secrets::device::is_real_age_public_key(&d.public_key))
        .map(|d| tcfs_crypto::AgeFileKeyRecipient {
            device_id: d.device_id.clone(),
            recipient: d.public_key.clone(),
        })
        .collect();
    if recipients.is_empty() {
        tracing::warn!(
            "wrap_mode={requested:?} enabled but no active age recipients; using master wrap"
        );
        return base;
    }

    let secret_path = tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
    let identity = match std::fs::read_to_string(&secret_path) {
        Ok(s) => DeviceUnwrapIdentity {
            device_id: device_id.to_string(),
            secret: s.trim().to_string(),
        },
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: local device secret unreadable ({e}); using master wrap"
            );
            return base;
        }
    };

    // Roll-call gate: PerDevice (CONTRACT) drops the master wrap, so it is only
    // safe when EVERY active device can do per-device unwrap. If not, degrade to
    // Dual (keeps the master fallback) and warn loudly.
    let effective = resolve_wrap_mode_with_roll_call(requested, &registry);
    base.with_wrap_mode(effective, recipients, Some(identity))
}

/// Apply the roll-call gate to a requested wrap mode.
///
/// `PerDevice` is downgraded to `Dual` (with a loud warning) unless every active
/// device carries a real age recipient. `Master`/`Dual` pass through unchanged.
pub(crate) fn resolve_wrap_mode_with_roll_call(
    requested: tcfs_core::config::WrapMode,
    registry: &tcfs_secrets::device::DeviceRegistry,
) -> tcfs_core::config::WrapMode {
    use tcfs_core::config::WrapMode;
    if requested != WrapMode::PerDevice {
        return requested;
    }
    let roll_call = registry.roll_call();
    if roll_call.all_capable() {
        return WrapMode::PerDevice;
    }
    tracing::warn!(
        active = roll_call.active,
        capable = roll_call.capable,
        blockers = ?roll_call.incapable_devices,
        "wrap_mode=PerDevice REFUSED by roll-call gate: not every active device has a real \
         age recipient; falling back to Dual (keeping the master wrap) to avoid locking out \
         devices. Enroll real age recipients for the listed devices to enable true revocation."
    );
    WrapMode::Dual
}

/// Implementation of the TcfsDaemon gRPC service
pub struct TcfsDaemonImpl {
    cred_store: SharedCredStore,
    config: Arc<TcfsConfig>,
    storage_ok: bool,
    storage_endpoint: String,
    start_time: std::time::Instant,
    state_cache: Arc<TokioMutex<tcfs_sync::state::StateCache>>,
    operator: Arc<TokioMutex<Option<opendal::Operator>>>,
    device_id: String,
    device_name: String,
    master_key: Arc<TokioMutex<Option<tcfs_crypto::MasterKey>>>,
    nats_ok: std::sync::atomic::AtomicBool,
    nats: Arc<TokioMutex<Option<tcfs_sync::NatsClient>>>,
    active_mounts: Arc<TokioMutex<std::collections::HashMap<String, tokio::process::Child>>>,
    path_locks: tcfs_sync::state::PathLocks,
    data_dir: std::path::PathBuf,
    /// VFS handle from active FUSE mount — used to invalidate negative cache
    /// on NATS events so remote files appear in readdir immediately.
    pub vfs_handle: tokio::sync::watch::Receiver<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    vfs_tx: tokio::sync::watch::Sender<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    // Auth infrastructure
    session_store: tcfs_auth::SessionStore,
    device_authorizations: tcfs_auth::DeviceAuthorizationStore,
    invite_redemptions: tcfs_auth::InviteRedemptionStore,
    totp_provider: Arc<tcfs_auth::totp::TotpProvider>,
    webauthn_provider: Arc<tcfs_auth::webauthn::WebAuthnProvider>,
    rate_limiter: tcfs_auth::RateLimiter,
}

/// Validate a client-provided relative path before joining it under a tempdir.
fn sanitize_rel_path(path: &str) -> std::result::Result<String, String> {
    use std::path::Component;

    if path.is_empty() {
        return Err("path must not be empty".to_string());
    }

    let rel_path = Path::new(path);
    if rel_path.is_absolute() {
        return Err(format!("absolute path not allowed: {path}"));
    }

    for component in rel_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(format!("path traversal not allowed: {path}"));
        }
    }

    Ok(path.to_string())
}

fn normalize_existing_path_ancestor(path: &Path) -> std::path::PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    for ancestor in path.ancestors().skip(1) {
        let Ok(canonical_ancestor) = std::fs::canonicalize(ancestor) else {
            continue;
        };
        let Ok(missing_suffix) = path.strip_prefix(ancestor) else {
            continue;
        };
        return canonical_ancestor.join(missing_suffix);
    }
    path.to_path_buf()
}

fn logical_rel_path_from_state_key(
    key: &str,
    state: &tcfs_sync::state::SyncState,
    sync_root: Option<&Path>,
    storage_prefix: &str,
) -> Option<String> {
    if let Some(root) = sync_root {
        // StateCache keys canonicalize the parent while preserving the final
        // component. Apply the same identity rule to the configured root so
        // macOS aliases such as /var and /private/var cannot orphan otherwise
        // exact manifest-bound metadata.
        let normalized_root = normalize_existing_path_ancestor(root);
        let normalized_key = normalize_existing_path_ancestor(Path::new(key));
        if let Ok(rel) = normalized_key.strip_prefix(&normalized_root) {
            let rel = rel.to_string_lossy();
            let rel = rel.trim_start_matches('/');
            if !rel.is_empty() {
                return Some(rel.to_string());
            }
        }
    }

    let index_prefix = format!("{}/index/", storage_prefix.trim_end_matches('/'));
    state
        .remote_path
        .strip_prefix(&index_prefix)
        .map(|rel| rel.trim_start_matches('/'))
        .filter(|rel| !rel.is_empty())
        .map(ToOwned::to_owned)
}

fn list_files_remainder<'a>(rel_path: &'a str, requested_prefix: &str) -> Option<&'a str> {
    if requested_prefix.is_empty() {
        return Some(rel_path);
    }
    rel_path
        .strip_prefix(requested_prefix)
        .and_then(|remainder| remainder.strip_prefix('/'))
        .filter(|remainder| !remainder.is_empty())
}

fn hydration_state_name(status: tcfs_sync::state::FileSyncStatus) -> &'static str {
    match status {
        tcfs_sync::state::FileSyncStatus::NotSynced => "not_synced",
        tcfs_sync::state::FileSyncStatus::Synced => "synced",
        tcfs_sync::state::FileSyncStatus::Active => "active",
        tcfs_sync::state::FileSyncStatus::Locked => "locked",
        tcfs_sync::state::FileSyncStatus::Conflict => "conflict",
    }
}

fn cached_state_matches_remote_manifest(
    state: &tcfs_sync::state::SyncState,
    manifest_key: &str,
) -> bool {
    state.remote_path == manifest_key
        || state
            .conflict
            .as_ref()
            .and_then(|conflict| conflict.remote_manifest_key.as_deref())
            == Some(manifest_key)
}

fn logical_rel_path_from_fs_path(path: &Path, sync_root: Option<&Path>) -> String {
    if let Some(root) = sync_root {
        if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel.to_string_lossy();
            let rel = rel.trim_start_matches('/');
            return rel.to_string();
        }
    }

    path.to_string_lossy().to_string()
}

fn normalize_watch_root(path: &str) -> String {
    path.trim_matches('/').to_string()
}

fn rel_path_matches_watch_roots(rel_path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        if root.is_empty() {
            true
        } else {
            rel_path == root
                || rel_path
                    .strip_prefix(root)
                    .is_some_and(|r| r.starts_with('/'))
        }
    })
}

impl TcfsDaemonImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cred_store: SharedCredStore,
        config: Arc<TcfsConfig>,
        storage_ok: bool,
        storage_endpoint: String,
        state_cache: Arc<TokioMutex<tcfs_sync::state::StateCache>>,
        operator: Arc<TokioMutex<Option<opendal::Operator>>>,
        path_locks: tcfs_sync::state::PathLocks,
        device_id: String,
        device_name: String,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> Self {
        let (vfs_tx, vfs_rx) = tokio::sync::watch::channel(None);

        let totp_config = tcfs_auth::totp::TotpConfig {
            issuer: config.auth.totp.issuer.clone(),
            digits: config.auth.totp.digits as usize,
            ..tcfs_auth::totp::TotpConfig::default()
        };
        let totp_provider = Arc::new(tcfs_auth::totp::TotpProvider::new(totp_config));

        let webauthn_config = tcfs_auth::webauthn::WebAuthnConfig {
            rp_name: config.auth.webauthn.relying_party_name.clone(),
            rp_id: config.auth.webauthn.relying_party_id.clone(),
            rp_origin: format!("https://{}", config.auth.webauthn.relying_party_id),
        };
        let webauthn_provider = Arc::new(
            tcfs_auth::webauthn::WebAuthnProvider::new(webauthn_config).unwrap_or_else(|e| {
                tracing::warn!("WebAuthn provider init failed: {e}, using defaults");
                tcfs_auth::webauthn::WebAuthnProvider::new(
                    tcfs_auth::webauthn::WebAuthnConfig::default(),
                )
                .expect("default WebAuthn config should always work")
            }),
        );

        let rate_limiter = tcfs_auth::RateLimiter::new(tcfs_auth::RateLimitConfig {
            max_attempts: config.auth.rate_limit.max_attempts,
            lockout_duration: chrono::Duration::seconds(config.auth.rate_limit.lockout_secs as i64),
            backoff_multiplier: config.auth.rate_limit.backoff_multiplier,
        });

        let data_dir = dirs::data_dir().unwrap_or_default().join("tcfsd");

        Self {
            cred_store,
            config,
            storage_ok,
            storage_endpoint,
            start_time: std::time::Instant::now(),
            state_cache,
            operator,
            device_id,
            device_name,
            data_dir,
            master_key: Arc::new(TokioMutex::new(master_key)),
            nats_ok: std::sync::atomic::AtomicBool::new(false),
            nats: Arc::new(TokioMutex::new(None)),
            active_mounts: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            path_locks,
            vfs_handle: vfs_rx,
            vfs_tx,
            session_store: tcfs_auth::SessionStore::new(),
            device_authorizations: tcfs_auth::DeviceAuthorizationStore::new(),
            invite_redemptions: tcfs_auth::InviteRedemptionStore::new(),
            totp_provider,
            webauthn_provider,
            rate_limiter,
        }
    }

    fn registered_root(
        &self,
        root_id: &str,
    ) -> std::result::Result<RegisteredRootRoute, tonic::Status> {
        validate_root_id(root_id).map_err(tonic::Status::invalid_argument)?;
        validate_registered_roots_config(&self.config)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let Some(root) = self.config.sync.roots.get(root_id) else {
            return Err(tonic::Status::not_found("registered root was not found"));
        };
        canonical_registered_root(&self.config, root_id, root)
            .map_err(tonic::Status::failed_precondition)
    }

    /// Registered roots are an authority boundary and must never inherit the
    /// daemon's development-only synthetic admin session.
    fn require_registered_root_auth_posture(&self) -> Result<(), tonic::Status> {
        if self.config.auth.require_session {
            Ok(())
        } else {
            Err(tonic::Status::failed_precondition(
                "registered-root operations require auth.require_session = true",
            ))
        }
    }

    /// Resolve a registered root only after its configured prefix is known to
    /// be in the caller's scope. Unknown and unauthorized IDs deliberately
    /// share one response so the registry cannot be enumerated.
    fn authorized_registered_root(
        &self,
        session: &tcfs_auth::Session,
        root_id: &str,
    ) -> std::result::Result<RegisteredRootRoute, tonic::Status> {
        validate_root_id(root_id).map_err(tonic::Status::invalid_argument)?;
        let not_found = || tonic::Status::not_found("registered root was not found");
        let Some(root) = self.config.sync.roots.get(root_id) else {
            return Err(not_found());
        };
        if !registered_prefix_allowed(session, &root.remote_prefix) {
            return Err(not_found());
        }
        self.registered_root(root_id)
    }

    async fn resolve_registered_git_keep_both_repo(
        &self,
        route: &RegisteredRootRoute,
        path: &Path,
        mode: tcfs_sync::conflict_git::GitKeepBothMode,
    ) -> Result<tonic::Response<ResolveRegisteredRootResponse>, tonic::Status> {
        if mode.is_execute() && route.policy != RegisteredRootPolicy::Resolve {
            return Err(tonic::Status::permission_denied(format!(
                "registered root '{}' policy is inspect-only; execute requires policy = \"resolve\"",
                route.root_id
            )));
        }

        let request_root = std::fs::canonicalize(path).map_err(|error| {
            tonic::Status::failed_precondition(format!(
                "canonicalizing requested repo root {}: {error}",
                path.display()
            ))
        })?;
        if request_root != route.local_root {
            return Err(tonic::Status::permission_denied(format!(
                "requested repo {} does not equal registered root '{}' local_root {}",
                request_root.display(),
                route.root_id,
                route.local_root.display()
            )));
        }

        let repo_anchor = tcfs_sync::conflict_git::GitRepoAnchor::capture(&route.local_root)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;

        // A direct gRPC client must not bypass the CLI's `.git` directory
        // check and send the resolver into shared worktree metadata.
        validate_standalone_git_root(route).map_err(tonic::Status::failed_precondition)?;

        let op = {
            let op_guard = self.operator.lock().await;
            op_guard
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };
        let enc_ctx = {
            let mk_guard = self.master_key.lock().await;
            mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk))
        };

        // Coordinate with `tcfs reconcile --state <this cache>` for the full
        // read/resolve/flush transaction. The lock is deliberately held across
        // remote reads and git safety checks so no stale plan can overwrite the
        // resolved snapshot.
        let _state_lock = tcfs_sync::state::StateFileLock::acquire(&route.state_path)
            .map_err(|error| tonic::Status::aborted(error.to_string()))?;
        let mut state = tcfs_sync::state::StateCache::open(&route.state_path)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        validate_conflict_cache_route(route, &state).map_err(tonic::Status::failed_precondition)?;

        let undo_state_dir = route
            .state_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        let result = tcfs_sync::conflict_git::resolve_repo_keep_both(
            &op,
            &mut state,
            &route.local_root,
            Some(&repo_anchor),
            &route.remote_prefix,
            &self.device_id,
            &undo_state_dir,
            mode,
            enc_ctx.as_ref(),
        )
        .await;

        match result {
            Ok(result) => {
                if mode.is_execute() {
                    // Existing NATS state events do not carry root identity.
                    // Publishing this path would let primary-root consumers
                    // apply it under the wrong local/prefix namespace.
                    tracing::info!(
                        root_id = %route.root_id,
                        "registered-root conflict resolved; suppressing non-root-aware NATS event"
                    );
                }
                let remaining_conflicts = state.conflicts().len();
                let summary = if mode.is_execute() && remaining_conflicts > 0 {
                    format!(
                        "{}; WARNING: {remaining_conflicts} named-root conflict(s) remain; inspect `tcfs conflicts --root {}` before claiming convergence",
                        result.summary(),
                        route.root_id
                    )
                } else {
                    result.summary()
                };
                Ok(tonic::Response::new(ResolveRegisteredRootResponse {
                    success: true,
                    resolved_path: result.repo_root.display().to_string(),
                    error: summary,
                    root_id: route.root_id.clone(),
                    local_root: route.local_root.display().to_string(),
                    remote_prefix: route.remote_prefix.clone(),
                    state_path: route.state_path.display().to_string(),
                }))
            }
            Err(error) => Ok(tonic::Response::new(ResolveRegisteredRootResponse {
                success: false,
                resolved_path: String::new(),
                error: error.to_string(),
                root_id: route.root_id.clone(),
                local_root: route.local_root.display().to_string(),
                remote_prefix: route.remote_prefix.clone(),
                state_path: route.state_path.display().to_string(),
            })),
        }
    }

    async fn resolve_git_keep_both_repo(
        &self,
        path: &Path,
        mode: tcfs_sync::conflict_git::GitKeepBothMode,
    ) -> Result<tonic::Response<ResolveConflictResponse>, tonic::Status> {
        // The compatibility RPC remains available for proven primary-cache
        // conflicts, but it must bind the authorized path before any await.
        // Registered roots are fenced out by the caller and use their own RPC.
        let repo_anchor = tcfs_sync::conflict_git::GitRepoAnchor::capture(path)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let op = {
            let op_guard = self.operator.lock().await;
            op_guard
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };
        let prefix = self.config.storage.resolved_prefix().to_string();
        let undo_state_dir = undo_bundle_state_dir(&self.config);
        let enc_ctx = {
            let mk_guard = self.master_key.lock().await;
            mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk))
        };

        let result = {
            let mut cache = self.state_cache.lock().await;
            if let Err(e) = cache.reload_from_disk() {
                tracing::warn!("failed to reload state cache: {e}");
            }
            tcfs_sync::conflict_git::resolve_repo_keep_both(
                &op,
                &mut cache,
                path,
                Some(&repo_anchor),
                &prefix,
                &self.device_id,
                &undo_state_dir,
                mode,
                enc_ctx.as_ref(),
            )
            .await
        };

        match result {
            Ok(result) => {
                if mode.is_execute() {
                    self.publish_conflict_resolved(&path.to_string_lossy(), "git_keep_both")
                        .await;
                }
                Ok(tonic::Response::new(ResolveConflictResponse {
                    success: true,
                    resolved_path: result.repo_root.display().to_string(),
                    error: result.summary(),
                }))
            }
            Err(e) => Ok(tonic::Response::new(ResolveConflictResponse {
                success: false,
                resolved_path: String::new(),
                error: format!("{e:#}"),
            })),
        }
    }

    async fn enrollment_bootstrap_for_invite(
        &self,
        invite: &tcfs_auth::EnrollmentInvite,
        bootstrap_session: &tcfs_auth::Session,
    ) -> Result<tcfs_auth::EnrollmentBootstrap, tonic::Status> {
        let (storage_access_key, storage_secret_key) =
            self.enrollment_storage_credentials(invite).await;
        if storage_access_key.is_none() || storage_secret_key.is_none() {
            return Err(tonic::Status::failed_precondition(
                "storage credentials unavailable for enrollment bootstrap",
            ));
        }

        let master_key_base64 = {
            let master = self.master_key.lock().await;
            let master = master.as_ref().ok_or_else(|| {
                tonic::Status::failed_precondition(
                    "daemon master key not loaded — cannot wrap enrollment bootstrap",
                )
            })?;
            base64::engine::general_purpose::STANDARD.encode(master.as_bytes())
        };

        Ok(tcfs_auth::EnrollmentBootstrap {
            nats_url: invite
                .nats_url
                .clone()
                .or_else(|| Some(self.config.sync.nats_url.clone()).filter(|url| !url.is_empty())),
            storage_endpoint: invite.storage_endpoint.clone().or_else(|| {
                Some(self.config.storage.endpoint.clone()).filter(|url| !url.is_empty())
            }),
            storage_bucket: invite.storage_bucket.clone().or_else(|| {
                Some(self.config.storage.bucket.clone()).filter(|bucket| !bucket.is_empty())
            }),
            storage_access_key,
            storage_secret_key,
            remote_prefix: invite
                .remote_prefix
                .clone()
                .or_else(|| Some(self.config.storage.resolved_prefix().to_string())),
            master_key_base64: Some(master_key_base64),
            encryption_salt: invite
                .encryption_salt
                .clone()
                .or_else(|| self.config.crypto.kdf_salt.clone()),
            session_token: Some(bootstrap_session.token.clone()),
            session_expires_at: bootstrap_session
                .expires_at
                .map(|expires_at| expires_at.timestamp()),
        })
    }

    async fn enrollment_storage_credentials(
        &self,
        invite: &tcfs_auth::EnrollmentInvite,
    ) -> (Option<String>, Option<String>) {
        if invite.storage_access_key.is_some() && invite.storage_secret_key.is_some() {
            return (
                invite.storage_access_key.clone(),
                invite.storage_secret_key.clone(),
            );
        }

        let store = self.cred_store.read().await;
        if let Some(s3) = store.as_ref().and_then(|store| store.s3.as_ref()) {
            return (
                Some(s3.access_key_id.clone()),
                Some(s3.secret_access_key.expose_secret().to_string()),
            );
        }

        (
            invite.storage_access_key.clone(),
            invite.storage_secret_key.clone(),
        )
    }

    /// Get a clone of the session store (for background tasks).
    pub fn session_store(&self) -> tcfs_auth::SessionStore {
        self.session_store.clone()
    }

    #[cfg(test)]
    fn with_data_dir(mut self, data_dir: std::path::PathBuf) -> Self {
        self.data_dir = data_dir;
        self
    }

    /// Load persisted TOTP credentials from disk.
    pub async fn load_totp_credentials(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.totp_provider.load_from_file(path).await
    }

    /// Load persisted sessions from disk.
    pub async fn load_sessions(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.session_store.load_from_file(path).await
    }

    /// Load persisted invite redemptions from disk.
    pub async fn load_invite_redemptions(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.invite_redemptions.load_from_file(path).await
    }

    /// Load enrollment-derived device authorizations from disk.
    pub async fn load_device_authorizations(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.device_authorizations.load_from_file(path).await
    }

    /// Save sessions to disk (called after session changes).
    async fn persist_sessions(&self) {
        let path = self.data_dir.join("sessions.json");
        if let Err(e) = self.session_store.save_to_file(&path).await {
            tracing::warn!("failed to persist sessions: {e}");
        }
    }

    /// Save invite redemptions to disk (called after successful invite use).
    async fn persist_invite_redemptions(&self) -> anyhow::Result<()> {
        let path = self.data_dir.join("invite-redemptions.json");
        self.invite_redemptions.save_to_file(&path).await
    }

    async fn persist_device_authorizations(&self) -> anyhow::Result<()> {
        let path = self.data_dir.join("device-authorizations.json");
        self.device_authorizations.save_to_file(&path).await
    }

    /// The daemon's own enrolled identity is the only safe bootstrap admin.
    /// Every other device must arrive through a signed invite.
    pub async fn ensure_local_device_authorization(&self) -> anyhow::Result<()> {
        if self
            .device_authorizations
            .get(&self.device_id)
            .await
            .is_none()
        {
            self.device_authorizations
                .authorize(
                    self.device_id.clone(),
                    self.device_name.clone(),
                    tcfs_auth::DevicePermissions::admin(),
                )
                .await;
            self.persist_device_authorizations().await?;
        }
        Ok(())
    }

    /// Validate a session token from gRPC request metadata.
    ///
    /// Returns Ok(Session) if the session is valid, or a gRPC UNAUTHENTICATED
    /// error if auth is required and the token is missing/invalid/expired.
    ///
    /// When `config.auth.require_session` is false, this returns a synthetic
    /// session with full permissions (bypass mode). A warning is logged on
    /// each bypassed request.
    async fn require_session<T>(
        &self,
        request: &tonic::Request<T>,
    ) -> Result<tcfs_auth::Session, tonic::Status> {
        if !self.config.auth.require_session {
            tracing::warn!(
                "AUTH BYPASS: request granted full permissions — \
                 set auth.require_session=true for production"
            );
            return Ok(tcfs_auth::Session::new(&self.device_id, "local", "bypass")
                .with_permissions(tcfs_auth::DevicePermissions::admin()));
        }

        // Extract token from "authorization" metadata
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_string());

        match token {
            Some(t) => match self.session_store.validate(&t).await {
                Some(mut session) => {
                    let authorization = self
                        .device_authorizations
                        .get(&session.device_id)
                        .await
                        .ok_or_else(|| {
                            tonic::Status::unauthenticated("session device is no longer enrolled")
                        })?;
                    // Enrollment authority is live truth. Persisted sessions
                    // cannot retain grants after a device is scoped or revoked.
                    session.device_name = authorization.device_name;
                    session.permissions = authorization.permissions;
                    Ok(session)
                }
                None => Err(tonic::Status::unauthenticated(
                    "invalid or expired session token",
                )),
            },
            None => Err(tonic::Status::unauthenticated(
                "session token required — run 'tcfs auth verify' first",
            )),
        }
    }

    /// Check that the session has the required permission, returning
    /// PERMISSION_DENIED if not.
    fn check_permission(
        session: &tcfs_auth::Session,
        permission: &str,
    ) -> Result<(), tonic::Status> {
        let allowed = match permission {
            "mount" => session.permissions.can_mount,
            "push" => session.permissions.can_push,
            "pull" => session.permissions.can_pull,
            "admin" => session.permissions.can_admin,
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(tonic::Status::permission_denied(format!(
                "device {} lacks '{}' permission",
                session.device_id, permission
            )))
        }
    }

    /// Get a handle to the state cache for shutdown flushing.
    pub fn state_cache_handle(&self) -> Arc<TokioMutex<tcfs_sync::state::StateCache>> {
        self.state_cache.clone()
    }

    /// Get a handle to the NATS client for shutdown notification.
    pub fn nats_handle(&self) -> Arc<TokioMutex<Option<tcfs_sync::NatsClient>>> {
        self.nats.clone()
    }

    /// Get a handle to the master key for background tasks (e.g., periodic reconciliation).
    pub fn master_key_handle(&self) -> Arc<TokioMutex<Option<tcfs_crypto::MasterKey>>> {
        self.master_key.clone()
    }

    fn lock_path_for_request(&self, path: &Path) -> std::path::PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        if let Some(root) = self.config.sync.sync_root.as_deref() {
            return root.join(path);
        }
        path.to_path_buf()
    }

    /// Publish a ConflictResolved event via NATS (best-effort).
    async fn publish_conflict_resolved(&self, rel_path: &str, resolution: &str) {
        if let Some(nats) = self.nats.lock().await.as_ref() {
            // Build merged vclock from state cache
            let merged_vclock = {
                let cache = self.state_cache.lock().await;
                let path = std::path::PathBuf::from(rel_path);
                cache
                    .get(&path)
                    .map(|e| e.vclock.clone())
                    .unwrap_or_default()
            };

            let event = tcfs_sync::StateEvent::ConflictResolved {
                device_id: self.device_id.clone(),
                rel_path: rel_path.to_string(),
                resolution: resolution.to_string(),
                merged_vclock,
                timestamp: tcfs_sync::StateEvent::now(),
            };
            if let Err(e) = nats.publish_state_event(&event).await {
                tracing::warn!("failed to publish ConflictResolved: {e}");
            }
        }
    }

    /// Set the NATS client (called from daemon after connecting).
    pub fn set_nats(&self, client: tcfs_sync::NatsClient) {
        // set_nats_ok is implicitly true if we have a client
        self.nats_ok
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // We need a runtime handle since this might be called from sync context
        // but the Mutex is tokio::sync::Mutex, so just use block_in_place
        let nats = self.nats.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async {
                *nats.lock().await = Some(client);
            });
        });
    }
}

#[tonic::async_trait]
impl TcfsDaemon for TcfsDaemonImpl {
    async fn status(
        &self,
        _request: tonic::Request<StatusRequest>,
    ) -> Result<tonic::Response<StatusResponse>, tonic::Status> {
        let uptime = self.start_time.elapsed().as_secs() as i64;
        let mount_count = self.active_mounts.lock().await.len() as i32;
        let storage_prefix = self.config.storage.resolved_prefix().to_string();
        let operator = self.operator.lock().await.as_ref().cloned();
        let storage_ok = match operator {
            Some(op) => {
                match tcfs_storage::check_health_for_prefix_detailed(&op, &storage_prefix).await {
                    Ok(report) => {
                        tracing::debug!(
                            health_path = %report.path,
                            elapsed_ms = report.elapsed_ms,
                            entry_count = report.entry_count,
                            "status storage health probe passed"
                        );
                        true
                    }
                    Err(err) => {
                        warn!(
                            health_kind = %err.kind(),
                            health_path = %err.path(),
                            elapsed_ms = err.elapsed_ms(),
                            backend_kind = err.backend_kind().unwrap_or("none"),
                            "status storage health probe failed: {err}"
                        );
                        false
                    }
                }
            }
            None => false,
        };

        Ok(tonic::Response::new(StatusResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            storage_endpoint: sanitize_http_endpoint_for_display(&self.storage_endpoint),
            storage_ok,
            nats_ok: self.nats_ok.load(std::sync::atomic::Ordering::Relaxed),
            active_mounts: mount_count,
            uptime_secs: uptime,
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            conflict_mode: self.config.sync.conflict_mode.clone(),
        }))
    }

    async fn credential_status(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<CredentialStatusResponse>, tonic::Status> {
        let store = self.cred_store.read().await;
        match store.as_ref() {
            Some(cs) => Ok(tonic::Response::new(CredentialStatusResponse {
                loaded: true,
                source: cs.source.clone(),
                loaded_at: 0,
                needs_reload: false,
            })),
            None => Ok(tonic::Response::new(CredentialStatusResponse {
                loaded: false,
                source: "none".into(),
                loaded_at: 0,
                needs_reload: true,
            })),
        }
    }

    async fn mount(
        &self,
        request: tonic::Request<MountRequest>,
    ) -> Result<tonic::Response<MountResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
        let req = request.into_inner();

        if req.mountpoint.is_empty() || req.remote.is_empty() {
            return Ok(tonic::Response::new(MountResponse {
                success: false,
                error: "mountpoint and remote are required".into(),
            }));
        }

        let mountpoint = std::path::PathBuf::from(&req.mountpoint);

        // Check not already mounted
        {
            let mounts = self.active_mounts.lock().await;
            if mounts.contains_key(&req.mountpoint) {
                return Ok(tonic::Response::new(MountResponse {
                    success: false,
                    error: format!("already mounted at: {}", req.mountpoint),
                }));
            }
        }

        // Ensure mountpoint directory exists
        if !mountpoint.exists() {
            std::fs::create_dir_all(&mountpoint).map_err(|e| {
                tonic::Status::internal(format!("create mountpoint {}: {e}", req.mountpoint))
            })?;
        }

        // Get the storage operator from daemon state
        let op = {
            let guard = self.operator.lock().await;
            guard
                .clone()
                .ok_or_else(|| tonic::Status::unavailable("storage operator not initialized"))?
        };

        // Parse prefix from remote spec
        let (endpoint, _bucket, prefix) = tcfs_storage::parse_remote_spec(&req.remote)
            .map_err(|e| tonic::Status::invalid_argument(format!("bad remote spec: {e}")))?;
        let endpoint_display = sanitize_http_endpoint_for_display(&endpoint);
        let use_nfs = req.options.iter().any(|o| o == "nfs");
        let backend = if use_nfs { "NFS loopback" } else { "FUSE" };

        info!(
            mountpoint = %req.mountpoint,
            endpoint = %endpoint_display,
            backend = %backend,
            "spawning mount"
        );

        let mp = mountpoint.clone();
        let cache_dir = self.config.fuse.cache_dir.clone();
        let cache_max = self.config.fuse.cache_max_mb * 1024 * 1024;
        let neg_ttl = self.config.fuse.negative_cache_ttl_secs;
        let mountpoint_key = req.mountpoint.clone();
        let active_mounts_watcher = self.active_mounts.clone();

        if use_nfs {
            // NFS loopback (fallback — use --nfs flag or "nfs" option)
            let mount_handle = tokio::spawn(async move {
                tracing::info!("NFS mount task starting");
                match tcfs_nfs::serve_and_mount(tcfs_nfs::NfsMountConfig {
                    op,
                    prefix,
                    mountpoint: mp,
                    port: 0,
                    cache_dir: std::path::PathBuf::from(&cache_dir),
                    cache_max_bytes: cache_max,
                    negative_ttl_secs: neg_ttl,
                })
                .await
                {
                    Ok(()) => tracing::warn!("NFS serve_and_mount returned Ok"),
                    Err(e) => tracing::error!(error = %e, "NFS mount failed"),
                }
            });

            let mk = mountpoint_key.clone();
            tokio::spawn(async move {
                let _ = mount_handle.await;
                active_mounts_watcher.lock().await.remove(&mk);
            });
        } else {
            // FUSE3 (default — unprivileged mount via fusermount3)
            //
            // Wire NATS publish callback: when a file is flushed to S3 via
            // FUSE write, notify other hosts so their FUSE mounts pick it up.
            let nats_handle = self.nats.clone();
            let flush_device_id = self.device_id.clone();
            let mount_device_id = self.device_id.clone();
            let flush_prefix = prefix.clone();
            let mount_master_key = self.master_key.clone();
            let encryption_required = self.config.crypto.enabled;
            let on_flush: Option<tcfs_vfs::OnFlushCallback> = Some(std::sync::Arc::new(
                move |vpath: &str,
                      file_hash: &str,
                      manifest_object_id: &str,
                      size: u64,
                      _chunks: usize,
                      vclock: &tcfs_sync::conflict::VectorClock| {
                    let path = match tcfs_vfs::virtual_path_to_canonical_rel_path(vpath) {
                        Ok(path) => path.to_string(),
                        Err(error) => {
                            tracing::warn!(
                                path = %vpath,
                                %error,
                                "refusing to publish invalid VFS flush path"
                            );
                            return;
                        }
                    };
                    let nats = nats_handle.clone();
                    let device = flush_device_id.clone();
                    let file_hash = file_hash.to_string();
                    let manifest_object_id = manifest_object_id.to_string();
                    let vclock = vclock.clone();
                    let pfx = flush_prefix.clone();
                    tokio::spawn(async move {
                        if let Some(ref client) = *nats.lock().await {
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id: device,
                                rel_path: path.clone(),
                                blake3: file_hash,
                                size,
                                vclock,
                                manifest_path: format!("{}/manifests/{}", pfx, manifest_object_id),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            };
                            if let Err(e) = client.publish_state_event(&event).await {
                                tracing::warn!(path = %path, "NATS publish on FUSE flush failed: {e}");
                            } else {
                                tracing::debug!(path = %path, "NATS FileSynced published from FUSE write");
                            }
                        }
                    });
                },
            ));

            let vfs_sender = self.vfs_tx.clone();
            let mount_handle = tokio::spawn(async move {
                tracing::info!("FUSE mount task starting");
                match tcfs_fuse::mount(
                    tcfs_fuse::MountConfig {
                        op,
                        prefix,
                        mountpoint: mp,
                        cache_dir: std::path::PathBuf::from(&cache_dir),
                        cache_max_bytes: cache_max,
                        negative_ttl_secs: neg_ttl,
                        read_only: req.read_only,
                        allow_other: false,
                        on_flush,
                        device_id: mount_device_id,
                        master_key: Some(mount_master_key),
                        encryption_required,
                    },
                    Some(&vfs_sender),
                )
                .await
                {
                    Ok(()) => tracing::info!("FUSE mount unmounted cleanly"),
                    Err(e) => tracing::error!(error = %e, "FUSE mount failed"),
                }
                // Clear VFS handle on unmount
                let _ = vfs_sender.send(None);
            });

            let mk = mountpoint_key.clone();
            tokio::spawn(async move {
                let _ = mount_handle.await;
                active_mounts_watcher.lock().await.remove(&mk);
            });
        }

        // Give the mount a moment to establish
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Record as active
        {
            let mut mounts = self.active_mounts.lock().await;
            mounts.entry(req.mountpoint.clone()).or_insert_with(|| {
                tokio::process::Command::new("sleep")
                    .arg("infinity")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn sentinel")
            });
        }

        Ok(tonic::Response::new(MountResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn unmount(
        &self,
        request: tonic::Request<UnmountRequest>,
    ) -> Result<tonic::Response<UnmountResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
        let req = request.into_inner();
        if req.mountpoint.is_empty() {
            return Ok(tonic::Response::new(UnmountResponse {
                success: false,
                error: "mountpoint is required".into(),
            }));
        }

        info!(mountpoint = %req.mountpoint, "unmount requested");

        // Try fusermount3 first, fallback to fusermount
        let result = tokio::process::Command::new("fusermount3")
            .args(["-u", &req.mountpoint])
            .output()
            .await;

        let ok = match result {
            Ok(output) if output.status.success() => true,
            _ => {
                // Fallback to fusermount
                match tokio::process::Command::new("fusermount")
                    .args(["-u", &req.mountpoint])
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => true,
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        return Ok(tonic::Response::new(UnmountResponse {
                            success: false,
                            error: format!("fusermount failed: {stderr}"),
                        }));
                    }
                    Err(e) => {
                        return Ok(tonic::Response::new(UnmountResponse {
                            success: false,
                            error: format!("neither fusermount3 nor fusermount available: {e}"),
                        }));
                    }
                }
            }
        };

        if ok {
            // Remove from active mounts and kill child if still running
            let mut mounts = self.active_mounts.lock().await;
            if let Some(mut child) = mounts.remove(&req.mountpoint) {
                let _ = child.kill().await;
            }

            info!(mountpoint = %req.mountpoint, "unmounted");
        }

        Ok(tonic::Response::new(UnmountResponse {
            success: ok,
            error: String::new(),
        }))
    }

    // ── Push: client-streaming upload ─────────────────────────────────────

    type PushStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<PushProgress, tonic::Status>> + Send>,
    >;

    async fn push(
        &self,
        request: tonic::Request<tonic::Streaming<PushChunk>>,
    ) -> Result<tonic::Response<Self::PushStream>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "push")?;
        use tokio_stream::StreamExt;

        let state_cache = self.state_cache.clone();
        let prefix = self.config.storage.resolved_prefix().to_string();

        let mut stream = request.into_inner();

        // Collect the streamed chunks into a file buffer
        let mut path: Option<String> = None;
        let mut data = Vec::new();
        let mut saw_last = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if saw_last {
                return Err(tonic::Status::invalid_argument(
                    "push stream included data after its terminal chunk",
                ));
            }
            match path.as_deref() {
                None => {
                    if chunk.path.is_empty() {
                        return Err(tonic::Status::invalid_argument(
                            "first push chunk must include a non-empty path",
                        ));
                    }
                    path = Some(chunk.path.clone());
                }
                Some(expected_path) if expected_path != chunk.path => {
                    return Err(tonic::Status::invalid_argument(
                        "push stream changed paths between chunks",
                    ));
                }
                Some(_) => {}
            }
            if chunk.offset != data.len() as u64 {
                return Err(tonic::Status::invalid_argument(format!(
                    "push chunk offset {} does not match assembled length {}",
                    chunk.offset,
                    data.len()
                )));
            }
            data.extend_from_slice(&chunk.data);
            saw_last = chunk.last;
        }

        let path =
            path.ok_or_else(|| tonic::Status::invalid_argument("no path provided in push stream"))?;
        if !saw_last {
            return Err(tonic::Status::invalid_argument(
                "push stream ended before its terminal chunk",
            ));
        }

        let path = sanitize_rel_path(&path).map_err(tonic::Status::invalid_argument)?;
        validate_fixed_ingress_path(Path::new(&path), "push logical path")
            .map_err(tonic::Status::invalid_argument)?;
        if let Some(sync_root) = self.config.sync.sync_root.as_deref() {
            let logical_target = tcfs_core::config::expand_tilde(sync_root).join(&path);
            tcfs_core::config::validate_sync_selection_excludes_master_key(
                &self.config,
                &logical_target,
            )
            .map_err(tonic::Status::invalid_argument)?;
        }

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
        let op = op.clone();

        let lock_path = self.lock_path_for_request(Path::new(&path));
        let _lock_guard = self.path_locks.lock(&lock_path).await;

        // Write to a temp file and upload via sync engine
        let tmp_dir =
            tempfile::tempdir().map_err(|e| tonic::Status::internal(format!("tempdir: {e}")))?;
        let local_path = tmp_dir.path().join(&path);
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| tonic::Status::internal(format!("mkdir: {e}")))?;
        }
        std::fs::write(&local_path, &data)
            .map_err(|e| tonic::Status::internal(format!("write temp: {e}")))?;

        let total_bytes = data.len() as u64;
        let device_id = self.device_id.clone();

        // Normalize the path for consistent S3 index keys (matches pull's resolve_manifest_path)
        let sync_root = self.config.sync.sync_root.as_deref();
        let normalized_rel =
            tcfs_sync::engine::normalize_rel_path(std::path::Path::new(&path), sync_root);

        let result = {
            let mut cache = state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk));
            tcfs_sync::engine::upload_file_with_device(
                &op,
                &local_path,
                &prefix,
                &mut cache,
                None,
                &device_id,
                Some(&normalized_rel),
                enc_ctx.as_ref(),
            )
            .await
        };

        match result {
            Ok(upload) => {
                // Record conflict in state cache if detected
                if let Some(tcfs_sync::conflict::SyncOutcome::Conflict(ref info)) = upload.outcome {
                    tracing::warn!(
                        path = %path,
                        local_device = %info.local_device,
                        remote_device = %info.remote_device,
                        "push: conflict detected"
                    );
                    let mut cache = state_cache.lock().await;
                    if cache.mark_conflict(&local_path, info.clone()) {
                        let _ = cache.flush();
                    }
                }

                if let Some(error) =
                    legacy_push_rejection_reason(upload.skipped, upload.outcome.as_ref())
                {
                    let progress = PushProgress {
                        bytes_sent: 0,
                        total_bytes,
                        chunk_hash: String::new(),
                        done: true,
                        error,
                    };
                    return Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                        progress,
                    )))));
                }

                // Rel-path publication is owned by upload_file_with_device so
                // the daemon follows the same crash-aware manifest/index flow
                // as CLI and tree push.

                // Publish state event if NATS is connected and file was actually uploaded
                if !upload.skipped {
                    // Read the actual vclock from state cache (keyed by temp local_path)
                    let vclock = {
                        let cache = state_cache.lock().await;
                        cache
                            .get(&local_path)
                            .map(|e| e.vclock.clone())
                            .unwrap_or_default()
                    };

                    let nats = self.nats.clone();
                    let device_id = self.device_id.clone();
                    let rel_path = path.clone();
                    let blake3 = upload.hash.clone();
                    let size = total_bytes;
                    let remote_path = upload.remote_path.clone();
                    tokio::spawn(async move {
                        if let Some(nats) = nats.lock().await.as_ref() {
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id,
                                rel_path,
                                blake3,
                                size,
                                vclock,
                                manifest_path: remote_path,
                                timestamp: tcfs_sync::StateEvent::now(),
                            };
                            if let Err(e) = nats.publish_state_event(&event).await {
                                tracing::warn!("failed to publish state event: {e}");
                            }
                        }
                    });
                }

                let progress = PushProgress {
                    bytes_sent: total_bytes,
                    total_bytes,
                    chunk_hash: upload.hash,
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                let progress = PushProgress {
                    bytes_sent: 0,
                    total_bytes,
                    chunk_hash: String::new(),
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Pull: server-streaming download ───────────────────────────────────

    type PullStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<PullProgress, tonic::Status>> + Send>,
    >;

    type PullExactStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<PullProgress, tonic::Status>> + Send>,
    >;

    async fn pull_exact(
        &self,
        request: tonic::Request<PullExactRequest>,
    ) -> Result<tonic::Response<Self::PullExactStream>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();

        validate_fixed_ingress_path(Path::new(&req.remote_path), "PullExact logical path")
            .map_err(tonic::Status::invalid_argument)?;
        if req.expected_version.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "PullExact expected_version must be non-empty",
            ));
        }

        let op = {
            let operator = self.operator.lock().await;
            operator
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };
        let prefix = self.config.storage.resolved_prefix().to_string();
        let selected =
            tcfs_sync::engine::read_exact_visible_index_selection(&op, &req.remote_path, &prefix)
                .await
                .map_err(|error| {
                    tonic::Status::internal(format!(
                        "read exact FileProvider index version: {error:#}"
                    ))
                })?
                .ok_or_else(|| {
                    tonic::Status::not_found(format!(
                        "no exact index entry for FileProvider item: {}",
                        req.remote_path
                    ))
                })?;
        if req.expected_version != selected.manifest_hash {
            return Err(file_provider_version_mismatch_status(
                &req.expected_version,
                &selected.manifest_hash,
            ));
        }
        let snapshot = tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(
            &op,
            &req.remote_path,
            &prefix,
        )
        .await
        .map_err(|error| {
            tonic::Status::internal(format!("resolve exact FileProvider manifest: {error:#}"))
        })?
        .ok_or_else(|| {
            tonic::Status::not_found(format!(
                "no exact index entry for FileProvider item: {}",
                req.remote_path
            ))
        })?;

        if snapshot.kind() != tcfs_sync::index_entry::RemoteEntryKind::RegularFile {
            return Err(tonic::Status::failed_precondition(
                "PullExact currently supports regular files only",
            ));
        }

        let selected_version = snapshot.manifest_object_id().to_string();
        if req.expected_version != selected_version {
            return Err(file_provider_version_mismatch_status(
                &req.expected_version,
                &selected_version,
            ));
        }

        // Hydrate only into a daemon-owned temporary path. PullExact never
        // receives or writes a client destination, and does not mutate the
        // daemon state cache. The snapshot boundary rechecks both the exact
        // index selection and this absent local target immediately before its
        // final rename.
        let temp_dir = tempfile::tempdir()
            .map_err(|error| tonic::Status::internal(format!("PullExact tempdir: {error}")))?;
        let content_path = temp_dir.path().join("exact-content");
        let expected_local =
            tcfs_sync::engine::capture_local_fingerprint(&content_path).map_err(|error| {
                tonic::Status::internal(format!("fingerprint PullExact staging path: {error}"))
            })?;
        let hydrate_result = {
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk));
            tcfs_sync::engine::hydrate_indexed_snapshot_with_device(
                &op,
                &snapshot,
                &content_path,
                None,
                &self.device_id,
                None,
                enc_ctx.as_ref(),
                &expected_local,
            )
            .await
        };

        let download = match hydrate_result {
            Ok(download) => download,
            Err(error) => {
                // A concurrent index advance is a version mismatch, not a
                // generic storage failure. Re-resolve once to distinguish it
                // without classifying corruption or transport errors as absent.
                match tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(
                    &op,
                    &req.remote_path,
                    &prefix,
                )
                .await
                {
                    Ok(Some(current)) if current.manifest_object_id() != selected_version => {
                        return Err(file_provider_version_mismatch_status(
                            &req.expected_version,
                            current.manifest_object_id(),
                        ));
                    }
                    Ok(None) => {
                        return Err(file_provider_version_mismatch_status(
                            &req.expected_version,
                            "<deleted>",
                        ));
                    }
                    Err(recheck_error) => {
                        return Err(tonic::Status::internal(format!(
                            "PullExact hydration failed ({error:#}); authority recheck failed: {recheck_error:#}"
                        )));
                    }
                    Ok(Some(_)) => {
                        return Err(tonic::Status::data_loss(format!(
                            "hydrate exact FileProvider content: {error:#}"
                        )));
                    }
                }
            }
        };

        if download.bytes != snapshot.size() {
            return Err(tonic::Status::data_loss(format!(
                "PullExact byte count mismatch: manifest {}, hydrated {}",
                snapshot.size(),
                download.bytes
            )));
        }

        let total_bytes = snapshot.size();
        let (stream_tx, stream_rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;

            // Keep the temporary directory alive until streaming completes or
            // the receiver disconnects.
            let _temp_dir = temp_dir;
            let mut file = match tokio::fs::File::open(&content_path).await {
                Ok(file) => file,
                Err(error) => {
                    let _ = stream_tx
                        .send(Err(tonic::Status::internal(format!(
                            "open PullExact staged content: {error}"
                        ))))
                        .await;
                    return;
                }
            };
            let mut buffer = vec![0u8; EXACT_PULL_CHUNK_SIZE];
            let mut bytes_received = 0u64;

            loop {
                let read = match file.read(&mut buffer).await {
                    Ok(read) => read,
                    Err(error) => {
                        let _ = stream_tx
                            .send(Err(tonic::Status::internal(format!(
                                "read PullExact staged content: {error}"
                            ))))
                            .await;
                        return;
                    }
                };
                if read == 0 {
                    break;
                }
                bytes_received = match bytes_received.checked_add(read as u64) {
                    Some(total) => total,
                    None => {
                        let _ = stream_tx
                            .send(Err(tonic::Status::data_loss(
                                "PullExact byte counter overflow",
                            )))
                            .await;
                        return;
                    }
                };
                if bytes_received > total_bytes {
                    let _ = stream_tx
                        .send(Err(tonic::Status::data_loss(
                            "PullExact streamed more bytes than the selected manifest",
                        )))
                        .await;
                    return;
                }
                let progress = PullProgress {
                    bytes_received,
                    total_bytes,
                    done: false,
                    error: String::new(),
                    data: buffer[..read].to_vec(),
                    exact_content: false,
                    version_token: String::new(),
                };
                if stream_tx.send(Ok(progress)).await.is_err() {
                    return;
                }
            }

            if bytes_received != total_bytes {
                let _ = stream_tx
                    .send(Err(tonic::Status::data_loss(format!(
                        "PullExact staged length mismatch: expected {total_bytes}, streamed {bytes_received}"
                    ))))
                    .await;
                return;
            }

            let terminal = PullProgress {
                bytes_received,
                total_bytes,
                done: true,
                error: String::new(),
                data: Vec::new(),
                exact_content: true,
                version_token: selected_version,
            };
            let _ = stream_tx.send(Ok(terminal)).await;
        });

        Ok(tonic::Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(stream_rx),
        )))
    }

    async fn pull(
        &self,
        request: tonic::Request<PullRequest>,
    ) -> Result<tonic::Response<Self::PullStream>, tonic::Status> {
        let session = self.require_session(&request).await?;
        let req_inner = request.get_ref();
        info!(
            remote = %req_inner.remote_path,
            local = %req_inner.local_path,
            "pull requested"
        );
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();

        validate_fixed_ingress_path(Path::new(&req.remote_path), "pull logical path")
            .map_err(tonic::Status::invalid_argument)?;
        let requested_local_path = std::path::PathBuf::from(&req.local_path);
        validate_fixed_ingress_path(&requested_local_path, "pull destination")
            .map_err(tonic::Status::invalid_argument)?;
        tcfs_core::config::validate_sync_selection_excludes_master_key(
            &self.config,
            &requested_local_path,
        )
        .map_err(tonic::Status::invalid_argument)?;
        let (local_path, local_rel_path) =
            primary_sync_root_target(&self.config, &requested_local_path, "pull")
                .map_err(tonic::Status::invalid_argument)?;
        validate_pull_destination(&self.config, &local_path, &local_rel_path)
            .map_err(tonic::Status::invalid_argument)?;

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
        let op = op.clone();

        let prefix = self.config.storage.resolved_prefix().to_string();
        let state_cache = self.state_cache.clone();

        let sync_root = self.config.sync.sync_root.as_deref();
        let _lock_guard = self.path_locks.lock(&local_path).await;

        let resolved_manifest = tcfs_sync::engine::resolve_manifest_reference(
            &op,
            &req.remote_path,
            &prefix,
            sync_root,
        )
        .await
        .map_err(|e| tonic::Status::not_found(format!("resolve manifest: {e}")))?;

        if let Some(resolved_rel_path) = resolved_manifest.rel_path() {
            validate_fixed_ingress_path(Path::new(resolved_rel_path), "resolved pull logical path")
                .map_err(tonic::Status::invalid_argument)?;
        }

        // Explicit `.../manifests/<id>` compatibility reads have no index rel to
        // preflight here. The shared download boundary validates their parsed
        // manifest-bound rel_path before any local or cache mutation; it still
        // cannot redirect the canonical, in-root destination selected above.

        let result = {
            let mut cache = state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk));
            tcfs_sync::engine::download_resolved_file_with_device(
                &op,
                &resolved_manifest,
                &local_path,
                &prefix,
                None,
                &self.device_id,
                Some(&mut cache),
                enc_ctx.as_ref(),
            )
            .await
        };

        match result {
            Ok(dl) => {
                let progress = PullProgress {
                    bytes_received: dl.bytes,
                    total_bytes: dl.bytes,
                    done: true,
                    error: String::new(),
                    ..Default::default()
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                warn!(error = %e, "pull download failed");
                let progress = PullProgress {
                    bytes_received: 0,
                    total_bytes: 0,
                    done: true,
                    error: format!("{e}"),
                    ..Default::default()
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Hydrate ───────────────────────────────────────────────────────────

    type HydrateStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<HydrateProgress, tonic::Status>> + Send>,
    >;

    async fn hydrate(
        &self,
        request: tonic::Request<HydrateRequest>,
    ) -> Result<tonic::Response<Self::HydrateStream>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();
        let requested_stub_path = std::path::PathBuf::from(&req.stub_path);

        info!(stub = %req.stub_path, "hydrate requested");

        // Hydration mutates local state. Bind the requested stub and its real
        // target to the configured primary root before reading either path.
        let (stub_path, _) =
            primary_sync_root_target(&self.config, &requested_stub_path, "hydrate").map_err(
                |error| tonic::Status::invalid_argument(format!("invalid stub path: {error}")),
            )?;
        let stub_metadata = std::fs::symlink_metadata(&requested_stub_path)
            .map_err(|error| tonic::Status::not_found(format!("read stub metadata: {error}")))?;
        if stub_metadata.file_type().is_symlink() || !stub_metadata.is_file() {
            return Err(tonic::Status::invalid_argument(
                "hydrate stub must be a regular, non-symlink file",
            ));
        }

        let requested_real_path = tcfs_vfs::stub_to_real_name(requested_stub_path.as_os_str())
            .ok_or_else(|| {
                tonic::Status::invalid_argument(format!(
                    "cannot derive real name from stub: {}",
                    req.stub_path
                ))
            })?;
        validate_fixed_ingress_path(&requested_real_path, "hydrate logical path")
            .map_err(tonic::Status::invalid_argument)?;
        tcfs_core::config::validate_sync_selection_excludes_master_key(
            &self.config,
            &requested_real_path,
        )
        .map_err(tonic::Status::invalid_argument)?;
        let (real_path, local_rel_path) =
            primary_sync_root_target(&self.config, &requested_real_path, "hydrate").map_err(
                |error| tonic::Status::invalid_argument(format!("invalid hydrate target: {error}")),
            )?;
        if tcfs_vfs::real_to_stub_name(real_path.as_os_str()) != stub_path {
            return Err(tonic::Status::invalid_argument(
                "stub and hydrated target do not resolve to the same root path",
            ));
        }
        validate_fixed_ingress_path(Path::new(&local_rel_path), "hydrate logical path")
            .map_err(tonic::Status::invalid_argument)?;
        tcfs_core::config::validate_sync_selection_excludes_master_key(&self.config, &real_path)
            .map_err(tonic::Status::invalid_argument)?;

        // Read and parse stub file
        let stub_content = std::fs::read_to_string(&stub_path)
            .map_err(|e| tonic::Status::not_found(format!("read stub: {e}")))?;
        let meta = tcfs_vfs::StubMeta::parse(&stub_content)
            .map_err(|e| tonic::Status::invalid_argument(format!("parse stub: {e}")))?;

        // The stub oid and size are historical hints, not immutable authority.
        // Bind origin exactly to the local target, then resolve today's typed
        // path index so an old placeholder follows copy-on-write updates.
        let prefix = self.config.storage.resolved_prefix().to_string();
        let origin_prefix = format!("seaweedfs://{prefix}/");
        let rel_path = meta.origin.strip_prefix(&origin_prefix).ok_or_else(|| {
            tonic::Status::invalid_argument(
                "stub origin is outside the daemon's configured storage prefix",
            )
        })?;
        tcfs_sync::index_entry::validate_canonical_rel_path(rel_path).map_err(|error| {
            tonic::Status::invalid_argument(format!("invalid stub origin path: {error}"))
        })?;
        if rel_path != local_rel_path {
            return Err(tonic::Status::invalid_argument(format!(
                "stub origin path '{rel_path}' does not match local hydrate target '{local_rel_path}'"
            )));
        }

        let op = {
            let operator = self.operator.lock().await;
            operator
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };

        let total_bytes = meta.size;
        let _lock_guard = self.path_locks.lock(&real_path).await;
        let expected_local =
            tcfs_sync::engine::capture_local_fingerprint(&real_path).map_err(|error| {
                tonic::Status::failed_precondition(format!("fingerprint hydrate target: {error}"))
            })?;
        let snapshot =
            tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(&op, rel_path, &prefix)
                .await
                .map_err(|error| {
                    tonic::Status::not_found(format!("resolve exact stub origin: {error}"))
                })?
                .ok_or_else(|| {
                    tonic::Status::not_found(format!(
                        "no current index entry for stub origin: {rel_path}"
                    ))
                })?;

        let result = {
            let mut cache = self.state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| build_encryption_context(&self.config, &self.device_id, mk));
            tcfs_sync::engine::hydrate_indexed_snapshot_with_device(
                &op,
                &snapshot,
                &real_path,
                None,
                &self.device_id,
                Some(&mut cache),
                enc_ctx.as_ref(),
                &expected_local,
            )
            .await
        };

        match result {
            Ok(dl) => {
                // Remove stub file after successful hydration
                let _ = std::fs::remove_file(&stub_path);

                info!(
                    real_path = %real_path.display(),
                    bytes = dl.bytes,
                    "hydration complete"
                );

                let progress = HydrateProgress {
                    bytes_received: dl.bytes,
                    total_bytes: dl.bytes,
                    local_path: real_path.to_string_lossy().to_string(),
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                let progress = HydrateProgress {
                    bytes_received: 0,
                    total_bytes,
                    local_path: String::new(),
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Unsync ────────────────────────────────────────────────────────────

    async fn unsync(
        &self,
        request: tonic::Request<UnsyncRequest>,
    ) -> Result<tonic::Response<UnsyncResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
        let req = request.into_inner();
        let path = std::path::PathBuf::from(&req.path);
        let _lock_guard = self.path_locks.lock(&path).await;

        info!(path = %req.path, force = req.force, "unsync requested");

        let mut cache = self.state_cache.lock().await;
        if cache.get(&path).is_none() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("path not in sync state: {}", req.path),
            }));
        }

        // Dirty-child safety check: if this is a directory, verify no children
        // have unsynced local modifications before removing from state cache.
        if path.is_dir() && !req.force {
            let children = cache.children_with_prefix(&path);
            let mut dirty_paths = Vec::new();
            for (child_key, _child_state) in &children {
                let child_path = std::path::Path::new(child_key);
                if child_path.exists() {
                    if let Ok(Some(reason)) = cache.needs_sync(child_path) {
                        dirty_paths.push(format!("{}: {reason}", child_key));
                    }
                }
            }
            if !dirty_paths.is_empty() {
                let count = dirty_paths.len();
                let detail = dirty_paths
                    .into_iter()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Ok(tonic::Response::new(UnsyncResponse {
                    success: false,
                    stub_path: String::new(),
                    error: format!(
                        "{count} dirty child(ren) with unsynced changes (use force=true to override): {detail}"
                    ),
                }));
            }

            // Transition children to NotSynced (preserve metadata for re-hydration)
            let child_keys: Vec<String> = children.into_iter().map(|(k, _)| k).collect();
            for child_key in &child_keys {
                let child_path = std::path::PathBuf::from(child_key);
                cache.set_status(&child_path, tcfs_sync::state::FileSyncStatus::NotSynced);
            }
        }

        // Transition to NotSynced instead of removing — preserves metadata for re-hydration
        cache.set_status(&path, tcfs_sync::state::FileSyncStatus::NotSynced);
        if let Err(e) = cache.flush() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("state cache flush failed: {e}"),
            }));
        }

        // Evict from VFS disk cache if VFS is active
        // Clone the Arc out of the watch::Ref to avoid holding a !Send borrow across .await
        let mut bytes_freed = 0u64;
        let vfs_opt = self.vfs_handle.borrow().clone();
        if let Some(ref vfs) = vfs_opt {
            match vfs.unsync_path(&req.path).await {
                Ok(result) => {
                    info!(
                        path = %req.path,
                        bytes_freed = result.bytes_freed,
                        was_cached = result.was_cached,
                        "unsync: VFS cache evicted"
                    );
                    bytes_freed = result.bytes_freed;
                }
                Err(e) => {
                    warn!(path = %req.path, error = %e, "unsync: VFS cache eviction failed (non-fatal)");
                }
            }
        }

        info!(path = %req.path, bytes_freed, "unsynced successfully");

        Ok(tonic::Response::new(UnsyncResponse {
            success: true,
            stub_path: String::new(),
            error: String::new(),
        }))
    }

    // ── Sync Status ───────────────────────────────────────────────────────

    async fn sync_status(
        &self,
        request: tonic::Request<SyncStatusRequest>,
    ) -> Result<tonic::Response<SyncStatusResponse>, tonic::Status> {
        let req = request.into_inner();
        let path = std::path::PathBuf::from(&req.path);

        let cache = self.state_cache.lock().await;

        match cache.get(&path) {
            Some(entry) => {
                let state = if entry.status == tcfs_sync::state::FileSyncStatus::Synced {
                    match cache.needs_sync(&path) {
                        Ok(Some(_)) => "pending".to_string(),
                        Ok(None) => entry.status.to_string(),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %path.display(),
                                "needs_sync failed during sync_status; reporting unknown"
                            );
                            "unknown".to_string()
                        }
                    }
                } else {
                    entry.status.to_string()
                };

                Ok(tonic::Response::new(SyncStatusResponse {
                    path: req.path,
                    state,
                    blake3: entry.blake3.clone(),
                    size: entry.size,
                    last_synced: entry.last_synced as i64,
                }))
            }
            None => {
                // Check if it needs sync
                let state = match cache.needs_sync(&path) {
                    Ok(None) => "unknown",
                    Ok(Some(_reason)) => "pending",
                    Err(_) => "unknown",
                };
                Ok(tonic::Response::new(SyncStatusResponse {
                    path: req.path,
                    state: state.into(),
                    blake3: String::new(),
                    size: 0,
                    last_synced: 0,
                }))
            }
        }
    }

    // ── List Files ────────────────────────────────────────────────────────

    async fn list_files(
        &self,
        request: tonic::Request<ListFilesRequest>,
    ) -> Result<tonic::Response<ListFilesResponse>, tonic::Status> {
        let req = request.into_inner();
        let requested_prefix = req.prefix.strip_suffix('/').unwrap_or(&req.prefix);
        if !requested_prefix.is_empty() {
            tcfs_sync::index_entry::validate_canonical_rel_path(requested_prefix)
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
        }

        let op = {
            let operator = self.operator.lock().await;
            operator
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };
        let storage_prefix = self.config.storage.resolved_prefix();
        let namespace = tcfs_sync::reconcile::list_remote_namespace(&op, storage_prefix)
            .await
            .map_err(|error| {
                tonic::Status::failed_precondition(format!(
                    "remote namespace is unavailable or invalid: {error:#}"
                ))
            })?;

        // The remote index is listing authority. Cache entries only enrich a
        // remote file when they are bound to that exact current manifest.
        let cached_by_rel = {
            let cache = self.state_cache.lock().await;
            let mut by_rel = std::collections::HashMap::new();
            for (key, state) in cache.all_entries() {
                if let Some(rel_path) = logical_rel_path_from_state_key(
                    &key,
                    state,
                    self.config.sync.sync_root.as_deref(),
                    storage_prefix,
                ) {
                    by_rel.entry(rel_path).or_insert_with(|| state.clone());
                }
            }
            by_rel
        };

        let mut directory_paths = std::collections::BTreeSet::new();
        let mut files = Vec::new();
        for (rel_path, remote_entry) in &namespace.files {
            let Some(remainder) = list_files_remainder(rel_path, requested_prefix) else {
                continue;
            };
            if let Some((directory, _)) = remainder.split_once('/') {
                let directory_path = if requested_prefix.is_empty() {
                    format!("{directory}/")
                } else {
                    format!("{requested_prefix}/{directory}/")
                };
                directory_paths.insert(directory_path);
                continue;
            }

            let manifest_key = format!(
                "{}/manifests/{}",
                storage_prefix.trim_end_matches('/'),
                remote_entry.manifest_hash
            );
            let cached = cached_by_rel
                .get(rel_path)
                .filter(|state| cached_state_matches_remote_manifest(state, &manifest_key));
            files.push(FileEntry {
                path: rel_path.clone(),
                filename: remainder.to_string(),
                size: remote_entry.size,
                last_synced: cached.map_or(0, |state| state.last_synced as i64),
                is_directory: false,
                blake3: cached.map_or_else(String::new, |state| state.blake3.clone()),
                hydration_state: cached
                    .map_or("not_synced", |state| hydration_state_name(state.status))
                    .to_string(),
                version_token: remote_entry.manifest_hash.clone(),
            });
        }

        for rel_path in &namespace.directories {
            let Some(remainder) = list_files_remainder(rel_path, requested_prefix) else {
                continue;
            };
            let directory = remainder.split('/').next().unwrap_or(remainder);
            let directory_path = if requested_prefix.is_empty() {
                format!("{directory}/")
            } else {
                format!("{requested_prefix}/{directory}/")
            };
            directory_paths.insert(directory_path);
        }

        files.extend(directory_paths.into_iter().map(|path| {
            let filename = path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            FileEntry {
                path,
                filename,
                size: 0,
                last_synced: 0,
                is_directory: true,
                blake3: String::new(),
                hydration_state: String::new(),
                version_token: String::new(),
            }
        }));
        files.sort_by(|left, right| left.path.cmp(&right.path));

        Ok(tonic::Response::new(ListFilesResponse { files }))
    }

    // ── Conflict inspection / resolution ─────────────────────────────────

    async fn list_conflicts(
        &self,
        request: tonic::Request<ListConflictsRequest>,
    ) -> Result<tonic::Response<ListConflictsResponse>, tonic::Status> {
        if !request.get_ref().root_id.is_empty() {
            self.require_registered_root_auth_posture()?;
        }
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "pull")?;
        let root_id = request.into_inner().root_id;

        if root_id.is_empty() {
            check_registered_prefix_permission(&session, self.config.storage.resolved_prefix())?;
            let cache = self.state_cache.lock().await;
            return Ok(tonic::Response::new(ListConflictsResponse {
                root_id: "primary".to_string(),
                local_root: self
                    .config
                    .sync
                    .sync_root
                    .as_deref()
                    .map(tcfs_core::config::expand_tilde)
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                remote_prefix: self.config.storage.resolved_prefix().to_string(),
                state_path: tcfs_core::config::expand_tilde(&self.config.sync.state_db)
                    .with_extension("json")
                    .display()
                    .to_string(),
                conflicts: conflict_records(&cache),
            }));
        }

        let route = self.authorized_registered_root(&session, &root_id)?;
        let _state_lock = tcfs_sync::state::StateFileLock::acquire(&route.state_path)
            .map_err(|error| tonic::Status::aborted(error.to_string()))?;
        let state = tcfs_sync::state::StateCache::open(&route.state_path)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        validate_conflict_cache_route(&route, &state)
            .map_err(tonic::Status::failed_precondition)?;

        Ok(tonic::Response::new(ListConflictsResponse {
            root_id: route.root_id,
            local_root: route.local_root.display().to_string(),
            remote_prefix: route.remote_prefix,
            state_path: route.state_path.display().to_string(),
            conflicts: conflict_records(&state),
        }))
    }

    async fn resolve_registered_root(
        &self,
        request: tonic::Request<ResolveRegisteredRootRequest>,
    ) -> Result<tonic::Response<ResolveRegisteredRootResponse>, tonic::Status> {
        self.require_registered_root_auth_posture()?;
        let session = self.require_session(&request).await?;
        let req = request.into_inner();

        let mode = match RegisteredRootResolveMode::try_from(req.mode) {
            Ok(RegisteredRootResolveMode::GitKeepBothDryRun) => {
                tcfs_sync::conflict_git::GitKeepBothMode::DryRun
            }
            Ok(RegisteredRootResolveMode::GitKeepBothExecute) => {
                tcfs_sync::conflict_git::GitKeepBothMode::Execute
            }
            Ok(RegisteredRootResolveMode::Unspecified) | Err(_) => {
                return Err(tonic::Status::invalid_argument(
                    "registered-root resolution mode must be git keep-both dry-run or execute",
                ));
            }
        };

        // Permission checks stay ahead of route lookup so a session cannot use
        // known-vs-unknown behavior to enumerate the registry. Dry-run is an
        // inspection operation and reads remote ref content, so it requires
        // pull. Execute performs the same reads and then mutates refs/state, so
        // it additionally requires push.
        Self::check_permission(&session, "pull")?;
        if mode.is_execute() {
            Self::check_permission(&session, "push")?;
        }
        let route = self.authorized_registered_root(&session, &req.root_id)?;

        if !req.operator_cli {
            tracing::warn!(
                root_id = %route.root_id,
                path = %req.path,
                "refusing registered-root keep-both: missing explicit operator intent"
            );
            return Ok(tonic::Response::new(ResolveRegisteredRootResponse {
                success: false,
                resolved_path: String::new(),
                error: "registered-root git keep-both requires explicit operator intent in addition to an appropriately permissioned authenticated session; run it deliberately from `tcfs resolve --root <id> ...`"
                    .to_string(),
                root_id: route.root_id,
                local_root: route.local_root.display().to_string(),
                remote_prefix: route.remote_prefix,
                state_path: route.state_path.display().to_string(),
            }));
        }

        self.resolve_registered_git_keep_both_repo(&route, Path::new(&req.path), mode)
            .await
    }

    async fn resolve_conflict(
        &self,
        request: tonic::Request<ResolveConflictRequest>,
    ) -> Result<tonic::Response<ResolveConflictResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        let req = request.into_inner();

        let resolution = match req.resolution.as_str() {
            "keep_local"
            | "keep_remote"
            | "keep_both"
            | "defer"
            | "git_keep_both_dry_run"
            | "git_keep_both_execute" => req.resolution.clone(),
            other => {
                return Ok(tonic::Response::new(ResolveConflictResponse {
                    success: false,
                    resolved_path: String::new(),
                    error: format!(
                        "invalid resolution '{}': use defer, or repository keep-both through the shipped CLI",
                        other
                    ),
                }));
            }
        };

        // Capability checks must precede prefix routing and state lookup: a
        // push-only session must not use a failing remote-read strategy to
        // probe primary-cache enrollment or remote content. Retired per-file
        // strategies retain their historical capability checks before one
        // uniform refusal. `defer` remains behind the historical push authority
        // even though it is a no-op. Git dry-run requires pull; execute requires
        // both pull and push.
        match resolution.as_str() {
            "keep_remote" | "git_keep_both_dry_run" => {
                Self::check_permission(&session, "pull")?;
            }
            "keep_both" | "git_keep_both_execute" => {
                Self::check_permission(&session, "pull")?;
                Self::check_permission(&session, "push")?;
            }
            "keep_local" | "defer" => {
                Self::check_permission(&session, "push")?;
            }
            _ => unreachable!("resolution was validated above"),
        }

        check_registered_prefix_permission(&session, self.config.storage.resolved_prefix())?;
        if matches!(
            resolution.as_str(),
            "keep_local" | "keep_remote" | "keep_both"
        ) {
            tracing::warn!(
                resolution = %resolution,
                "refusing retired legacy per-file mutation before path or state lookup"
            );
            return Ok(tonic::Response::new(ResolveConflictResponse {
                success: false,
                resolved_path: String::new(),
                error: concat!(
                    "legacy per-file mutation is disabled because it cannot bind the ",
                    "requested path to an authenticated root and indexed manifest; use ",
                    "defer, or use the root-scoped repository resolver for Git conflicts"
                )
                .to_string(),
            }));
        }

        if resolution == "defer" {
            info!("conflict deferred without path or state lookup");
            return Ok(tonic::Response::new(ResolveConflictResponse {
                success: true,
                resolved_path: req.path,
                error: String::new(),
            }));
        }

        let mode = repo_keep_both_mode(&resolution)
            .expect("validated non-file, non-defer resolution is repository keep-both");

        // The repository mutation is operator-deliberate. MCP exposes no
        // conflict-resolution tool. This client-supplied bit is defense in
        // depth, not attestation; authenticated mode-specific permissions are
        // the authorization boundary. Refuse before path or state lookup.
        if !req.operator_cli {
            tracing::warn!(
                resolution = %resolution,
                "refusing repo-group git keep-both: missing explicit operator intent"
            );
            return Ok(tonic::Response::new(ResolveConflictResponse {
                success: false,
                resolved_path: String::new(),
                error:
                    "repo-group git keep-both requires explicit operator intent in addition \
                        to an appropriately permissioned authenticated session; run it deliberately from the CLI \
                        (`tcfs resolve <repo> --strategy keep-both [--execute]`)"
                        .to_string(),
            }));
        }

        let path = std::path::PathBuf::from(&req.path);
        let primary_prefix = self.config.storage.resolved_prefix().to_string();
        let canonical_requested_path = canonicalize_with_missing_tail(&path).ok();

        // Reload state from disk in case the CLI wrote new entries. Repository
        // routing is permitted only when this exact primary cache contains a
        // current Git conflict under the selected storage prefix.
        let primary_repo_conflict = {
            let mut cache = self.state_cache.lock().await;
            if let Err(e) = cache.reload_from_disk() {
                tracing::warn!("failed to reload state cache: {e}");
            }
            let git_dir = canonical_requested_path
                .as_deref()
                .unwrap_or(&path)
                .join(".git");
            cache.conflicts().iter().any(|(cache_key, entry)| {
                Path::new(cache_key).starts_with(&git_dir)
                    && primary_conflict_entry_matches_prefix(entry, &primary_prefix)
            })
        };

        if !self.config.sync.roots.is_empty() {
            let primary_route_proven = if self.config.sync.sync_root.is_some() {
                rootless_path_is_within_primary_sync_root(&self.config, &path)
            } else {
                primary_repo_conflict
            };
            if rootless_path_may_belong_to_registered_root(&self.config, &path)
                || !primary_route_proven
            {
                return Err(tonic::Status::failed_precondition(
                    "legacy primary-cache conflict route is unavailable for this path; use an authorized absolute primary path or `tcfs resolve <repo> --root <id>`",
                ));
            }
        }

        let primary_path_allowed = self.config.sync.sync_root.is_none()
            || rootless_path_is_within_primary_sync_root(&self.config, &path);
        if !primary_path_allowed
            || rootless_path_may_belong_to_registered_root(&self.config, &path)
            || !primary_repo_conflict
            || canonical_requested_path.as_deref() != Some(path.as_path())
        {
            return Err(tonic::Status::failed_precondition(
                "legacy primary-cache conflict route is unavailable for this path; use an authorized absolute primary path or `tcfs resolve <repo> --root <id>`",
            ));
        }
        let repo_path = canonical_requested_path.as_deref().ok_or_else(|| {
            tonic::Status::failed_precondition(
                "legacy primary-cache conflict route is unavailable for this path; use an authorized absolute primary path or `tcfs resolve <repo> --root <id>`",
            )
        })?;
        self.resolve_git_keep_both_repo(repo_path, mode).await
    }

    // ── Watch ─────────────────────────────────────────────────────────────

    type WatchStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<WatchEvent, tonic::Status>> + Send>,
    >;

    async fn watch(
        &self,
        request: tonic::Request<WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
        use notify::{RecursiveMode, Watcher};
        use tracing::{debug, warn};

        let req = request.into_inner();
        if req.paths.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "at least one path is required",
            ));
        }

        let since = req.since_timestamp;
        info!(paths = ?req.paths, since, "watch requested");
        if since > 0 {
            return Err(tonic::Status::failed_precondition(
                "authoritative incremental Watch journal is unavailable; preserve the prior anchor and perform a full ListFiles refresh",
            ));
        }
        let watch_roots: Vec<String> = req
            .paths
            .iter()
            .map(|path| normalize_watch_root(path))
            .collect();
        let watch_operator = {
            let operator = self.operator.lock().await;
            operator
                .as_ref()
                .cloned()
                .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?
        };
        let watch_storage_prefix = self.config.storage.resolved_prefix().to_string();

        let (async_tx, async_rx) = tokio::sync::mpsc::channel(256);

        // ── Live local filesystem events via notify ─────────────────────────
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let sync_root = self.config.sync.sync_root.clone();
        let operator_for_notify = watch_operator.clone();
        let storage_prefix_for_notify = watch_storage_prefix.clone();
        let runtime_for_notify = tokio::runtime::Handle::current();
        let mut watch_targets = Vec::new();

        for root in &watch_roots {
            let Some(path) = sync_root
                .as_deref()
                .map(|sync_root| {
                    if root.is_empty() {
                        sync_root.to_path_buf()
                    } else {
                        sync_root.join(root)
                    }
                })
                .or_else(|| {
                    if root.is_empty() {
                        None
                    } else {
                        Some(std::path::PathBuf::from(root))
                    }
                })
            else {
                debug!("watch: local notify disabled for empty path without sync_root");
                continue;
            };

            if path.exists() {
                watch_targets.push((root.clone(), path));
            } else {
                debug!(
                    root,
                    path = %path.display(),
                    "watch: local notify target missing; using cache/NATS only"
                );
            }
        }

        if !watch_targets.is_empty() {
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    let _ = sync_tx.send(res);
                })
                .map_err(|e| tonic::Status::internal(format!("create watcher: {e}")))?;

            for (root, path) in &watch_targets {
                watcher
                    .watch(path, RecursiveMode::Recursive)
                    .map_err(|e| tonic::Status::internal(format!("watch {root}: {e}")))?;
            }

            let notify_tx = async_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _watcher = watcher;
                loop {
                    if notify_tx.is_closed() {
                        break;
                    }
                    let result = match sync_rx.recv_timeout(std::time::Duration::from_secs(1)) {
                        Ok(result) => result,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let event = match result {
                        Ok(event) => {
                            let event_type = match event.kind {
                                notify::EventKind::Create(_) => "created",
                                notify::EventKind::Modify(_) => "modified",
                                notify::EventKind::Remove(_) => "deleted",
                                notify::EventKind::Access(_) => continue,
                                notify::EventKind::Other => continue,
                                notify::EventKind::Any => continue,
                            };
                            let path = event.paths.first().cloned().unwrap_or_default();
                            let logical_path =
                                logical_rel_path_from_fs_path(&path, sync_root.as_deref());
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;
                            runtime_for_notify.block_on(authoritative_watch_event(
                                &operator_for_notify,
                                &storage_prefix_for_notify,
                                &logical_path,
                                event_type,
                                timestamp,
                                String::new(),
                            ))
                        }
                        Err(error) => Err(tonic::Status::internal(format!(
                            "local Watch notification failed: {error}"
                        ))),
                    };
                    let authority_failed = event.is_err();
                    if notify_tx.blocking_send(event).is_err() || authority_failed {
                        break; // Client disconnected
                    }
                }
            });
        } else {
            debug!("watch: no local notify targets; using cache/NATS only");
        }

        // ── Live remote events via NATS STATE_UPDATES ───────────────────────
        // Use an ephemeral consumer so Watch callers don't compete with
        // the daemon's durable state_sync_loop consumer for messages.
        let nats_tx = async_tx;
        let nats_client = self.nats.clone();
        let operator_for_nats = watch_operator;
        let storage_prefix_for_nats = watch_storage_prefix;
        let watch_roots_for_nats = watch_roots.clone();
        tokio::spawn(async move {
            let client = nats_client.lock().await;
            let Some(nats) = client.as_ref() else {
                debug!("watch: NATS not connected, skipping remote events");
                return;
            };
            match nats.state_consumer_ephemeral().await {
                Ok(mut consumer) => {
                    use futures::StreamExt;
                    while let Some(msg_result) = consumer.next().await {
                        match msg_result {
                            Ok(state_msg) => {
                                let event = state_msg.event.clone();
                                // Ack before processing (at-most-once for watch events is fine)
                                let _ = state_msg.ack().await;

                                let watch_event = match event {
                                    tcfs_sync::StateEvent::FileSynced {
                                        device_id: dev,
                                        rel_path,
                                        timestamp,
                                        ..
                                    } => {
                                        if !rel_path_matches_watch_roots(
                                            &rel_path,
                                            &watch_roots_for_nats,
                                        ) {
                                            continue;
                                        }
                                        authoritative_watch_event(
                                            &operator_for_nats,
                                            &storage_prefix_for_nats,
                                            &rel_path,
                                            "modified",
                                            timestamp as i64,
                                            dev,
                                        )
                                        .await
                                    }
                                    tcfs_sync::StateEvent::FileDeleted {
                                        device_id: dev,
                                        rel_path,
                                        timestamp,
                                        ..
                                    } => {
                                        if !rel_path_matches_watch_roots(
                                            &rel_path,
                                            &watch_roots_for_nats,
                                        ) {
                                            continue;
                                        }
                                        authoritative_watch_event(
                                            &operator_for_nats,
                                            &storage_prefix_for_nats,
                                            &rel_path,
                                            "deleted",
                                            timestamp as i64,
                                            dev,
                                        )
                                        .await
                                    }
                                    tcfs_sync::StateEvent::FileRenamed {
                                        device_id: dev,
                                        new_path,
                                        timestamp,
                                        ..
                                    } => {
                                        if !rel_path_matches_watch_roots(
                                            &new_path,
                                            &watch_roots_for_nats,
                                        ) {
                                            continue;
                                        }
                                        authoritative_watch_event(
                                            &operator_for_nats,
                                            &storage_prefix_for_nats,
                                            &new_path,
                                            "renamed",
                                            timestamp as i64,
                                            dev,
                                        )
                                        .await
                                    }
                                    _ => continue, // Skip DeviceOnline/Offline etc
                                };
                                let authority_failed = watch_event.is_err();
                                if nats_tx.send(watch_event).await.is_err() || authority_failed {
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!("watch: NATS state consumer error: {e}");
                                let _ = nats_tx
                                    .send(Err(tonic::Status::unavailable(format!(
                                        "Watch NATS consumer failed: {e}"
                                    ))))
                                    .await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("watch: failed to create NATS state consumer: {e}");
                    let _ = nats_tx
                        .send(Err(tonic::Status::unavailable(format!(
                            "create Watch NATS consumer: {e}"
                        ))))
                        .await;
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(async_rx);
        Ok(tonic::Response::new(Box::pin(stream)))
    }

    // ── Auth (encryption key management) ────────────────────────────────

    async fn auth_unlock(
        &self,
        request: tonic::Request<AuthUnlockRequest>,
    ) -> Result<tonic::Response<AuthUnlockResponse>, tonic::Status> {
        let req = request.into_inner();

        if req.master_key.len() != tcfs_crypto::KEY_SIZE {
            return Ok(tonic::Response::new(AuthUnlockResponse {
                success: false,
                error: format!(
                    "master key must be {} bytes, got {}",
                    tcfs_crypto::KEY_SIZE,
                    req.master_key.len()
                ),
            }));
        }

        let mut key_bytes = [0u8; tcfs_crypto::KEY_SIZE];
        key_bytes.copy_from_slice(&req.master_key);
        let master_key = tcfs_crypto::MasterKey::from_bytes(key_bytes);

        let mut guard = self.master_key.lock().await;
        *guard = Some(master_key);

        info!("encryption unlocked via gRPC");
        Ok(tonic::Response::new(AuthUnlockResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn auth_lock(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<AuthLockResponse>, tonic::Status> {
        let mut guard = self.master_key.lock().await;
        let was_unlocked = guard.is_some();
        *guard = None;

        if was_unlocked {
            info!("encryption locked via gRPC");
        }

        Ok(tonic::Response::new(AuthLockResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn auth_status(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<AuthStatusResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;
        let unlocked = self.master_key.lock().await.is_some();

        // Build available methods dynamically
        let mut methods = vec!["master_key".into()];
        if self.totp_provider.is_available() {
            methods.push("totp".into());
        }
        if self.webauthn_provider.is_available() {
            methods.push("webauthn".into());
        }

        // Check active session count
        self.session_store.cleanup_expired().await;
        let active_sessions = self.session_store.active_count().await;

        Ok(tonic::Response::new(AuthStatusResponse {
            unlocked,
            crypto_enabled: self.config.crypto.enabled,
            session_device_id: self.device_id.clone(),
            auth_method: if active_sessions > 0 {
                "session".into()
            } else if unlocked {
                "master_key".into()
            } else {
                String::new()
            },
            available_methods: methods,
        }))
    }

    // ── MFA Enrollment ───────────────────────────────────────────────────

    async fn auth_enroll(
        &self,
        request: tonic::Request<AuthEnrollRequest>,
    ) -> Result<tonic::Response<AuthEnrollResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;

        let session = self.require_session(&request).await?;
        let req = request.into_inner();
        // A signed invite supplies a short-lived session scoped to the joining
        // device. It may enroll that device's own MFA credential, but managing
        // any other identity remains an administrator operation.
        if req.device_id != session.device_id {
            Self::check_permission(&session, "admin")?;
        }
        info!(device_id = %req.device_id, method = %req.method, "auth enroll requested");

        match req.method.as_str() {
            "totp" => match self.totp_provider.register(&req.device_id).await {
                Ok(reg) => {
                    // Persist TOTP credentials to data dir
                    let cred_path = std::path::PathBuf::from(&format!(
                        "{}/tcfsd/totp-credentials.json",
                        dirs::data_dir().unwrap_or_default().display()
                    ));
                    if let Err(e) = self.totp_provider.save_to_file(&cred_path).await {
                        tracing::warn!("failed to persist TOTP credentials: {e}");
                    }
                    Ok(tonic::Response::new(AuthEnrollResponse {
                        success: true,
                        registration_data: reg.data,
                        instructions: reg.instructions,
                        error: String::new(),
                    }))
                }
                Err(e) => Ok(tonic::Response::new(AuthEnrollResponse {
                    success: false,
                    registration_data: Vec::new(),
                    instructions: String::new(),
                    error: format!("TOTP enrollment failed: {e}"),
                })),
            },
            "webauthn" => match self.webauthn_provider.register(&req.device_id).await {
                Ok(reg) => {
                    // Persist WebAuthn credentials
                    let cred_path = std::path::PathBuf::from(&format!(
                        "{}/tcfsd/webauthn-credentials.json",
                        dirs::data_dir().unwrap_or_default().display()
                    ));
                    if let Err(e) = self.webauthn_provider.save_to_file(&cred_path).await {
                        tracing::warn!("failed to persist WebAuthn credentials: {e}");
                    }
                    Ok(tonic::Response::new(AuthEnrollResponse {
                        success: true,
                        registration_data: reg.data,
                        instructions: reg.instructions,
                        error: String::new(),
                    }))
                }
                Err(e) => Ok(tonic::Response::new(AuthEnrollResponse {
                    success: false,
                    registration_data: Vec::new(),
                    instructions: String::new(),
                    error: format!("WebAuthn enrollment failed: {e}"),
                })),
            },
            other => Ok(tonic::Response::new(AuthEnrollResponse {
                success: false,
                registration_data: Vec::new(),
                instructions: String::new(),
                error: format!("unsupported auth method: {other}"),
            })),
        }
    }

    async fn auth_complete_enroll(
        &self,
        request: tonic::Request<AuthCompleteEnrollRequest>,
    ) -> Result<tonic::Response<AuthCompleteEnrollResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        let req = request.into_inner();
        if req.device_id != session.device_id {
            Self::check_permission(&session, "admin")?;
        }
        info!(device_id = %req.device_id, method = %req.method, "auth complete enroll requested");

        match req.method.as_str() {
            "webauthn" => {
                match self
                    .webauthn_provider
                    .complete_registration_from_bytes(&req.device_id, &req.attestation_data)
                    .await
                {
                    Ok(()) => {
                        // Persist updated credentials
                        let cred_path = std::path::PathBuf::from(&format!(
                            "{}/tcfsd/webauthn-credentials.json",
                            dirs::data_dir().unwrap_or_default().display()
                        ));
                        if let Err(e) = self.webauthn_provider.save_to_file(&cred_path).await {
                            tracing::warn!("failed to persist WebAuthn credentials: {e}");
                        }
                        Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                            success: true,
                            error: String::new(),
                        }))
                    }
                    Err(e) => Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                        success: false,
                        error: format!("registration completion failed: {e}"),
                    })),
                }
            }
            "totp" => {
                // TOTP doesn't have a second step — enroll + first verify completes it
                Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                    success: true,
                    error: String::new(),
                }))
            }
            other => Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                success: false,
                error: format!("unsupported method for complete_enroll: {other}"),
            })),
        }
    }

    async fn auth_challenge(
        &self,
        request: tonic::Request<AuthChallengeRequest>,
    ) -> Result<tonic::Response<AuthChallengeResponse>, tonic::Status> {
        let req = request.into_inner();
        info!(device_id = %req.device_id, method = %req.method, "auth challenge requested");

        let provider: &dyn tcfs_auth::AuthProvider = match req.method.as_str() {
            "totp" => self.totp_provider.as_ref(),
            "webauthn" => self.webauthn_provider.as_ref(),
            other => {
                return Err(tonic::Status::invalid_argument(format!(
                    "unsupported auth method: {other}"
                )));
            }
        };

        match provider.challenge(&req.device_id).await {
            Ok(challenge) => Ok(tonic::Response::new(AuthChallengeResponse {
                challenge_id: challenge.challenge_id,
                data: challenge.data,
                prompt: challenge.prompt,
                expires_at: challenge.expires_at,
            })),
            Err(e) => Err(tonic::Status::failed_precondition(format!(
                "{} challenge failed: {e}",
                req.method
            ))),
        }
    }

    async fn auth_verify(
        &self,
        request: tonic::Request<AuthVerifyRequest>,
    ) -> Result<tonic::Response<AuthVerifyResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;

        let req = request.into_inner();
        info!(device_id = %req.device_id, "auth verify requested");

        // Rate limit check — reject early if device is locked out
        if let Some(remaining_secs) = self.rate_limiter.check(&req.device_id).await {
            tracing::warn!(device_id = %req.device_id, remaining_secs, "auth attempt rejected: rate limited");
            return Ok(tonic::Response::new(AuthVerifyResponse {
                success: false,
                session_token: String::new(),
                error: format!("too many failed attempts, try again in {remaining_secs}s"),
            }));
        }

        let response = tcfs_auth::AuthResponse {
            challenge_id: req.challenge_id,
            data: req.data,
            device_id: req.device_id.clone(),
        };

        // Try TOTP first, then WebAuthn (method is implicit in the response data)
        let (auth_method, verify_result) = match self.totp_provider.verify(&response).await {
            Ok(r @ tcfs_auth::VerifyResult::Success { .. }) => ("totp", Ok(r)),
            _ => ("webauthn", self.webauthn_provider.verify(&response).await),
        };

        match verify_result {
            Ok(tcfs_auth::VerifyResult::Success {
                session_token: _,
                device_id,
            }) => {
                if device_id != req.device_id {
                    self.rate_limiter.record_failure(&req.device_id).await;
                    return Ok(tonic::Response::new(AuthVerifyResponse {
                        success: false,
                        session_token: String::new(),
                        error: "verified identity does not match requested device".into(),
                    }));
                }
                let Some(authorization) = self.device_authorizations.get(&device_id).await else {
                    self.rate_limiter.record_failure(&device_id).await;
                    tracing::warn!(%device_id, "authenticated device has no enrollment authorization");
                    return Ok(tonic::Response::new(AuthVerifyResponse {
                        success: false,
                        session_token: String::new(),
                        error: "device is not enrolled or has been revoked".into(),
                    }));
                };
                let session =
                    tcfs_auth::Session::new(&device_id, &authorization.device_name, auth_method)
                        .with_permissions(authorization.permissions)
                        .with_expiry(self.config.auth.session_expiry_hours);
                let token = session.token.clone();
                self.session_store.insert(session).await;
                self.persist_sessions().await;
                self.rate_limiter.clear(&device_id).await;

                info!(device_id = %device_id, method = auth_method, "auth succeeded, session created");
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: true,
                    session_token: token,
                    error: String::new(),
                }))
            }
            Ok(tcfs_auth::VerifyResult::Failure { reason }) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: reason,
                }))
            }
            Ok(tcfs_auth::VerifyResult::Expired) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: "challenge expired".into(),
                }))
            }
            Err(e) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: format!("verification error: {e}"),
                }))
            }
        }
    }

    // ── Session Revocation ────────────────────────────────────────────────

    async fn auth_revoke(
        &self,
        request: tonic::Request<AuthRevokeRequest>,
    ) -> Result<tonic::Response<AuthRevokeResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "admin")?;
        let req = request.into_inner();

        if !req.session_token.is_empty() {
            // Revoke specific session by token
            info!(
                token_prefix = &req.session_token[..8.min(req.session_token.len())],
                "revoking session by token"
            );
            self.session_store.revoke(&req.session_token).await;
        } else if !req.device_id.is_empty() {
            // Revoke all sessions for device
            info!(device_id = %req.device_id, "revoking all sessions for device");
            self.session_store.revoke_device(&req.device_id).await;
        } else {
            return Ok(tonic::Response::new(AuthRevokeResponse {
                success: false,
                error: "must specify session_token or device_id".into(),
            }));
        }

        self.persist_sessions().await;
        Ok(tonic::Response::new(AuthRevokeResponse {
            success: true,
            error: String::new(),
        }))
    }

    // ── Device Enrollment ────────────────────────────────────────────────

    async fn device_enroll(
        &self,
        request: tonic::Request<DeviceEnrollRequest>,
    ) -> Result<tonic::Response<DeviceEnrollResponse>, tonic::Status> {
        let req = request.into_inner();
        info!(device_name = %req.device_name, platform = %req.platform, "device enroll requested");

        // Decode and validate the enrollment invite
        let invite = tcfs_auth::EnrollmentInvite::decode_any(&req.invite_data)
            .map_err(|e| tonic::Status::invalid_argument(format!("invalid invite: {e}")))?;

        if invite.is_expired() {
            return Ok(tonic::Response::new(DeviceEnrollResponse {
                success: false,
                error: "invite has expired".into(),
                ..Default::default()
            }));
        }

        if !tcfs_secrets::device::is_real_age_public_key(&req.public_key) {
            return Ok(tonic::Response::new(DeviceEnrollResponse {
                success: false,
                error: "invalid device public key; expected age X25519 recipient".into(),
                ..Default::default()
            }));
        }

        // Validate invite signature against master key
        if let Some(mk) = self.master_key.lock().await.as_ref() {
            let signing_key: [u8; 32] = *blake3::hash(mk.as_bytes()).as_bytes();
            if !invite.verify_signature(&signing_key) {
                return Ok(tonic::Response::new(DeviceEnrollResponse {
                    success: false,
                    error: "invalid invite signature".into(),
                    ..Default::default()
                }));
            }
        } else {
            // No master key loaded — cannot verify invite
            return Err(tonic::Status::failed_precondition(
                "daemon master key not loaded — cannot verify invite signature",
            ));
        }

        let device_id = uuid::Uuid::new_v4().to_string();
        let bootstrap_session =
            tcfs_auth::Session::new(&device_id, &req.device_name, "enrollment_invite")
                .with_permissions(invite.permissions.clone())
                .with_expiry(1);
        let bootstrap = self
            .enrollment_bootstrap_for_invite(&invite, &bootstrap_session)
            .await?;
        let bootstrap_json = serde_json::to_vec(&bootstrap).map_err(|e| {
            tonic::Status::internal(format!("serializing enrollment bootstrap: {e}"))
        })?;
        let wrapped_bootstrap_age =
            tcfs_secrets::age::encrypt_for_recipient(&req.public_key, &bootstrap_json).map_err(
                |e| tonic::Status::invalid_argument(format!("wrapping enrollment bootstrap: {e}")),
            )?;

        match self
            .invite_redemptions
            .claim(
                &invite.invite_id,
                &invite.nonce,
                &req.device_name,
                &req.public_key,
                &req.platform,
            )
            .await
        {
            Ok(_) => {
                if let Err(e) = self.persist_invite_redemptions().await {
                    tracing::warn!(error = %e, invite_id = %invite.invite_id, "failed to persist invite redemption");
                    return Err(tonic::Status::internal(
                        "failed to persist invite redemption state",
                    ));
                }
            }
            Err(tcfs_auth::InviteRedemptionError::AlreadyRedeemed { .. }) => {
                return Ok(tonic::Response::new(DeviceEnrollResponse {
                    success: false,
                    error: "invite has already been redeemed".into(),
                    ..Default::default()
                }));
            }
        }

        // Enroll device in the local registry
        self.device_authorizations
            .authorize(
                device_id.clone(),
                req.device_name.clone(),
                invite.permissions.clone(),
            )
            .await;
        if let Err(error) = self.persist_device_authorizations().await {
            tracing::error!(
                %error,
                %device_id,
                "failed to persist enrolled device authorization"
            );
            return Err(tonic::Status::internal(
                "failed to persist enrolled device authorization",
            ));
        }
        self.session_store.insert(bootstrap_session).await;
        self.persist_sessions().await;
        info!(
            device_id = %device_id,
            device_name = %req.device_name,
            platform = %req.platform,
            invited_by = %invite.created_by,
            "device enrolled via invite"
        );

        Ok(tonic::Response::new(DeviceEnrollResponse {
            success: true,
            device_id,
            nats_url: bootstrap.nats_url.unwrap_or_default(),
            storage_endpoint: bootstrap.storage_endpoint.unwrap_or_default(),
            available_auth_methods: vec!["totp".into()],
            error: String::new(),
            storage_bucket: bootstrap.storage_bucket.unwrap_or_default(),
            storage_access_key: String::new(),
            storage_secret: String::new(),
            remote_prefix: bootstrap.remote_prefix.unwrap_or_default(),
            encryption_passphrase: String::new(),
            encryption_salt: bootstrap.encryption_salt.unwrap_or_default(),
            wrapped_bootstrap_age,
        }))
    }

    // ── Diagnostics ──────────────────────────────────────────────────────

    async fn diagnostics(
        &self,
        _request: tonic::Request<DiagnosticsRequest>,
    ) -> Result<tonic::Response<DiagnosticsResponse>, tonic::Status> {
        let cache = self.state_cache.lock().await;

        // Count conflicts
        let mut conflict_count = 0i32;
        for (_key, state) in StateCacheBackend::all_entries(&*cache) {
            if state.conflict.is_some() {
                conflict_count += 1;
            }
        }

        // Count auto-unsync eligible files
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let max_age = self.config.sync.auto_unsync_max_age_secs;
        let eligible = if max_age > 0 {
            StateCacheBackend::all_entries(&*cache)
                .iter()
                .filter(|(_, s)| now.saturating_sub(s.last_synced) > max_age)
                .count() as i32
        } else {
            0
        };

        Ok(tonic::Response::new(DiagnosticsResponse {
            state_cache_entries: StateCacheBackend::len(&*cache) as i32,
            conflict_count,
            last_nats_seq: cache.last_nats_seq() as i64,
            nats_connected: self.nats_ok.load(std::sync::atomic::Ordering::Relaxed),
            auto_unsync_eligible: eligible,
            auto_unsync_max_age_secs: max_age as i64,
            storage_reachable: self.storage_ok,
            uptime_secs: self.start_time.elapsed().as_secs() as i64,
            device_id: self.device_id.clone(),
        }))
    }
}

/// Bind a Unix domain socket, removing any stale socket and creating parent dirs.
///
/// `UnixListener::bind` is synchronous on Unix. On macOS we have observed
/// App Group paths wedge inside `bind(2)`, so keep the filesystem work on the
/// blocking pool. That lets callers put a real timeout around optional sockets
/// without sacrificing a Tokio runtime worker.
#[cfg(unix)]
fn bind_uds_blocking(socket_path: PathBuf) -> Result<StdUnixListener> {
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = StdUnixListener::bind(&socket_path)?;
    if let Err(e) = listener.set_nonblocking(true) {
        drop(listener);
        let _ = std::fs::remove_file(&socket_path);
        return Err(anyhow::anyhow!("setting socket nonblocking mode: {e}"));
    }

    // Restrict socket to owner-only access (prevents other users from connecting)
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)) {
        drop(listener);
        let _ = std::fs::remove_file(&socket_path);
        return Err(anyhow::anyhow!("setting socket permissions: {e}"));
    }

    Ok(listener)
}

#[cfg(unix)]
fn spawn_uds_bind_with<F>(socket_path: PathBuf, binder: F) -> JoinHandle<Result<StdUnixListener>>
where
    F: FnOnce(PathBuf) -> Result<StdUnixListener> + Send + 'static,
{
    tokio::task::spawn_blocking(move || binder(socket_path))
}

#[cfg(unix)]
async fn finish_uds_bind(
    bind_task: &mut JoinHandle<Result<StdUnixListener>>,
) -> Result<UnixListenerStream> {
    let listener = bind_task
        .await
        .map_err(|e| anyhow::anyhow!("Unix socket bind task failed: {e}"))??;
    let listener = UnixListener::from_std(listener)?;

    Ok(UnixListenerStream::new(listener))
}

#[cfg(unix)]
async fn bind_uds_with<F>(socket_path: &Path, binder: F) -> Result<UnixListenerStream>
where
    F: FnOnce(PathBuf) -> Result<StdUnixListener> + Send + 'static,
{
    let mut bind_task = spawn_uds_bind_with(socket_path.to_path_buf(), binder);
    finish_uds_bind(&mut bind_task).await
}

#[cfg(unix)]
fn cleanup_late_uds_bind(socket_path: PathBuf, mut bind_task: JoinHandle<Result<StdUnixListener>>) {
    tokio::spawn(async move {
        match finish_uds_bind(&mut bind_task).await {
            Ok(listener) => {
                drop(listener);
                if let Err(e) = std::fs::remove_file(&socket_path) {
                    warn!(
                        socket = %socket_path.display(),
                        "late Unix socket bind completed after shutdown but cleanup failed: {e}"
                    );
                }
            }
            Err(e) => {
                warn!(
                    socket = %socket_path.display(),
                    "late Unix socket bind completed after shutdown with error: {e}"
                );
            }
        }
    });
}

#[cfg(unix)]
async fn bind_optional_uds_with_warning<F>(
    socket_path: PathBuf,
    warn_after: std::time::Duration,
    shutdown: Arc<tokio::sync::Notify>,
    binder: F,
) -> Option<Result<UnixListenerStream>>
where
    F: FnOnce(PathBuf) -> Result<StdUnixListener> + Send + 'static,
{
    let mut bind_task = spawn_uds_bind_with(socket_path.clone(), binder);

    match tokio::time::timeout(warn_after, finish_uds_bind(&mut bind_task)).await {
        Ok(result) => Some(result),
        Err(_) => {
            warn!(
                socket = %socket_path.display(),
                "FileProvider gRPC socket bind still pending; primary daemon socket remains active"
            );

            tokio::select! {
                result = finish_uds_bind(&mut bind_task) => Some(result),
                _ = shutdown.notified() => {
                    cleanup_late_uds_bind(socket_path, bind_task);
                    None
                }
            }
        }
    }
}

#[cfg(unix)]
async fn bind_uds(socket_path: &Path) -> Result<UnixListenerStream> {
    bind_uds_with(socket_path, bind_uds_blocking).await
}

/// Start the gRPC server on a Unix domain socket with graceful shutdown support.
///
/// If `fileprovider_socket` is provided, a second server is spawned on that socket
/// for sandboxed macOS FileProvider access (App Group container).
pub async fn serve(
    socket_path: &Path,
    fileprovider_socket: Option<&Path>,
    listen: Option<&str>,
    impl_: TcfsDaemonImpl,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let primary = bind_uds(socket_path).await?;
    info!(socket = %socket_path.display(), "gRPC server ready");

    let service = TcfsDaemonServer::new(impl_);

    let tcp_handle = if let Some(addr) = listen {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        info!(addr = %local_addr, "gRPC TCP listener ready");

        let tcp_service = service.clone();
        let tcp_shutdown = Arc::new(tokio::sync::Notify::new());
        let tcp_shutdown_clone = tcp_shutdown.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = Server::builder()
                .add_service(tcp_service)
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    tcp_shutdown_clone.notified(),
                )
                .await
            {
                tracing::warn!("TCP gRPC server error: {e}");
            }
        });

        Some((handle, tcp_shutdown))
    } else {
        None
    };

    // Spawn a second gRPC server on the FileProvider socket if configured.
    //
    // This listener is optional. Keep its bind path off the startup critical
    // path so a stale App Group socket cannot leave the primary daemon socket
    // bound but unserved.
    let fp_handle = if let Some(fp_path) = fileprovider_socket {
        let fp_path = fp_path.to_path_buf();
        let fp_service = service.clone();
        let fp_shutdown = Arc::new(tokio::sync::Notify::new());
        let fp_shutdown_clone = fp_shutdown.clone();

        let handle = tokio::spawn(async move {
            let secondary = match bind_optional_uds_with_warning(
                fp_path.clone(),
                std::time::Duration::from_secs(5),
                fp_shutdown_clone.clone(),
                bind_uds_blocking,
            )
            .await
            {
                Some(Ok(listener)) => listener,
                Some(Err(e)) => {
                    warn!(
                        socket = %fp_path.display(),
                        "FileProvider gRPC socket bind failed; primary daemon socket remains active: {e}"
                    );
                    return;
                }
                None => {
                    return;
                }
            };

            info!(socket = %fp_path.display(), "gRPC FileProvider socket ready");
            if let Err(e) = Server::builder()
                .add_service(fp_service)
                .serve_with_incoming_shutdown(secondary, fp_shutdown_clone.notified())
                .await
            {
                tracing::warn!("FileProvider gRPC server error: {e}");
            }
        });

        Some((handle, fp_shutdown))
    } else {
        None
    };

    let result = Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(primary, shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"));

    // Stop the FileProvider server when the primary shuts down
    if let Some((handle, notify)) = fp_handle {
        notify.notify_one();
        if tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .is_err()
        {
            warn!("FileProvider gRPC server did not stop within timeout");
        }
    }
    if let Some((handle, notify)) = tcp_handle {
        notify.notify_one();
        let _ = handle.await;
    }

    result
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use opendal::services::Memory;
    use opendal::Operator;
    use secrecy::{ExposeSecret, SecretString};
    use tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient;
    use tokio_stream::StreamExt;
    use tonic::transport::{Channel, Endpoint, Uri};
    use tower::service_fn;

    #[test]
    fn legacy_push_rejects_non_publication_outcomes() {
        let remote_newer = tcfs_sync::conflict::SyncOutcome::RemoteNewer;
        assert!(legacy_push_rejection_reason(false, Some(&remote_newer))
            .unwrap()
            .contains("remote version is newer"));

        let conflict =
            tcfs_sync::conflict::SyncOutcome::Conflict(tcfs_sync::conflict::ConflictInfo {
                rel_path: "conflict.txt".into(),
                local_vclock: tcfs_sync::conflict::VectorClock::new(),
                remote_vclock: tcfs_sync::conflict::VectorClock::new(),
                local_blake3: "local".into(),
                remote_blake3: "remote".into(),
                local_device: "local-device".into(),
                remote_device: "remote-device".into(),
                detected_at: 1,
                times_recorded: 1,
                remote_manifest_key: None,
            });
        let error = legacy_push_rejection_reason(false, Some(&conflict)).unwrap();
        assert!(error.contains("conflict"));
        assert!(error.contains("local-device"));
        assert!(error.contains("remote-device"));

        let up_to_date = tcfs_sync::conflict::SyncOutcome::UpToDate;
        assert!(legacy_push_rejection_reason(true, Some(&up_to_date))
            .unwrap()
            .contains("skipped publication"));
        assert!(legacy_push_rejection_reason(false, Some(&up_to_date)).is_none());
    }

    /// Build a TcfsDaemonImpl with in-memory components for testing.
    fn test_daemon_with_operator_master_and_session_requirement(
        operator_value: Option<Operator>,
        master_key: Option<tcfs_crypto::MasterKey>,
        require_session: bool,
    ) -> TcfsDaemonImpl {
        let mut config = TcfsConfig::default();
        config.auth.require_session = require_session;
        config.storage.bucket = "data".into();
        config.storage.remote_prefix = Some("data".into());
        let config = Arc::new(config);
        let cred_store = crate::cred_store::new_shared();
        let state_dir = tempfile::tempdir().unwrap().keep();
        let state_path = state_dir.join("state.json");
        let state_cache = Arc::new(TokioMutex::new(
            tcfs_sync::state::StateCache::open(&state_path).unwrap(),
        ));
        let operator = Arc::new(TokioMutex::new(operator_value.clone()));

        TcfsDaemonImpl::new(
            cred_store,
            config,
            operator_value.is_some(),
            "http://test:8333".into(),
            state_cache,
            operator,
            tcfs_sync::state::PathLocks::new(),
            "test-device-id".into(),
            "test-device".into(),
            master_key,
        )
    }

    fn test_daemon_with_operator_and_master(
        operator_value: Option<Operator>,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> TcfsDaemonImpl {
        test_daemon_with_operator_master_and_session_requirement(operator_value, master_key, false)
    }

    fn test_daemon_with_required_sessions() -> TcfsDaemonImpl {
        test_daemon_with_operator_master_and_session_requirement(None, None, true)
    }

    fn test_daemon_with_operator(operator_value: Option<Operator>) -> TcfsDaemonImpl {
        test_daemon_with_operator_and_master(operator_value, None)
    }

    fn test_daemon() -> TcfsDaemonImpl {
        test_daemon_with_operator(None)
    }

    fn test_daemon_with_registered_root_session_requirement(
        temp: &tempfile::TempDir,
        root_id: &str,
        local_root: &Path,
        require_session: bool,
    ) -> TcfsDaemonImpl {
        let reconcile_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&reconcile_dir).unwrap();
        let state_path = reconcile_dir.join(format!("{root_id}.json"));
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        state.set_device_id("test-device".to_string());
        state.flush().unwrap();

        let mut config = TcfsConfig::default();
        config.auth.require_session = require_session;
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.storage.remote_prefix = Some("data".into());
        config.sync.state_db = temp.path().join("primary.db");
        config.sync.roots.insert(
            root_id.into(),
            RegisteredRootConfig {
                local_root: local_root.to_path_buf(),
                remote_prefix: format!("roots/{root_id}"),
                state_path,
                policy: RegisteredRootPolicy::Resolve,
            },
        );

        TcfsDaemonImpl::new(
            crate::cred_store::new_shared(),
            Arc::new(config),
            false,
            "memory://".into(),
            Arc::new(TokioMutex::new(
                tcfs_sync::state::StateCache::open(&temp.path().join("primary.json")).unwrap(),
            )),
            Arc::new(TokioMutex::new(None)),
            tcfs_sync::state::PathLocks::new(),
            "test-device".into(),
            "test-device".into(),
            None,
        )
    }

    fn test_daemon_with_registered_root(
        temp: &tempfile::TempDir,
        root_id: &str,
        local_root: &Path,
    ) -> TcfsDaemonImpl {
        test_daemon_with_registered_root_session_requirement(temp, root_id, local_root, false)
    }

    fn memory_operator() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    async fn seed_remote_file(op: &Operator, rel_path: &str) -> String {
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: blake3::hash(b"").to_hex().to_string(),
            file_size: 0,
            chunks: Vec::new(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "remote-test-device".into(),
            written_at: 1_700_000_000,
            rel_path: Some(rel_path.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        let manifest_bytes = manifest.to_bytes().unwrap();
        let manifest_id = tcfs_sync::index_entry::manifest_object_id(&manifest_bytes);
        op.write(&format!("data/manifests/{manifest_id}"), manifest_bytes)
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            op,
            "data",
            &format!("data/index/{rel_path}"),
            &tcfs_sync::index_entry::RemoteIndexEntry::new(&manifest_id, 0, 0),
        )
        .await
        .unwrap();
        manifest_id
    }

    fn run_test_git(repo: &Path, args: &[&str]) {
        #[cfg(windows)]
        let null_device = "NUL";
        #[cfg(not(windows))]
        let null_device = "/dev/null";
        let output = std::process::Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env("GIT_CONFIG_GLOBAL", null_device)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-c")
            .arg(format!("core.hooksPath={null_device}"))
            .arg("-c")
            .arg("commit.gpgSign=false")
            .arg("-c")
            .arg("tag.gpgSign=false")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_clean_test_repo(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        run_test_git(repo, &["init", "-q"]);
        run_test_git(repo, &["config", "user.name", "TCFS Test"]);
        run_test_git(repo, &["config", "user.email", "tcfs@example.invalid"]);
        std::fs::write(repo.join("README.md"), b"stable root\n").unwrap();
        run_test_git(repo, &["add", "README.md"]);
        run_test_git(repo, &["commit", "-q", "-m", "initial"]);
    }

    fn test_device_keypair() -> tcfs_secrets::device::LocalDeviceKey {
        tcfs_secrets::device::generate_local_device_key()
    }

    async fn insert_test_s3_credentials(
        daemon: &TcfsDaemonImpl,
        access_key: &str,
        secret_key: &str,
    ) {
        daemon
            .cred_store
            .write()
            .await
            .replace(tcfs_secrets::CredStore {
                s3: Some(tcfs_secrets::S3Credentials {
                    access_key_id: access_key.into(),
                    secret_access_key: SecretString::from(secret_key.to_string()),
                    endpoint: daemon.config.storage.endpoint.clone(),
                    region: daemon.config.storage.region.clone(),
                }),
                source: "test".into(),
            });
    }

    fn request_with_bearer<T>(message: T, token: &str) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
    }

    async fn insert_test_session(
        daemon: &TcfsDaemonImpl,
        device_id: &str,
        permissions: tcfs_auth::DevicePermissions,
    ) -> String {
        daemon
            .device_authorizations
            .authorize(device_id, device_id, permissions.clone())
            .await;
        let session =
            tcfs_auth::Session::new(device_id, device_id, "test").with_permissions(permissions);
        let token = session.token.clone();
        daemon.session_store.insert(session).await;
        token
    }

    fn primary_io_test_daemon(
        temp: &tempfile::TempDir,
        sync_root: &Path,
        operator_value: Option<Operator>,
        master_key_file: Option<PathBuf>,
        require_session: bool,
    ) -> TcfsDaemonImpl {
        let mut config = TcfsConfig::default();
        config.auth.require_session = require_session;
        config.sync.sync_root = Some(sync_root.to_path_buf());
        config.sync.state_db = temp.path().join("hydrate-state.db");
        config.storage.bucket = "data".into();
        config.storage.remote_prefix = Some("data".into());
        config.crypto.master_key_file = master_key_file;

        TcfsDaemonImpl::new(
            crate::cred_store::new_shared(),
            Arc::new(config),
            operator_value.is_some(),
            "memory://".into(),
            Arc::new(TokioMutex::new(
                tcfs_sync::state::StateCache::open(&temp.path().join("hydrate-state.json"))
                    .unwrap(),
            )),
            Arc::new(TokioMutex::new(operator_value)),
            tcfs_sync::state::PathLocks::new(),
            "hydrate-test-device".into(),
            "hydrate-test-device".into(),
            None,
        )
    }

    fn hydrate_test_daemon(
        temp: &tempfile::TempDir,
        sync_root: &Path,
        operator_value: Option<Operator>,
    ) -> TcfsDaemonImpl {
        primary_io_test_daemon(temp, sync_root, operator_value, None, false)
    }

    async fn expect_pull_error(
        daemon: &TcfsDaemonImpl,
        token: &str,
        remote_path: &str,
        local_path: &Path,
    ) -> tonic::Status {
        match daemon
            .pull(request_with_bearer(
                PullRequest {
                    remote_path: remote_path.into(),
                    local_path: local_path.display().to_string(),
                },
                token,
            ))
            .await
        {
            Ok(_) => panic!("pull should have been rejected before storage access"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn pull_rejects_fixed_logical_paths_before_storage_or_cache_access() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let destination = sync_root.join("unchanged.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();
        let daemon = primary_io_test_daemon(&temp, &sync_root, None, None, true);
        let token = insert_test_session(
            &daemon,
            "pull-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        for remote_path in ["master.key", ".rotate-pending", ".env"] {
            let error = expect_pull_error(&daemon, &token, remote_path, &destination).await;
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(
                error.message().contains("fixed security deny-set"),
                "{remote_path}: {error}"
            );
            assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
            assert!(daemon.state_cache.lock().await.get(&destination).is_none());
        }

        for name in ["master.key", ".rotate-pending", ".env"] {
            let blocked_destination = sync_root.join(name);
            std::fs::write(&blocked_destination, b"keep-sensitive-name-bytes").unwrap();
            let error =
                expect_pull_error(&daemon, &token, "notes/safe.txt", &blocked_destination).await;
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(
                error.message().contains("fixed security deny-set"),
                "{error}"
            );
            assert_eq!(
                std::fs::read(&blocked_destination).unwrap(),
                b"keep-sensitive-name-bytes"
            );
            assert!(daemon
                .state_cache
                .lock()
                .await
                .get(&blocked_destination)
                .is_none());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_rejects_fixed_rel_discovered_during_resolution_before_cache_or_write() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let destination = sync_root.join("unchanged.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        let hidden_rel = ".env";
        let hidden_source = sync_root.join(hidden_rel);
        let logical_alias = sync_root.join("innocent.txt");
        std::fs::write(&hidden_source, b"keep-sensitive-source-bytes").unwrap();
        std::os::unix::fs::symlink(&hidden_source, &logical_alias).unwrap();
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: blake3::hash(b"").to_hex().to_string(),
            file_size: 0,
            chunks: Vec::new(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "hostile-peer".into(),
            written_at: 1_700_000_000,
            rel_path: Some(hidden_rel.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        let manifest_bytes = manifest.to_bytes().unwrap();
        let object_id = tcfs_sync::index_entry::manifest_object_id(&manifest_bytes);
        let op = memory_operator();
        op.write(&format!("data/manifests/{object_id}"), manifest_bytes)
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            &format!("data/index/{hidden_rel}"),
            &tcfs_sync::index_entry::RemoteIndexEntry::new(&object_id, 0, 0),
        )
        .await
        .unwrap();

        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op), None, true);
        let token = insert_test_session(
            &daemon,
            "fallback-pull-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;
        let error = expect_pull_error(
            &daemon,
            &token,
            &logical_alias.display().to_string(),
            &destination,
        )
        .await;

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error.message().contains("fixed security deny-set"),
            "{error}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
        assert_eq!(
            std::fs::read(&hidden_source).unwrap(),
            b"keep-sensitive-source-bytes"
        );
        assert!(daemon.state_cache.lock().await.get(&destination).is_none());
    }

    #[tokio::test]
    async fn file_provider_pull_exact_miss_never_uses_same_filename_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let destination = sync_root.join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        let op = memory_operator();
        op.write(
            "data/index/other/doc.txt",
            tcfs_sync::index_entry::RemoteIndexEntry::new("other-object", 0, 0).to_legacy_bytes(),
        )
        .await
        .unwrap();
        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op), None, true);
        let token = insert_test_session(
            &daemon,
            "file-provider-exact-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        let error = match daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "missing/doc.txt".into(),
                    expected_version: "missing-version".into(),
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("an exact FileProvider miss must not use basename fallback"),
            Err(error) => error,
        };

        assert_eq!(error.code(), tonic::Code::NotFound);
        assert!(
            error
                .message()
                .contains("no exact index entry for FileProvider item"),
            "{error}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
        assert!(daemon.state_cache.lock().await.get(&destination).is_none());
    }

    #[tokio::test]
    async fn file_provider_pull_exact_requires_version_and_marks_empty_content() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let op = memory_operator();
        let version = seed_remote_file(&op, "empty.txt").await;
        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op), None, true);
        let token = insert_test_session(
            &daemon,
            "file-provider-version-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        let stale = match daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "empty.txt".into(),
                    expected_version: "stale-version".into(),
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("stale exact version must fail before content streaming"),
            Err(error) => error,
        };
        assert_eq!(stale.code(), tonic::Code::FailedPrecondition);
        assert!(
            stale
                .message()
                .starts_with(FILE_PROVIDER_VERSION_MISMATCH_PREFIX),
            "{stale}"
        );

        let mut stream = daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "empty.txt".into(),
                    expected_version: version.clone(),
                },
                &token,
            ))
            .await
            .unwrap()
            .into_inner();
        let terminal = tokio_stream::StreamExt::next(&mut stream)
            .await
            .expect("empty PullExact must emit a terminal marker")
            .unwrap();
        assert!(terminal.done);
        assert!(terminal.exact_content);
        assert_eq!(terminal.version_token, version);
        assert_eq!(terminal.bytes_received, 0);
        assert_eq!(terminal.total_bytes, 0);
        assert!(terminal.data.is_empty());
        assert!(tokio_stream::StreamExt::next(&mut stream).await.is_none());
    }

    #[tokio::test]
    async fn file_provider_pull_exact_rejects_stale_before_manifest_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let op = memory_operator();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            "data/index/corrupt.txt",
            &tcfs_sync::index_entry::RemoteIndexEntry::new("missing-manifest", 0, 0),
        )
        .await
        .unwrap();
        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op.clone()), None, true);
        let token = insert_test_session(
            &daemon,
            "file-provider-corrupt-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        let stale = match daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "corrupt.txt".into(),
                    expected_version: "stale-manifest".into(),
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("stale exact version must win over unrelated manifest corruption"),
            Err(error) => error,
        };
        assert_eq!(stale.code(), tonic::Code::FailedPrecondition);
        assert!(
            stale
                .message()
                .starts_with(FILE_PROVIDER_VERSION_MISMATCH_PREFIX),
            "{stale}"
        );

        let error = match daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "corrupt.txt".into(),
                    expected_version: "missing-manifest".into(),
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("missing selected manifest is corruption, not logical absence"),
            Err(error) => error,
        };
        assert_ne!(error.code(), tonic::Code::NotFound);
        assert_eq!(error.code(), tonic::Code::Internal);

        let preparing = tcfs_sync::index_entry::VersionedIndexEntry::preparing(
            None,
            tcfs_sync::index_entry::PendingIndexEntry::new(
                "pending-manifest",
                0,
                0,
                "data/staging/manifests/00000000-0000-4000-8000-000000000000-pending-manifest.json",
            ),
        )
        .to_json_bytes()
        .unwrap();
        op.write("data/index/pending.txt", preparing.clone())
            .await
            .unwrap();
        op.write(
            "data/manifests/pending-manifest",
            b"must not be parsed by a bound pending-only pull".to_vec(),
        )
        .await
        .unwrap();
        let pending = match daemon
            .pull_exact(request_with_bearer(
                PullExactRequest {
                    remote_path: "pending.txt".into(),
                    expected_version: "pending-manifest".into(),
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("bound PullExact must not recover a never-visible pending version"),
            Err(error) => error,
        };
        assert_eq!(pending.code(), tonic::Code::NotFound);
        assert_eq!(
            op.read("data/index/pending.txt").await.unwrap().to_vec(),
            preparing
        );
    }

    #[tokio::test]
    async fn pull_rejects_outside_root_and_custom_master_key_alias_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let key_path = sync_root.join("custom-key-material.bin");
        std::fs::write(&key_path, b"keep-key-bytes").unwrap();
        #[cfg(unix)]
        let key_alias = {
            let alias = sync_root.join("key-alias.bin");
            std::os::unix::fs::symlink(&key_path, &alias).unwrap();
            alias
        };
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"keep-outside-bytes").unwrap();

        let daemon = primary_io_test_daemon(&temp, &sync_root, None, Some(key_path.clone()), true);
        let token = insert_test_session(
            &daemon,
            "scoped-pull-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        let outside_error = expect_pull_error(&daemon, &token, "notes/safe.txt", &outside).await;
        assert_eq!(outside_error.code(), tonic::Code::InvalidArgument);
        assert!(outside_error
            .message()
            .contains("outside configured sync.sync_root"));
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep-outside-bytes");

        let error = expect_pull_error(&daemon, &token, "notes/safe.txt", &key_path).await;
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error.message().contains("crypto.master_key_file"),
            "{error}"
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), b"keep-key-bytes");
        assert!(daemon.state_cache.lock().await.get(&key_path).is_none());
        #[cfg(unix)]
        {
            let error = expect_pull_error(&daemon, &token, "notes/safe.txt", &key_alias).await;
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(
                error.message().contains("crypto.master_key_file"),
                "{error}"
            );
            assert_eq!(std::fs::read(&key_path).unwrap(), b"keep-key-bytes");
        }
    }

    #[tokio::test]
    async fn hydrate_rejects_fixed_targets_before_storage_or_cache_access() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let daemon = primary_io_test_daemon(&temp, &sync_root, None, None, true);
        let token = insert_test_session(
            &daemon,
            "hydrate-fixed-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        for name in ["master.key", ".rotate-pending", ".env"] {
            let real_path = sync_root.join(name);
            let stub_path = sync_root.join(tcfs_vfs::real_to_stub_name(std::ffi::OsStr::new(name)));
            std::fs::write(&real_path, b"keep-real-bytes").unwrap();
            let stub = tcfs_vfs::StubMeta::for_upload(
                blake3::hash(b"old").to_hex().as_ref(),
                3,
                1,
                "data",
                name,
            );
            let stub_bytes = stub.to_bytes();
            std::fs::write(&stub_path, &stub_bytes).unwrap();

            let error = match daemon
                .hydrate(request_with_bearer(
                    HydrateRequest {
                        stub_path: stub_path.display().to_string(),
                        partial_ok: false,
                    },
                    &token,
                ))
                .await
            {
                Ok(_) => panic!("fixed-deny hydrate should fail before storage access"),
                Err(error) => error,
            };
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(
                error.message().contains("fixed security deny-set"),
                "{error}"
            );
            assert_eq!(std::fs::read(&real_path).unwrap(), b"keep-real-bytes");
            assert_eq!(std::fs::read(&stub_path).unwrap(), stub_bytes);
            assert!(daemon.state_cache.lock().await.get(&real_path).is_none());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hydrate_rejects_custom_master_key_through_symlinked_directory_alias() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        let real_dir = sync_root.join("real");
        let alias_dir = sync_root.join("alias");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();
        let key_path = real_dir.join("custom-key-material.bin");
        let canonical_stub = real_dir.join("custom-key-material.bin.tc");
        let aliased_stub = alias_dir.join("custom-key-material.bin.tc");
        std::fs::write(&key_path, b"keep-key-bytes").unwrap();
        let stub = tcfs_vfs::StubMeta::for_upload(
            blake3::hash(b"old").to_hex().as_ref(),
            3,
            1,
            "data",
            "real/custom-key-material.bin",
        );
        let stub_bytes = stub.to_bytes();
        std::fs::write(&canonical_stub, &stub_bytes).unwrap();

        let daemon = primary_io_test_daemon(&temp, &sync_root, None, Some(key_path.clone()), true);
        let token = insert_test_session(
            &daemon,
            "hydrate-key-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;
        let error = match daemon
            .hydrate(request_with_bearer(
                HydrateRequest {
                    stub_path: aliased_stub.display().to_string(),
                    partial_ok: false,
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("master-key hydrate alias should fail before storage access"),
            Err(error) => error,
        };

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error.message().contains("crypto.master_key_file"),
            "{error}"
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), b"keep-key-bytes");
        assert_eq!(std::fs::read(&canonical_stub).unwrap(), stub_bytes);
        assert!(daemon.state_cache.lock().await.get(&key_path).is_none());
    }

    #[tokio::test]
    async fn hydrate_stale_stub_uses_current_exact_index() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        let rel_path = "notes/todo.txt";
        std::fs::create_dir_all(sync_root.join("notes")).unwrap();

        let current_hash = blake3::hash(b"").to_hex().to_string();
        let stale_hash = blake3::hash(b"old content").to_hex().to_string();
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: current_hash.clone(),
            file_size: 0,
            chunks: Vec::new(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "honey".into(),
            written_at: 1_700_000_000,
            rel_path: Some(rel_path.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        let manifest_bytes = manifest.to_bytes().unwrap();
        let manifest_id = tcfs_sync::index_entry::manifest_object_id(&manifest_bytes);
        let op = memory_operator();
        op.write(&format!("data/manifests/{manifest_id}"), manifest_bytes)
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            &format!("data/index/{rel_path}"),
            &tcfs_sync::index_entry::RemoteIndexEntry::new(&manifest_id, 0, 0),
        )
        .await
        .unwrap();

        let stub_path = sync_root.join("notes/todo.txt.tc");
        let stub = tcfs_vfs::StubMeta::for_upload(&stale_hash, 11, 1, "data", rel_path);
        std::fs::write(&stub_path, stub.to_bytes()).unwrap();

        let daemon = hydrate_test_daemon(&temp, &sync_root, Some(op));
        let token = insert_test_session(
            &daemon,
            "hydrate-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;
        let response = daemon
            .hydrate(request_with_bearer(
                HydrateRequest {
                    stub_path: stub_path.display().to_string(),
                    partial_ok: false,
                },
                &token,
            ))
            .await
            .unwrap();
        let progress = response
            .into_inner()
            .next()
            .await
            .expect("hydrate progress")
            .unwrap();

        assert!(progress.done);
        assert!(
            progress.error.is_empty(),
            "hydrate error: {}",
            progress.error
        );
        assert_eq!(progress.bytes_received, 0);
        assert_eq!(progress.total_bytes, 0);
        assert_eq!(std::fs::read(sync_root.join(rel_path)).unwrap(), b"");
        assert!(!stub_path.exists());
    }

    #[tokio::test]
    async fn hydrate_exact_index_miss_never_uses_same_filename_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        let requested_rel = "notes/todo.txt";
        let other_rel = "other/todo.txt";
        std::fs::create_dir_all(sync_root.join("notes")).unwrap();

        let empty_hash = blake3::hash(b"").to_hex().to_string();
        let other_manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: empty_hash,
            file_size: 0,
            chunks: Vec::new(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "remote-peer".into(),
            written_at: 1_700_000_000,
            rel_path: Some(other_rel.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        let other_bytes = other_manifest.to_bytes().unwrap();
        let other_id = tcfs_sync::index_entry::manifest_object_id(&other_bytes);
        let op = memory_operator();
        op.write(&format!("data/manifests/{other_id}"), other_bytes)
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            &format!("data/index/{other_rel}"),
            &tcfs_sync::index_entry::RemoteIndexEntry::new(&other_id, 0, 0),
        )
        .await
        .unwrap();

        let stub_path = sync_root.join("notes/todo.txt.tc");
        let stub = tcfs_vfs::StubMeta::for_upload(
            blake3::hash(b"stale").to_hex().as_ref(),
            5,
            1,
            "data",
            requested_rel,
        );
        let stub_bytes = stub.to_bytes();
        std::fs::write(&stub_path, &stub_bytes).unwrap();

        let daemon = hydrate_test_daemon(&temp, &sync_root, Some(op));
        let token = insert_test_session(
            &daemon,
            "hydrate-exact-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;
        let error = match daemon
            .hydrate(request_with_bearer(
                HydrateRequest {
                    stub_path: stub_path.display().to_string(),
                    partial_ok: false,
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("exact hydrate miss must not use filename fallback"),
            Err(error) => error,
        };

        assert_eq!(error.code(), tonic::Code::NotFound);
        assert!(error.message().contains("no current index entry"));
        assert_eq!(std::fs::read(&stub_path).unwrap(), stub_bytes);
        let real_path = sync_root.join(requested_rel);
        assert!(std::fs::symlink_metadata(&real_path).is_err());
        assert!(daemon.state_cache.lock().await.get(&real_path).is_none());
    }

    #[tokio::test]
    async fn hydrate_rejects_stub_origin_for_a_different_local_path() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(sync_root.join("notes")).unwrap();
        let stub_path = sync_root.join("notes/todo.txt.tc");
        let stub = tcfs_vfs::StubMeta::for_upload(
            blake3::hash(b"old").to_hex().as_ref(),
            3,
            1,
            "data",
            "other/path.txt",
        );
        std::fs::write(&stub_path, stub.to_bytes()).unwrap();
        let daemon = hydrate_test_daemon(&temp, &sync_root, None);
        let token = insert_test_session(
            &daemon,
            "hydrate-reader",
            tcfs_auth::DevicePermissions::read_only(),
        )
        .await;

        let error = match daemon
            .hydrate(request_with_bearer(
                HydrateRequest {
                    stub_path: stub_path.display().to_string(),
                    partial_ok: false,
                },
                &token,
            ))
            .await
        {
            Ok(_) => panic!("mismatched stub origin must be rejected"),
            Err(error) => error,
        };

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error
                .message()
                .contains("does not match local hydrate target"),
            "unexpected error: {error}"
        );
    }

    fn test_sync_state(remote_path: &str, last_synced: u64) -> tcfs_sync::state::SyncState {
        tcfs_sync::state::SyncState {
            blake3: "test-blake3".into(),
            size: 123,
            mtime: 0,
            chunk_count: 1,
            remote_path: remote_path.into(),
            last_synced,
            vclock: tcfs_sync::conflict::VectorClock::default(),
            device_id: "remote-device".into(),
            conflict: None,
            status: tcfs_sync::state::FileSyncStatus::NotSynced,
        }
    }

    async fn connect_test_client(socket_path: &Path) -> TcfsDaemonClient<Channel> {
        let path = socket_path.to_path_buf();
        let mut last_err = None;

        for _ in 0..50 {
            match Endpoint::from_static("http://[::]:0")
                .connect_with_connector(service_fn({
                    let path = path.clone();
                    move |_: Uri| {
                        let path = path.clone();
                        async move {
                            let stream = tokio::net::UnixStream::connect(&path).await?;
                            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                        }
                    }
                }))
                .await
            {
                Ok(channel) => return TcfsDaemonClient::new(channel),
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }

        panic!(
            "failed to connect test client to {}: {}",
            socket_path.display(),
            last_err.unwrap()
        );
    }

    async fn spawn_test_server(
        daemon: TcfsDaemonImpl,
    ) -> (
        tempfile::TempDir,
        tokio::task::JoinHandle<Result<()>>,
        Arc<tokio::sync::Notify>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tcfsd.sock");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let shutdown_for_server = shutdown.clone();

        let handle = tokio::spawn(async move {
            serve(
                &socket_path,
                None,
                None,
                daemon,
                shutdown_for_server.notified(),
            )
            .await
        });

        (dir, handle, shutdown)
    }

    #[tokio::test]
    async fn status_returns_version() {
        let mut daemon = test_daemon();
        daemon.storage_endpoint =
            "https://status-user:STATUS-secret@storage.example.test:8333/STATUS-path?signature=STATUS-query#STATUS-fragment"
                .into();
        let resp = daemon
            .status(tonic::Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(resp.device_id, "test-device-id");
        assert_eq!(resp.device_name, "test-device");
        assert_eq!(resp.storage_endpoint, "https://storage.example.test:8333");
        for forbidden in [
            "status-user",
            "STATUS-secret",
            "STATUS-path",
            "STATUS-query",
            "STATUS-fragment",
        ] {
            assert!(
                !resp.storage_endpoint.contains(forbidden),
                "status endpoint leaked {forbidden}: {}",
                resp.storage_endpoint
            );
        }
        assert!(!resp.storage_ok);
        assert!(!resp.nats_ok);
        assert_eq!(resp.active_mounts, 0);
        assert!(resp.uptime_secs >= 0);
    }

    #[tokio::test]
    async fn bind_uds_timeout_can_fire_while_blocking_bind_is_stuck() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tcfsd.sock");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let release_for_binder = release.clone();

        let socket_path_for_bind = socket_path.clone();
        let mut bind_task = tokio::spawn(async move {
            bind_uds_with(&socket_path_for_bind, move |_path| {
                let _ = started_tx.send(());
                let (lock, cvar) = &*release_for_binder;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = cvar.wait(released).unwrap();
                }
                Err(anyhow::anyhow!("released test binder"))
            })
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("blocking binder did not start")
            .expect("blocking binder did not signal start");

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut bind_task).await;
        assert!(
            result.is_err(),
            "bind_uds timeout should fire even while blocking bind is stuck"
        );

        let (lock, cvar) = &*release;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
        let result = bind_task.await.expect("bind task panicked");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn optional_uds_late_bind_is_served_after_startup_budget() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tcfsd-fileprovider.sock");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let release_for_binder = release.clone();
        let shutdown = Arc::new(tokio::sync::Notify::new());

        let socket_path_for_bind = socket_path.clone();
        let mut bind_task = tokio::spawn(async move {
            bind_optional_uds_with_warning(
                socket_path_for_bind,
                std::time::Duration::from_millis(20),
                shutdown,
                move |path| {
                    let _ = started_tx.send(());
                    let (lock, cvar) = &*release_for_binder;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = cvar.wait(released).unwrap();
                    }
                    bind_uds_blocking(path)
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("blocking binder did not start")
            .expect("blocking binder did not signal start");

        let startup_budget_result =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut bind_task).await;
        assert!(
            startup_budget_result.is_err(),
            "optional bind should keep waiting after the startup warning budget"
        );

        let (lock, cvar) = &*release;
        *lock.lock().unwrap() = true;
        cvar.notify_all();

        let stream = tokio::time::timeout(std::time::Duration::from_secs(2), bind_task)
            .await
            .expect("late bind task timed out")
            .expect("late bind task panicked")
            .expect("late bind returned None")
            .expect("late bind failed");

        let client = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("late-bound socket should accept connections");
        drop(client);
        drop(stream);
    }

    #[tokio::test]
    async fn primary_uds_serves_when_fileprovider_socket_bind_fails() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tcfsd.sock");
        let not_a_dir = dir.path().join("not-a-dir");
        std::fs::write(&not_a_dir, b"not a directory").unwrap();
        let fileprovider_socket = not_a_dir.join("tcfsd-fileprovider.sock");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let shutdown_for_server = shutdown.clone();

        let socket_path_for_server = socket_path.clone();
        let handle = tokio::spawn(async move {
            serve(
                &socket_path_for_server,
                Some(&fileprovider_socket),
                None,
                daemon,
                shutdown_for_server.notified(),
            )
            .await
        });

        let mut client = connect_test_client(&socket_path).await;
        let resp = client
            .status(tonic::Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.device_id, "test-device-id");

        shutdown.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("server shutdown timed out")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn status_rechecks_live_storage_health() {
        let daemon = test_daemon_with_operator(Some(memory_operator()));
        let resp = daemon
            .status(tonic::Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert!(resp.storage_ok);
    }

    #[tokio::test]
    async fn credential_status_empty() {
        let daemon = test_daemon();
        let resp = daemon
            .credential_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.loaded);
        assert_eq!(resp.source, "none");
        assert!(resp.needs_reload);
    }

    #[tokio::test]
    async fn auth_status_locked_by_default() {
        let daemon = test_daemon();
        let resp = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.unlocked);
        assert!(resp.available_methods.contains(&"master_key".to_string()));
        assert!(resp.auth_method.is_empty());
        assert_eq!(resp.session_device_id, "test-device-id");
    }

    #[tokio::test]
    async fn auth_unlock_then_lock_roundtrip() {
        let daemon = test_daemon();

        // Unlock with a 32-byte key
        let key = vec![0xAA; tcfs_crypto::KEY_SIZE];
        let resp = daemon
            .auth_unlock(tonic::Request::new(AuthUnlockRequest { master_key: key }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.success);

        // Verify unlocked
        let status = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(status.unlocked);

        // Lock
        let resp = daemon
            .auth_lock(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.success);

        // Verify locked
        let status = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(!status.unlocked);
    }

    #[tokio::test]
    async fn auth_unlock_wrong_key_size_fails() {
        let daemon = test_daemon();

        let resp = daemon
            .auth_unlock(tonic::Request::new(AuthUnlockRequest {
                master_key: vec![0x00; 16], // too short
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("must be"));
    }

    #[tokio::test]
    async fn auth_enroll_allows_self_or_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let request = AuthEnrollRequest {
            device_id: "new-device".into(),
            method: "totp".into(),
        };

        let missing = daemon
            .auth_enroll(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_enroll(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);

        let self_request = AuthEnrollRequest {
            device_id: "regular-device".into(),
            method: "unsupported".into(),
        };
        let self_allowed = daemon
            .auth_enroll(request_with_bearer(self_request, &user_token))
            .await
            .unwrap()
            .into_inner();
        assert!(!self_allowed.success);
        assert!(self_allowed.error.contains("unsupported auth method"));

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let mut unsupported = request;
        unsupported.method = "unsupported".into();
        let allowed = daemon
            .auth_enroll(request_with_bearer(unsupported, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(!allowed.success);
        assert!(allowed.error.contains("unsupported auth method"));
    }

    #[tokio::test]
    async fn auth_complete_enroll_allows_self_or_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let request = AuthCompleteEnrollRequest {
            device_id: "new-device".into(),
            method: "totp".into(),
            attestation_data: Vec::new(),
        };

        let missing = daemon
            .auth_complete_enroll(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_complete_enroll(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);

        let self_request = AuthCompleteEnrollRequest {
            device_id: "regular-device".into(),
            method: "totp".into(),
            attestation_data: Vec::new(),
        };
        let self_allowed = daemon
            .auth_complete_enroll(request_with_bearer(self_request, &user_token))
            .await
            .unwrap()
            .into_inner();
        assert!(
            self_allowed.success,
            "self completion failed: {}",
            self_allowed.error
        );

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let allowed = daemon
            .auth_complete_enroll(request_with_bearer(request, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(allowed.success, "admin request failed: {}", allowed.error);
    }

    #[tokio::test]
    async fn auth_revoke_requires_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let target_token = insert_test_session(
            &daemon,
            "target-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let request = AuthRevokeRequest {
            session_token: target_token.clone(),
            device_id: String::new(),
        };

        let missing = daemon
            .auth_revoke(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_revoke(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);
        assert!(daemon.session_store.validate(&target_token).await.is_some());

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let allowed = daemon
            .auth_revoke(request_with_bearer(request, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(allowed.success, "admin revoke failed: {}", allowed.error);
        assert!(daemon.session_store.validate(&target_token).await.is_none());
    }

    #[tokio::test]
    async fn device_enroll_rejects_reused_invite() {
        let key_bytes = [0xA5; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.storage_access_key = Some("test-access".into());
        invite.storage_secret_key = Some("test-secret".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();
        let keypair = test_device_keypair();

        let req = DeviceEnrollRequest {
            invite_data: invite_data.clone(),
            device_name: "new-laptop".into(),
            public_key: keypair.public_key.clone(),
            platform: "linux-x86_64".into(),
        };

        let first = daemon
            .device_enroll(tonic::Request::new(req.clone()))
            .await
            .unwrap()
            .into_inner();
        assert!(first.success, "first enrollment failed: {}", first.error);
        assert!(temp.path().join("tcfsd/invite-redemptions.json").exists());

        let second = daemon
            .device_enroll(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert!(!second.success);
        assert!(second.error.contains("already been redeemed"));
    }

    #[tokio::test]
    async fn device_enroll_rejects_invalid_public_key_before_claiming_invite() {
        let key_bytes = [0xC7; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.storage_access_key = Some("test-access".into());
        invite.storage_secret_key = Some("test-secret".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();

        let rejected = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data: invite_data.clone(),
                device_name: "new-laptop".into(),
                public_key: "not-an-age-recipient".into(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!rejected.success);
        assert!(rejected.error.contains("invalid device public key"));
        assert!(!temp.path().join("tcfsd/invite-redemptions.json").exists());

        let keypair = test_device_keypair();
        let accepted = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data,
                device_name: "new-laptop".into(),
                public_key: keypair.public_key.clone(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(accepted.success, "retry failed: {}", accepted.error);
    }

    #[tokio::test]
    async fn device_enroll_wraps_bootstrap_to_joining_device_key() {
        let key_bytes = [0xB6; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));
        insert_test_s3_credentials(&daemon, "daemon-access", "daemon-secret").await;

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.remote_prefix = Some("tenant/a".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();
        let keypair = test_device_keypair();

        let resp = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data,
                device_name: "new-laptop".into(),
                public_key: keypair.public_key.clone(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(resp.success, "enrollment failed: {}", resp.error);
        assert!(resp.storage_access_key.is_empty());
        assert!(resp.storage_secret.is_empty());
        assert!(resp.encryption_passphrase.is_empty());
        assert!(resp
            .wrapped_bootstrap_age
            .contains("BEGIN AGE ENCRYPTED FILE"));

        let identity = tcfs_secrets::IdentityProvider {
            key_data: keypair.secret_key.expose_secret().to_string(),
            source: "test".into(),
        };
        let plaintext = tcfs_secrets::age::decrypt_with_identity(
            &identity,
            resp.wrapped_bootstrap_age.as_bytes(),
        )
        .unwrap();
        let bootstrap: tcfs_auth::EnrollmentBootstrap = serde_json::from_slice(&plaintext).unwrap();

        assert_eq!(
            bootstrap.storage_endpoint.as_deref(),
            Some("https://s3.example.invalid")
        );
        assert_eq!(bootstrap.storage_bucket.as_deref(), Some("tcfs"));
        assert_eq!(
            bootstrap.storage_access_key.as_deref(),
            Some("daemon-access")
        );
        assert_eq!(
            bootstrap.storage_secret_key.as_deref(),
            Some("daemon-secret")
        );
        assert_eq!(bootstrap.remote_prefix.as_deref(), Some("tenant/a"));
        let expected_master_key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        assert_eq!(
            bootstrap.master_key_base64.as_deref(),
            Some(expected_master_key.as_str())
        );
        let bootstrap_token = bootstrap
            .session_token
            .as_deref()
            .expect("encrypted bootstrap must carry a joining-device session");
        assert!(bootstrap.session_expires_at.is_some());
        let session = daemon
            .session_store
            .validate(bootstrap_token)
            .await
            .expect("bootstrap session must be live");
        assert_eq!(session.device_id, resp.device_id);
        assert_eq!(session.device_name, "new-laptop");
        assert!(!session.permissions.can_admin);
    }

    #[tokio::test]
    async fn diagnostics_empty_state() {
        let daemon = test_daemon();
        let resp = daemon
            .diagnostics(tonic::Request::new(DiagnosticsRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state_cache_entries, 0);
        assert_eq!(resp.conflict_count, 0);
        assert!(!resp.nats_connected);
        assert!(!resp.storage_reachable);
        assert_eq!(resp.device_id, "test-device-id");
    }

    #[tokio::test]
    async fn sync_status_unknown_path() {
        let daemon = test_daemon();
        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: "/nonexistent/file.txt".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        // Unknown path returns empty/default state
        assert!(resp.state.is_empty() || resp.state == "unknown");
    }

    #[tokio::test]
    async fn sync_status_reports_explicit_conflict_state() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("conflicted.txt");
        std::fs::write(&tracked, b"conflicted").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let mut entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "abc123".to_string(),
                1,
                "data/manifests/abc123".to_string(),
            )
            .unwrap();
            entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: tracked.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state, "conflict");
    }

    #[tokio::test]
    async fn sync_status_reports_pending_for_modified_tracked_file() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("modified.txt");
        std::fs::write(&tracked, b"alpha").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "tracked-alpha".to_string(),
                1,
                "data/manifests/alpha".to_string(),
            )
            .unwrap();
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        std::fs::write(&tracked, b"alpha updated").unwrap();

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: tracked.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state, "pending");
    }

    /// When a Synced entry exists but `needs_sync` errors (e.g. the path can't
    /// be stat'd), we must NOT report the stale "synced" state. We report
    /// "unknown" instead so observers can see the IO failure rather than
    /// trusting a lie.
    #[tokio::test]
    async fn sync_status_surfaces_needs_sync_error_instead_of_synced() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        // Path that will never exist — needs_sync's std::fs::metadata will Err.
        let missing = dir.path().join("vanished.txt");

        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(
                &missing,
                tcfs_sync::state::SyncState {
                    blake3: "deadbeef".into(),
                    size: 42,
                    mtime: 1_700_000_000,
                    chunk_count: 1,
                    remote_path: "data/manifests/deadbeef".into(),
                    last_synced: 1_700_000_000,
                    vclock: tcfs_sync::conflict::VectorClock::default(),
                    device_id: "test-device-id".into(),
                    conflict: None,
                    status: tcfs_sync::state::FileSyncStatus::Synced,
                },
            );
            cache.flush().unwrap();
        }

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: missing.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_ne!(
            resp.state, "synced",
            "needs_sync Err was silently collapsed into \"synced\""
        );
        assert_eq!(resp.state, "unknown");
    }

    #[tokio::test]
    async fn list_files_uses_live_remote_namespace_and_cache_only_for_bound_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let op = memory_operator();

        let live_manifest = seed_remote_file(&op, "live.txt").await;
        seed_remote_file(&op, "new.txt").await;
        tcfs_sync::engine::publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();
        op.write(
            "data/index/stale.txt",
            tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        op.write(
            "data/index/ghost/.tcfs_dir",
            tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();

        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op), None, false);
        {
            let mut cache = daemon.state_cache.lock().await;
            let mut live =
                test_sync_state(&format!("data/manifests/{live_manifest}"), 1_700_000_000);
            live.blake3 = "bound-live-content".into();
            live.status = tcfs_sync::state::FileSyncStatus::Synced;
            cache.set(&sync_root.join("live.txt"), live);
            cache.set(
                &sync_root.join("stale.txt"),
                test_sync_state("data/index/stale.txt", 1_600_000_000),
            );
            cache.flush().unwrap();
        }

        let response = daemon
            .list_files(tonic::Request::new(ListFilesRequest {
                prefix: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        let paths: Vec<&str> = response
            .files
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, vec!["empty/", "live.txt", "new.txt"]);

        let live = response
            .files
            .iter()
            .find(|entry| entry.path == "live.txt")
            .unwrap();
        assert_eq!(live.size, 0, "remote index owns current file metadata");
        assert_eq!(live.blake3, "bound-live-content");
        assert_eq!(live.hydration_state, "synced");
        assert_eq!(live.version_token, live_manifest);

        let new = response
            .files
            .iter()
            .find(|entry| entry.path == "new.txt")
            .unwrap();
        assert_eq!(new.hydration_state, "not_synced");
        assert!(new.blake3.is_empty());
        assert!(!new.version_token.is_empty());
        assert!(paths.iter().all(|path| *path != "stale.txt"));
        assert!(paths.iter().all(|path| *path != "ghost/"));
    }

    #[test]
    fn logical_rel_path_prefers_sync_root_key_for_manifest_entries() {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("ci-smoke/0.12.9/hello.txt");
        let state = test_sync_state("data/manifests/abc123", 1_700_000_000);

        let rel = logical_rel_path_from_state_key(
            &key.to_string_lossy(),
            &state,
            Some(root.path()),
            "data",
        )
        .unwrap();

        assert_eq!(rel, "ci-smoke/0.12.9/hello.txt");
    }

    #[cfg(unix)]
    #[test]
    fn logical_rel_path_normalizes_sync_root_alias_like_state_cache() {
        let temp = tempfile::tempdir().unwrap();
        let canonical_root = temp.path().join("canonical");
        let aliased_root = temp.path().join("alias");
        std::fs::create_dir(&canonical_root).unwrap();
        std::os::unix::fs::symlink(&canonical_root, &aliased_root).unwrap();
        let key = canonical_root.join("nested/hello.txt");
        let state = test_sync_state("data/manifests/abc123", 1_700_000_000);

        let rel = logical_rel_path_from_state_key(
            &key.to_string_lossy(),
            &state,
            Some(&aliased_root),
            "data",
        )
        .unwrap();

        assert_eq!(rel, "nested/hello.txt");
    }

    #[test]
    fn logical_rel_path_falls_back_to_remote_index_key() {
        let root = tempfile::tempdir().unwrap();
        let key = "/tmp/outside-tcfs-state-cache-key";
        let state = test_sync_state("data/index/remote/only.txt", 1_700_000_000);

        let rel = logical_rel_path_from_state_key(key, &state, Some(root.path()), "data").unwrap();

        assert_eq!(rel, "remote/only.txt");
    }

    #[tokio::test]
    async fn watch_delete_requires_authoritative_tombstone() {
        let op = memory_operator();
        let live_version = seed_remote_file(&op, "live.txt").await;

        let stale_delete = authoritative_watch_event(
            &op,
            "data",
            "live.txt",
            "deleted",
            1_700_000_000,
            "remote-device".into(),
        )
        .await
        .unwrap();
        assert_eq!(stale_delete.event_type, "modified");
        assert_eq!(stale_delete.version_token, live_version);

        let missing = match authoritative_watch_event(
            &op,
            "data",
            "missing.txt",
            "deleted",
            1_700_000_001,
            "remote-device".into(),
        )
        .await
        {
            Ok(_) => panic!("missing index authority must not authorize a deletion"),
            Err(error) => error,
        };
        assert_eq!(missing.code(), tonic::Code::FailedPrecondition);
        assert!(missing.message().contains("missing (not tombstoned)"));

        op.write(
            "data/index/deleted.txt",
            tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        let deleted = authoritative_watch_event(
            &op,
            "data",
            "deleted.txt",
            "modified",
            1_700_000_002,
            "remote-device".into(),
        )
        .await
        .unwrap();
        assert_eq!(deleted.event_type, "deleted");
        assert!(deleted.version_token.is_empty());
    }

    #[tokio::test]
    async fn watch_manifest_resolver_error_terminates_authority() {
        let op = memory_operator();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            "data/index/corrupt.txt",
            &tcfs_sync::index_entry::RemoteIndexEntry::new("missing-manifest", 0, 0),
        )
        .await
        .unwrap();

        let error = match authoritative_watch_event(
            &op,
            "data",
            "corrupt.txt",
            "modified",
            1_700_000_000,
            "remote-device".into(),
        )
        .await
        {
            Ok(_) => panic!("corrupt manifest authority must terminate Watch"),
            Err(error) => error,
        };
        assert_eq!(error.code(), tonic::Code::Internal);
        assert!(error
            .message()
            .contains("resolve authoritative Watch manifest"));
    }

    #[tokio::test]
    async fn watch_rejects_partial_cache_catch_up_without_advancing_anchor() {
        let temp = tempfile::tempdir().unwrap();
        let sync_root = temp.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let op = memory_operator();
        seed_remote_file(&op, "cached.txt").await;
        seed_remote_file(&op, "remote-only.txt").await;
        let daemon = primary_io_test_daemon(&temp, &sync_root, Some(op), None, false);
        {
            let mut cache = daemon.state_cache.lock().await;
            let mut stale = test_sync_state("data/manifests/forged-stale-token", 1_700_000_000);
            stale.device_id = "stale-cache-device".into();
            cache.set(&sync_root.join("cached.txt"), stale);
            cache.flush().unwrap();
        }

        let error = match daemon
            .watch(tonic::Request::new(WatchRequest {
                paths: vec![String::new()],
                since_timestamp: 1,
            }))
            .await
        {
            Ok(_) => panic!(
                "partial cache catch-up must not hide the remote-only path or stale cache token"
            ),
            Err(error) => error,
        };
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(
            error
                .message()
                .starts_with("authoritative incremental Watch journal is unavailable;"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn registered_root_inspect_and_execute_share_isolated_cache() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);

        let root_id = "git-roam-tool-daemon";
        let reconcile_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&reconcile_dir).unwrap();
        let root_state_path = reconcile_dir.join(format!("{root_id}.json"));
        let conflict_path = repo.join(".git/index");
        {
            let mut state =
                tcfs_sync::state::StateCache::open(&root_state_path).expect("root state");
            let mut entry = test_sync_state("git-roam/tool-daemon/index/.git/index", 42);
            entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
            entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
                rel_path: ".git/index".into(),
                local_blake3: "local".into(),
                remote_blake3: "remote".into(),
                local_device: "neo".into(),
                remote_device: "honey".into(),
                local_vclock: tcfs_sync::conflict::VectorClock::new(),
                remote_vclock: tcfs_sync::conflict::VectorClock::new(),
                detected_at: 42,
                times_recorded: 6,
                remote_manifest_key: None,
            });
            state.set(&conflict_path, entry);

            let mut ordinary = test_sync_state("git-roam/tool-daemon/index/README.md", 43);
            ordinary.status = tcfs_sync::state::FileSyncStatus::Conflict;
            ordinary.conflict = Some(tcfs_sync::conflict::ConflictInfo {
                rel_path: "README.md".into(),
                local_blake3: "same-bytes".into(),
                remote_blake3: "same-bytes".into(),
                local_device: "neo".into(),
                remote_device: "honey".into(),
                local_vclock: tcfs_sync::conflict::VectorClock::new(),
                remote_vclock: tcfs_sync::conflict::VectorClock::new(),
                detected_at: 43,
                times_recorded: 6,
                remote_manifest_key: Some("git-roam/tool-daemon/manifests/readme".into()),
            });
            state.set(&repo.join("README.md"), ordinary);
            state.flush().unwrap();
        }

        let primary_state_path = temp.path().join("primary.json");
        let primary_cache = {
            let mut state =
                tcfs_sync::state::StateCache::open(&primary_state_path).expect("primary state");
            state.set(
                &temp.path().join("primary-sentinel"),
                test_sync_state("data/index/primary-sentinel", 7),
            );
            state.flush().unwrap();
            state
        };
        let primary_before = std::fs::read(&primary_state_path).unwrap();

        let mut config = TcfsConfig::default();
        config.auth.require_session = true;
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.storage.remote_prefix = Some("data".into());
        config.sync.state_db = temp.path().join("primary.db");
        config.sync.roots.insert(
            root_id.into(),
            RegisteredRootConfig {
                local_root: repo.clone(),
                remote_prefix: "git-roam/tool-daemon".into(),
                state_path: root_state_path.clone(),
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        validate_registered_roots_config(&config).unwrap();

        let daemon = TcfsDaemonImpl::new(
            crate::cred_store::new_shared(),
            Arc::new(config),
            true,
            "memory://".into(),
            Arc::new(TokioMutex::new(primary_cache)),
            Arc::new(TokioMutex::new(Some(memory_operator()))),
            tcfs_sync::state::PathLocks::new(),
            "neo".into(),
            "neo".into(),
            None,
        );
        let permissions = tcfs_auth::DevicePermissions {
            can_pull: true,
            can_push: true,
            allowed_prefixes: vec!["git-roam/tool-daemon".into()],
            ..Default::default()
        };
        let token = insert_test_session(&daemon, "root-operator", permissions).await;

        let inspected = daemon
            .list_conflicts(request_with_bearer(
                ListConflictsRequest {
                    root_id: root_id.into(),
                },
                &token,
            ))
            .await
            .expect("inspect registered root")
            .into_inner();
        assert_eq!(inspected.root_id, root_id);
        assert_eq!(
            PathBuf::from(inspected.state_path),
            std::fs::canonicalize(&root_state_path).unwrap()
        );
        assert_eq!(inspected.remote_prefix, "git-roam/tool-daemon");
        assert_eq!(inspected.conflicts.len(), 2);
        assert_eq!(inspected.conflicts[0].rel_path, ".git/index");
        assert_eq!(inspected.conflicts[0].times_recorded, 6);

        let resolved = daemon
            .resolve_registered_root(request_with_bearer(
                ResolveRegisteredRootRequest {
                    root_id: root_id.into(),
                    path: repo.display().to_string(),
                    mode: RegisteredRootResolveMode::GitKeepBothExecute.into(),
                    operator_cli: true,
                },
                &token,
            ))
            .await
            .expect("resolve registered root")
            .into_inner();
        assert!(resolved.success, "{}", resolved.error);
        assert_eq!(resolved.root_id, root_id);
        assert_eq!(
            PathBuf::from(&resolved.local_root),
            std::fs::canonicalize(&repo).unwrap()
        );
        assert_eq!(resolved.remote_prefix, "git-roam/tool-daemon");
        assert_eq!(
            PathBuf::from(&resolved.state_path),
            std::fs::canonicalize(&root_state_path).unwrap()
        );
        assert!(resolved.error.contains("WARNING: 1 named-root conflict"));

        let root_after = tcfs_sync::state::StateCache::open(&root_state_path).unwrap();
        let remaining = root_after.conflicts();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].1.conflict.as_ref().unwrap().rel_path,
            "README.md",
            "repo-group keep-both must not silently resolve ordinary files"
        );
        assert_eq!(
            std::fs::read(&primary_state_path).unwrap(),
            primary_before,
            "registered-root execution must not touch the primary cache"
        );
    }

    #[tokio::test]
    async fn registered_root_rootless_repo_rejects_route_and_retired_files_are_uniform() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon = test_daemon_with_registered_root(&temp, "named", &repo);
        let error = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: repo.display().to_string(),
                resolution: "git_keep_both_dry_run".into(),
                operator_cli: true,
            }))
            .await
            .expect_err("legacy repo route must not enter a registered root");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(error.message().contains("--root"), "{error}");
        assert!(!error.message().contains("named"), "{error}");

        let unrelated = temp.path().join("ordinary-missing");
        let mut expected_response = None;
        for (path, resolution) in [
            (repo.join("README.md"), "keep_local"),
            (repo.join("deleted.txt"), "keep_remote"),
            (unrelated, "keep_local"),
            (PathBuf::from("repo/README.md"), "keep_local"),
        ] {
            let response = daemon
                .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                    path: path.display().to_string(),
                    resolution: resolution.into(),
                    operator_cli: false,
                }))
                .await
                .expect("retired verb returns a uniform response")
                .into_inner();
            assert!(!response.success);
            let observed = (response.resolved_path, response.error);
            if let Some(expected) = &expected_response {
                assert_eq!(&observed, expected, "retired verb became a path oracle");
            } else {
                expected_response = Some(observed);
            }
        }
    }

    #[tokio::test]
    async fn registered_root_retires_unrooted_primary_per_file_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("named-repo");
        init_clean_test_repo(&repo);
        let primary_path = temp.path().join("primary-file.txt");
        std::fs::write(&primary_path, b"primary\n").unwrap();
        let daemon = test_daemon_with_registered_root(&temp, "named", &repo);

        let mut entry = test_sync_state("data/manifests/primary-file", 42);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: "primary-file.txt".into(),
            local_blake3: "local".into(),
            remote_blake3: "remote".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            detected_at: 42,
            times_recorded: 1,
            remote_manifest_key: Some("data/manifests/primary-file-peer".into()),
        });
        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(&primary_path, entry);
            cache.flush().unwrap();
        }

        let response = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: primary_path.display().to_string(),
                resolution: "keep_local".into(),
                operator_cli: false,
            }))
            .await
            .expect("retired primary per-file route returns a bounded response")
            .into_inner();

        assert!(!response.success);
        assert!(response
            .error
            .contains("legacy per-file mutation is disabled"));
        let cache = daemon.state_cache.lock().await;
        assert!(cache.get(&primary_path).is_some_and(|state| {
            state.status == tcfs_sync::state::FileSyncStatus::Conflict && state.conflict.is_some()
        }));
    }

    #[tokio::test]
    async fn registered_root_rootless_restricted_session_cannot_probe_enrollment() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon =
            test_daemon_with_registered_root_session_requirement(&temp, "named", &repo, true);
        let permissions = tcfs_auth::DevicePermissions {
            can_push: true,
            allowed_prefixes: vec!["data".into()],
            ..Default::default()
        };
        let token = insert_test_session(&daemon, "restricted-device", permissions).await;
        let mut expected_response = None;

        for path in [
            repo.clone(),
            repo.join("deleted.txt"),
            temp.path().join("unrelated-missing"),
            PathBuf::from("repo/deleted.txt"),
        ] {
            let response = daemon
                .resolve_conflict(request_with_bearer(
                    ResolveConflictRequest {
                        path: path.display().to_string(),
                        resolution: "keep_local".into(),
                        operator_cli: false,
                    },
                    &token,
                ))
                .await
                .expect("authorized retired verb returns a uniform response")
                .into_inner();
            assert!(!response.success);
            let observed = (response.resolved_path, response.error);
            if let Some(expected) = &expected_response {
                assert_eq!(&observed, expected, "restricted route became an oracle");
            } else {
                expected_response = Some(observed);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn registered_root_rootless_resolve_rejects_symlink_alias_and_missing_child() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let alias = temp.path().join("repo-alias");
        symlink(&repo, &alias).unwrap();
        let daemon = test_daemon_with_registered_root(&temp, "named", &repo);

        let error = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: alias.display().to_string(),
                resolution: "git_keep_both_dry_run".into(),
                operator_cli: true,
            }))
            .await
            .expect_err("symlink alias must not bypass registered-root routing");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(error.message().contains("--root"));

        let response = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: alias.join("deleted.txt").display().to_string(),
                resolution: "keep_remote".into(),
                operator_cli: false,
            }))
            .await
            .expect("retired verb is rejected before alias inspection")
            .into_inner();
        assert!(!response.success);
        assert!(response
            .error
            .contains("legacy per-file mutation is disabled"));
    }

    #[test]
    fn registered_root_definition_rejects_state_escape_and_bad_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        let mut root = RegisteredRootConfig {
            local_root: temp.path().join("repo"),
            remote_prefix: "../other-root".into(),
            state_path: temp.path().join("reconcile/root.json"),
            policy: RegisteredRootPolicy::Resolve,
        };

        let error = validate_registered_root_definition(&config, "root", &root).unwrap_err();
        assert!(error.contains("remote_prefix"), "{error}");

        root.remote_prefix = "safe/root".into();
        root.state_path = temp.path().join("outside/root.json");
        let error = validate_registered_root_definition(&config, "root", &root).unwrap_err();
        assert!(error.contains("daemon-owned root-state fence"), "{error}");

        root.state_path = temp.path().join("reconcile/root.json");
        config.sync.roots.insert("root".into(), root);
        config.sync.roots.insert(
            "nested".into(),
            RegisteredRootConfig {
                local_root: temp.path().join("nested"),
                remote_prefix: "safe/root/nested".into(),
                state_path: temp.path().join("reconcile/nested.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        let error = validate_registered_roots_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlapping remote prefixes"), "{error}");

        let nested = config.sync.roots.get_mut("nested").unwrap();
        nested.remote_prefix = "other/root".into();
        nested.local_root = temp.path().join("repo/child");
        let error = validate_registered_roots_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlapping local roots"), "{error}");
    }

    #[test]
    fn registered_root_rejects_primary_lexical_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.sync_root = Some(primary.clone());
        config.storage.remote_prefix = Some("roots/primary".into());
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: primary.join("nested"),
                remote_prefix: "roots/named".into(),
                state_path: temp.path().join("reconcile/named.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );

        let error = validate_registered_roots_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlaps primary sync_root"), "{error}");

        let named = config.sync.roots.get_mut("named").unwrap();
        named.local_root = temp.path().join("primary-other");
        named.remote_prefix = "roots/primary/nested".into();
        let error = validate_registered_roots_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlaps primary storage prefix"), "{error}");

        config.sync.roots.get_mut("named").unwrap().remote_prefix = "roots/primary-other".into();
        validate_registered_roots_config(&config)
            .expect("component-prefix and path string collisions are disjoint");
        assert!(remote_prefixes_overlap(
            "roots/primary/nested",
            "roots/primary"
        ));
        assert!(remote_prefixes_overlap(
            "roots/primary",
            "roots/primary/nested"
        ));
        assert!(!remote_prefixes_overlap(
            "roots/primary-other",
            "roots/primary"
        ));
    }

    #[test]
    fn startup_rejects_custom_master_key_inside_primary_or_named_root() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let named = temp.path().join("named");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&named).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.sync_root = Some(primary.clone());
        config.crypto.master_key_file = Some(primary.join("secrets/custom-vault.bin"));
        let error = validate_registered_roots_config(&config)
            .expect_err("startup must reject a custom key in the primary root")
            .to_string();
        assert!(error.contains("primary sync.sync_root"), "{error}");

        config.sync.sync_root = Some(temp.path().join("other-primary"));
        config.storage.remote_prefix = Some("roots/primary".into());
        config.crypto.master_key_file = Some(named.join("custom-vault.bin"));
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: named,
                remote_prefix: "roots/named".into(),
                state_path: temp.path().join("reconcile/named.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        let error = validate_registered_roots_config(&config)
            .expect_err("startup must reject a custom key in a named root")
            .to_string();
        assert!(error.contains("registered root 'named'"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn startup_rejects_master_key_through_existing_root_symlink_alias() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real_root = temp.path().join("real-root");
        std::fs::create_dir(&real_root).unwrap();
        let root_alias = temp.path().join("root-alias");
        symlink(&real_root, &root_alias).unwrap();

        let mut config = TcfsConfig::default();
        config.sync.sync_root = Some(root_alias);
        config.crypto.master_key_file = Some(real_root.join("custom-vault.bin"));

        let error = validate_registered_roots_config(&config)
            .expect_err("startup must resolve existing root aliases")
            .to_string();
        assert!(error.contains("primary sync.sync_root"), "{error}");
    }

    #[test]
    fn registered_root_state_directory_rejects_lexical_primary_and_named_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let named = temp.path().join("named");
        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.sync_root = Some(primary.clone());
        config.storage.remote_prefix = Some("roots/primary".into());
        config.sync.root_state_dir = Some(primary.join("tcfs-state"));
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: named.clone(),
                remote_prefix: "roots/named".into(),
                state_path: primary.join("tcfs-state/named.json"),
                policy: RegisteredRootPolicy::Resolve,
            },
        );

        let error = validate_registered_roots_config(&config)
            .expect_err("state directory inside primary root must be rejected")
            .to_string();
        assert!(error.contains("state directory"), "{error}");
        assert!(error.contains("primary sync_root"), "{error}");

        config.sync.root_state_dir = Some(named.join("tcfs-state"));
        config.sync.roots.get_mut("named").unwrap().state_path =
            named.join("tcfs-state/named.json");
        let error = validate_registered_roots_config(&config)
            .expect_err("state directory inside named root must be rejected")
            .to_string();
        assert!(error.contains("state directory"), "{error}");
        assert!(error.contains("registered root 'named'"), "{error}");
    }

    #[test]
    fn registered_root_object_key_fence_rejects_lexical_escapes() {
        assert!(object_key_is_within_prefix(
            "git-roam/repo/index/.git/index",
            "git-roam/repo"
        ));
        for key in [
            "git-roam/repo",
            "git-roam/repo/",
            "git-roam/repo/../other",
            "git-roam/repo/./index",
            "git-roam/repo//index",
            "git-roam/repo/index\\escape",
            "git-roam/repository/index",
        ] {
            assert!(
                !object_key_is_within_prefix(key, "git-roam/repo"),
                "unsafe object key passed prefix fence: {key}"
            );
        }
    }

    #[test]
    fn primary_conflict_route_requires_current_prefix_and_conflict_state() {
        let mut entry = test_sync_state("data/manifests/current", 42);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: "docs/file.txt".into(),
            local_blake3: "local".into(),
            remote_blake3: "remote".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            detected_at: 42,
            times_recorded: 1,
            remote_manifest_key: Some("data/manifests/peer".into()),
        });
        assert!(primary_conflict_entry_matches_prefix(&entry, "data"));

        entry.remote_path = "old-prefix/manifests/current".into();
        assert!(!primary_conflict_entry_matches_prefix(&entry, "data"));
        entry.remote_path = "data/manifests/current".into();
        entry.conflict.as_mut().unwrap().remote_manifest_key =
            Some("named-root/manifests/peer".into());
        assert!(!primary_conflict_entry_matches_prefix(&entry, "data"));
        entry.conflict.as_mut().unwrap().remote_manifest_key = Some("data/manifests/peer".into());
        entry.status = tcfs_sync::state::FileSyncStatus::Synced;
        assert!(!primary_conflict_entry_matches_prefix(&entry, "data"));
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_missing_tail_rejects_dangling_symlink_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let dangling = temp.path().join("dangling");
        symlink(temp.path().join("missing-target"), &dangling).unwrap();
        let requested = dangling.join("file.txt");
        let error = canonicalize_with_missing_tail(&requested)
            .expect_err("dangling symlink must not become an ordinary missing tail");
        assert!(error.contains("unresolved symlink"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn registered_root_rejects_canonical_symlink_aliases_and_nesting() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let child = real.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let alias = temp.path().join("alias");
        symlink(&real, &alias).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.roots.insert(
            "root-a".into(),
            RegisteredRootConfig {
                local_root: real.clone(),
                remote_prefix: "roots/a".into(),
                state_path: temp.path().join("reconcile/root-a.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        config.sync.roots.insert(
            "root-b".into(),
            RegisteredRootConfig {
                local_root: alias,
                remote_prefix: "roots/b".into(),
                state_path: temp.path().join("reconcile/root-b.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        validate_registered_roots_config(&config)
            .expect("distinct lexical paths and prefixes pass startup validation");

        let canonical_real = std::fs::canonicalize(&real).unwrap();
        let error = validate_canonical_local_root_isolation(&config, "root-a", &canonical_real)
            .expect_err("symlink alias must be rejected at runtime");
        assert!(error.contains("overlap after canonicalization"), "{error}");

        config.sync.roots.remove("root-b");
        let child_alias = temp.path().join("child-alias");
        symlink(&child, &child_alias).unwrap();
        config.sync.roots.insert(
            "root-c".into(),
            RegisteredRootConfig {
                local_root: child_alias,
                remote_prefix: "roots/c".into(),
                state_path: temp.path().join("reconcile/root-c.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        let error = validate_canonical_local_root_isolation(&config, "root-a", &canonical_real)
            .expect_err("canonical nested alias must be rejected at runtime");
        assert!(error.contains("overlap after canonicalization"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn registered_root_rejects_primary_canonical_aliases_and_nesting() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let child = primary.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let alias = temp.path().join("primary-alias");
        symlink(&primary, &alias).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.sync_root = Some(primary.clone());
        config.storage.remote_prefix = Some("roots/primary".into());
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: alias.clone(),
                remote_prefix: "roots/named".into(),
                state_path: temp.path().join("reconcile/named.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        validate_registered_roots_config(&config)
            .expect("distinct lexical primary and named paths pass startup validation");

        let canonical_alias = std::fs::canonicalize(&alias).unwrap();
        let error = validate_canonical_local_root_isolation(&config, "named", &canonical_alias)
            .expect_err("primary symlink alias must be rejected at runtime");
        assert!(error.contains("primary sync_root"), "{error}");

        let child_alias = temp.path().join("primary-child-alias");
        symlink(&child, &child_alias).unwrap();
        config.sync.roots.get_mut("named").unwrap().local_root = child_alias.clone();
        validate_registered_roots_config(&config)
            .expect("distinct lexical nested alias passes startup validation");
        let canonical_child = std::fs::canonicalize(&child_alias).unwrap();
        let error = validate_canonical_local_root_isolation(&config, "named", &canonical_child)
            .expect_err("canonical primary nesting must be rejected at runtime");
        assert!(error.contains("primary sync_root"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn registered_root_state_directory_rejects_canonical_named_alias() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        let real_state_dir = named.join("machine-state");
        std::fs::create_dir_all(&real_state_dir).unwrap();
        let alias_state_dir = temp.path().join("state-alias");
        symlink(&real_state_dir, &alias_state_dir).unwrap();
        let state_path = alias_state_dir.join("named.json");
        std::fs::write(&state_path, b"{}").unwrap();
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.root_state_dir = Some(alias_state_dir.clone());
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: named,
                remote_prefix: "roots/named".into(),
                state_path,
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        validate_registered_roots_config(&config)
            .expect("lexically disjoint alias passes startup validation");

        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("canonical state directory inside named root must be rejected");
        assert!(error.contains("state directory"), "{error}");
        assert!(error.contains("after canonicalization"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn registered_root_rejects_local_root_alias_below_writable_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().join("selected-repo");
        let alternate = temp.path().join("alternate-repo");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(&alternate).unwrap();
        let selected_marker = selected.join("marker");
        let alternate_marker = alternate.join("marker");
        std::fs::write(&selected_marker, b"selected").unwrap();
        std::fs::write(&alternate_marker, b"alternate").unwrap();

        let writable_parent = temp.path().join("shared-routes");
        std::fs::create_dir_all(&writable_parent).unwrap();
        std::fs::set_permissions(&writable_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let configured_alias = writable_parent.join("named");
        symlink(&selected, &configured_alias).unwrap();

        let state_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state_path = state_dir.join("named.json");
        std::fs::write(&state_path, b"{}").unwrap();
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.root_state_dir = Some(state_dir);
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: configured_alias,
                remote_prefix: "roots/named".into(),
                state_path,
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        validate_registered_roots_config(&config)
            .expect("lexically valid alias passes startup mapping validation");

        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("writable lexical parent must not route a registered root");
        assert!(error.contains("untrusted original path chain"), "{error}");
        assert!(error.contains("writable by another principal"), "{error}");
        assert_eq!(std::fs::read(&selected_marker).unwrap(), b"selected");
        assert_eq!(std::fs::read(&alternate_marker).unwrap(), b"alternate");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn registered_root_rejects_state_directory_alias_below_writable_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let selected_state_dir = temp.path().join("selected-state");
        let alternate_state_dir = temp.path().join("alternate-state");
        std::fs::create_dir_all(&selected_state_dir).unwrap();
        std::fs::create_dir_all(&alternate_state_dir).unwrap();
        std::fs::set_permissions(&selected_state_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(&alternate_state_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let selected_state = selected_state_dir.join("named.json");
        let alternate_state = alternate_state_dir.join("named.json");
        std::fs::write(&selected_state, b"{\"selected\":true}").unwrap();
        std::fs::write(&alternate_state, b"{\"alternate\":true}").unwrap();
        std::fs::set_permissions(&selected_state, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&alternate_state, std::fs::Permissions::from_mode(0o600)).unwrap();

        let writable_parent = temp.path().join("shared-state-routes");
        std::fs::create_dir_all(&writable_parent).unwrap();
        std::fs::set_permissions(&writable_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let configured_state_dir = writable_parent.join("reconcile");
        symlink(&selected_state_dir, &configured_state_dir).unwrap();
        let state_path = configured_state_dir.join("named.json");

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.root_state_dir = Some(configured_state_dir);
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: repo,
                remote_prefix: "roots/named".into(),
                state_path,
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        validate_registered_roots_config(&config)
            .expect("lexically valid state alias passes startup mapping validation");

        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("writable lexical parent must not route a registered state cache");
        assert!(error.contains("untrusted original path chain"), "{error}");
        assert!(error.contains("writable by another principal"), "{error}");
        assert_eq!(
            std::fs::read(&selected_state).unwrap(),
            b"{\"selected\":true}"
        );
        assert_eq!(
            std::fs::read(&alternate_state).unwrap(),
            b"{\"alternate\":true}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registered_root_state_cache_rejects_hardlinks_and_primary_inode_alias() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let state_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&state_dir).unwrap();
        let state_path = state_dir.join("named.json");
        std::fs::write(&state_path, b"{}").unwrap();
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: repo,
                remote_prefix: "roots/named".into(),
                state_path: state_path.clone(),
                policy: RegisteredRootPolicy::Resolve,
            },
        );

        let outside_alias = temp.path().join("outside-state-alias.json");
        std::fs::hard_link(&state_path, &outside_alias).unwrap();
        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("hardlinked state must be rejected");
        assert!(error.contains("exactly one hard link"), "{error}");

        std::fs::remove_file(&outside_alias).unwrap();
        config.sync.state_db = state_dir.join("named.db");
        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("primary and named cache must not share an inode");
        assert!(error.contains("aliases primary state cache"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn registered_root_rejects_writable_state_ancestor() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("trusted/repo");
        std::fs::create_dir_all(&repo).unwrap();

        let writable_parent = temp.path().join("shared-state-parent");
        let state_dir = writable_parent.join("reconcile");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::set_permissions(&writable_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state_path = state_dir.join("named.json");
        std::fs::write(&state_path, b"{}").unwrap();
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.root_state_dir = Some(state_dir);
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: repo,
                remote_prefix: "roots/named".into(),
                state_path,
                policy: RegisteredRootPolicy::Resolve,
            },
        );

        let error =
            canonical_registered_root(&config, "named", config.sync.roots.get("named").unwrap())
                .expect_err("a writable state ancestor can redirect the named authority");
        assert!(error.contains("untrusted"), "{error}");
        assert!(error.contains("path chain"), "{error}");
        assert!(error.contains("writable by another principal"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn registered_root_peer_canonicalization_errors_fail_closed() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().join("selected");
        std::fs::create_dir_all(&selected).unwrap();
        let loop_a = temp.path().join("loop-a");
        let loop_b = temp.path().join("loop-b");
        symlink(&loop_b, &loop_a).unwrap();
        symlink(&loop_a, &loop_b).unwrap();
        let state_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&state_dir).unwrap();
        let selected_state = state_dir.join("selected.json");
        std::fs::write(&selected_state, b"{}").unwrap();
        std::fs::set_permissions(&selected_state, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut config = TcfsConfig::default();
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.roots.insert(
            "selected".into(),
            RegisteredRootConfig {
                local_root: selected,
                remote_prefix: "roots/selected".into(),
                state_path: selected_state,
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        config.sync.roots.insert(
            "loop".into(),
            RegisteredRootConfig {
                local_root: loop_a,
                remote_prefix: "roots/loop".into(),
                state_path: state_dir.join("loop.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        validate_registered_roots_config(&config).unwrap();

        let error = canonical_registered_root(
            &config,
            "selected",
            config.sync.roots.get("selected").unwrap(),
        )
        .expect_err("peer symlink-loop errors must not be treated as unavailable");
        assert!(
            error.contains("canonicalizing registered root 'loop'"),
            "{error}"
        );
    }

    #[test]
    fn registered_root_prefix_authorization_is_component_bounded() {
        let mut session = tcfs_auth::Session::new("device-1", "neo", "test");
        session.permissions.allowed_prefixes = vec!["git-roam/other".into()];
        let error = check_registered_prefix_permission(&session, "git-roam/tool-daemon")
            .expect_err("sibling prefix must be rejected");
        assert_eq!(error.code(), tonic::Code::PermissionDenied);

        session.permissions.allowed_prefixes = vec!["git-roam/tool".into()];
        assert!(check_registered_prefix_permission(&session, "git-roam/tool-daemon").is_err());

        session.permissions.allowed_prefixes = vec!["git-roam".into()];
        check_registered_prefix_permission(&session, "git-roam/tool-daemon")
            .expect("ancestor prefix should authorize the registered root");

        session.permissions.allowed_prefixes = vec!["git-roam/tool-daemon/".into()];
        check_registered_prefix_permission(&session, "git-roam/tool-daemon")
            .expect("trailing slash should normalize for an exact grant");
    }

    #[test]
    fn registered_root_unknown_id_does_not_enumerate_enrollment() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon = test_daemon_with_registered_root(&temp, "secret-root", &repo);

        let error = daemon
            .registered_root("missing")
            .expect_err("unknown root must return not found");
        assert_eq!(error.code(), tonic::Code::NotFound);
        assert!(!error.message().contains("secret-root"), "{error}");
        assert!(!error.message().contains("missing"), "{error}");
        assert!(!error.message().contains("available"), "{error}");
    }

    #[test]
    fn registered_root_unauthorized_id_is_indistinguishable_from_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon = test_daemon_with_registered_root(&temp, "secret-root", &repo);
        let mut session = tcfs_auth::Session::new("device-1", "neo", "test");
        session.permissions.allowed_prefixes = vec!["roots/other".into()];

        let unauthorized = daemon
            .authorized_registered_root(&session, "secret-root")
            .expect_err("known but unauthorized root must be hidden");
        let unknown = daemon
            .authorized_registered_root(&session, "missing")
            .expect_err("unknown root must be hidden");

        assert_eq!(unauthorized.code(), tonic::Code::NotFound);
        assert_eq!(unauthorized.code(), unknown.code());
        assert_eq!(unauthorized.message(), unknown.message());
        for forbidden in [
            "secret-root",
            "roots/secret-root",
            &repo.display().to_string(),
        ] {
            assert!(
                !unauthorized.message().contains(forbidden),
                "{unauthorized}"
            );
        }
    }

    #[tokio::test]
    async fn registered_root_auth_bypass_fails_before_route_lookup() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon = test_daemon_with_registered_root(&temp, "known", &repo);

        // Preserve the development-only compatibility posture for the primary
        // cache while named roots fail closed before their registry is read.
        daemon
            .list_conflicts(tonic::Request::new(ListConflictsRequest {
                root_id: String::new(),
            }))
            .await
            .expect("primary conflict inspection may use configured auth bypass");

        let known_list = daemon
            .list_conflicts(tonic::Request::new(ListConflictsRequest {
                root_id: "known".into(),
            }))
            .await
            .expect_err("known registered root must reject auth bypass");
        let unknown_list = daemon
            .list_conflicts(tonic::Request::new(ListConflictsRequest {
                root_id: "missing".into(),
            }))
            .await
            .expect_err("unknown registered root must reject auth bypass");
        let known_resolve = daemon
            .resolve_registered_root(tonic::Request::new(ResolveRegisteredRootRequest {
                root_id: "known".into(),
                path: repo.display().to_string(),
                mode: RegisteredRootResolveMode::GitKeepBothDryRun.into(),
                operator_cli: true,
            }))
            .await
            .expect_err("known registered root resolve must reject auth bypass");
        let unknown_resolve = daemon
            .resolve_registered_root(tonic::Request::new(ResolveRegisteredRootRequest {
                root_id: "missing".into(),
                path: repo.display().to_string(),
                mode: RegisteredRootResolveMode::GitKeepBothDryRun.into(),
                operator_cli: true,
            }))
            .await
            .expect_err("unknown registered root resolve must reject auth bypass");

        let expected = (
            tonic::Code::FailedPrecondition,
            "registered-root operations require auth.require_session = true",
        );
        for error in [known_list, unknown_list, known_resolve, unknown_resolve] {
            assert_eq!((error.code(), error.message()), expected);
        }
    }

    #[tokio::test]
    async fn registered_root_resolution_permissions_match_mode_before_route_lookup() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let daemon =
            test_daemon_with_registered_root_session_requirement(&temp, "named", &repo, true);
        *daemon.operator.lock().await = Some(memory_operator());

        let permissions = |can_pull, can_push| tcfs_auth::DevicePermissions {
            can_pull,
            can_push,
            allowed_prefixes: vec!["roots/named".into()],
            ..Default::default()
        };
        let request =
            |root_id: &str, mode: RegisteredRootResolveMode| ResolveRegisteredRootRequest {
                root_id: root_id.into(),
                path: repo.display().to_string(),
                mode: mode.into(),
                operator_cli: true,
            };

        let pull_only = insert_test_session(
            &daemon,
            "pull-only-root-inspector",
            permissions(true, false),
        )
        .await;
        let inspected = daemon
            .resolve_registered_root(request_with_bearer(
                request("named", RegisteredRootResolveMode::GitKeepBothDryRun),
                &pull_only,
            ))
            .await
            .expect("pull-only session may inspect with dry-run")
            .into_inner();
        assert!(inspected.success, "{}", inspected.error);

        let push_only =
            insert_test_session(&daemon, "push-only-root-writer", permissions(false, true)).await;
        let mut dry_run_denials = Vec::new();
        for root_id in ["named", "missing"] {
            let error = daemon
                .resolve_registered_root(request_with_bearer(
                    request(root_id, RegisteredRootResolveMode::GitKeepBothDryRun),
                    &push_only,
                ))
                .await
                .expect_err("push-only session must not read through dry-run");
            dry_run_denials.push((error.code(), error.message().to_string()));
        }
        assert_eq!(
            dry_run_denials[0], dry_run_denials[1],
            "permission denial must happen before registered-root lookup"
        );
        assert_eq!(dry_run_denials[0].0, tonic::Code::PermissionDenied);
        assert!(dry_run_denials[0].1.contains("pull"));

        let pull_only_execute = daemon
            .resolve_registered_root(request_with_bearer(
                request("named", RegisteredRootResolveMode::GitKeepBothExecute),
                &pull_only,
            ))
            .await
            .expect_err("execute requires push in addition to pull");
        assert_eq!(pull_only_execute.code(), tonic::Code::PermissionDenied);
        assert!(pull_only_execute.message().contains("push"));

        let push_only_execute = daemon
            .resolve_registered_root(request_with_bearer(
                request("named", RegisteredRootResolveMode::GitKeepBothExecute),
                &push_only,
            ))
            .await
            .expect_err("execute must not read remote state without pull");
        assert_eq!(push_only_execute.code(), tonic::Code::PermissionDenied);
        assert!(push_only_execute.message().contains("pull"));

        let read_write =
            insert_test_session(&daemon, "read-write-root-operator", permissions(true, true)).await;
        let executed = daemon
            .resolve_registered_root(request_with_bearer(
                request("named", RegisteredRootResolveMode::GitKeepBothExecute),
                &read_write,
            ))
            .await
            .expect("pull+push session may execute")
            .into_inner();
        assert!(executed.success, "{}", executed.error);
    }

    #[tokio::test]
    async fn primary_resolution_permissions_match_strategy_before_state_lookup() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let repo = std::fs::canonicalize(repo).unwrap();
        let missing = repo.parent().unwrap().join("missing");
        let daemon = test_daemon_with_operator_master_and_session_requirement(
            Some(memory_operator()),
            None,
            true,
        );
        let mut index_conflict = test_sync_state("data/manifests/index", 42);
        index_conflict.status = tcfs_sync::state::FileSyncStatus::Conflict;
        index_conflict.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: ".git/index".into(),
            local_blake3: "local-index".into(),
            remote_blake3: "remote-index".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            detected_at: 42,
            times_recorded: 1,
            remote_manifest_key: None,
        });
        {
            let mut state = daemon.state_cache.lock().await;
            state.set(&repo.join(".git/index"), index_conflict);
            state.flush().unwrap();
        }

        let permissions = |can_pull, can_push| tcfs_auth::DevicePermissions {
            can_pull,
            can_push,
            allowed_prefixes: vec!["data".into()],
            ..Default::default()
        };
        let request = |path: &Path, resolution: &str, operator_cli| ResolveConflictRequest {
            path: path.display().to_string(),
            resolution: resolution.into(),
            operator_cli,
        };

        let pull_only = insert_test_session(
            &daemon,
            "pull-only-primary-reader",
            permissions(true, false),
        )
        .await;
        let dry_run = daemon
            .resolve_conflict(request_with_bearer(
                request(&repo, "git_keep_both_dry_run", true),
                &pull_only,
            ))
            .await
            .expect("pull-only session may inspect primary keep-both")
            .into_inner();
        assert!(dry_run.success, "{}", dry_run.error);

        for resolution in ["git_keep_both_execute", "keep_both", "keep_local", "defer"] {
            let error = daemon
                .resolve_conflict(request_with_bearer(
                    request(&missing, resolution, true),
                    &pull_only,
                ))
                .await
                .expect_err("pull-only session must not use a push-authorized strategy");
            assert_eq!(error.code(), tonic::Code::PermissionDenied);
            assert!(error.message().contains("push"), "{resolution}: {error}");
        }

        let push_only = insert_test_session(
            &daemon,
            "push-only-primary-writer",
            permissions(false, true),
        )
        .await;
        for resolution in [
            "git_keep_both_dry_run",
            "git_keep_both_execute",
            "keep_remote",
            "keep_both",
        ] {
            let mut denials = Vec::new();
            for path in [&repo, &missing] {
                let error = daemon
                    .resolve_conflict(request_with_bearer(
                        request(path, resolution, true),
                        &push_only,
                    ))
                    .await
                    .expect_err("push-only session must not enter a remote-read strategy");
                denials.push((error.code(), error.message().to_string()));
            }
            assert_eq!(
                denials[0], denials[1],
                "{resolution} permission denial must precede state/path lookup"
            );
            assert_eq!(denials[0].0, tonic::Code::PermissionDenied);
            assert!(
                denials[0].1.contains("pull"),
                "{resolution}: {:?}",
                denials[0]
            );
        }

        let deferred = daemon
            .resolve_conflict(request_with_bearer(
                request(&missing, "defer", false),
                &push_only,
            ))
            .await
            .expect("legacy defer remains available to a push-authorized session")
            .into_inner();
        assert!(deferred.success, "{}", deferred.error);
    }

    #[tokio::test]
    async fn pull_only_primary_keep_remote_is_retired_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let operator = memory_operator();
        let daemon = test_daemon_with_operator_master_and_session_requirement(
            Some(operator.clone()),
            None,
            true,
        );

        let remote_source = temp.path().join("remote-source.txt");
        std::fs::write(&remote_source, b"remote bytes").unwrap();
        let mut upload_state =
            tcfs_sync::state::StateCache::open(&temp.path().join("upload-state.json")).unwrap();
        let uploaded = tcfs_sync::engine::upload_file(
            &operator,
            &remote_source,
            "data",
            &mut upload_state,
            None,
        )
        .await
        .unwrap();

        let local_path = temp.path().join("primary.txt");
        std::fs::write(&local_path, b"local bytes").unwrap();
        let mut entry = test_sync_state(&uploaded.remote_path, 42);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: "primary.txt".into(),
            local_blake3: "local".into(),
            remote_blake3: uploaded.hash,
            local_device: "neo".into(),
            remote_device: "honey".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            detected_at: 42,
            times_recorded: 1,
            remote_manifest_key: Some(uploaded.remote_path.clone()),
        });
        {
            let mut state = daemon.state_cache.lock().await;
            state.set(&local_path, entry);
            state.flush().unwrap();
        }

        let permissions = tcfs_auth::DevicePermissions {
            can_pull: true,
            can_push: false,
            allowed_prefixes: vec!["data".into()],
            ..Default::default()
        };
        let token = insert_test_session(&daemon, "pull-only-keep-remote", permissions).await;
        let response = daemon
            .resolve_conflict(request_with_bearer(
                ResolveConflictRequest {
                    path: local_path.display().to_string(),
                    resolution: "keep_remote".into(),
                    operator_cli: false,
                },
                &token,
            ))
            .await
            .expect("retired primary per-file verb returns a bounded response")
            .into_inner();

        assert!(!response.success);
        assert!(response
            .error
            .contains("legacy per-file mutation is disabled"));
        assert_eq!(std::fs::read(&local_path).unwrap(), b"local bytes");
    }

    #[test]
    fn registered_root_cache_and_git_metadata_fences_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /tmp/shared-worktree").unwrap();
        let route = RegisteredRootRoute {
            root_id: "repo".into(),
            local_root: repo.clone(),
            remote_prefix: "git-roam/repo".into(),
            state_path: temp.path().join("reconcile/repo.json"),
            policy: RegisteredRootPolicy::Resolve,
        };

        let error = validate_standalone_git_root(&route).unwrap_err();
        assert!(error.contains("linked or redirected"), "{error}");

        let mut state = tcfs_sync::state::StateCache::open(&temp.path().join("unsafe.json"))
            .expect("unsafe test state");
        let mut entry = test_sync_state("git-roam/repo/index/.git/index", 42);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: ".git/index".into(),
            local_blake3: "local".into(),
            remote_blake3: "remote".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            detected_at: 42,
            times_recorded: 1,
            remote_manifest_key: None,
        });
        state.set(&repo.join("../outside/.git/index"), entry);
        let error = validate_conflict_cache_route(&route, &state).unwrap_err();
        assert!(error.contains("outside local_root"), "{error}");
    }

    #[tokio::test]
    async fn registered_root_execute_obeys_inspect_only_policy() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_clean_test_repo(&repo);
        let reconcile_dir = temp.path().join("reconcile");
        std::fs::create_dir_all(&reconcile_dir).unwrap();
        let state_path = reconcile_dir.join("docs.json");
        std::fs::write(&state_path, b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let mut config = TcfsConfig::default();
        config.auth.require_session = true;
        config.daemon.socket = temp.path().join("tcfsd.sock");
        config.sync.roots.insert(
            "docs".into(),
            RegisteredRootConfig {
                local_root: repo.clone(),
                remote_prefix: "docs".into(),
                state_path,
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        let daemon = TcfsDaemonImpl::new(
            crate::cred_store::new_shared(),
            Arc::new(config),
            true,
            "memory://".into(),
            Arc::new(TokioMutex::new(
                tcfs_sync::state::StateCache::open(&temp.path().join("primary.json")).unwrap(),
            )),
            Arc::new(TokioMutex::new(Some(memory_operator()))),
            tcfs_sync::state::PathLocks::new(),
            "neo".into(),
            "neo".into(),
            None,
        );
        let permissions = tcfs_auth::DevicePermissions {
            can_pull: true,
            can_push: true,
            allowed_prefixes: vec!["docs".into()],
            ..Default::default()
        };
        let token = insert_test_session(&daemon, "docs-operator", permissions).await;

        let error = daemon
            .resolve_registered_root(request_with_bearer(
                ResolveRegisteredRootRequest {
                    root_id: "docs".into(),
                    path: repo.display().to_string(),
                    mode: RegisteredRootResolveMode::GitKeepBothExecute.into(),
                    operator_cli: true,
                },
                &token,
            ))
            .await
            .expect_err("inspect-only root must reject execute");
        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert!(error.message().contains("inspect-only"));
    }

    #[tokio::test]
    async fn resolve_conflict_invalid_resolution() {
        let daemon = test_daemon();
        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                resolution: "invalid_strategy".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("invalid resolution"));
    }

    #[tokio::test]
    async fn resolve_conflict_repo_git_keep_both_requires_operator_cli() {
        // BLOCKING 1b: repo-group git keep-both must be refused (fail-closed)
        // when the explicit operator-intent bit is absent. MCP cannot set the
        // bit; a generic protobuf client can, so authenticated mode-specific
        // pull/push permissions remain the authorization boundary. Both modes
        // are gated before work.
        let daemon = test_daemon();
        for resolution in ["git_keep_both_dry_run", "git_keep_both_execute"] {
            let resp = daemon
                .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                    path: "myrepo".into(),
                    resolution: resolution.into(),
                    operator_cli: false,
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(
                !resp.success,
                "{resolution} must be refused without explicit operator intent"
            );
            assert!(
                resp.error.contains("operator intent"),
                "{resolution} error must direct to the CLI, got: {}",
                resp.error
            );
        }
    }

    #[tokio::test]
    async fn resolve_conflict_retires_git_internal_path_before_classification() {
        // Retired verbs are refused uniformly before any path classification.
        let daemon = test_daemon();
        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: "myrepo/.git/refs/heads/main".into(),
                resolution: "keep_remote".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.success, "keep_remote on a .git path must be refused");
        assert!(resp.error.contains("legacy per-file mutation is disabled"));
        assert!(resp.resolved_path.is_empty());

        // keep_both and keep_local on a `.git` path are likewise refused.
        for strat in ["keep_both", "keep_local"] {
            let resp = daemon
                .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                    path: "myrepo/.git/index".into(),
                    resolution: strat.into(),
                    ..Default::default()
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(!resp.success, "{strat} on a .git path must be refused");
            assert!(
                resp.error.contains("legacy per-file mutation is disabled"),
                "strat={strat}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_conflict_retires_symlink_alias_without_classification() {
        // The retired verb must return without resolving the alias or using its
        // recorded conflict as a path/state oracle.
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git_heads = repo.join(".git/refs/heads");
        std::fs::create_dir_all(&git_heads).unwrap();
        let git_path = git_heads.join("main");
        std::fs::write(&git_path, b"0123456789012345678901234567890123456789\n").unwrap();

        let alias = dir.path().join("refslink");
        std::os::unix::fs::symlink(repo.join(".git/refs"), &alias).unwrap();
        let alias_path = alias.join("heads/main");

        let mut entry = test_sync_state("data/manifests/git", 1_700_000_000);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: "repo/.git/refs/heads/main".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            local_blake3: "local-git".into(),
            remote_blake3: "remote-git".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 43,
            times_recorded: 1,
            remote_manifest_key: None,
        });

        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(&git_path, entry);
            cache.flush().unwrap();
        }

        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: alias_path.display().to_string(),
                resolution: "keep_remote".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.resolved_path.is_empty());
        assert!(resp.error.contains("legacy per-file mutation is disabled"));
    }

    #[tokio::test]
    async fn resolve_conflict_retires_without_consulting_stored_git_path() {
        // A recorded Git rel_path must not change the uniform retired response.
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let request_path = dir.path().join("visible-conflict");
        std::fs::write(&request_path, b"ordinary-looking path").unwrap();

        let mut entry = test_sync_state("data/manifests/git", 1_700_000_000);
        entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
        entry.conflict = Some(tcfs_sync::conflict::ConflictInfo {
            rel_path: "repo/.git/refs/heads/main".into(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            local_blake3: "local-git".into(),
            remote_blake3: "remote-git".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 44,
            times_recorded: 1,
            remote_manifest_key: None,
        });

        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(&request_path, entry);
            cache.flush().unwrap();
        }

        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: request_path.display().to_string(),
                resolution: "keep_remote".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.resolved_path.is_empty());
        assert!(resp.error.contains("legacy per-file mutation is disabled"));
        assert!(daemon
            .state_cache
            .lock()
            .await
            .get(&request_path)
            .is_some_and(|entry| entry.conflict.is_some()));
    }

    #[tokio::test]
    async fn resolve_conflict_defer_allowed_on_git_internal_path() {
        // `defer` is a no-op and returns before path or state inspection.
        let daemon = test_daemon();
        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                path: "myrepo/.git/refs/heads/main".into(),
                resolution: "defer".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.success, "defer must be allowed on a .git path");
        assert!(resp.error.is_empty());
    }

    #[tokio::test]
    async fn resolve_conflict_retires_normal_per_file_mutations() {
        // The legacy RPC has no daemon-selected root/manifest binding. All
        // ordinary per-file mutation strategies therefore fail closed too.
        let daemon = test_daemon();
        for resolution in ["keep_local", "keep_remote", "keep_both"] {
            let resp = daemon
                .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                    path: "notes/todo.txt".into(),
                    resolution: resolution.into(),
                    ..Default::default()
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(!resp.success, "{resolution} must fail closed");
            assert!(
                resp.error.contains("legacy per-file mutation is disabled"),
                "{resolution}: {}",
                resp.error
            );
        }
    }

    #[tokio::test]
    async fn mount_missing_required_fields_returns_error_response() {
        let daemon = test_daemon();

        let resp = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: String::new(),
                mountpoint: String::new(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("mountpoint and remote are required"));
    }

    #[tokio::test]
    async fn mount_requires_initialized_operator() {
        let daemon = test_daemon();
        let mountpoint_dir = tempfile::tempdir().unwrap();

        let err = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: "s3://127.0.0.1/test/data".into(),
                mountpoint: mountpoint_dir.path().join("mnt").display().to_string(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::Unavailable);
        assert!(err.message().contains("storage operator not initialized"));
    }

    #[tokio::test]
    async fn mount_rejects_duplicate_active_mountpoint() {
        let daemon = test_daemon();
        let mountpoint = tempfile::tempdir().unwrap();
        let mountpoint_str = mountpoint.path().display().to_string();
        let sentinel = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        daemon
            .active_mounts
            .lock()
            .await
            .insert(mountpoint_str.clone(), sentinel);

        let resp = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: "s3://127.0.0.1/test/data".into(),
                mountpoint: mountpoint_str.clone(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("already mounted"));

        let child = { daemon.active_mounts.lock().await.remove(&mountpoint_str) };
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
    }

    #[tokio::test]
    async fn unmount_requires_mountpoint() {
        let daemon = test_daemon();

        let resp = daemon
            .unmount(tonic::Request::new(UnmountRequest {
                mountpoint: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("mountpoint is required"));
    }

    #[tokio::test]
    async fn push_then_pull_streams_complete_successfully_over_uds() {
        let primary = tempfile::tempdir().unwrap();
        let sync_root = primary.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let daemon =
            primary_io_test_daemon(&primary, &sync_root, Some(memory_operator()), None, false);
        let (socket_dir, server_handle, shutdown) = spawn_test_server(daemon).await;
        let socket_path = socket_dir.path().join("tcfsd.sock");
        let mut client = connect_test_client(&socket_path).await;

        let empty_first = tokio_stream::iter(vec![
            PushChunk {
                path: String::new(),
                data: b"must not be reassigned".to_vec(),
                offset: 0,
                last: false,
            },
            PushChunk {
                path: "docs/reassigned.txt".into(),
                data: Vec::new(),
                offset: b"must not be reassigned".len() as u64,
                last: true,
            },
        ]);
        let error = match client.push(tonic::Request::new(empty_first)).await {
            Ok(_) => panic!("an empty first push path must fail before buffering another path"),
            Err(error) => error,
        };
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("first push chunk"));

        let unterminated = PushChunk {
            path: "docs/unterminated.txt".into(),
            data: b"must not publish".to_vec(),
            offset: 0,
            last: false,
        };
        let error = match client
            .push(tonic::Request::new(tokio_stream::once(unterminated)))
            .await
        {
            Ok(_) => panic!("EOF before the terminal push chunk must fail"),
            Err(error) => error,
        };
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("before its terminal chunk"));

        let push_chunk = PushChunk {
            path: "docs/hello.txt".into(),
            data: b"hello over grpc".to_vec(),
            offset: 0,
            last: true,
        };

        let mut push_stream = client
            .push(tonic::Request::new(tokio_stream::once(push_chunk)))
            .await
            .unwrap()
            .into_inner();

        let push_progress = push_stream
            .message()
            .await
            .unwrap()
            .expect("push should yield a final progress message");
        assert!(push_progress.done);
        assert!(
            push_progress.error.is_empty(),
            "push error: {}",
            push_progress.error
        );
        assert_eq!(push_progress.bytes_sent, b"hello over grpc".len() as u64);
        assert_eq!(push_progress.total_bytes, b"hello over grpc".len() as u64);
        assert!(!push_progress.chunk_hash.is_empty());
        assert!(push_stream.message().await.unwrap().is_none());

        let output_path = sync_root.join("downloaded.txt");

        let mut pull_stream = client
            .pull(tonic::Request::new(PullRequest {
                remote_path: "docs/hello.txt".into(),
                local_path: output_path.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        let pull_progress = pull_stream
            .message()
            .await
            .unwrap()
            .expect("pull should yield a final progress message");
        assert!(pull_progress.done);
        assert!(
            pull_progress.error.is_empty(),
            "pull error: {}",
            pull_progress.error
        );
        assert_eq!(
            pull_progress.bytes_received,
            b"hello over grpc".len() as u64
        );
        assert_eq!(pull_progress.total_bytes, b"hello over grpc".len() as u64);
        assert!(pull_stream.message().await.unwrap().is_none());
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            b"hello over grpc".to_vec()
        );

        let listing = client
            .list_files(tonic::Request::new(ListFilesRequest {
                prefix: "docs".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        let hello = listing
            .files
            .iter()
            .find(|entry| entry.path == "docs/hello.txt")
            .expect("pushed file must be listed by exact logical path");
        assert!(!hello.version_token.is_empty());

        let mut exact_stream = client
            .pull_exact(tonic::Request::new(PullExactRequest {
                remote_path: "docs/hello.txt".into(),
                expected_version: hello.version_token.clone(),
            }))
            .await
            .unwrap()
            .into_inner();
        let mut exact_bytes = Vec::new();
        let mut exact_terminal = None;
        while let Some(progress) = exact_stream.message().await.unwrap() {
            assert!(progress.error.is_empty(), "exact pull: {}", progress.error);
            exact_bytes.extend_from_slice(&progress.data);
            if progress.done {
                assert!(
                    exact_terminal.is_none(),
                    "duplicate PullExact terminal marker"
                );
                exact_terminal = Some(progress);
            }
        }
        let exact_terminal = exact_terminal.expect("PullExact must emit a terminal marker");
        assert!(exact_terminal.exact_content);
        assert_eq!(exact_terminal.version_token, hello.version_token);
        assert_eq!(exact_terminal.bytes_received, exact_bytes.len() as u64);
        assert_eq!(exact_terminal.total_bytes, exact_bytes.len() as u64);
        assert_eq!(exact_bytes, b"hello over grpc");

        shutdown.notify_one();
        let server_result = server_handle.await.unwrap();
        assert!(
            server_result.is_ok(),
            "server exited with error: {server_result:?}"
        );
    }

    #[tokio::test]
    async fn unsync_waits_for_active_path_lock() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("tracked.txt");
        std::fs::write(&tracked, b"tracked").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "abc123".to_string(),
                1,
                "data/manifests/abc123".to_string(),
            )
            .unwrap();
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        let state_cache = daemon.state_cache_handle();
        let guard = daemon.path_locks.lock(&tracked).await;
        let tracked_str = tracked.display().to_string();

        let handle = tokio::spawn(async move {
            daemon
                .unsync(tonic::Request::new(UnsyncRequest {
                    path: tracked_str,
                    force: false,
                }))
                .await
                .unwrap()
                .into_inner()
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "unsync should wait while the path lock is held"
        );

        drop(guard);

        let resp = handle.await.unwrap();
        assert!(resp.success, "unsync error: {}", resp.error);

        let cache = state_cache.lock().await;
        let entry = cache
            .get(&tracked)
            .expect("state should still exist after unsync");
        assert_eq!(entry.status, tcfs_sync::state::FileSyncStatus::NotSynced);
    }

    #[test]
    fn sanitize_rejects_parent_traversal() {
        assert!(sanitize_rel_path("../../etc/passwd").is_err());
        assert!(sanitize_rel_path("foo/../../../bar").is_err());
        assert!(sanitize_rel_path("..").is_err());
    }

    #[test]
    fn sanitize_rejects_absolute_paths() {
        assert!(sanitize_rel_path("/etc/passwd").is_err());
        assert!(sanitize_rel_path("/tmp/file.txt").is_err());
    }

    #[test]
    fn sanitize_rejects_empty_path() {
        assert!(sanitize_rel_path("").is_err());
    }

    #[test]
    fn sanitize_accepts_valid_relative_paths() {
        assert_eq!(sanitize_rel_path("file.txt").unwrap(), "file.txt");
        assert_eq!(sanitize_rel_path("a/b/c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(
            sanitize_rel_path("docs/nested/deep.md").unwrap(),
            "docs/nested/deep.md"
        );
        assert_eq!(sanitize_rel_path("./current.txt").unwrap(), "./current.txt");
    }

    // ── TIN-1417: roll-call gate for the PerDevice CONTRACT ────────────────

    #[test]
    fn roll_call_gate_passes_per_device_when_all_capable() {
        use tcfs_core::config::WrapMode;
        let mut reg = tcfs_secrets::device::DeviceRegistry::default();
        let real = tcfs_secrets::device::generate_local_device_key().public_key;
        reg.enroll("a", &real, None);
        assert_eq!(
            resolve_wrap_mode_with_roll_call(WrapMode::PerDevice, &reg),
            WrapMode::PerDevice,
            "all active devices capable => PerDevice honored"
        );
    }

    #[test]
    fn roll_call_gate_downgrades_per_device_to_dual_when_blocked() {
        use tcfs_core::config::WrapMode;
        let mut reg = tcfs_secrets::device::DeviceRegistry::default();
        let real = tcfs_secrets::device::generate_local_device_key().public_key;
        reg.enroll("a", &real, None);
        // A second active device without a real age recipient blocks the contract.
        reg.enroll("placeholder", "age1xoxd-not-a-real-key", None);
        assert_eq!(
            resolve_wrap_mode_with_roll_call(WrapMode::PerDevice, &reg),
            WrapMode::Dual,
            "a non-capable active device must downgrade PerDevice -> Dual"
        );
    }

    #[test]
    fn roll_call_gate_passes_dual_and_master_through() {
        use tcfs_core::config::WrapMode;
        let reg = tcfs_secrets::device::DeviceRegistry::default();
        assert_eq!(
            resolve_wrap_mode_with_roll_call(WrapMode::Master, &reg),
            WrapMode::Master
        );
        assert_eq!(
            resolve_wrap_mode_with_roll_call(WrapMode::Dual, &reg),
            WrapMode::Dual
        );
    }
}
