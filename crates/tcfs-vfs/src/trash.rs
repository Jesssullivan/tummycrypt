//! Sync Trash — staged deletes as a safety net.
//!
//! Instead of permanently deleting index entries on `unlink`, the VFS writes
//! an immutable `.tcfs-trash/` safety copy and conditionally tombstones the
//! live index object. This allows:
//! - Accidental delete recovery via `tcfs trash restore`
//! - Explicit logical purge after a configurable retention period
//! - List trashed items with deletion timestamp
//!
//! Trash entry key format: `{prefix}/.tcfs-trash/{timestamp}-{uuid}/{rel_path}`
//! (legacy timestamp-only generations remain readable).
//! The original index entry content is preserved verbatim.

use anyhow::{Context, Result};
use opendal::{ErrorKind, Operator};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

/// Validate that a relative path does not contain traversal components.
///
/// Rejects paths with `..`, absolute prefixes, or other components that
/// could escape the intended prefix when used in S3 key construction.
fn validate_rel_path(rel_path: &str) -> Result<()> {
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .with_context(|| format!("validating canonical trash relative path: {rel_path:?}"))
}

fn validate_remote_prefix(prefix: &str) -> Result<&str> {
    anyhow::ensure!(
        !prefix.is_empty()
            && prefix.trim_matches('/') == prefix
            && !prefix.contains('\\')
            && !prefix.chars().any(char::is_control)
            && !prefix
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == ".."),
        "trash prefix must be one canonical relative storage prefix: {prefix:?}"
    );
    Ok(prefix)
}

fn prefixed_key(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{prefix}/{suffix}")
    }
}

fn index_key_for_rel_path(prefix: &str, rel_path: &str) -> String {
    prefixed_key(prefix, &format!("index/{rel_path}"))
}

fn namespace_claim_for_index_rel_path(
    rel_path: &str,
) -> Result<(&str, tcfs_sync::index_entry::PortableNamespaceRole)> {
    if let Some(parent) = rel_path
        .strip_suffix("/.tcfs_dir")
        .filter(|parent| !parent.is_empty())
    {
        validate_rel_path(parent)?;
        Ok((
            parent,
            tcfs_sync::index_entry::PortableNamespaceRole::Directory,
        ))
    } else {
        validate_rel_path(rel_path)?;
        Ok((
            rel_path,
            tcfs_sync::index_entry::PortableNamespaceRole::File,
        ))
    }
}

/// Validate every manifest pointer carried by a trash safety copy before the
/// live index is tombstoned or restored. Trash evidence is immutable, but its
/// referenced objects can still be missing, corrupt, cross-root, or bound to a
/// different logical path; none of those records may become index authority.
async fn validate_trash_index_bindings(
    op: &Operator,
    prefix: &str,
    rel_path: &str,
    index_bytes: &[u8],
) -> Result<()> {
    if rel_path.ends_with("/.tcfs_dir") {
        anyhow::ensure!(
            index_bytes == tcfs_sync::index_entry::DIRECTORY_MARKER_BYTES,
            "directory marker is not the canonical live payload: {rel_path}"
        );
        return Ok(());
    }

    let record = tcfs_sync::index_entry::parse_index_entry_record(index_bytes)
        .with_context(|| format!("parsing trash index record for {rel_path:?}"))?;
    anyhow::ensure!(
        record.visible_entry().is_some() || record.pending_entry().is_some(),
        "trash index record has no live or pending manifest authority: {rel_path}"
    );
    let manifest_prefix = prefixed_key(prefix, "manifests");

    if let Some(current) = record.visible_entry() {
        let manifest_key =
            tcfs_sync::index_entry::manifest_key(&manifest_prefix, &current.manifest_hash);
        let manifest_bytes = op
            .read(&manifest_key)
            .await
            .with_context(|| format!("reading trash-bound manifest: {manifest_key}"))?
            .to_vec();
        tcfs_sync::engine::validate_indexed_manifest_entry_binding(
            &manifest_bytes,
            &current.manifest_hash,
            current,
            rel_path,
        )
        .with_context(|| format!("validating trash-bound manifest for {rel_path:?}"))?;
    }

    if let Some(pending) = record.pending_entry() {
        tcfs_sync::index_entry::validate_staged_manifest_key(&manifest_prefix, pending)
            .with_context(|| format!("validating trash-bound staged key for {rel_path:?}"))?;
        let staged_bytes = op
            .read(&pending.staged_manifest_key)
            .await
            .with_context(|| {
                format!(
                    "reading trash-bound staged manifest: {}",
                    pending.staged_manifest_key
                )
            })?
            .to_vec();
        tcfs_sync::engine::validate_indexed_manifest_entry_binding(
            &staged_bytes,
            &pending.manifest_hash,
            &pending.as_remote_entry(),
            rel_path,
        )
        .with_context(|| format!("validating trash-bound staged manifest for {rel_path:?}"))?;
    }

    Ok(())
}

/// A single trashed item.
#[derive(Debug, Clone)]
pub struct TrashEntry {
    /// Original relative path (e.g., "docs/file.txt")
    pub original_path: String,
    /// Unix timestamp when the file was trashed.
    pub trashed_at: u64,
    /// Key in the S3 trash prefix (for restore/purge).
    pub trash_key: String,
    /// Index content observed while listing (for display only). Restore binds
    /// itself to a fresh read of `trash_key`.
    pub index_content: String,
    /// Whether this generation has current tombstone/completion authority, is
    /// a recoverable historical generation, or remains indeterminate. This is
    /// informational; restore and purge always re-read remote authority.
    pub generation_state: TrashGenerationState,
}

/// One independently retained object that could not be interpreted as a
/// normal visible trash generation.
#[derive(Debug, Clone)]
pub struct TrashScanIssue {
    pub trash_key: String,
    pub error: String,
}

/// Valid visible generations plus per-object issues. Failure of the global
/// listing operation itself is still returned as an error.
#[derive(Debug, Clone)]
pub struct TrashScanReport {
    pub entries: Vec<TrashEntry>,
    pub issues: Vec<TrashScanIssue>,
}

/// Result of a best-effort logical purge. Independently valid generations may
/// be claimed while every retained per-object failure remains visible to the
/// caller and automation.
#[derive(Debug, Clone)]
pub struct TrashPurgeReport {
    pub purged: usize,
    pub issues: Vec<TrashScanIssue>,
}

/// Durable state of one immutable trash generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashGenerationState {
    /// The exact live-index tombstone or post-tombstone marker binds this copy.
    Completed,
    /// A timestamp-only generation written by the historical trash protocol.
    /// Restore remains safe because it never overwrites a different live path.
    LegacyRecoverable,
    /// Safety evidence exists, but the corresponding delete did not reach (or
    /// could not durably record) its linearization point.
    Indeterminate,
}

impl TrashGenerationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::LegacyRecoverable => "legacy",
            Self::Indeterminate => "indeterminate",
        }
    }
}

fn validate_trash_key_binding(prefix: &str, entry: &TrashEntry) -> Result<()> {
    let trash_prefix = prefixed_key(prefix, ".tcfs-trash/");
    let remainder = entry
        .trash_key
        .strip_prefix(&trash_prefix)
        .with_context(|| {
            format!(
                "trash key is outside selected prefix {trash_prefix:?}: {:?}",
                entry.trash_key
            )
        })?;
    let (generation, original_path) = remainder
        .split_once('/')
        .context("trash key is missing its generation/path separator")?;
    parse_trash_generation(generation)?;
    anyhow::ensure!(
        original_path == entry.original_path,
        "trash key path does not match the selected original path"
    );
    Ok(())
}

fn parse_trash_generation(generation: &str) -> Result<(u64, bool)> {
    let (timestamp, uuid) = generation
        .split_once('-')
        .map_or((generation, None), |(timestamp, uuid)| {
            (timestamp, Some(uuid))
        });
    anyhow::ensure!(
        !timestamp.is_empty() && timestamp.bytes().all(|byte| byte.is_ascii_digit()),
        "trash key has an invalid timestamp generation"
    );
    let timestamp = timestamp
        .parse::<u64>()
        .context("trash key has an invalid timestamp generation")?;

    let Some(uuid_text) = uuid else {
        return Ok((timestamp, true));
    };
    let uuid = uuid::Uuid::parse_str(uuid_text)
        .context("trash key has an invalid UUID generation suffix")?;
    anyhow::ensure!(
        uuid.hyphenated().to_string() == uuid_text,
        "trash key UUID generation suffix is not canonical"
    );
    Ok((timestamp, false))
}

fn trash_key_is_legacy(prefix: &str, entry: &TrashEntry) -> Result<bool> {
    validate_trash_key_binding(prefix, entry)?;
    let trash_prefix = prefixed_key(prefix, ".tcfs-trash/");
    let remainder = entry
        .trash_key
        .strip_prefix(&trash_prefix)
        .context("validated trash key lost its selected prefix")?;
    let (generation, _) = remainder
        .split_once('/')
        .context("validated trash key lost its generation separator")?;
    Ok(parse_trash_generation(generation)?.1)
}

fn trash_key_timestamp(prefix: &str, entry: &TrashEntry) -> Result<u64> {
    validate_trash_key_binding(prefix, entry)?;
    let trash_prefix = prefixed_key(prefix, ".tcfs-trash/");
    let remainder = entry
        .trash_key
        .strip_prefix(&trash_prefix)
        .context("validated trash key lost its selected prefix")?;
    let (generation, _) = remainder
        .split_once('/')
        .context("validated trash key lost its generation separator")?;
    Ok(parse_trash_generation(generation)?.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsentInstallOutcome {
    Created,
    AlreadyExact,
}

#[cfg(test)]
fn memory_absent_install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn prove_object_bytes(op: &Operator, key: &str, expected: &[u8]) -> Result<()> {
    let observed = op
        .read(key)
        .await
        .with_context(|| format!("proving exact object bytes: {key}"))?
        .to_vec();
    anyhow::ensure!(
        observed == expected,
        "refusing destructive follow-up because destination bytes differ: {key}"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrashLifecycleClaim {
    Restore,
    Purge,
}

impl TrashLifecycleClaim {
    fn as_str(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Purge => "purge",
        }
    }
}

fn trash_lifecycle_object_id(trash_key: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tcfs-trash-lifecycle-v1\0");
    hasher.update(trash_key.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn trash_lifecycle_claim_key(prefix: &str, trash_key: &str) -> String {
    prefixed_key(
        prefix,
        &format!(
            ".tcfs-trash-lifecycle/v1/{}",
            trash_lifecycle_object_id(trash_key)
        ),
    )
}

fn trash_restore_complete_key(prefix: &str, trash_key: &str) -> String {
    prefixed_key(
        prefix,
        &format!(
            ".tcfs-trash-restore-complete/v1/{}",
            trash_lifecycle_object_id(trash_key)
        ),
    )
}

fn trash_delete_complete_key(prefix: &str, trash_key: &str) -> String {
    prefixed_key(
        prefix,
        &format!(
            ".tcfs-trash-delete-complete/v1/{}",
            trash_lifecycle_object_id(trash_key)
        ),
    )
}

fn trash_lifecycle_claim_bytes(claim: TrashLifecycleClaim, trash_key: &str) -> Vec<u8> {
    format!(
        "tcfs-trash-lifecycle-v1\nclaim={}\ntrash_key={trash_key}\n",
        claim.as_str()
    )
    .into_bytes()
}

fn trash_restore_complete_bytes(trash_key: &str) -> Vec<u8> {
    format!("tcfs-trash-restore-complete-v1\ntrash_key={trash_key}\n").into_bytes()
}

fn trash_delete_complete_bytes(trash_key: &str, evidence: &[u8]) -> Vec<u8> {
    let evidence_hash = blake3::hash(evidence).to_hex();
    format!(
        "tcfs-trash-delete-complete-v1\ntrash_key={trash_key}\nevidence_blake3={evidence_hash}\n"
    )
    .into_bytes()
}

async fn read_trash_lifecycle_claim(
    op: &Operator,
    prefix: &str,
    trash_key: &str,
) -> Result<Option<TrashLifecycleClaim>> {
    let claim_key = trash_lifecycle_claim_key(prefix, trash_key);
    match op.read(&claim_key).await {
        Ok(observed) => {
            let observed = observed.to_vec();
            for claim in [TrashLifecycleClaim::Restore, TrashLifecycleClaim::Purge] {
                if observed == trash_lifecycle_claim_bytes(claim, trash_key) {
                    return Ok(Some(claim));
                }
            }
            anyhow::bail!("invalid trash lifecycle claim bytes: {claim_key}")
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::Error::new(error))
            .with_context(|| format!("reading trash lifecycle claim: {claim_key}")),
    }
}

async fn claim_trash_lifecycle(
    op: &Operator,
    prefix: &str,
    trash_key: &str,
    claim: TrashLifecycleClaim,
) -> Result<()> {
    let claim_key = trash_lifecycle_claim_key(prefix, trash_key);
    let expected = trash_lifecycle_claim_bytes(claim, trash_key);
    install_absent_or_accept_exact(op, &claim_key, &expected)
        .await
        .with_context(|| {
            format!(
                "claiming trash generation for {}: {claim_key}",
                claim.as_str()
            )
        })?;
    Ok(())
}

async fn trash_restore_is_complete(op: &Operator, prefix: &str, trash_key: &str) -> Result<bool> {
    let marker_key = trash_restore_complete_key(prefix, trash_key);
    let expected = trash_restore_complete_bytes(trash_key);
    match op.read(&marker_key).await {
        Ok(observed) => {
            anyhow::ensure!(
                observed.to_vec() == expected,
                "invalid trash restore-completion marker bytes: {marker_key}"
            );
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(anyhow::Error::new(error))
            .with_context(|| format!("reading trash restore-completion marker: {marker_key}")),
    }
}

async fn complete_trash_restore(op: &Operator, prefix: &str, trash_key: &str) -> Result<()> {
    let marker_key = trash_restore_complete_key(prefix, trash_key);
    let expected = trash_restore_complete_bytes(trash_key);
    install_absent_or_accept_exact(op, &marker_key, &expected)
        .await
        .with_context(|| format!("completing trash restore: {marker_key}"))?;
    Ok(())
}

async fn trash_delete_marker_is_complete(
    op: &Operator,
    prefix: &str,
    trash_key: &str,
    evidence: &[u8],
) -> Result<bool> {
    let marker_key = trash_delete_complete_key(prefix, trash_key);
    let expected = trash_delete_complete_bytes(trash_key, evidence);
    match op.read(&marker_key).await {
        Ok(observed) => {
            anyhow::ensure!(
                observed.to_vec() == expected,
                "invalid trash delete-completion marker bytes: {marker_key}"
            );
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(anyhow::Error::new(error))
            .with_context(|| format!("reading trash delete-completion marker: {marker_key}")),
    }
}

async fn trash_generation_state(
    op: &Operator,
    prefix: &str,
    rel_path: &str,
    trash_key: &str,
    evidence: &[u8],
) -> Result<TrashGenerationState> {
    if trash_delete_marker_is_complete(op, prefix, trash_key, evidence).await? {
        return Ok(TrashGenerationState::Completed);
    }

    let index_key = index_key_for_rel_path(prefix, rel_path);
    if let Some(record) =
        tcfs_sync::index_entry::read_index_entry_record_from_store(op, &index_key).await?
    {
        if let Some(bound) = record.deletion_evidence() {
            if bound.matches_trash_generation(prefix, rel_path, trash_key, evidence)? {
                return Ok(TrashGenerationState::Completed);
            }
        }
    }

    let synthetic = TrashEntry {
        original_path: rel_path.to_string(),
        trashed_at: 0,
        trash_key: trash_key.to_string(),
        index_content: String::new(),
        generation_state: TrashGenerationState::Indeterminate,
    };
    if trash_key_is_legacy(prefix, &synthetic)? {
        Ok(TrashGenerationState::LegacyRecoverable)
    } else {
        Ok(TrashGenerationState::Indeterminate)
    }
}

async fn complete_trash_delete(
    op: &Operator,
    prefix: &str,
    trash_key: &str,
    evidence: &[u8],
) -> Result<()> {
    let marker_key = trash_delete_complete_key(prefix, trash_key);
    let expected = trash_delete_complete_bytes(trash_key, evidence);
    install_absent_or_accept_exact(op, &marker_key, &expected)
        .await
        .with_context(|| format!("completing exact trash delete: {marker_key}"))?;
    Ok(())
}

/// Install an immutable safety copy without ever overwriting an object that
/// appeared concurrently. A lost successful response is accepted only after
/// an exact-byte read proves that the intended value is present.
async fn install_absent_or_accept_exact(
    op: &Operator,
    key: &str,
    expected: &[u8],
) -> Result<AbsentInstallOutcome> {
    if op.info().full_capability().write_with_if_not_exists {
        let outcome = match op
            .write_with(key, expected.to_vec())
            .if_not_exists(true)
            .await
        {
            Ok(_) => AbsentInstallOutcome::Created,
            Err(write_error) => match op.read(key).await {
                Ok(observed) if observed.to_vec() == expected => AbsentInstallOutcome::AlreadyExact,
                Ok(_) => {
                    anyhow::bail!(
                        "refusing to overwrite existing destination with different bytes: {key}"
                    )
                }
                Err(read_error) if read_error.kind() == ErrorKind::NotFound => {
                    return Err(anyhow::Error::new(write_error))
                        .with_context(|| format!("atomically creating absent object: {key}"));
                }
                Err(read_error) => {
                    return Err(anyhow::Error::new(read_error)).with_context(|| {
                        format!("checking destination after conditional write failed: {key}")
                    });
                }
            },
        };
        prove_object_bytes(op, key, expected).await?;
        return Ok(outcome);
    }

    // OpenDAL Memory has no external endpoint and advertises no conditional
    // writes. Keep its process-local emulation confined to this crate's tests.
    #[cfg(test)]
    if tcfs_storage::memory_conditional_write_emulation_is_registered_for_tests(op)? {
        let _guard = memory_absent_install_lock().lock().await;
        let outcome = match op.read(key).await {
            Ok(observed) if observed.to_vec() == expected => AbsentInstallOutcome::AlreadyExact,
            Ok(_) => {
                anyhow::bail!(
                    "refusing to overwrite existing destination with different bytes: {key}"
                )
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                op.write(key, expected.to_vec())
                    .await
                    .with_context(|| format!("creating guarded test object: {key}"))?;
                AbsentInstallOutcome::Created
            }
            Err(error) => {
                return Err(anyhow::Error::new(error))
                    .with_context(|| format!("reading guarded test destination: {key}"));
            }
        };
        prove_object_bytes(op, key, expected).await?;
        return Ok(outcome);
    }

    anyhow::bail!(
        "trash safety requires atomic absent-object creation; refusing unsafe write: {key}"
    )
}

async fn trash_bound_index_entry(
    op: &Operator,
    prefix: &str,
    index_key: &str,
    trash_key: &str,
    rel_path: &str,
    content_bytes: &[u8],
) -> Result<()> {
    install_absent_or_accept_exact(op, trash_key, content_bytes)
        .await
        .with_context(|| format!("installing trash entry without overwrite: {trash_key}"))?;

    let deletion_evidence = tcfs_sync::index_entry::DeletionEvidence::for_trash_generation(
        prefix,
        rel_path,
        trash_key,
        content_bytes,
    )?;

    if index_key.ends_with("/.tcfs_dir") {
        tcfs_sync::index_entry::tombstone_directory_marker_if_exact_with_evidence(
            op,
            prefix,
            index_key,
            content_bytes,
            deletion_evidence,
        )
        .await
        .with_context(|| format!("logically deleting exact directory marker: {index_key}"))?;
    } else {
        tcfs_sync::index_entry::tombstone_index_entry_if_exact_with_evidence(
            op,
            prefix,
            index_key,
            content_bytes,
            deletion_evidence,
        )
        .await
        .with_context(|| format!("logically deleting exact source index: {index_key}"))?;
    }
    prove_object_bytes(op, trash_key, content_bytes)
        .await
        .with_context(|| format!("revalidating trash evidence after tombstone: {trash_key}"))?;
    // This marker is deliberately installed only after the exact tombstone.
    // The tombstone itself binds the same key/digest, so a lost marker response
    // remains provably completed without promoting a failed source CAS.
    complete_trash_delete(op, prefix, trash_key, content_bytes).await?;
    Ok(())
}

/// Preserve an index entry in trash, then conditionally tombstone the live key.
///
/// Returns the trash key where the entry was stored.
pub async fn trash_index_entry(
    op: &Operator,
    prefix: &str,
    index_key: &str,
    rel_path: &str,
) -> Result<String> {
    let prefix = validate_remote_prefix(prefix)?;
    validate_rel_path(rel_path)?;
    let expected_index_key = index_key_for_rel_path(prefix, rel_path);
    anyhow::ensure!(
        index_key == expected_index_key,
        "index key is outside the selected trash root/path: expected {expected_index_key:?}, got {index_key:?}"
    );

    // Read the original index entry
    let content = op
        .read(index_key)
        .await
        .with_context(|| format!("reading index for trash: {index_key}"))?;
    let content_bytes = content.to_bytes();
    validate_trash_index_bindings(op, prefix, rel_path, &content_bytes)
        .await
        .with_context(|| format!("validating index before trash: {index_key}"))?;

    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .context("verifying conditional writes before trashing an index entry")?;

    // Generate trash key with timestamp
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let generation = format!("{now}-{}", uuid::Uuid::new_v4().hyphenated());
    let trash_key = prefixed_key(prefix, &format!(".tcfs-trash/{generation}/{rel_path}"));

    trash_bound_index_entry(op, prefix, index_key, &trash_key, rel_path, &content_bytes).await?;

    debug!(original = %rel_path, trash = %trash_key, "safely tombstoned with trash evidence");
    Ok(trash_key)
}

fn parse_listed_trash_identity(prefix: &str, path: &str) -> Result<(u64, String)> {
    let trash_prefix = prefixed_key(prefix, ".tcfs-trash/");
    let after_trash = path.strip_prefix(&trash_prefix).with_context(|| {
        format!("trash listing returned a key outside {trash_prefix:?}: {path:?}")
    })?;
    let (generation, original_path) = after_trash
        .split_once('/')
        .with_context(|| format!("malformed trash key: {path:?}"))?;
    let timestamp = parse_trash_generation(generation)
        .with_context(|| format!("invalid trash generation in key: {path:?}"))?
        .0;
    validate_rel_path(original_path)
        .with_context(|| format!("invalid original path in trash key: {path:?}"))?;
    Ok((timestamp, original_path.to_string()))
}

/// List all trashed items under the given prefix.
///
/// This compatibility API remains strict so an unqualified restore cannot
/// overlook an unreadable generation for the same logical path. Operator list
/// and purge surfaces use [`scan_trash`] to isolate unrelated corrupt objects.
pub async fn list_trash(op: &Operator, prefix: &str) -> Result<Vec<TrashEntry>> {
    let report = scan_trash(op, prefix).await?;
    if let Some(issue) = report.issues.first() {
        anyhow::bail!(
            "trash scan retained {} unreadable object(s); first issue at {}: {}",
            report.issues.len(),
            issue.trash_key,
            issue.error
        );
    }
    Ok(report.entries)
}

/// Read one exact trash generation without listing unrelated namespace keys.
///
/// This keeps an operator-provided `--trash-key` usable even when another
/// malformed/corrupt generation makes the conservative whole-prefix listing
/// fail closed.
pub async fn read_exact_trash_entry(
    op: &Operator,
    prefix: &str,
    original_path: &str,
    trash_key: &str,
) -> Result<Option<TrashEntry>> {
    let prefix = validate_remote_prefix(prefix)?;
    validate_rel_path(original_path)?;
    let mut entry = TrashEntry {
        original_path: original_path.to_string(),
        trashed_at: 0,
        trash_key: trash_key.to_string(),
        index_content: String::new(),
        generation_state: TrashGenerationState::Indeterminate,
    };
    entry.trashed_at = trash_key_timestamp(prefix, &entry)?;

    match read_trash_lifecycle_claim(op, prefix, trash_key).await? {
        Some(TrashLifecycleClaim::Purge) => return Ok(None),
        Some(TrashLifecycleClaim::Restore)
            if trash_restore_is_complete(op, prefix, trash_key).await? =>
        {
            return Ok(None)
        }
        Some(TrashLifecycleClaim::Restore) | None => {}
    }

    let content = match op.read(trash_key).await {
        Ok(content) => content.to_bytes(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::Error::new(error))
                .with_context(|| format!("reading exact trash entry: {trash_key}"))
        }
    };
    validate_trash_index_bindings(op, prefix, original_path, &content)
        .await
        .with_context(|| format!("validating exact trash entry bindings: {trash_key}"))?;
    entry.index_content = String::from_utf8_lossy(&content).to_string();
    entry.generation_state =
        trash_generation_state(op, prefix, original_path, trash_key, &content).await?;
    Ok(Some(entry))
}

/// Scan independently valid generations while retaining every key that cannot
/// be parsed or revalidated. Per-object failures are reported without hiding
/// valid rows; global listing failures still abort the scan.
pub async fn scan_trash(op: &Operator, prefix: &str) -> Result<TrashScanReport> {
    let prefix = validate_remote_prefix(prefix)?;
    let trash_prefix = prefixed_key(prefix, ".tcfs-trash/");
    let lister = op
        .list_with(&trash_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing trash: {trash_prefix}"))?;
    let mut entries = Vec::new();
    let mut issues = Vec::new();

    for listed in lister {
        let path = listed.path().to_string();
        if path.ends_with('/') {
            continue;
        }
        let (_, original_path) = match parse_listed_trash_identity(prefix, &path) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(
                    trash_key = %path,
                    error = %error,
                    "retaining malformed trash key during trash scan"
                );
                issues.push(TrashScanIssue {
                    trash_key: path,
                    error: format!("{error:#}"),
                });
                continue;
            }
        };
        match read_exact_trash_entry(op, prefix, &original_path, &path).await {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    trash_key = %path,
                    error = %error,
                    "retaining unverifiable trash generation during trash scan"
                );
                issues.push(TrashScanIssue {
                    trash_key: path,
                    error: format!("{error:#}"),
                });
            }
        }
    }

    entries.sort_by(|a, b| {
        b.trashed_at
            .cmp(&a.trashed_at)
            .then_with(|| b.trash_key.cmp(&a.trash_key))
    });
    Ok(TrashScanReport { entries, issues })
}

async fn validate_restore_destination_preflight(
    op: &Operator,
    index_key: &str,
    restored_bytes: &[u8],
) -> Result<()> {
    let observed = match op.read(index_key).await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(anyhow::Error::new(error))
                .with_context(|| format!("reading restore destination preflight: {index_key}"))
        }
    };
    if observed == restored_bytes {
        return Ok(());
    }
    let parsed = tcfs_sync::index_entry::parse_index_entry_record(&observed)
        .with_context(|| format!("validating restore destination preflight: {index_key}"))?;
    anyhow::ensure!(
        parsed.state() == tcfs_sync::index_entry::IndexEntryState::Deleted,
        "restore destination contains a different live index entry; preserving both values: {index_key}"
    );
    Ok(())
}

/// Restore a trashed item back to its original index location.
pub async fn restore_trash_entry(op: &Operator, prefix: &str, entry: &TrashEntry) -> Result<()> {
    let prefix = validate_remote_prefix(prefix)?;
    validate_rel_path(&entry.original_path)?;
    validate_trash_key_binding(prefix, entry)?;
    match read_trash_lifecycle_claim(op, prefix, &entry.trash_key).await? {
        Some(TrashLifecycleClaim::Purge) => {
            anyhow::bail!(
                "trash entry was already claimed for purge and cannot be restored: {}",
                entry.trash_key
            )
        }
        Some(TrashLifecycleClaim::Restore)
            if trash_restore_is_complete(op, prefix, &entry.trash_key).await? =>
        {
            return Ok(())
        }
        Some(TrashLifecycleClaim::Restore) | None => {}
    }

    let clean = &entry.original_path;
    let index_key = index_key_for_rel_path(prefix, clean);
    // Bind the restore to the live trash object rather than the lossy/list-time
    // display copy carried in `TrashEntry`.
    let trash_bytes = op
        .read(&entry.trash_key)
        .await
        .with_context(|| format!("reading live trash entry: {}", entry.trash_key))?
        .to_vec();

    let generation_state =
        trash_generation_state(op, prefix, clean, &entry.trash_key, &trash_bytes).await?;
    anyhow::ensure!(
        generation_state != TrashGenerationState::Indeterminate,
        "trash generation is indeterminate and cannot be restored automatically: {}",
        entry.trash_key
    );

    validate_trash_index_bindings(op, prefix, clean, &trash_bytes)
        .await
        .with_context(|| format!("validating live trash evidence: {}", entry.trash_key))?;

    // Reject a deterministic live conflict before reserving the namespace or
    // taking the immutable restore claim. The final publication still performs
    // its own compare-and-swap to close the concurrent-update window.
    validate_restore_destination_preflight(op, &index_key, &trash_bytes).await?;

    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .context("verifying conditional writes before restoring a trash entry")?;

    let (namespace_path, namespace_role) = namespace_claim_for_index_rel_path(clean)?;
    tcfs_sync::index_entry::admit_portable_namespace_entry(
        op,
        prefix,
        namespace_path,
        namespace_role,
    )
    .await
    .context("admitting restored index path into the portable namespace")?;

    // The claim is the linearization point shared with purge. Only a restore
    // claimant may mutate the live index. A separate completion marker keeps an
    // interrupted restore visible and retryable without reopening the purge race.
    claim_trash_lifecycle(op, prefix, &entry.trash_key, TrashLifecycleClaim::Restore).await?;

    // If this CAS loses to a concurrent publisher, retain the Restore claim.
    // Releasing one object cannot fence another process that already observed
    // the same claim and may still publish this safety copy; allowing Purge to
    // supersede it would reopen restore-after-purge. The evidence therefore
    // stays operator-visible and retryable after the live conflict is resolved.
    tcfs_sync::index_entry::restore_index_entry_from_safety_copy(
        op,
        prefix,
        &index_key,
        &trash_bytes,
    )
    .await
    .with_context(|| format!("restoring index entry with compare-and-swap: {index_key}"))?;

    // Retain the immutable evidence. Completion hides the restored generation;
    // a crash before this point leaves the restore claim visible for retry.
    prove_object_bytes(op, &entry.trash_key, &trash_bytes)
        .await
        .with_context(|| format!("revalidating trash entry: {}", entry.trash_key))?;
    complete_trash_restore(op, prefix, &entry.trash_key).await?;

    debug!(
        path = %entry.original_path,
        "restored from trash"
    );
    Ok(())
}

async fn storage_object_unix_timestamp(op: &Operator, key: &str) -> Result<Option<u64>> {
    let metadata = op
        .stat(key)
        .await
        .with_context(|| format!("reading storage timestamp: {key}"))?;
    let Some(last_modified) = metadata.last_modified() else {
        return Ok(None);
    };
    let system_time: SystemTime = last_modified.into();
    let seconds = system_time
        .duration_since(UNIX_EPOCH)
        .context("storage returned a pre-epoch object timestamp")?
        .as_secs();
    Ok(Some(seconds))
}

async fn trash_retention_anchor_key(
    op: &Operator,
    prefix: &str,
    entry: &TrashEntry,
    evidence: &[u8],
    generation_state: TrashGenerationState,
) -> Result<Option<String>> {
    match generation_state {
        TrashGenerationState::Indeterminate => Ok(None),
        // Historical generations predate a durable delete-completion proof.
        // Their safety-copy creation time may precede deletion by an arbitrary
        // interval, so only explicit purge-all may retire them.
        TrashGenerationState::LegacyRecoverable => Ok(None),
        TrashGenerationState::Completed => {
            if trash_delete_marker_is_complete(op, prefix, &entry.trash_key, evidence).await? {
                return Ok(Some(trash_delete_complete_key(prefix, &entry.trash_key)));
            }

            let index_key = index_key_for_rel_path(prefix, &entry.original_path);
            let record = tcfs_sync::index_entry::read_index_entry_record_from_store(op, &index_key)
                .await?
                .with_context(|| {
                    format!(
                        "completed trash generation lost its delete authority: {}",
                        entry.trash_key
                    )
                })?;
            let bound = record.deletion_evidence().with_context(|| {
                format!(
                    "completed trash generation has no bound tombstone evidence: {}",
                    entry.trash_key
                )
            })?;
            anyhow::ensure!(
                bound.matches_trash_generation(
                    prefix,
                    &entry.original_path,
                    &entry.trash_key,
                    evidence,
                )?,
                "completed trash tombstone no longer binds generation: {}",
                entry.trash_key
            );
            Ok(Some(index_key))
        }
    }
}

async fn storage_clock_now(op: &Operator, prefix: &str) -> Result<u64> {
    // One fixed key is both the storage-time sample and a cross-process mutex.
    // A failed cleanup blocks later age purges instead of leaking one unique
    // object per retry. The random invocation bytes prove cleanup ownership.
    let clock_key = prefixed_key(prefix, ".tcfs-trash-clock/v1/clock");
    let invocation = uuid::Uuid::new_v4().hyphenated().to_string();
    let clock_bytes =
        format!("tcfs-trash-clock-v1\nkey={clock_key}\ninvocation={invocation}\n").into_bytes();
    install_absent_or_accept_exact(op, &clock_key, &clock_bytes)
        .await
        .context("installing storage-authoritative trash retention clock")?;

    let metadata = op
        .stat(&clock_key)
        .await
        .with_context(|| format!("reading storage retention clock metadata: {clock_key}"))?;
    // A successful stat proves which version may be cleaned up even when the
    // backend omits Last-Modified. Preserve the timestamp error until after
    // cleanup so that capability omission does not permanently wedge the
    // fixed guard. A stat failure above retains the guard because its version
    // is unknown and therefore cannot be safely deleted.
    let timestamp = metadata
        .last_modified()
        .context("storage backend omitted last-modified time for trash retention clock")
        .and_then(|last_modified| {
            let system_time: SystemTime = last_modified.into();
            Ok(system_time
                .duration_since(UNIX_EPOCH)
                .context("storage returned a pre-epoch trash retention clock")?
                .as_secs())
        });
    let version = metadata
        .version()
        .filter(|version| !version.is_empty())
        .map(str::to_owned);

    // Re-prove the invocation bytes immediately before cleanup. Version-aware
    // deletion closes the race on versioned stores; the fixed, internal mutex
    // key excludes a second protocol writer on unversioned stores.
    let cleanup = match prove_object_bytes(op, &clock_key, &clock_bytes).await {
        Ok(()) => match version {
            Some(version) if op.info().full_capability().delete_with_version => {
                op.delete_with(&clock_key).version(&version).await
            }
            Some(_) => anyhow::bail!(
                "storage returned a versioned trash retention clock but cannot delete that exact version; retaining guard: {clock_key}"
            ),
            None => op.delete(&clock_key).await,
        },
        Err(error) => return Err(error.context("revalidating trash retention clock ownership")),
    };
    if let Err(error) = cleanup {
        return Err(anyhow::Error::new(error))
            .with_context(|| format!("removing temporary trash retention clock: {clock_key}"));
    }
    match op.read(&clock_key).await {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Ok(observed) if observed.to_vec() == clock_bytes => {
            anyhow::bail!("storage retained the deleted trash retention clock: {clock_key}")
        }
        Ok(_) => {
            anyhow::bail!("trash retention clock changed during cleanup: {clock_key}")
        }
        Err(error) => {
            return Err(anyhow::Error::new(error))
                .with_context(|| format!("verifying trash retention clock cleanup: {clock_key}"))
        }
    }
    timestamp
}

/// Logically purge trashed items older than `max_age_secs`.
///
/// An exclusive lifecycle claim hides each generation while retaining immutable
/// evidence for a future reachability-safe physical GC. Returns the number marked.
pub async fn purge_old_trash(
    op: &Operator,
    prefix: &str,
    max_age_secs: u64,
) -> Result<TrashPurgeReport> {
    let prefix = validate_remote_prefix(prefix)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .context("verifying conditional writes before purging trash")?;
    // Zero is the explicit purge-all path and needs no age clock. Retention
    // purges use storage-assigned timestamps for both operands so skewed client
    // clocks cannot hide a fresh generation.
    let storage_now = if max_age_secs == 0 {
        None
    } else {
        Some(storage_clock_now(op, prefix).await?)
    };

    let scan = scan_trash(op, prefix).await?;
    for issue in &scan.issues {
        tracing::warn!(
            trash_key = %issue.trash_key,
            error = %issue.error,
            "retaining unreadable trash object during purge"
        );
    }
    let entries = scan.entries;
    let mut issues = scan.issues;
    let mut purged = 0;

    for entry in &entries {
        if let Err(error) = validate_trash_key_binding(prefix, entry) {
            tracing::warn!(
                trash_key = %entry.trash_key,
                error = %error,
                "retaining trash generation whose key changed during purge"
            );
            issues.push(TrashScanIssue {
                trash_key: entry.trash_key.clone(),
                error: format!("trash key changed during purge: {error:#}"),
            });
            continue;
        }
        let evidence = match op.read(&entry.trash_key).await {
            Ok(evidence) => evidence.to_vec(),
            Err(error) => {
                tracing::warn!(
                    path = %entry.original_path,
                    trash_key = %entry.trash_key,
                    error = %error,
                    "retaining trash generation whose evidence cannot be re-read"
                );
                issues.push(TrashScanIssue {
                    trash_key: entry.trash_key.clone(),
                    error: format!("trash evidence cannot be re-read: {error:#}"),
                });
                continue;
            }
        };
        let generation_state = match trash_generation_state(
            op,
            prefix,
            &entry.original_path,
            &entry.trash_key,
            &evidence,
        )
        .await
        {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(
                    path = %entry.original_path,
                    trash_key = %entry.trash_key,
                    error = %error,
                    "retaining trash generation whose delete authority cannot be revalidated"
                );
                issues.push(TrashScanIssue {
                    trash_key: entry.trash_key.clone(),
                    error: format!("trash delete authority cannot be revalidated: {error:#}"),
                });
                continue;
            }
        };
        if generation_state == TrashGenerationState::Indeterminate {
            tracing::warn!(
                path = %entry.original_path,
                trash_key = %entry.trash_key,
                "retaining indeterminate trash generation during purge"
            );
            issues.push(TrashScanIssue {
                trash_key: entry.trash_key.clone(),
                error: "trash generation remains indeterminate".to_string(),
            });
            continue;
        }
        let old_enough = if let Some(storage_now) = storage_now {
            let anchor_key =
                match trash_retention_anchor_key(op, prefix, entry, &evidence, generation_state)
                    .await
                {
                    Ok(Some(anchor_key)) => anchor_key,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(
                            path = %entry.original_path,
                            trash_key = %entry.trash_key,
                            error = %error,
                            "retaining trash generation without a proved retention anchor"
                        );
                        issues.push(TrashScanIssue {
                            trash_key: entry.trash_key.clone(),
                            error: format!("trash retention anchor cannot be proved: {error:#}"),
                        });
                        continue;
                    }
                };
            let stored_at = match storage_object_unix_timestamp(op, &anchor_key).await {
                Ok(Some(stored_at)) => stored_at,
                Ok(None) => {
                    tracing::warn!(
                        path = %entry.original_path,
                        trash_key = %entry.trash_key,
                        anchor_key = %anchor_key,
                        "retaining trash generation without storage last-modified metadata"
                    );
                    issues.push(TrashScanIssue {
                        trash_key: entry.trash_key.clone(),
                        error: format!(
                            "trash retention anchor has no storage last-modified metadata: {anchor_key}"
                        ),
                    });
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        path = %entry.original_path,
                        trash_key = %entry.trash_key,
                        anchor_key = %anchor_key,
                        error = %error,
                        "retaining trash generation whose retention anchor cannot be read"
                    );
                    issues.push(TrashScanIssue {
                        trash_key: entry.trash_key.clone(),
                        error: format!("trash retention anchor cannot be read: {error:#}"),
                    });
                    continue;
                }
            };
            if stored_at > storage_now {
                tracing::warn!(
                    path = %entry.original_path,
                    trash_key = %entry.trash_key,
                    anchor_key = %anchor_key,
                    stored_at,
                    storage_now,
                    "retaining trash generation with an impossible future storage timestamp"
                );
                issues.push(TrashScanIssue {
                    trash_key: entry.trash_key.clone(),
                    error: format!(
                        "trash retention anchor timestamp {stored_at} is newer than storage clock {storage_now}"
                    ),
                });
                continue;
            }
            storage_now - stored_at >= max_age_secs
        } else {
            true
        };
        if old_enough {
            if let Err(e) =
                claim_trash_lifecycle(op, prefix, &entry.trash_key, TrashLifecycleClaim::Purge)
                    .await
            {
                tracing::warn!(
                    path = %entry.original_path,
                    error = %e,
                    "failed to mark trash entry purged"
                );
                issues.push(TrashScanIssue {
                    trash_key: entry.trash_key.clone(),
                    error: format!("failed to claim trash generation for purge: {e:#}"),
                });
            } else {
                purged += 1;
            }
        }
    }

    if purged > 0 {
        debug!(purged, "purged old trash entries");
    }
    Ok(TrashPurgeReport { purged, issues })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;

    fn memory_op() -> Operator {
        let builder = Memory::default();
        let op = Operator::new(builder).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    async fn seed_test_manifest(
        op: &Operator,
        prefix: &str,
        rel_path: &str,
        manifest_hash: &str,
        size: u64,
        chunks: usize,
    ) {
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: manifest_hash.to_string(),
            file_size: size,
            chunks: (0..chunks).map(|index| format!("chunk-{index}")).collect(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "trash-test".into(),
            written_at: 0,
            rel_path: Some(rel_path.to_string()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        op.write(
            &format!("{prefix}/manifests/{manifest_hash}"),
            manifest.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    }

    async fn mark_test_trash_delete_complete(
        op: &Operator,
        prefix: &str,
        trash_key: &str,
        evidence: &[u8],
    ) {
        complete_trash_delete(op, prefix, trash_key, evidence)
            .await
            .unwrap();
    }

    #[test]
    fn trash_generation_requires_numeric_legacy_or_canonical_uuid() {
        assert_eq!(parse_trash_generation("123").unwrap(), (123, true));
        assert_eq!(
            parse_trash_generation("123-00000000-0000-4000-8000-000000000000").unwrap(),
            (123, false)
        );
        assert!(parse_trash_generation("123-garbage").is_err());
        assert!(parse_trash_generation("123-00000000-0000-4000-8000-00000000000A").is_err());
        assert!(parse_trash_generation("12\n3").is_err());
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let op = memory_op();
        let prefix = "test";

        // Create an index entry
        let index_key = "test/index/doc.txt";
        op.write(
            index_key,
            b"manifest_hash=abc123\nsize=100\nchunks=1".to_vec(),
        )
        .await
        .unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "abc123", 100, 1).await;

        // Trash it
        let trash_key = trash_index_entry(&op, prefix, index_key, "doc.txt")
            .await
            .unwrap();

        // Original is logically absent behind a durable tombstone.
        let tombstone = tcfs_sync::index_entry::read_index_entry_record_from_store(&op, index_key)
            .await
            .unwrap()
            .unwrap();
        assert!(tombstone.visible_entry().is_none());

        // Should appear in trash list
        let entries = list_trash(&op, prefix).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_path, "doc.txt");
        assert!(entries[0].index_content.contains("manifest_hash=abc123"));
        assert_eq!(entries[0].generation_state, TrashGenerationState::Completed);

        // Restore it
        restore_trash_entry(&op, prefix, &entries[0]).await.unwrap();

        // Original should be back
        assert!(op.read(index_key).await.is_ok());

        // Trash evidence remains durable but is hidden by a restored marker.
        let entries = list_trash(&op, prefix).await.unwrap();
        assert_eq!(entries.len(), 0);
        assert!(op.exists(&trash_key).await.unwrap());
    }

    #[tokio::test]
    async fn evidence_bound_tombstone_survives_lost_completion_marker() {
        let op = memory_op();
        let prefix = "test";
        let rel_path = "doc.txt";
        let index_key = "test/index/doc.txt";
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let original = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        op.write(index_key, original.clone()).await.unwrap();
        op.write(trash_key, original.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, rel_path, "original", 100, 1).await;

        let deletion_evidence = tcfs_sync::index_entry::DeletionEvidence::for_trash_generation(
            prefix, rel_path, trash_key, &original,
        )
        .unwrap();
        tcfs_sync::index_entry::tombstone_index_entry_if_exact_with_evidence(
            &op,
            prefix,
            index_key,
            &original,
            deletion_evidence,
        )
        .await
        .unwrap();

        assert!(!op
            .exists(&trash_delete_complete_key(prefix, trash_key))
            .await
            .unwrap());
        let entry = list_trash(&op, prefix).await.unwrap().remove(0);
        assert_eq!(entry.generation_state, TrashGenerationState::Completed);
        restore_trash_entry(&op, prefix, &entry).await.unwrap();
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original);
    }

    #[tokio::test]
    async fn historical_timestamp_generation_remains_recoverable() {
        let op = memory_op();
        let prefix = "test";
        let trash_key = "test/.tcfs-trash/123/doc.txt";
        let original = b"manifest_hash=legacy\nsize=100\nchunks=1".to_vec();
        op.write(trash_key, original.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "legacy", 100, 1).await;

        let entry = list_trash(&op, prefix).await.unwrap().remove(0);
        assert_eq!(
            entry.generation_state,
            TrashGenerationState::LegacyRecoverable
        );
        restore_trash_entry(&op, prefix, &entry).await.unwrap();
        assert_eq!(
            op.read("test/index/doc.txt").await.unwrap().to_vec(),
            original
        );
    }

    #[tokio::test]
    async fn legacy_corrupt_or_unbound_payloads_remain_issues_during_purge_all() {
        let op = memory_op();
        let prefix = "test";
        let corrupt_key = "test/.tcfs-trash/123/corrupt.txt";
        let missing_manifest_key = "test/.tcfs-trash/124/missing.txt";
        op.write(corrupt_key, b"not an index entry".to_vec())
            .await
            .unwrap();
        op.write(
            missing_manifest_key,
            b"manifest_hash=missing\nsize=1\nchunks=1".to_vec(),
        )
        .await
        .unwrap();

        let scan = scan_trash(&op, prefix).await.unwrap();
        assert!(scan.entries.is_empty());
        assert_eq!(scan.issues.len(), 2);

        let purge = purge_old_trash(&op, prefix, 0).await.unwrap();
        assert_eq!(purge.purged, 0);
        assert_eq!(purge.issues.len(), 2);
        assert!(op.exists(corrupt_key).await.unwrap());
        assert!(op.exists(missing_manifest_key).await.unwrap());
        assert_eq!(scan_trash(&op, prefix).await.unwrap().issues.len(), 2);
    }

    #[tokio::test]
    async fn exact_generation_read_bypasses_unrelated_malformed_listing_key() {
        let op = memory_op();
        let prefix = "test";
        let trash_key = "test/.tcfs-trash/123/doc.txt";
        let original = b"manifest_hash=legacy\nsize=100\nchunks=1".to_vec();
        op.write(trash_key, original.clone()).await.unwrap();
        op.write(
            "test/.tcfs-trash/not-a-timestamp/unrelated.txt",
            b"corrupt".to_vec(),
        )
        .await
        .unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "legacy", 100, 1).await;

        let report = scan_trash(&op, prefix).await.unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].trash_key, trash_key);
        assert_eq!(report.issues.len(), 1);
        assert!(report.issues[0].trash_key.contains("not-a-timestamp"));
        assert!(list_trash(&op, prefix).await.is_err());
        let entry = read_exact_trash_entry(&op, prefix, "doc.txt", trash_key)
            .await
            .unwrap()
            .expect("exact valid generation remains addressable");
        restore_trash_entry(&op, prefix, &entry).await.unwrap();
        assert_eq!(
            op.read("test/index/doc.txt").await.unwrap().to_vec(),
            original
        );
    }

    #[tokio::test]
    async fn purge_claims_valid_generation_while_retaining_malformed_neighbor() {
        let op = memory_op();
        let prefix = "test";
        let valid_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/valid.txt";
        let malformed_key = "test/.tcfs-trash/not-a-timestamp/unrelated.txt";
        let evidence = b"manifest_hash=valid\nsize=7\nchunks=1".to_vec();
        op.write(valid_key, evidence.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "valid.txt", "valid", 7, 1).await;
        mark_test_trash_delete_complete(&op, prefix, valid_key, &evidence).await;
        op.write(malformed_key, b"retain-corrupt".to_vec())
            .await
            .unwrap();

        assert_eq!(purge_old_trash(&op, prefix, 0).await.unwrap().purged, 1);
        assert_eq!(
            read_trash_lifecycle_claim(&op, prefix, valid_key)
                .await
                .unwrap(),
            Some(TrashLifecycleClaim::Purge)
        );
        assert!(op.exists(malformed_key).await.unwrap());
        assert!(list_trash(&op, prefix).await.is_err());
    }

    #[tokio::test]
    async fn purge_isolates_corrupt_lifecycle_metadata() {
        let op = memory_op();
        let prefix = "test";
        let valid_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/valid.txt";
        let corrupt_key = "test/.tcfs-trash/124-00000000-0000-4000-8000-000000000001/corrupt.txt";
        let valid = b"manifest_hash=valid\nsize=7\nchunks=1".to_vec();
        let corrupt = b"manifest_hash=corrupt\nsize=8\nchunks=1".to_vec();
        op.write(valid_key, valid.clone()).await.unwrap();
        op.write(corrupt_key, corrupt.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "valid.txt", "valid", 7, 1).await;
        seed_test_manifest(&op, prefix, "corrupt.txt", "corrupt", 8, 1).await;
        mark_test_trash_delete_complete(&op, prefix, valid_key, &valid).await;
        mark_test_trash_delete_complete(&op, prefix, corrupt_key, &corrupt).await;
        let corrupt_claim = trash_lifecycle_claim_key(prefix, corrupt_key);
        op.write(&corrupt_claim, b"invalid lifecycle bytes".to_vec())
            .await
            .unwrap();

        let report = scan_trash(&op, prefix).await.unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].trash_key, valid_key);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].trash_key, corrupt_key);
        assert_eq!(purge_old_trash(&op, prefix, 0).await.unwrap().purged, 1);
        assert_eq!(
            read_trash_lifecycle_claim(&op, prefix, valid_key)
                .await
                .unwrap(),
            Some(TrashLifecycleClaim::Purge)
        );
        assert_eq!(
            op.read(&corrupt_claim).await.unwrap().to_vec(),
            b"invalid lifecycle bytes"
        );
        assert!(op.exists(corrupt_key).await.unwrap());
    }

    #[tokio::test]
    async fn trash_rejects_missing_manifest_before_tombstoning_live_index() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        let original = b"manifest_hash=missing\nsize=100\nchunks=1".to_vec();
        op.write(index_key, original.clone()).await.unwrap();

        let error = trash_index_entry(&op, "test", index_key, "doc.txt")
            .await
            .expect_err("unbound trash source must fail before logical delete");

        assert!(format!("{error:#}").contains("reading trash-bound manifest"));
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original);
        assert!(list_trash(&op, "test").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn restore_rejects_path_mismatched_manifest_before_claim_or_publication() {
        let op = memory_op();
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let evidence = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        op.write(trash_key, evidence.clone()).await.unwrap();
        mark_test_trash_delete_complete(&op, "test", trash_key, &evidence).await;
        seed_test_manifest(&op, "test", "other.txt", "original", 100, 1).await;
        let entry = TrashEntry {
            original_path: "doc.txt".into(),
            trashed_at: 123,
            trash_key: trash_key.into(),
            index_content: String::new(),
            generation_state: TrashGenerationState::Completed,
        };

        let error = restore_trash_entry(&op, "test", &entry)
            .await
            .expect_err("path-mismatched trash evidence must fail before publication");

        assert!(format!("{error:#}").contains("manifest rel_path mismatch"));
        assert!(!op.exists("test/index/doc.txt").await.unwrap());
        assert!(read_trash_lifecycle_claim(&op, "test", trash_key)
            .await
            .unwrap()
            .is_none());
        assert!(op
            .list("test/.tcfs-namespace/v1/")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn restore_refuses_recreated_destination_with_different_bytes() {
        let op = memory_op();
        let prefix = "test";
        let index_key = "test/index/doc.txt";
        let original = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        let recreated = b"manifest_hash=recreated\nsize=200\nchunks=2".to_vec();
        op.write(index_key, original).await.unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "original", 100, 1).await;

        trash_index_entry(&op, prefix, index_key, "doc.txt")
            .await
            .unwrap();
        let entry = list_trash(&op, prefix).await.unwrap().remove(0);

        // Deterministically model another publisher recreating the destination
        // after the restore snapshot was listed but before its absent create.
        op.write(index_key, recreated.clone()).await.unwrap();
        let error = restore_trash_entry(&op, prefix, &entry)
            .await
            .expect_err("restore must not overwrite a concurrently recreated index");

        assert!(
            format!("{error:#}").contains("different live index entry"),
            "unexpected restore error: {error:#}"
        );
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), recreated);
        assert!(read_trash_lifecycle_claim(&op, prefix, &entry.trash_key)
            .await
            .unwrap()
            .is_none());
        assert!(
            op.exists(&entry.trash_key).await.unwrap(),
            "failed restore must preserve the trash evidence"
        );
    }

    #[tokio::test]
    async fn claimed_restore_conflict_stays_retryable_and_cannot_be_purged() {
        let op = memory_op();
        let prefix = "test";
        let index_key = "test/index/doc.txt";
        let original = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        let recreated = b"manifest_hash=recreated\nsize=200\nchunks=2".to_vec();
        op.write(index_key, original).await.unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "original", 100, 1).await;
        trash_index_entry(&op, prefix, index_key, "doc.txt")
            .await
            .unwrap();
        let entry = list_trash(&op, prefix).await.unwrap().remove(0);

        // This is the durable state after a publisher wins the destination
        // race immediately after the restore claim linearizes.
        claim_trash_lifecycle(&op, prefix, &entry.trash_key, TrashLifecycleClaim::Restore)
            .await
            .unwrap();
        op.write(index_key, recreated.clone()).await.unwrap();
        let error = restore_trash_entry(&op, prefix, &entry)
            .await
            .expect_err("a claimed restore must preserve a concurrent live publisher");

        assert!(format!("{error:#}").contains("different live index entry"));
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), recreated);
        assert_eq!(
            read_trash_lifecycle_claim(&op, prefix, &entry.trash_key)
                .await
                .unwrap(),
            Some(TrashLifecycleClaim::Restore)
        );
        assert_eq!(purge_old_trash(&op, prefix, 0).await.unwrap().purged, 0);
        assert_eq!(list_trash(&op, prefix).await.unwrap().len(), 1);
        assert!(op.exists(&entry.trash_key).await.unwrap());
    }

    #[tokio::test]
    async fn restore_accepts_exact_idempotent_destination() {
        let op = memory_op();
        let prefix = "test";
        let index_key = "test/index/doc.txt";
        let original = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        op.write(index_key, original.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "original", 100, 1).await;

        trash_index_entry(&op, prefix, index_key, "doc.txt")
            .await
            .unwrap();
        let entry = list_trash(&op, prefix).await.unwrap().remove(0);
        op.write(index_key, original.clone()).await.unwrap();

        restore_trash_entry(&op, prefix, &entry).await.unwrap();

        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original);
        assert!(op.exists(&entry.trash_key).await.unwrap());
        assert!(list_trash(&op, prefix).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn trash_revalidation_preserves_source_changed_after_snapshot() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let snapshot = b"manifest_hash=old\nsize=1\nchunks=1".to_vec();
        let concurrent = b"manifest_hash=new\nsize=2\nchunks=1".to_vec();
        op.write(index_key, snapshot.clone()).await.unwrap();
        seed_test_manifest(&op, "test", "doc.txt", "old", 1, 1).await;

        // Bind the old bytes, then inject the update in the exact window that
        // preceded the old unconditional source delete.
        let bound = op.read(index_key).await.unwrap().to_vec();
        op.write(index_key, concurrent.clone()).await.unwrap();
        let error = trash_bound_index_entry(&op, "test", index_key, trash_key, "doc.txt", &bound)
            .await
            .expect_err("changed source must not be deleted");

        assert!(
            format!("{error:#}").contains("changed before logical delete"),
            "unexpected trash error: {error:#}"
        );
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), concurrent);
        assert_eq!(op.read(trash_key).await.unwrap().to_vec(), snapshot);
        let entries = list_trash(&op, "test").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].generation_state,
            TrashGenerationState::Indeterminate
        );
        let restore_error = restore_trash_entry(&op, "test", &entries[0])
            .await
            .expect_err("indeterminate safety copy must not be restored automatically");
        assert!(format!("{restore_error:#}").contains("indeterminate"));
        assert_eq!(purge_old_trash(&op, "test", 0).await.unwrap().purged, 0);
        assert!(op.exists(trash_key).await.unwrap());
    }

    #[tokio::test]
    async fn trash_rejects_an_index_key_outside_the_selected_root_and_path() {
        let op = memory_op();
        let foreign_key = "other/index/doc.txt";
        op.write(foreign_key, b"foreign".to_vec()).await.unwrap();

        let error = trash_index_entry(&op, "test", foreign_key, "doc.txt")
            .await
            .expect_err("cross-root index key must be rejected");

        assert!(format!("{error:#}").contains("outside the selected trash root/path"));
        assert!(op.exists(foreign_key).await.unwrap());
    }

    #[tokio::test]
    async fn restore_rejects_a_forged_cross_root_trash_key() {
        let op = memory_op();
        let foreign_key = "other/.tcfs-trash/1/doc.txt";
        op.write(foreign_key, b"foreign".to_vec()).await.unwrap();
        let entry = TrashEntry {
            original_path: "doc.txt".into(),
            trashed_at: 1,
            trash_key: foreign_key.into(),
            index_content: String::new(),
            generation_state: TrashGenerationState::Indeterminate,
        };

        let error = restore_trash_entry(&op, "test", &entry)
            .await
            .expect_err("cross-root trash key must be rejected");

        assert!(format!("{error:#}").contains("outside selected prefix"));
        assert!(!op.exists("test/index/doc.txt").await.unwrap());
        assert!(op.exists(foreign_key).await.unwrap());
    }

    #[tokio::test]
    async fn repeated_same_second_trash_events_use_distinct_generation_keys() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        let content = b"manifest_hash=same\nsize=1\nchunks=1".to_vec();
        op.write(index_key, content.clone()).await.unwrap();
        seed_test_manifest(&op, "test", "doc.txt", "same", 1, 1).await;
        let first = trash_index_entry(&op, "test", index_key, "doc.txt")
            .await
            .unwrap();
        op.write(index_key, content).await.unwrap();
        let second = trash_index_entry(&op, "test", index_key, "doc.txt")
            .await
            .unwrap();

        assert_ne!(first, second);
        assert!(op.exists(&first).await.unwrap());
        assert!(op.exists(&second).await.unwrap());
    }

    #[tokio::test]
    async fn same_second_trash_entries_have_deterministic_full_key_order() {
        let op = memory_op();
        let lower = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let higher = "test/.tcfs-trash/123-ffffffff-ffff-4fff-bfff-ffffffffffff/doc.txt";
        let content = b"manifest_hash=same\nsize=1\nchunks=1".to_vec();
        op.write(lower, content.clone()).await.unwrap();
        op.write(higher, content).await.unwrap();
        seed_test_manifest(&op, "test", "doc.txt", "same", 1, 1).await;

        let entries = list_trash(&op, "test").await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].trash_key, higher);
        assert_eq!(entries[1].trash_key, lower);
    }

    #[tokio::test]
    async fn restore_and_purge_claims_are_mutually_exclusive() {
        let op = memory_op();
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";

        let (restore, purge) = tokio::join!(
            claim_trash_lifecycle(&op, "test", trash_key, TrashLifecycleClaim::Restore),
            claim_trash_lifecycle(&op, "test", trash_key, TrashLifecycleClaim::Purge),
        );

        assert_ne!(restore.is_ok(), purge.is_ok());
        let winner = read_trash_lifecycle_claim(&op, "test", trash_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(winner == TrashLifecycleClaim::Restore, restore.is_ok());
        assert_eq!(winner == TrashLifecycleClaim::Purge, purge.is_ok());
    }

    #[tokio::test]
    async fn interrupted_restore_claim_remains_visible_and_retryable() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        let original = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        op.write(index_key, original.clone()).await.unwrap();
        seed_test_manifest(&op, "test", "doc.txt", "original", 100, 1).await;
        trash_index_entry(&op, "test", index_key, "doc.txt")
            .await
            .unwrap();
        let entry = list_trash(&op, "test").await.unwrap().remove(0);

        claim_trash_lifecycle(&op, "test", &entry.trash_key, TrashLifecycleClaim::Restore)
            .await
            .unwrap();
        assert_eq!(list_trash(&op, "test").await.unwrap().len(), 1);

        restore_trash_entry(&op, "test", &entry).await.unwrap();
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original);
        assert!(list_trash(&op, "test").await.unwrap().is_empty());

        // A completed stale handle is idempotent and cannot resurrect this old
        // generation after the path is deleted again.
        tcfs_sync::index_entry::tombstone_index_entry(&op, "test", index_key)
            .await
            .unwrap();
        restore_trash_entry(&op, "test", &entry).await.unwrap();
        let current = tcfs_sync::index_entry::read_index_entry_record_from_store(&op, index_key)
            .await
            .unwrap()
            .unwrap();
        assert!(current.visible_entry().is_none());
    }

    #[tokio::test]
    async fn malformed_restore_payload_does_not_reserve_the_namespace() {
        let op = memory_op();
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let evidence = b"not an index record".to_vec();
        op.write(trash_key, evidence.clone()).await.unwrap();
        mark_test_trash_delete_complete(&op, "test", trash_key, &evidence).await;
        let entry = TrashEntry {
            original_path: "doc.txt".into(),
            trashed_at: 123,
            trash_key: trash_key.into(),
            index_content: String::new(),
            generation_state: TrashGenerationState::Completed,
        };

        restore_trash_entry(&op, "test", &entry)
            .await
            .expect_err("malformed evidence must fail before namespace admission");
        let reservations = op.list("test/.tcfs-namespace/v1/").await.unwrap();
        assert!(reservations.is_empty());
        assert!(read_trash_lifecycle_claim(&op, "test", trash_key)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unregistered_memory_accessor_cannot_use_test_absent_write_emulation() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let error = install_absent_or_accept_exact(&op, "test/object", b"bytes")
            .await
            .expect_err("scheme alone must not enable test emulation");
        assert!(format!("{error:#}").contains("requires atomic absent-object creation"));
    }

    #[tokio::test]
    async fn retention_anchor_starts_at_delete_authority_not_safety_copy() {
        let op = memory_op();
        let prefix = "test";
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let evidence = b"manifest_hash=original\nsize=100\nchunks=1".to_vec();
        op.write(trash_key, evidence.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "doc.txt", "original", 100, 1).await;
        mark_test_trash_delete_complete(&op, prefix, trash_key, &evidence).await;
        let entry = read_exact_trash_entry(&op, prefix, "doc.txt", trash_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            trash_retention_anchor_key(
                &op,
                prefix,
                &entry,
                &evidence,
                TrashGenerationState::Completed,
            )
            .await
            .unwrap(),
            Some(trash_delete_complete_key(prefix, trash_key))
        );

        // If the post-delete marker was lost, the evidence-bound tombstone is
        // the exact deletion linearization artifact and becomes the anchor.
        op.delete(&trash_delete_complete_key(prefix, trash_key))
            .await
            .unwrap();
        let bound = tcfs_sync::index_entry::DeletionEvidence::for_trash_generation(
            prefix, "doc.txt", trash_key, &evidence,
        )
        .unwrap();
        op.write(
            "test/index/doc.txt",
            tcfs_sync::index_entry::VersionedIndexEntry::deleted_with_evidence(bound)
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            trash_retention_anchor_key(
                &op,
                prefix,
                &entry,
                &evidence,
                TrashGenerationState::Completed,
            )
            .await
            .unwrap(),
            Some("test/index/doc.txt".to_string())
        );

        let legacy = TrashEntry {
            original_path: "legacy.txt".into(),
            trashed_at: 1,
            trash_key: "test/.tcfs-trash/1/legacy.txt".into(),
            index_content: String::new(),
            generation_state: TrashGenerationState::LegacyRecoverable,
        };
        assert!(trash_retention_anchor_key(
            &op,
            prefix,
            &legacy,
            b"legacy",
            TrashGenerationState::LegacyRecoverable,
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn retention_requires_storage_age_and_explicit_all_retains_evidence() {
        let op = memory_op();
        let prefix = "test";

        // Manually write a trash entry with timestamp 0 (very old)
        let old_key = "test/.tcfs-trash/0/old_file.txt";
        let evidence = b"manifest_hash=old\nsize=50\nchunks=1".to_vec();
        op.write(old_key, evidence.clone()).await.unwrap();
        seed_test_manifest(&op, prefix, "old_file.txt", "old", 50, 1).await;
        mark_test_trash_delete_complete(&op, prefix, old_key, &evidence).await;

        // The backend exposes no storage Last-Modified value, so the forged old
        // client timestamp cannot make this entry retention-eligible.
        let error = purge_old_trash(&op, prefix, 1)
            .await
            .expect_err("age purge must fail closed without a storage clock");
        assert!(format!("{error:#}").contains("omitted last-modified"));
        assert!(op.list("test/.tcfs-trash-clock/").await.unwrap().is_empty());

        // Explicit purge-all (zero) needs no age assertion and remains usable.
        let report = purge_old_trash(&op, prefix, 0).await.unwrap();
        assert_eq!(report.purged, 1);

        // Evidence remains durable but the purged marker hides it.
        assert!(op.read(old_key).await.is_ok());
        assert!(list_trash(&op, prefix).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_storage_clock_blocks_retry_without_leaking_another_key() {
        let op = memory_op();
        let prefix = "test";
        let trash_key = "test/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt";
        let evidence = b"manifest_hash=original\nsize=1\nchunks=1".to_vec();
        op.write(trash_key, evidence.clone()).await.unwrap();
        mark_test_trash_delete_complete(&op, prefix, trash_key, &evidence).await;
        let clock_key = "test/.tcfs-trash-clock/v1/clock";
        op.write(clock_key, b"stale-owned-clock".to_vec())
            .await
            .unwrap();

        let error = purge_old_trash(&op, prefix, 1)
            .await
            .expect_err("a stale fixed clock must block another age purge");
        assert!(format!("{error:#}").contains("different bytes"));
        assert_eq!(op.list("test/.tcfs-trash-clock/").await.unwrap().len(), 1);
        assert!(read_trash_lifecycle_claim(&op, prefix, trash_key)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn empty_trash_list() {
        let op = memory_op();
        let entries = list_trash(&op, "prefix").await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn empty_or_root_like_trash_prefix_is_rejected() {
        let op = memory_op();
        for prefix in ["", "/", "test/"] {
            assert!(
                list_trash(&op, prefix).await.is_err(),
                "unexpectedly accepted root-like trash prefix {prefix:?}"
            );
        }
    }

    // ── Path traversal tests ────────────────────────────────────────────

    #[test]
    fn validate_rel_path_normal() {
        assert!(validate_rel_path("doc.txt").is_ok());
        assert!(validate_rel_path("dir/file.txt").is_ok());
        assert!(validate_rel_path("a/b/c/deep.md").is_ok());
        assert!(validate_rel_path(".hidden").is_ok());
    }

    #[test]
    fn validate_rel_path_rejects_parent_dir() {
        assert!(validate_rel_path("../escape.txt").is_err());
        assert!(validate_rel_path("dir/../../etc/passwd").is_err());
        assert!(validate_rel_path("ok/../sneaky").is_err());
    }

    #[test]
    fn validate_rel_path_rejects_absolute() {
        assert!(validate_rel_path("/etc/passwd").is_err());
        assert!(validate_rel_path("/root/.ssh/id_rsa").is_err());
    }

    #[test]
    fn validate_rel_path_rejects_empty() {
        assert!(validate_rel_path("").is_err());
    }

    #[test]
    fn validate_rel_path_rejects_noncanonical_portable_aliases() {
        for path in [
            "dir\\file.txt",
            "dir//file.txt",
            "dir/./file.txt",
            "Cafe\u{301}.txt",
            "name.",
            "CON.txt",
            "bad\nname",
        ] {
            assert!(
                validate_rel_path(path).is_err(),
                "unexpectedly accepted noncanonical trash path {path:?}"
            );
        }
    }

    #[tokio::test]
    async fn trash_rejects_traversal() {
        let op = memory_op();
        let index_key = "test/index/doc.txt";
        op.write(index_key, b"content".to_vec()).await.unwrap();

        let result = trash_index_entry(&op, "test", index_key, "../escape").await;
        assert!(result.is_err());

        // Original should still exist (not deleted)
        assert!(op.read(index_key).await.is_ok());
    }
}
