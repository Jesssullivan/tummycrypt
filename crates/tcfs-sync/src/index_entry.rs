use anyhow::{bail, Context, Result};
use futures::TryStreamExt;
use opendal::{ErrorKind, Operator};
use serde::{Deserialize, Serialize};
use tcfs_core::config::RegisteredRootPlanContractV1;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

/// Canonical payload for an otherwise-empty directory's reserved index marker.
pub const DIRECTORY_MARKER_BYTES: &[u8] = b"type=directory\n";

/// Validate an identifier before it is interpolated as one storage-key
/// component.
///
/// Manifest and chunk identifiers come from remote metadata. Treating them as
/// opaque strings permits values such as `../other-root/object` to escape the
/// namespace selected by the caller on hierarchical backends. The current
/// fleet has historical non-hex fixtures, so this boundary deliberately checks
/// component safety rather than enforcing the eventual 64-hex object-id
/// contract.
pub(crate) fn validate_storage_key_component(value: &str, description: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.trim() != value
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        bail!("{description} must be one safe storage-key component: {value:?}");
    }
    Ok(())
}

/// Content address used for immutable manifest objects.
pub fn manifest_object_id(manifest_bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tcfs-sync-manifest-object-v1\0");
    hasher.update(manifest_bytes);
    tcfs_chunks::hash_to_hex(&hasher.finalize())
}

fn legacy_symlink_target_id(target: &str) -> String {
    let mut data = b"tcfs-symlink-v1\0".to_vec();
    data.extend_from_slice(target.as_bytes());
    tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data))
}

fn verify_pending_manifest_bytes(
    pending: &PendingIndexEntry,
    index_key: &str,
    manifest_prefix: &str,
    manifest_key: &str,
    bytes: &[u8],
) -> Result<bool> {
    let actual = manifest_object_id(bytes);
    let manifest_prefix = manifest_prefix.trim_end_matches('/');
    let root_prefix = manifest_prefix
        .strip_suffix("/manifests")
        .or_else(|| (manifest_prefix == "manifests").then_some(""))
        .context("manifest prefix must end at a manifests namespace")?;
    let index_prefix = if root_prefix.is_empty() {
        "index/".to_string()
    } else {
        format!("{root_prefix}/index/")
    };
    let rel_path = index_key
        .strip_prefix(&index_prefix)
        .filter(|rel_path| !rel_path.is_empty())
        .context("index key is outside its root index namespace")?;
    validate_canonical_rel_path(rel_path)?;

    let compatible_legacy_identity = match pending.kind {
        RemoteEntryKind::RegularFile => {
            let manifest = crate::manifest::SyncManifest::from_bytes(bytes)
                .context("parsing regular manifest during recovery")?;
            anyhow::ensure!(
                matches!(manifest.version, 2 | 3),
                "indexed regular manifest recovery requires JSON schema v2 or v3"
            );
            if manifest.version == 3 {
                anyhow::ensure!(
                    !manifest.wrapped_file_keys.is_empty(),
                    "indexed regular manifest v3 is missing per-device wrapped keys"
                );
            }
            validate_storage_key_component(
                &manifest.file_hash,
                "recovery regular manifest file_hash",
            )?;
            anyhow::ensure!(
                manifest.rel_path.as_deref() == Some(rel_path),
                "recovery manifest rel_path mismatch: expected {:?}, got {:?}",
                rel_path,
                manifest.rel_path
            );
            anyhow::ensure!(
                manifest.file_size == pending.size,
                "recovery manifest size mismatch: pending {}, manifest {}",
                pending.size,
                manifest.file_size
            );
            anyhow::ensure!(
                manifest.chunks.len() == pending.chunks,
                "recovery manifest chunk-count mismatch: pending {}, manifest {}",
                pending.chunks,
                manifest.chunks.len()
            );
            manifest.file_hash == pending.manifest_hash
        }
        RemoteEntryKind::Symlink => {
            let manifest = crate::manifest::SymlinkManifest::from_bytes(bytes)
                .context("parsing symlink manifest during recovery")?;
            let expected_target = pending
                .symlink_target
                .as_deref()
                .context("symlink recovery missing index target")?;
            anyhow::ensure!(
                manifest.symlink_target == expected_target,
                "recovery symlink target mismatch"
            );
            anyhow::ensure!(
                manifest.rel_path.as_deref() == Some(rel_path),
                "recovery manifest rel_path mismatch: expected {:?}, got {:?}",
                rel_path,
                manifest.rel_path
            );
            anyhow::ensure!(
                pending.size == manifest.symlink_target.len() as u64 && pending.chunks == 0,
                "recovery symlink metadata mismatch"
            );
            legacy_symlink_target_id(&manifest.symlink_target) == pending.manifest_hash
        }
    };

    anyhow::ensure!(
        actual == pending.manifest_hash || compatible_legacy_identity,
        "pending manifest content id mismatch at {manifest_key}: expected {}, got {actual}",
        pending.manifest_hash
    );
    Ok(actual == pending.manifest_hash)
}

pub(crate) fn validate_relative_storage_key(value: &str, description: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') {
        bail!("{description} must be a non-empty relative storage key: {value:?}");
    }
    for component in value.split('/') {
        validate_storage_key_component(component, description)?;
    }
    Ok(())
}

fn is_windows_reserved_device_name(component: &str) -> bool {
    let basename = component.split('.').next().unwrap_or(component);
    let basename = basename.to_ascii_uppercase();

    if matches!(
        basename.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }

    let Some(port) = basename
        .strip_prefix("COM")
        .or_else(|| basename.strip_prefix("LPT"))
    else {
        return false;
    };
    matches!(port, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        || matches!(port, "¹" | "²" | "³")
}

fn is_windows_dot_git_short_name(component: &str) -> bool {
    component.eq_ignore_ascii_case("git~1")
}

/// Validate a user-visible path before it becomes an index key or a local
/// hydration target.
///
/// The accepted spelling is deliberately portable: UTF-8 NFC, `/` separators,
/// no absolute/platform prefix, no empty or dot components, and canonical Git
/// metadata casing. Callers must reject rather than silently normalize remote
/// input so two serialized names can never acquire the same local meaning.
pub fn validate_canonical_rel_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        bail!("path must be a non-empty canonical relative path: {value:?}");
    }
    if value.nfc().collect::<String>() != value {
        bail!("path must use Unicode NFC spelling: {value:?}");
    }

    let mut inside_git = false;
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("path contains an unsafe empty/dot component: {value:?}");
        }
        if component.ends_with('.') || component.ends_with(' ') {
            bail!("path component has a Windows-aliased trailing dot or space: {value:?}");
        }
        if component
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            bail!("path component contains a character forbidden by Windows: {value:?}");
        }
        if is_windows_reserved_device_name(component) {
            bail!("path contains a reserved Windows device name: {value:?}");
        }
        if is_windows_dot_git_short_name(component) {
            bail!("path contains a Windows short-name alias of reserved .git: {value:?}");
        }
        if component != ".git" && component.eq_ignore_ascii_case(".git") {
            bail!("path contains a non-canonical alias of reserved .git: {value:?}");
        }
        if inside_git {
            const RESERVED_GIT_COMPONENTS: &[&str] = &[
                "refs",
                "objects",
                "modules",
                "worktrees",
                "tcfs-undo",
                "info",
                "pack",
                "hooks",
                "logs",
                "heads",
                "tags",
                "remotes",
                "HEAD",
                "FETCH_HEAD",
                "ORIG_HEAD",
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "BISECT_LOG",
                "packed-refs",
                "index",
                "config",
                "config.worktree",
                "commondir",
                "alternates",
                "http-alternates",
                "tcfs.lock",
                "index.lock",
                "HEAD.lock",
                "packed-refs.lock",
                "shallow",
                "shallow.lock",
                "multi-pack-index",
                "commit-graph",
            ];
            if let Some(canonical) = RESERVED_GIT_COMPONENTS
                .iter()
                .find(|canonical| component.eq_ignore_ascii_case(canonical))
            {
                if component != *canonical {
                    bail!(
                        "path contains a non-canonical alias of Git metadata {canonical:?}: {value:?}"
                    );
                }
            }
            if component.to_ascii_lowercase().ends_with(".lock") && !component.ends_with(".lock") {
                bail!("path contains a non-canonical Git lock suffix: {value:?}");
            }
        }
        if component == ".git" {
            inside_git = true;
        }
    }
    Ok(())
}

/// Portable, Unicode-aware key used only for namespace collision detection.
/// The original NFC spelling remains the user-visible path and index key.
pub fn portable_casefold_path(value: &str) -> Result<String> {
    validate_canonical_rel_path(value)?;
    Ok(value.case_fold().nfc().collect())
}

/// Logical role held by a portable namespace path.
///
/// Reservations are intentionally monotonic: once a spelling has been claimed
/// as a file or directory, deleting its index object does not release the
/// spelling for a case-only alias or a conflicting role.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableNamespaceRole {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PortableNamespaceReservationV1 {
    version: u8,
    exact_path: String,
    folded_path: String,
    role: PortableNamespaceRole,
}

impl PortableNamespaceReservationV1 {
    fn new(exact_path: String, role: PortableNamespaceRole) -> Result<Self> {
        validate_namespace_logical_path(&exact_path)?;
        let folded_path = portable_casefold_path(&exact_path)?;
        Ok(Self {
            version: 1,
            exact_path,
            folded_path,
            role,
        })
    }

    fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        let reservation: Self =
            serde_json::from_slice(bytes).context("parsing portable namespace reservation v1")?;
        anyhow::ensure!(
            reservation.version == 1,
            "unsupported portable namespace reservation version: {}",
            reservation.version
        );
        validate_namespace_logical_path(&reservation.exact_path)
            .context("invalid exact path in portable namespace reservation")?;
        let expected_folded = portable_casefold_path(&reservation.exact_path)?;
        anyhow::ensure!(
            reservation.folded_path == expected_folded,
            "portable namespace reservation folded path does not match its exact path"
        );
        Ok(reservation)
    }

    fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("serializing portable namespace reservation v1")
    }
}

pub(crate) fn validate_namespace_logical_path(rel_path: &str) -> Result<()> {
    validate_canonical_rel_path(rel_path)?;
    anyhow::ensure!(
        !rel_path
            .split('/')
            .any(|component| component.eq_ignore_ascii_case(".tcfs_dir")),
        "remote index path uses reserved TCFS directory-marker component: {rel_path:?}"
    );
    Ok(())
}

fn namespace_claims_for_path(
    rel_path: &str,
    leaf_role: PortableNamespaceRole,
) -> Result<Vec<PortableNamespaceReservationV1>> {
    validate_namespace_logical_path(rel_path)?;
    let components: Vec<&str> = rel_path.split('/').collect();
    let mut claims = Vec::with_capacity(components.len());
    for end in 1..=components.len() {
        let role = if end == components.len() {
            leaf_role
        } else {
            PortableNamespaceRole::Directory
        };
        claims.push(PortableNamespaceReservationV1::new(
            components[..end].join("/"),
            role,
        )?);
    }
    Ok(claims)
}

pub(crate) fn namespace_logical_entry_from_index_path(
    rel_path: &str,
) -> Result<(String, PortableNamespaceRole)> {
    if let Some(parent) = rel_path
        .strip_suffix("/.tcfs_dir")
        .filter(|parent| !parent.is_empty())
    {
        validate_namespace_logical_path(parent)?;
        return Ok((parent.to_owned(), PortableNamespaceRole::Directory));
    }

    validate_namespace_logical_path(rel_path)?;
    Ok((rel_path.to_owned(), PortableNamespaceRole::File))
}

fn namespace_reservation_object_id(folded_path: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tcfs-portable-namespace-reservation-v1\0");
    hasher.update(folded_path.as_bytes());
    tcfs_chunks::hash_to_hex(&hasher.finalize())
}

pub(crate) fn validate_remote_entry(entry: &RemoteIndexEntry, description: &str) -> Result<()> {
    validate_storage_key_component(
        &entry.manifest_hash,
        &format!("{description} manifest_hash"),
    )?;
    match (entry.kind, entry.symlink_target.as_ref()) {
        (RemoteEntryKind::RegularFile, None) => Ok(()),
        (RemoteEntryKind::Symlink, Some(target))
            if !target.is_empty() && !target.chars().any(char::is_control) =>
        {
            Ok(())
        }
        (RemoteEntryKind::Symlink, Some(_)) => {
            bail!("{description} symlink_target is empty or contains a control character")
        }
        (RemoteEntryKind::RegularFile, Some(_)) => {
            bail!("{description} regular-file entry must not carry symlink_target")
        }
        (RemoteEntryKind::Symlink, None) => {
            bail!("{description} symlink entry missing symlink_target")
        }
    }
}

fn validate_pending_entry(entry: &PendingIndexEntry, description: &str) -> Result<()> {
    validate_storage_key_component(
        &entry.manifest_hash,
        &format!("{description} manifest_hash"),
    )?;
    validate_relative_storage_key(
        &entry.staged_manifest_key,
        &format!("{description} staged_manifest_key"),
    )?;
    match (entry.kind, entry.symlink_target.as_ref()) {
        (RemoteEntryKind::RegularFile, None) => Ok(()),
        (RemoteEntryKind::Symlink, Some(target))
            if !target.is_empty() && !target.chars().any(char::is_control) =>
        {
            Ok(())
        }
        (RemoteEntryKind::Symlink, Some(_)) => {
            bail!("{description} symlink_target is empty or contains a control character")
        }
        (RemoteEntryKind::RegularFile, Some(_)) => {
            bail!("{description} regular-file entry must not carry symlink_target")
        }
        (RemoteEntryKind::Symlink, None) => {
            bail!("{description} symlink entry missing symlink_target")
        }
    }
}

/// Constrain a crash-recovery staging pointer to the same root and manifest id
/// as the index entry being recovered.
///
/// `PendingIndexEntry::staged_manifest_key` is serialized remote input. Without
/// this check, resolving or deleting an entry under root A can read and delete
/// an arbitrary object under root B through the daemon's global operator.
pub fn validate_staged_manifest_key(
    manifest_prefix: &str,
    pending: &PendingIndexEntry,
) -> Result<()> {
    validate_pending_entry(pending, "pending index entry")?;

    let manifest_prefix = manifest_prefix.trim_end_matches('/');
    let root_prefix = manifest_prefix
        .strip_suffix("/manifests")
        .or_else(|| (manifest_prefix == "manifests").then_some(""))
        .with_context(|| {
            format!("manifest prefix must end at a manifests namespace: {manifest_prefix:?}")
        })?;
    let staged_prefix = if root_prefix.is_empty() {
        "staging/manifests/".to_string()
    } else {
        format!("{root_prefix}/staging/manifests/")
    };
    let staged_name = pending
        .staged_manifest_key
        .strip_prefix(&staged_prefix)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "pending staged manifest key escapes its root staging namespace: {:?}",
                pending.staged_manifest_key
            )
        })?;
    validate_storage_key_component(staged_name, "pending staged manifest filename")?;

    let expected_suffix = format!("-{}.json", pending.manifest_hash);
    let transaction = staged_name
        .strip_suffix(&expected_suffix)
        .filter(|transaction| !transaction.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "pending staged manifest key is not bound to manifest_hash {:?}: {:?}",
                pending.manifest_hash,
                pending.staged_manifest_key
            )
        })?;
    validate_storage_key_component(transaction, "pending staged manifest transaction id")?;
    let transaction_id = uuid::Uuid::parse_str(transaction)
        .context("pending staged manifest transaction id must be a UUID")?;
    if transaction_id.hyphenated().to_string() != transaction {
        bail!(
            "pending staged manifest transaction id must use canonical hyphenated UUID spelling: {transaction:?}"
        );
    }
    Ok(())
}

/// Kind of object published at a path in the remote index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteEntryKind {
    #[default]
    RegularFile,
    Symlink,
}

impl RemoteEntryKind {
    fn is_regular_file(kind: &Self) -> bool {
        *kind == RemoteEntryKind::RegularFile
    }
}

/// A parsed remote index entry that points to a committed manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIndexEntry {
    pub manifest_hash: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub chunks: usize,
    #[serde(default, skip_serializing_if = "RemoteEntryKind::is_regular_file")]
    pub kind: RemoteEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
}

impl RemoteIndexEntry {
    pub fn new(manifest_hash: impl Into<String>, size: u64, chunks: usize) -> Self {
        Self {
            manifest_hash: manifest_hash.into(),
            size,
            chunks,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        }
    }

    pub fn new_symlink(manifest_hash: impl Into<String>, target: impl Into<String>) -> Self {
        let target = target.into();
        Self {
            manifest_hash: manifest_hash.into(),
            size: target.len() as u64,
            chunks: 0,
            kind: RemoteEntryKind::Symlink,
            symlink_target: Some(target),
        }
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == RemoteEntryKind::Symlink
    }

    pub fn to_legacy_bytes(&self) -> Vec<u8> {
        format!(
            "manifest_hash={}\nsize={}\nchunks={}\n",
            self.manifest_hash, self.size, self.chunks
        )
        .into_bytes()
    }
}

/// State for a versioned index entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexEntryState {
    Committed,
    Preparing,
    /// Logical deletion marker written with compare-and-swap.
    Deleted,
}

/// Logical state of one exact path-index key.
///
/// `Missing` is deliberately distinct from `Deleted`: only a durable v4
/// tombstone is remote deletion authority. A missing object can be a stale
/// LIST result, an interrupted legacy mutation, or lost/corrupt storage and
/// must never authorize removal of a local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactIndexPathState {
    Missing,
    Deleted,
    Live,
}

/// Pending manifest metadata recorded while a path publish is in-flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingIndexEntry {
    pub manifest_hash: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub chunks: usize,
    #[serde(default, skip_serializing_if = "RemoteEntryKind::is_regular_file")]
    pub kind: RemoteEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    pub staged_manifest_key: String,
}

impl PendingIndexEntry {
    pub fn new(
        manifest_hash: impl Into<String>,
        size: u64,
        chunks: usize,
        staged_manifest_key: impl Into<String>,
    ) -> Self {
        Self {
            manifest_hash: manifest_hash.into(),
            size,
            chunks,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
            staged_manifest_key: staged_manifest_key.into(),
        }
    }

    pub fn from_remote_entry(
        entry: &RemoteIndexEntry,
        staged_manifest_key: impl Into<String>,
    ) -> Self {
        Self {
            manifest_hash: entry.manifest_hash.clone(),
            size: entry.size,
            chunks: entry.chunks,
            kind: entry.kind,
            symlink_target: entry.symlink_target.clone(),
            staged_manifest_key: staged_manifest_key.into(),
        }
    }

    pub fn as_remote_entry(&self) -> RemoteIndexEntry {
        RemoteIndexEntry {
            manifest_hash: self.manifest_hash.clone(),
            size: self.size,
            chunks: self.chunks,
            kind: self.kind,
            symlink_target: self.symlink_target.clone(),
        }
    }
}

/// Fully parsed index entry, supporting both the legacy text format and the
/// planned versioned JSON format for durability work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedIndexEntry {
    Legacy(RemoteIndexEntry),
    V2(VersionedIndexEntry),
}

/// Immutable recovery evidence bound into a v4 deletion tombstone.
///
/// Ordinary tombstones may omit this field. Trash-backed deletion includes it
/// so a caller can prove which exact safety copy reached the delete
/// linearization point even if the later completion-marker response is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionEvidence {
    pub safety_copy_key: String,
    pub safety_copy_blake3: String,
}

impl DeletionEvidence {
    pub fn for_trash_generation(
        remote_prefix: &str,
        rel_path: &str,
        safety_copy_key: &str,
        safety_copy_bytes: &[u8],
    ) -> Result<Self> {
        let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
        validate_canonical_rel_path(rel_path)?;
        let trash_prefix = if prefix.is_empty() {
            ".tcfs-trash/".to_string()
        } else {
            format!("{prefix}/.tcfs-trash/")
        };
        let remainder = safety_copy_key
            .strip_prefix(&trash_prefix)
            .with_context(|| {
                format!(
                    "trash safety-copy key is outside canonical prefix {trash_prefix:?}: {safety_copy_key:?}"
                )
            })?;
        let (generation, evidence_rel_path) = remainder
            .split_once('/')
            .context("trash safety-copy key is missing its generation/path separator")?;
        validate_storage_key_component(generation, "trash safety-copy generation")?;
        anyhow::ensure!(
            evidence_rel_path == rel_path,
            "trash safety-copy path does not match deletion path: expected {rel_path:?}, got {evidence_rel_path:?}"
        );

        Ok(Self {
            safety_copy_key: safety_copy_key.to_string(),
            safety_copy_blake3: blake3::hash(safety_copy_bytes).to_hex().to_string(),
        })
    }

    pub fn matches_trash_generation(
        &self,
        remote_prefix: &str,
        rel_path: &str,
        safety_copy_key: &str,
        safety_copy_bytes: &[u8],
    ) -> Result<bool> {
        Ok(self
            == &Self::for_trash_generation(
                remote_prefix,
                rel_path,
                safety_copy_key,
                safety_copy_bytes,
            )?)
    }
}

fn validate_deletion_evidence(evidence: &DeletionEvidence) -> Result<()> {
    validate_relative_storage_key(&evidence.safety_copy_key, "deletion safety-copy key")?;
    anyhow::ensure!(
        evidence.safety_copy_blake3.len() == 64
            && evidence
                .safety_copy_blake3
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "deletion safety-copy digest must be 64 lowercase hexadecimal characters"
    );
    Ok(())
}

/// Versioned JSON index entry used by the #224 durability design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedIndexEntry {
    pub state: IndexEntryState,
    pub current: Option<RemoteIndexEntry>,
    pub pending: Option<PendingIndexEntry>,
    pub deletion_evidence: Option<DeletionEvidence>,
}

impl VersionedIndexEntry {
    pub fn committed(current: RemoteIndexEntry) -> Self {
        Self {
            state: IndexEntryState::Committed,
            current: Some(current),
            pending: None,
            deletion_evidence: None,
        }
    }

    pub fn preparing(current: Option<RemoteIndexEntry>, pending: PendingIndexEntry) -> Self {
        Self {
            state: IndexEntryState::Preparing,
            current,
            pending: Some(pending),
            deletion_evidence: None,
        }
    }

    pub fn deleted() -> Self {
        Self {
            state: IndexEntryState::Deleted,
            current: None,
            pending: None,
            deletion_evidence: None,
        }
    }

    pub fn deleted_with_evidence(deletion_evidence: DeletionEvidence) -> Self {
        Self {
            state: IndexEntryState::Deleted,
            current: None,
            pending: None,
            deletion_evidence: Some(deletion_evidence),
        }
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        if let Some(entry) = &self.current {
            validate_remote_entry(entry, "current index entry")?;
        }
        if let Some(entry) = &self.pending {
            validate_pending_entry(entry, "pending index entry")?;
        }
        if let Some(evidence) = &self.deletion_evidence {
            validate_deletion_evidence(evidence)?;
        }
        match self.state {
            IndexEntryState::Committed => anyhow::ensure!(
                self.current.is_some()
                    && self.pending.is_none()
                    && self.deletion_evidence.is_none(),
                "committed index entry requires current and forbids pending/deletion evidence"
            ),
            IndexEntryState::Preparing => anyhow::ensure!(
                self.pending.is_some() && self.deletion_evidence.is_none(),
                "preparing index entry requires pending and forbids deletion evidence"
            ),
            IndexEntryState::Deleted => anyhow::ensure!(
                self.current.is_none() && self.pending.is_none(),
                "deleted index entry must not retain current or pending manifests"
            ),
        }
        serde_json::to_vec_pretty(&VersionedIndexEntryWire {
            version: self.wire_version(),
            state: self.state,
            current: self.current.clone(),
            pending: self.pending.clone(),
            deletion_evidence: self.deletion_evidence.clone(),
        })
        .context("serializing versioned index entry")
    }

    fn wire_version(&self) -> u8 {
        if self.state == IndexEntryState::Deleted {
            return 4;
        }
        let current_regular = self
            .current
            .as_ref()
            .map(|entry| entry.kind == RemoteEntryKind::RegularFile)
            .unwrap_or(true);
        let pending_regular = self
            .pending
            .as_ref()
            .map(|entry| entry.kind == RemoteEntryKind::RegularFile)
            .unwrap_or(true);

        if current_regular && pending_regular {
            2
        } else {
            3
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VersionedIndexEntryWire {
    version: u8,
    state: IndexEntryState,
    #[serde(default)]
    current: Option<RemoteIndexEntry>,
    #[serde(default)]
    pending: Option<PendingIndexEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deletion_evidence: Option<DeletionEvidence>,
}

impl ParsedIndexEntry {
    pub fn state(&self) -> IndexEntryState {
        match self {
            ParsedIndexEntry::Legacy(_) => IndexEntryState::Committed,
            ParsedIndexEntry::V2(entry) => entry.state,
        }
    }

    /// Return the currently visible manifest pointer for path-based reads.
    ///
    /// For legacy entries this is the only entry. For versioned entries this is
    /// the committed/current pointer, which may be absent for a brand-new path
    /// that is still in a `preparing` state.
    pub fn visible_entry(&self) -> Option<&RemoteIndexEntry> {
        match self {
            ParsedIndexEntry::Legacy(entry) => Some(entry),
            ParsedIndexEntry::V2(entry) => entry.current.as_ref(),
        }
    }

    pub fn pending_entry(&self) -> Option<&PendingIndexEntry> {
        match self {
            ParsedIndexEntry::Legacy(_) => None,
            ParsedIndexEntry::V2(entry) => entry.pending.as_ref(),
        }
    }

    pub fn deletion_evidence(&self) -> Option<&DeletionEvidence> {
        match self {
            ParsedIndexEntry::Legacy(_) => None,
            ParsedIndexEntry::V2(entry) => entry.deletion_evidence.as_ref(),
        }
    }
}

/// Parse the current visible remote index entry for callers that only support
/// committed path pointers today.
pub fn parse_index_entry(data: &[u8]) -> Result<RemoteIndexEntry> {
    parse_index_entry_record(data)?
        .visible_entry()
        .cloned()
        .context("index entry has no visible current manifest")
}

/// Parse a remote index entry from either the legacy text format or the
/// versioned JSON format planned for crash-safe publish.
pub fn parse_index_entry_record(data: &[u8]) -> Result<ParsedIndexEntry> {
    let text = std::str::from_utf8(data).context("index entry is not valid UTF-8")?;
    let trimmed = text.trim_start();

    if trimmed.starts_with('{') {
        return parse_versioned_index_entry(trimmed);
    }

    parse_legacy_index_entry(trimmed).map(ParsedIndexEntry::Legacy)
}

fn parse_versioned_index_entry(text: &str) -> Result<ParsedIndexEntry> {
    let wire: VersionedIndexEntryWire =
        serde_json::from_str(text).context("parsing versioned index entry JSON")?;

    if !matches!(wire.version, 2..=4) {
        bail!("unsupported index entry version: {}", wire.version);
    }

    anyhow::ensure!(
        (wire.version == 4) == (wire.state == IndexEntryState::Deleted),
        "wire version 4 is reserved for deleted index entries"
    );

    if wire.version < 3 {
        let current_is_regular = wire
            .current
            .as_ref()
            .map(|entry| entry.kind == RemoteEntryKind::RegularFile)
            .unwrap_or(true);
        let pending_is_regular = wire
            .pending
            .as_ref()
            .map(|entry| entry.kind == RemoteEntryKind::RegularFile)
            .unwrap_or(true);
        if !current_is_regular || !pending_is_regular {
            bail!("non-regular index entry requires index version 3");
        }
    }

    for entry in wire.current.iter() {
        validate_remote_entry(entry, "current index entry")?;
        if entry.kind == RemoteEntryKind::Symlink && entry.symlink_target.is_none() {
            bail!("symlink index entry missing symlink_target");
        }
    }
    for entry in wire.pending.iter() {
        validate_pending_entry(entry, "pending index entry")?;
        if entry.kind == RemoteEntryKind::Symlink && entry.symlink_target.is_none() {
            bail!("symlink index entry missing symlink_target");
        }
    }
    if let Some(evidence) = &wire.deletion_evidence {
        validate_deletion_evidence(evidence)?;
    }

    match wire.state {
        IndexEntryState::Committed => {
            if wire.current.is_none() || wire.pending.is_some() || wire.deletion_evidence.is_some()
            {
                bail!(
                    "committed index entry requires current and forbids pending/deletion evidence"
                );
            }
        }
        IndexEntryState::Preparing => {
            if wire.pending.is_none() || wire.deletion_evidence.is_some() {
                bail!("preparing index entry requires pending and forbids deletion evidence");
            }
        }
        IndexEntryState::Deleted => {
            if wire.current.is_some() || wire.pending.is_some() {
                bail!("deleted index entry must not retain current or pending manifests");
            }
        }
    }

    Ok(ParsedIndexEntry::V2(VersionedIndexEntry {
        state: wire.state,
        current: wire.current,
        pending: wire.pending,
        deletion_evidence: wire.deletion_evidence,
    }))
}

pub fn manifest_key(manifest_prefix: &str, manifest_hash: &str) -> String {
    format!(
        "{}/{}",
        manifest_prefix.trim_end_matches('/'),
        manifest_hash
    )
}

pub async fn read_index_entry_record_from_store(
    op: &Operator,
    index_key: &str,
) -> Result<Option<ParsedIndexEntry>> {
    match op.read(index_key).await {
        Ok(bytes) => parse_index_entry_record(&bytes.to_vec()).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(anyhow::anyhow!(e)).with_context(|| format!("reading index entry: {index_key}"))
        }
    }
}

/// Read the logical state of one exact canonical path-index key.
///
/// Callers that may delete local data must require `Deleted`. `Missing` is not
/// a deletion signal, while committed and preparing records are both `Live`
/// because either can retain current or in-flight publication authority.
pub async fn read_exact_index_path_state(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
) -> Result<ExactIndexPathState> {
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_namespace_logical_path(rel_path)?;
    let index_key = if prefix.is_empty() {
        format!("index/{rel_path}")
    } else {
        format!("{prefix}/index/{rel_path}")
    };

    match read_index_entry_record_from_store(op, &index_key).await? {
        None => Ok(ExactIndexPathState::Missing),
        Some(record) if record.state() == IndexEntryState::Deleted => {
            Ok(ExactIndexPathState::Deleted)
        }
        Some(_) => Ok(ExactIndexPathState::Live),
    }
}

pub async fn write_committed_index_entry(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    entry: &RemoteIndexEntry,
) -> Result<()> {
    let bytes = VersionedIndexEntry::committed(entry.clone()).to_json_bytes()?;
    let (_, rel_path) = validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    admit_portable_namespace_entry(op, remote_prefix, rel_path, PortableNamespaceRole::File)
        .await?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    let guard = require_absent_index_entry_for_update(op, remote_prefix, index_key).await?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        bytes,
        guard,
        "creating committed entry",
    )
    .await
    .map(|_| ())
}

pub async fn write_preparing_index_entry(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    current: Option<RemoteIndexEntry>,
    pending: PendingIndexEntry,
) -> Result<()> {
    let bytes = VersionedIndexEntry::preparing(current, pending).to_json_bytes()?;
    let (_, rel_path) = validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    admit_portable_namespace_entry(op, remote_prefix, rel_path, PortableNamespaceRole::File)
        .await?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    let guard = require_absent_index_entry_for_update(op, remote_prefix, index_key).await?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        bytes,
        guard,
        "creating preparing entry",
    )
    .await
    .map(|_| ())
}

async fn tombstone_bound_index_snapshot(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    snapshot: IndexEntrySnapshot,
    deletion_evidence: Option<DeletionEvidence>,
) -> Result<ParsedIndexEntry> {
    let previous = snapshot.parsed.clone();
    let guard = write_guard_for_snapshot(op, index_key, &snapshot)?;
    let bytes = deletion_evidence.map_or_else(
        || VersionedIndexEntry::deleted().to_json_bytes(),
        |evidence| VersionedIndexEntry::deleted_with_evidence(evidence).to_json_bytes(),
    )?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        bytes,
        guard,
        "logically deleting index entry",
    )
    .await?;
    Ok(previous)
}

/// Atomically replace the current index object with a logical deletion marker.
///
/// The exact object observed by this function is protected with ETag If-Match
/// (or explicitly registered process-local Memory emulation in tests), so a
/// concurrent publisher is never removed by a proof-then-delete race.
pub async fn tombstone_index_entry(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
) -> Result<ParsedIndexEntry> {
    validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    let snapshot = read_index_entry_snapshot_from_store(op, index_key)
        .await?
        .with_context(|| format!("missing index entry: {index_key}"))?;
    tombstone_bound_index_snapshot(op, remote_prefix, index_key, snapshot, None).await
}

/// Atomically tombstone an index object only when its complete serialized
/// bytes still match the caller's safety copy.
pub async fn tombstone_index_entry_if_exact(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    expected_bytes: &[u8],
) -> Result<ParsedIndexEntry> {
    validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    parse_index_entry_record(expected_bytes).context("validating expected index snapshot")?;
    let snapshot = read_index_entry_snapshot_from_store(op, index_key)
        .await?
        .with_context(|| format!("missing index entry: {index_key}"))?;
    anyhow::ensure!(
        snapshot.raw_bytes == expected_bytes,
        "index entry changed before logical delete; preserving the concurrent value: {index_key}"
    );
    tombstone_bound_index_snapshot(op, remote_prefix, index_key, snapshot, None).await
}

/// Atomically tombstone an exact index snapshot while binding the deletion to
/// one immutable trash safety copy.
pub async fn tombstone_index_entry_if_exact_with_evidence(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    expected_bytes: &[u8],
    deletion_evidence: DeletionEvidence,
) -> Result<ParsedIndexEntry> {
    validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    parse_index_entry_record(expected_bytes).context("validating expected index snapshot")?;
    validate_deletion_evidence(&deletion_evidence)?;
    let snapshot = read_index_entry_snapshot_from_store(op, index_key)
        .await?
        .with_context(|| format!("missing index entry: {index_key}"))?;
    anyhow::ensure!(
        snapshot.raw_bytes == expected_bytes,
        "index entry changed before logical delete; preserving the concurrent value: {index_key}"
    );
    tombstone_bound_index_snapshot(
        op,
        remote_prefix,
        index_key,
        snapshot,
        Some(deletion_evidence),
    )
    .await
}

/// Report whether an exact reserved directory-marker key is logically visible.
pub async fn directory_marker_is_visible(op: &Operator, marker_key: &str) -> Result<bool> {
    let bytes = match op.read(marker_key).await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::Error::new(error))
                .with_context(|| format!("reading directory marker: {marker_key}"))
        }
    };
    if bytes == DIRECTORY_MARKER_BYTES {
        return Ok(true);
    }
    let parsed = parse_index_entry_record(&bytes)
        .with_context(|| format!("parsing directory marker state: {marker_key}"))?;
    anyhow::ensure!(
        parsed.state() == IndexEntryState::Deleted
            && parsed.visible_entry().is_none()
            && parsed.pending_entry().is_none(),
        "reserved directory marker has an invalid live index payload: {marker_key}"
    );
    Ok(false)
}

/// Atomically hide an exact legacy directory marker with a v4 tombstone.
pub async fn tombstone_directory_marker_if_exact(
    op: &Operator,
    remote_prefix: &str,
    marker_key: &str,
    expected_bytes: &[u8],
) -> Result<()> {
    let (_, rel_path) = validate_index_key_for_remote_prefix(remote_prefix, marker_key)?;
    anyhow::ensure!(
        rel_path.ends_with("/.tcfs_dir") && expected_bytes == DIRECTORY_MARKER_BYTES,
        "directory tombstone requires the exact canonical marker payload: {marker_key}"
    );
    ensure_index_write_semantics(op, remote_prefix, marker_key).await?;
    let snapshot = read_raw_index_snapshot_from_store(op, marker_key)
        .await?
        .with_context(|| format!("missing directory marker: {marker_key}"))?;
    anyhow::ensure!(
        snapshot.raw_bytes == expected_bytes,
        "directory marker changed before logical delete; preserving the concurrent value: {marker_key}"
    );
    let guard = write_guard_for_raw_snapshot(op, marker_key, &snapshot)?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        marker_key,
        VersionedIndexEntry::deleted().to_json_bytes()?,
        guard,
        "logically deleting directory marker",
    )
    .await
    .map(|_| ())
}

/// Atomically hide an exact directory marker while binding its deletion to one
/// immutable trash safety copy.
pub async fn tombstone_directory_marker_if_exact_with_evidence(
    op: &Operator,
    remote_prefix: &str,
    marker_key: &str,
    expected_bytes: &[u8],
    deletion_evidence: DeletionEvidence,
) -> Result<()> {
    let (_, rel_path) = validate_index_key_for_remote_prefix(remote_prefix, marker_key)?;
    anyhow::ensure!(
        rel_path.ends_with("/.tcfs_dir") && expected_bytes == DIRECTORY_MARKER_BYTES,
        "directory tombstone requires the exact canonical marker payload: {marker_key}"
    );
    validate_deletion_evidence(&deletion_evidence)?;
    ensure_index_write_semantics(op, remote_prefix, marker_key).await?;
    let snapshot = read_raw_index_snapshot_from_store(op, marker_key)
        .await?
        .with_context(|| format!("missing directory marker: {marker_key}"))?;
    anyhow::ensure!(
        snapshot.raw_bytes == expected_bytes,
        "directory marker changed before logical delete; preserving the concurrent value: {marker_key}"
    );
    let guard = write_guard_for_raw_snapshot(op, marker_key, &snapshot)?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        marker_key,
        VersionedIndexEntry::deleted_with_evidence(deletion_evidence).to_json_bytes()?,
        guard,
        "logically deleting directory marker",
    )
    .await
    .map(|_| ())
}

/// Restore a complete index record from an immutable safety copy.
///
/// Only an absent key, a logical deletion marker, or the exact same bytes may
/// be replaced. A live different entry is preserved. Even the idempotent case
/// performs a conditional same-byte write so success is bound to an unchanged
/// destination rather than a stale proof.
pub async fn restore_index_entry_from_safety_copy(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    restored_bytes: &[u8],
) -> Result<()> {
    let (_, rel_path) = validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    if rel_path.ends_with("/.tcfs_dir") {
        anyhow::ensure!(
            restored_bytes == DIRECTORY_MARKER_BYTES,
            "restored directory marker has noncanonical bytes: {index_key}"
        );
    } else {
        parse_index_entry_record(restored_bytes).context("validating restored index record")?;
    }

    let guard = match read_raw_index_snapshot_from_store(op, index_key).await? {
        None => {
            ensure_atomic_absent_create_supported(op, index_key)?;
            IndexEntryWriteGuard::Absent
        }
        Some(snapshot) if snapshot.raw_bytes == restored_bytes => {
            write_guard_for_raw_snapshot(op, index_key, &snapshot)?
        }
        Some(snapshot) => {
            let parsed = parse_index_entry_record(&snapshot.raw_bytes)
                .with_context(|| format!("validating existing restore destination: {index_key}"))?;
            if parsed.state() != IndexEntryState::Deleted {
                bail!(
                    "restore destination contains a different live index entry; preserving both values: {index_key}"
                );
            }
            write_guard_for_raw_snapshot(op, index_key, &snapshot)?
        }
    };

    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        restored_bytes.to_vec(),
        guard,
        "restoring index entry from safety copy",
    )
    .await
    .map(|_| ())
}

/// Publish the canonical raw directory marker over an absent key, an exact
/// existing marker, or a durable v4 deletion tombstone. All other values are
/// preserved as conflicting live/corrupt state.
pub(crate) async fn write_directory_marker_conditionally(
    op: &Operator,
    remote_prefix: &str,
    marker_key: &str,
) -> Result<()> {
    restore_index_entry_from_safety_copy(op, remote_prefix, marker_key, DIRECTORY_MARKER_BYTES)
        .await
        .with_context(|| format!("publishing canonical directory marker: {marker_key}"))
}

struct RawIndexEntrySnapshot {
    raw_bytes: Vec<u8>,
    /// ETag bound to `raw_bytes` by a conditional read when supported.
    cas_etag: Option<String>,
}

struct IndexEntrySnapshot {
    parsed: ParsedIndexEntry,
    raw_bytes: Vec<u8>,
    /// ETag that was bound to this snapshot by a conditional read. `None` means
    /// this backend cannot safely compare-and-swap a recovered entry.
    cas_etag: Option<String>,
}

/// Storage-native identity used to bind one observational raw-object read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RawObjectReadBindingV1 {
    Version {
        version: String,
        etag: Option<String>,
    },
    Etag {
        etag: String,
    },
}

impl RawObjectReadBindingV1 {
    pub(crate) fn version(&self) -> Option<&str> {
        match self {
            Self::Version { version, .. } => Some(version),
            Self::Etag { .. } => None,
        }
    }

    pub(crate) fn etag(&self) -> Option<&str> {
        match self {
            Self::Version { etag, .. } => etag.as_deref(),
            Self::Etag { etag } => Some(etag),
        }
    }
}

/// Result of proving whether one exact storage object can be read by identity.
///
/// `Unbound` deliberately carries no bytes: a backend without version- or
/// ETag-bound reads cannot contribute object content to a complete plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RawObjectReadV1 {
    Bound(RawObjectSnapshotV1),
    Unbound,
}

/// Exact bytes and identity from one read-only storage-object observation.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RawObjectSnapshotV1 {
    raw_bytes: Vec<u8>,
    raw_blake3: blake3::Hash,
    binding: RawObjectReadBindingV1,
}

impl std::fmt::Debug for RawObjectSnapshotV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawObjectSnapshotV1")
            .field("raw_bytes_len", &self.raw_bytes.len())
            .field("raw_blake3", &self.raw_blake3)
            .field("binding", &self.binding)
            .finish()
    }
}

impl RawObjectSnapshotV1 {
    pub(crate) fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }

    pub(crate) const fn binding(&self) -> &RawObjectReadBindingV1 {
        &self.binding
    }

    pub(crate) fn into_parts(self) -> (Vec<u8>, blake3::Hash, RawObjectReadBindingV1) {
        (self.raw_bytes, self.raw_blake3, self.binding)
    }
}

/// Exact index-object identity carried from conflict resolution into publish.
/// Production variants are backed by storage-native conditional writes; the
/// in-memory variant exists only so unit tests can exercise the state machine.
#[derive(Debug)]
pub(crate) enum IndexEntryWriteGuard {
    Absent,
    Present { etag: String },
    TestMemoryPresent { raw_bytes: Vec<u8> },
}

type WeakAccessor = std::sync::Weak<dyn opendal::raw::AccessDyn>;

fn memory_index_emulation_registry() -> &'static std::sync::Mutex<Vec<WeakAccessor>> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<Vec<WeakAccessor>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Register one exact OpenDAL Memory accessor for process-local conditional
/// write emulation in tests. Production accessors are never admitted by scheme,
/// and a separately constructed Memory accessor is not implicitly trusted.
#[doc(hidden)]
pub fn register_memory_index_emulation_for_tests(op: &Operator) -> Result<()> {
    anyhow::ensure!(
        op.info().scheme() == "memory",
        "conditional-write test emulation is restricted to OpenDAL Memory accessors"
    );
    tcfs_storage::register_memory_conditional_write_emulation_for_tests(op)?;
    let accessor = std::sync::Arc::downgrade(op.inner());
    let mut registered = memory_index_emulation_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registered.retain(|candidate| candidate.strong_count() > 0);
    if !registered
        .iter()
        .any(|candidate| candidate.ptr_eq(&accessor))
    {
        registered.push(accessor);
    }
    Ok(())
}

fn memory_index_emulation_is_registered(op: &Operator) -> bool {
    if op.info().scheme() != "memory" {
        return false;
    }
    let accessor = std::sync::Arc::downgrade(op.inner());
    let mut registered = memory_index_emulation_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registered.retain(|candidate| candidate.strong_count() > 0);
    registered
        .iter()
        .any(|candidate| candidate.ptr_eq(&accessor))
}

fn validate_index_key_for_remote_prefix<'prefix, 'key>(
    remote_prefix: &'prefix str,
    index_key: &'key str,
) -> Result<(&'prefix str, &'key str)> {
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    let index_prefix = if prefix.is_empty() {
        "index/".to_string()
    } else {
        format!("{prefix}/index/")
    };
    let rel_path = index_key
        .strip_prefix(&index_prefix)
        .filter(|rel_path| !rel_path.is_empty())
        .with_context(|| {
            format!("index write key is outside canonical prefix {index_prefix:?}: {index_key:?}")
        })?;
    validate_canonical_rel_path(rel_path)?;
    Ok((prefix, rel_path))
}

fn validate_manifest_key_for_remote_prefix<'a>(
    remote_prefix: &'a str,
    manifest_key: &str,
) -> Result<&'a str> {
    let prefix = remote_prefix.trim_end_matches('/');
    if !prefix.is_empty() {
        validate_relative_storage_key(prefix, "manifest write remote prefix")?;
    }
    let manifest_prefix = if prefix.is_empty() {
        "manifests/".to_string()
    } else {
        format!("{prefix}/manifests/")
    };
    let object_id = manifest_key
        .strip_prefix(&manifest_prefix)
        .filter(|object_id| !object_id.is_empty())
        .with_context(|| {
            format!(
                "manifest write key is outside canonical prefix {manifest_prefix:?}: {manifest_key:?}"
            )
        })?;
    validate_storage_key_component(object_id, "manifest write object id")?;
    Ok(prefix)
}

fn remote_prefix_from_manifest_prefix(manifest_prefix: &str) -> Result<&str> {
    let manifest_prefix = manifest_prefix.trim_end_matches('/');
    let prefix = manifest_prefix
        .strip_suffix("/manifests")
        .or_else(|| (manifest_prefix == "manifests").then_some(""))
        .context("manifest prefix must end at a canonical manifests namespace")?;
    if !prefix.is_empty() {
        validate_relative_storage_key(prefix, "recovery remote prefix")?;
    }
    Ok(prefix)
}

async fn ensure_index_write_semantics(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
) -> Result<()> {
    let (prefix, _) = validate_index_key_for_remote_prefix(remote_prefix, index_key)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .with_context(|| {
            format!("verifying conditional-write semantics for index publication: {index_key}")
        })
}

/// Visible value and exact storage identity observed as one publish baseline.
#[derive(Debug)]
pub(crate) struct ResolvedIndexEntryForUpdate {
    current: Option<RemoteIndexEntry>,
    guard: IndexEntryWriteGuard,
}

impl ResolvedIndexEntryForUpdate {
    pub(crate) fn current(&self) -> Option<&RemoteIndexEntry> {
        self.current.as_ref()
    }

    pub(crate) fn into_parts(self) -> (Option<RemoteIndexEntry>, IndexEntryWriteGuard) {
        (self.current, self.guard)
    }
}

fn ensure_atomic_absent_create_supported(op: &Operator, index_key: &str) -> Result<()> {
    let capability = op.info().full_capability();
    if capability.write_with_if_not_exists
        && capability.read_with_if_match
        && capability.write_with_if_match
    {
        return Ok(());
    }

    // OpenDAL's Memory backend exposes no conditional-write capability. Tests
    // may opt one exact accessor into process-local compare/write emulation.
    if memory_index_emulation_is_registered(op) {
        return Ok(());
    }

    bail!(
        "index publish requires atomic absent-object creation (If-None-Match *) plus ETag If-Match commit; refusing unsafe publish for {index_key}"
    )
}

fn write_guard_for_snapshot(
    _op: &Operator,
    index_key: &str,
    snapshot: &IndexEntrySnapshot,
) -> Result<IndexEntryWriteGuard> {
    write_guard_for_raw_snapshot(
        _op,
        index_key,
        &RawIndexEntrySnapshot {
            raw_bytes: snapshot.raw_bytes.clone(),
            cas_etag: snapshot.cas_etag.clone(),
        },
    )
}

fn write_guard_for_raw_snapshot(
    op: &Operator,
    index_key: &str,
    snapshot: &RawIndexEntrySnapshot,
) -> Result<IndexEntryWriteGuard> {
    if let Some(etag) = snapshot.cas_etag.as_ref() {
        return Ok(IndexEntryWriteGuard::Present { etag: etag.clone() });
    }

    if memory_index_emulation_is_registered(op) {
        return Ok(IndexEntryWriteGuard::TestMemoryPresent {
            raw_bytes: snapshot.raw_bytes.clone(),
        });
    }

    bail!(
        "index publish requires atomic conditional read/write with a usable ETag; refusing unsafe publish for {index_key}"
    )
}

fn snapshots_have_same_identity(
    op: &Operator,
    left: &IndexEntrySnapshot,
    right: &IndexEntrySnapshot,
) -> bool {
    match (left.cas_etag.as_deref(), right.cas_etag.as_deref()) {
        (Some(left), Some(right)) => left == right,
        (None, None) if memory_index_emulation_is_registered(op) => {
            left.raw_bytes == right.raw_bytes
        }
        _ => false,
    }
}

fn memory_index_write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(crate) fn validate_canonical_namespace_remote_prefix(remote_prefix: &str) -> Result<&str> {
    anyhow::ensure!(
        remote_prefix == remote_prefix.trim_end_matches('/'),
        "portable namespace remote prefix must be canonical without a trailing slash: {remote_prefix:?}"
    );
    if !remote_prefix.is_empty() {
        validate_relative_storage_key(remote_prefix, "portable namespace remote prefix")?;
    }
    Ok(remote_prefix)
}

pub(crate) fn namespace_index_prefix(remote_prefix: &str) -> String {
    if remote_prefix.is_empty() {
        "index/".to_owned()
    } else {
        format!("{remote_prefix}/index/")
    }
}

pub(crate) fn namespace_reservation_prefix(remote_prefix: &str) -> String {
    if remote_prefix.is_empty() {
        ".tcfs-namespace/v1/".to_owned()
    } else {
        format!("{remote_prefix}/.tcfs-namespace/v1/")
    }
}

fn namespace_reservation_key(
    remote_prefix: &str,
    reservation: &PortableNamespaceReservationV1,
) -> String {
    format!(
        "{}{}",
        namespace_reservation_prefix(remote_prefix),
        namespace_reservation_object_id(&reservation.folded_path)
    )
}

fn validate_namespace_claim_compatibility(
    existing: &PortableNamespaceReservationV1,
    candidate: &PortableNamespaceReservationV1,
) -> Result<()> {
    anyhow::ensure!(
        existing.folded_path == candidate.folded_path,
        "portable namespace reservation key contains an unrelated folded path"
    );
    if existing.exact_path != candidate.exact_path {
        bail!(
            "portable namespace spelling collision between {:?} and {:?}",
            candidate.exact_path,
            existing.exact_path
        );
    }
    if existing.role != candidate.role {
        bail!(
            "portable namespace file/ancestor collision at {:?}: existing {:?}, candidate {:?}",
            candidate.exact_path,
            existing.role,
            candidate.role
        );
    }
    anyhow::ensure!(
        existing == candidate,
        "portable namespace reservation is not semantically identical"
    );
    Ok(())
}

async fn validate_legacy_namespace_before_reservation(
    op: &Operator,
    remote_prefix: &str,
    candidate_rel_path: &str,
    candidate_role: PortableNamespaceRole,
) -> Result<Vec<PortableNamespaceReservationV1>> {
    let index_prefix = namespace_index_prefix(remote_prefix);
    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .with_context(|| {
            format!("listing legacy index namespace before reserving {candidate_rel_path:?}")
        })?;

    let mut logical_entries = Vec::new();
    for entry in entries {
        let key = entry.path();
        if key.ends_with('/') {
            continue;
        }
        let rel_path = key
            .strip_prefix(&index_prefix)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "namespace listing returned key outside requested prefix {index_prefix:?}: {key:?}"
                )
            })?;
        logical_entries.push(
            namespace_logical_entry_from_index_path(rel_path)
                .with_context(|| format!("invalid existing index path {rel_path:?}"))?,
        );
    }

    logical_entries.sort_by(|left, right| left.0.cmp(&right.0));
    logical_entries.push((candidate_rel_path.to_owned(), candidate_role));

    let mut observed = std::collections::BTreeMap::<String, PortableNamespaceReservationV1>::new();
    for (logical_path, role) in logical_entries {
        for claim in namespace_claims_for_path(&logical_path, role)? {
            if let Some(existing) = observed.get(&claim.folded_path) {
                validate_namespace_claim_compatibility(existing, &claim)?;
            } else {
                observed.insert(claim.folded_path.clone(), claim);
            }
        }
    }

    namespace_claims_for_path(candidate_rel_path, candidate_role)
}

async fn read_and_validate_namespace_reservation(
    op: &Operator,
    reservation_key: &str,
    expected: &PortableNamespaceReservationV1,
) -> Result<bool> {
    let bytes = match op.read(reservation_key).await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::anyhow!(error)).with_context(|| {
                format!("reading portable namespace reservation: {reservation_key}")
            })
        }
    };
    let existing = PortableNamespaceReservationV1::from_json_bytes(&bytes)
        .with_context(|| format!("invalid portable namespace reservation: {reservation_key}"))?;
    validate_namespace_claim_compatibility(&existing, expected).with_context(|| {
        format!("conflicting portable namespace reservation: {reservation_key}")
    })?;
    Ok(true)
}

async fn create_namespace_reservation_if_absent(
    op: &Operator,
    remote_prefix: &str,
    reservation: &PortableNamespaceReservationV1,
) -> Result<()> {
    let reservation_key = namespace_reservation_key(remote_prefix, reservation);
    let bytes = reservation.to_json_bytes()?;

    if op.info().full_capability().write_with_if_not_exists {
        match op
            .write_with(&reservation_key, bytes)
            .if_not_exists(true)
            .await
        {
            Ok(_) => return Ok(()),
            Err(write_error) => {
                let write_error_text = write_error.to_string();
                match read_and_validate_namespace_reservation(op, &reservation_key, reservation)
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => {
                        return Err(anyhow::anyhow!(write_error)).with_context(|| {
                            format!(
                                "atomically creating portable namespace reservation: {reservation_key}"
                            )
                        })
                    }
                    Err(validation_error) => {
                        return Err(validation_error).with_context(|| {
                            format!(
                                "reservation create failed ({write_error_text}) and no strictly identical reservation could be proven"
                            )
                        })
                    }
                }
            }
        }
    }

    if memory_index_emulation_is_registered(op) {
        let _guard = memory_index_write_lock().lock().await;
        if read_and_validate_namespace_reservation(op, &reservation_key, reservation).await? {
            return Ok(());
        }
        op.write(&reservation_key, bytes).await.with_context(|| {
            format!("creating conditionally guarded test namespace reservation: {reservation_key}")
        })?;
        return Ok(());
    }

    bail!(
        "portable namespace admission requires atomic absent-object creation; refusing unsafe reservation: {reservation_key}"
    )
}

async fn admit_portable_namespace_entry_with_hook<F, Fut>(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    role: PortableNamespaceRole,
    before_reservations: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_namespace_logical_path(rel_path)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .with_context(|| {
            format!(
                "verifying conditional-write semantics for portable namespace admission: {rel_path:?}"
            )
        })?;

    // Validate the complete legacy namespace, not just candidate-vs-existing,
    // before the first durable reservation. A corrupt or already ambiguous
    // legacy tree must not be partially blessed by v1 reservations.
    let reservations =
        validate_legacy_namespace_before_reservation(op, prefix, rel_path, role).await?;
    before_reservations().await?;

    // Reservations are monotonic. If a later claim conflicts, earlier claims
    // intentionally remain as durable evidence; admission never rolls them
    // back and therefore cannot reopen a spelling race.
    for reservation in &reservations {
        create_namespace_reservation_if_absent(op, prefix, reservation).await?;
    }
    Ok(())
}

/// Atomically reserve every cumulative portable namespace component for one
/// file or directory.
///
/// `remote_prefix` must be the explicit canonical storage root (for example,
/// `data`, never `data/`). The function first validates the entire legacy index
/// namespace, then creates immutable v1 reservations using absent-only writes.
pub async fn admit_portable_namespace_entry(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    role: PortableNamespaceRole,
) -> Result<()> {
    admit_portable_namespace_entry_with_hook(op, remote_prefix, rel_path, role, || {
        std::future::ready(Ok(()))
    })
    .await
}

/// Create one immutable object without ever overwriting a value that won the
/// same key. A collided object is accepted only when its bytes are exactly
/// identical.
async fn write_immutable_object_if_absent_exact(
    op: &Operator,
    object_key: &str,
    expected_bytes: &[u8],
    object_description: &str,
    mismatch_description: &str,
) -> Result<bool> {
    if op.info().full_capability().write_with_if_not_exists {
        match op
            .write_with(object_key, expected_bytes.to_vec())
            .if_not_exists(true)
            .await
        {
            Ok(_) => return Ok(true),
            Err(write_error) => {
                let write_error_text = write_error.to_string();
                match op.read(object_key).await {
                    Ok(existing) => {
                        anyhow::ensure!(
                            existing.to_vec().as_slice() == expected_bytes,
                            "{mismatch_description}: {object_key}"
                        );
                        return Ok(false);
                    }
                    Err(read_error) if read_error.kind() == ErrorKind::NotFound => {
                        return Err(anyhow::anyhow!(write_error)).with_context(|| {
                            format!("atomically creating {object_description}: {object_key}")
                        })
                    }
                    Err(read_error) => {
                        return Err(anyhow::anyhow!(read_error)).with_context(|| {
                            format!(
                                "{object_description} create failed ({write_error_text}) and collision identity could not be read: {object_key}"
                            )
                        })
                    }
                }
            }
        }
    }

    if memory_index_emulation_is_registered(op) {
        let _guard = memory_index_write_lock().lock().await;
        let created = match op.read(object_key).await {
            Ok(existing) => {
                anyhow::ensure!(
                    existing.to_vec().as_slice() == expected_bytes,
                    "{mismatch_description}: {object_key}"
                );
                false
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                op.write(object_key, expected_bytes.to_vec())
                    .await
                    .with_context(|| {
                        format!(
                            "creating conditionally guarded test {object_description}: {object_key}"
                        )
                    })?;
                true
            }
            Err(error) => {
                return Err(anyhow::anyhow!(error)).with_context(|| {
                    format!("reading conditionally guarded {object_description}: {object_key}")
                })
            }
        };
        return Ok(created);
    }

    bail!(
        "{object_description} requires atomic absent-object creation; refusing unsafe write: {object_key}"
    )
}

/// Install a content-addressed manifest using absent-only creation. A collided
/// key is idempotent only when its bytes match exactly.
pub(crate) async fn write_immutable_manifest_object_if_absent(
    op: &Operator,
    remote_prefix: &str,
    manifest_key: &str,
    expected_bytes: &[u8],
) -> Result<bool> {
    let prefix = validate_manifest_key_for_remote_prefix(remote_prefix, manifest_key)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .with_context(|| {
            format!("verifying conditional-write semantics for immutable manifest: {manifest_key}")
        })?;
    write_immutable_object_if_absent_exact(
        op,
        manifest_key,
        expected_bytes,
        "immutable manifest object",
        "existing immutable manifest object does not match its byte address",
    )
    .await
}

/// Read one exact storage object without invoking compatibility recovery or
/// any write-capability probe.
///
/// Version-bound reads are preferred, followed by ETag-bound reads. A backend
/// without either identity is represented explicitly as `Unbound`; callers
/// must not silently promote that observation into a complete plan.
pub(crate) async fn read_raw_object_snapshot_v1(
    op: &Operator,
    object_key: &str,
    max_bytes: u64,
) -> Result<Option<RawObjectReadV1>> {
    let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    anyhow::ensure!(
        max_bytes > 0,
        "read-only object snapshot bound must be nonzero: {object_key}"
    );
    anyhow::ensure!(
        u64::try_from(object_key.len()).context("storage key length does not fit u64")?
            <= remote_contract.max_storage_key_bytes(),
        "read-only object snapshot key exceeds the registered-root storage-key bound: {object_key:?}"
    );
    let metadata = match op.stat(object_key).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::anyhow!(error))
                .with_context(|| format!("statting read-only object snapshot: {object_key}"));
        }
    };
    anyhow::ensure!(
        metadata.is_file(),
        "read-only object snapshot is not a file: {object_key}"
    );
    anyhow::ensure!(
        metadata.content_length() > 0,
        "read-only object snapshot is empty: {object_key}"
    );
    anyhow::ensure!(
        metadata.content_length() <= max_bytes,
        "read-only object snapshot exceeds {max_bytes} bytes: {object_key}"
    );

    let checked_binding_token = |value: Option<&str>,
                                 description: &str|
     -> Result<Option<String>> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        anyhow::ensure!(
            u64::try_from(value.len()).context("object binding token length does not fit u64")?
                <= remote_contract.max_binding_token_bytes(),
            "{description} exceeds the registered-root binding-token bound: {object_key}"
        );
        Ok(Some(value.to_owned()))
    };
    let etag = checked_binding_token(metadata.etag(), "object ETag")?;
    let version = checked_binding_token(metadata.version(), "object version")?
        .filter(|version| version != "null");
    let capability = op.info().full_capability();
    let version = version.filter(|_| capability.read_with_version);
    let etag_for_read = etag
        .as_deref()
        .filter(|_| capability.read_with_if_match)
        .map(str::to_owned);
    if version.is_none() && etag_for_read.is_none() {
        return Ok(Some(RawObjectReadV1::Unbound));
    }
    let (reader_result, binding) = if let Some(version) = version.as_deref() {
        let reader = if let Some(etag) = etag_for_read.as_deref() {
            op.reader_with(object_key)
                .version(version)
                .if_match(etag)
                .await
        } else {
            op.reader_with(object_key).version(version).await
        };
        (
            reader,
            RawObjectReadBindingV1::Version {
                version: version.to_owned(),
                etag: etag.clone(),
            },
        )
    } else if let Some(etag) = etag_for_read.as_deref() {
        (
            op.reader_with(object_key).if_match(etag).await,
            RawObjectReadBindingV1::Etag {
                etag: etag.to_owned(),
            },
        )
    } else {
        unreachable!("unbound storage reads return before fetching object bytes")
    };

    let reader = match reader_result {
        Ok(reader) => reader,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConditionNotMatch
            ) =>
        {
            bail!("object changed after read-only snapshot stat: {object_key}");
        }
        Err(error) => {
            return Err(anyhow::anyhow!(error))
                .with_context(|| format!("reading read-only object snapshot: {object_key}"));
        }
    };
    // An exact `0..stat_length` range can silently accept a truncated prefix
    // if a broken provider reuses an ETag while the object grows. Keep the
    // conditional identity on one open-ended reader, consume it incrementally,
    // and retain at most max+1 bytes so growth is detected without unbounded
    // application allocation.
    let mut stream = match reader.into_stream(..).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConditionNotMatch
            ) =>
        {
            bail!("object changed after read-only snapshot stat: {object_key}");
        }
        Err(error) => {
            return Err(anyhow::anyhow!(error))
                .with_context(|| format!("opening read-only object snapshot: {object_key}"));
        }
    };
    let max_plus_one = max_bytes
        .checked_add(1)
        .context("read-only object snapshot bound cannot be incremented")?;
    let max_bytes_usize = usize::try_from(max_bytes)
        .context("read-only object snapshot bound does not fit memory")?;
    let max_plus_one_usize = usize::try_from(max_plus_one)
        .context("read-only object snapshot detection bound does not fit memory")?;
    let initial_capacity = usize::try_from(metadata.content_length())
        .context("read-only object metadata length does not fit memory")?;
    let mut raw_bytes = Vec::with_capacity(initial_capacity);
    loop {
        let buffer = match stream.try_next().await {
            Ok(Some(buffer)) => buffer,
            Ok(None) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConditionNotMatch
                ) =>
            {
                bail!("object changed after read-only snapshot stat: {object_key}");
            }
            Err(error) => {
                return Err(anyhow::anyhow!(error))
                    .with_context(|| format!("streaming read-only object snapshot: {object_key}"));
            }
        };
        for chunk in buffer {
            let retained = raw_bytes.len();
            let remaining = max_plus_one_usize.saturating_sub(retained);
            let copy_len = remaining.min(chunk.len());
            raw_bytes.extend_from_slice(&chunk[..copy_len]);
            anyhow::ensure!(
                raw_bytes.len() <= max_bytes_usize && copy_len == chunk.len(),
                "object exceeded read-only snapshot bound while reading: {object_key}"
            );
        }
    }
    anyhow::ensure!(
        u64::try_from(raw_bytes.len()).context("read-only object length does not fit u64")?
            == metadata.content_length(),
        "object length changed during read-only snapshot: {object_key}"
    );
    let raw_blake3 = blake3::hash(&raw_bytes);
    Ok(Some(RawObjectReadV1::Bound(RawObjectSnapshotV1 {
        raw_bytes,
        raw_blake3,
        binding,
    })))
}

async fn read_raw_index_snapshot_from_store(
    op: &Operator,
    index_key: &str,
) -> Result<Option<RawIndexEntrySnapshot>> {
    let capability = op.info().full_capability();
    let (raw_bytes, cas_etag) = if capability.read_with_if_match && capability.write_with_if_match {
        let metadata = match op.stat(index_key).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(anyhow::anyhow!(error))
                    .with_context(|| format!("statting index entry: {index_key}"));
            }
        };
        let etag = metadata
            .etag()
            .filter(|etag| !etag.is_empty())
            .map(str::to_owned);

        if let Some(etag) = etag {
            let bytes = match op.read_with(index_key).if_match(&etag).await {
                Ok(bytes) => bytes.to_vec(),
                Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {
                    bail!("index entry changed while binding crash-recovery snapshot: {index_key}");
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    bail!("index entry disappeared while binding crash-recovery snapshot: {index_key}");
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(error)).with_context(|| {
                        format!("reading conditionally bound index entry: {index_key}")
                    });
                }
            };
            (bytes, Some(etag))
        } else {
            let bytes = op
                .read(index_key)
                .await
                .with_context(|| format!("reading index entry without a usable ETag: {index_key}"))?
                .to_vec();
            (bytes, None)
        }
    } else {
        let bytes = match op.read(index_key).await {
            Ok(bytes) => bytes.to_vec(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(anyhow::anyhow!(error))
                    .with_context(|| format!("reading index entry: {index_key}"));
            }
        };
        (bytes, None)
    };

    Ok(Some(RawIndexEntrySnapshot {
        raw_bytes,
        cas_etag,
    }))
}

async fn read_index_entry_snapshot_from_store(
    op: &Operator,
    index_key: &str,
) -> Result<Option<IndexEntrySnapshot>> {
    let Some(snapshot) = read_raw_index_snapshot_from_store(op, index_key).await? else {
        return Ok(None);
    };
    let parsed = parse_index_entry_record(&snapshot.raw_bytes)
        .with_context(|| format!("parsing index entry snapshot: {index_key}"))?;
    Ok(Some(IndexEntrySnapshot {
        parsed,
        raw_bytes: snapshot.raw_bytes,
        cas_etag: snapshot.cas_etag,
    }))
}

async fn bind_written_index_entry(
    op: &Operator,
    index_key: &str,
    expected_bytes: &[u8],
    returned_etag: Option<String>,
) -> Result<IndexEntryWriteGuard> {
    if let Some(etag) = returned_etag.filter(|etag| !etag.is_empty()) {
        return Ok(IndexEntryWriteGuard::Present { etag });
    }

    let capability = op.info().full_capability();
    if capability.read_with_if_match && capability.write_with_if_match {
        let metadata = op
            .stat(index_key)
            .await
            .with_context(|| format!("rebinding conditionally written index entry: {index_key}"))?;
        let etag = metadata
            .etag()
            .filter(|etag| !etag.is_empty())
            .context("conditionally written index entry has no usable ETag")?
            .to_owned();
        let observed = match op.read_with(index_key).if_match(&etag).await {
            Ok(bytes) => bytes.to_vec(),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConditionNotMatch | ErrorKind::NotFound
                ) =>
            {
                bail!(
                    "index entry changed immediately after conditional publish write; retry required: {index_key}"
                )
            }
            Err(error) => {
                return Err(anyhow::anyhow!(error)).with_context(|| {
                    format!("reading conditionally written index entry: {index_key}")
                })
            }
        };
        anyhow::ensure!(
            observed.as_slice() == expected_bytes,
            "index entry changed immediately after conditional publish write; retry required: {index_key}"
        );
        return Ok(IndexEntryWriteGuard::Present { etag });
    }

    if memory_index_emulation_is_registered(op) {
        return Ok(IndexEntryWriteGuard::TestMemoryPresent {
            raw_bytes: expected_bytes.to_vec(),
        });
    }

    bail!("conditionally written index entry could not be rebound to a usable ETag: {index_key}")
}

async fn write_index_entry_conditionally(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    bytes: Vec<u8>,
    guard: IndexEntryWriteGuard,
    transition: &str,
) -> Result<IndexEntryWriteGuard> {
    // Keep the final mutation primitive fail-closed even if a future caller
    // forgets to run the publication preflight before acquiring its guard.
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;

    match guard {
        IndexEntryWriteGuard::Present { etag } => {
            let metadata = match op
                .write_with(index_key, bytes.clone())
                .if_match(&etag)
                .await
            {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConditionNotMatch | ErrorKind::NotFound
                    ) =>
                {
                    bail!(
                        "index entry changed during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
                    )
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(error)).with_context(|| {
                        format!(
                            "conditionally writing index entry during {transition}: {index_key}"
                        )
                    })
                }
            };
            let returned_etag = metadata
                .etag()
                .filter(|etag| !etag.is_empty())
                .map(str::to_owned);
            bind_written_index_entry(op, index_key, &bytes, returned_etag).await
        }
        IndexEntryWriteGuard::Absent => {
            ensure_atomic_absent_create_supported(op, index_key)?;
            if op.info().full_capability().write_with_if_not_exists {
                let metadata = match op
                    .write_with(index_key, bytes.clone())
                    .if_not_exists(true)
                    .await
                {
                    Ok(metadata) => metadata,
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists
                        ) =>
                    {
                        bail!(
                            "index entry appeared during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
                        )
                    }
                    Err(error) => {
                        if op.exists(index_key).await.unwrap_or(false) {
                            bail!(
                                "index entry appeared during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
                            );
                        }
                        return Err(anyhow::anyhow!(error)).with_context(|| {
                            format!(
                                "atomically creating absent index entry during {transition}: {index_key}"
                            )
                        });
                    }
                };
                let returned_etag = metadata
                    .etag()
                    .filter(|etag| !etag.is_empty())
                    .map(str::to_owned);
                return bind_written_index_entry(op, index_key, &bytes, returned_etag).await;
            }

            if memory_index_emulation_is_registered(op) {
                let _guard = memory_index_write_lock().lock().await;
                anyhow::ensure!(
                    !op.exists(index_key)
                        .await
                        .with_context(|| format!("checking absent test index entry: {index_key}"))?,
                    "index entry appeared during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
                );
                op.write(index_key, bytes.clone()).await.with_context(|| {
                    format!("creating conditionally guarded test index entry: {index_key}")
                })?;
                return Ok(IndexEntryWriteGuard::TestMemoryPresent { raw_bytes: bytes });
            }

            bail!("atomic absent-object index creation became unavailable: {index_key}")
        }
        IndexEntryWriteGuard::TestMemoryPresent { raw_bytes } => {
            let _guard = memory_index_write_lock().lock().await;
            let observed = match op.read(index_key).await {
                Ok(bytes) => bytes.to_vec(),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    bail!(
                        "index entry disappeared during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
                    )
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(error)).with_context(|| {
                        format!("revalidating conditionally guarded test index: {index_key}")
                    })
                }
            };
            anyhow::ensure!(
                observed == raw_bytes,
                "index entry changed during {transition}; refusing to overwrite another publisher and preserving publish evidence: {index_key}"
            );
            op.write(index_key, bytes.clone()).await.with_context(|| {
                format!("writing conditionally guarded test index entry: {index_key}")
            })?;
            Ok(IndexEntryWriteGuard::TestMemoryPresent { raw_bytes: bytes })
        }
    }
}

/// Bind an atomic absent-object write for a caller that holds the fresh-prefix
/// contract. The eventual write still enforces absence at the storage boundary;
/// the contract is never implemented as a check-then-unconditional-write.
pub(crate) async fn require_absent_index_entry_for_update(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
) -> Result<IndexEntryWriteGuard> {
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    ensure_atomic_absent_create_supported(op, index_key)?;
    Ok(IndexEntryWriteGuard::Absent)
}

/// Resolve the visible entry and retain the exact index-object identity used by
/// that decision. Recovery may legitimately rewrite a preparing record, so the
/// resolver rebinds until the visible value and guard describe the same object.
pub(crate) async fn resolve_visible_index_entry_for_update(
    op: &Operator,
    index_key: &str,
    manifest_prefix: &str,
) -> Result<ResolvedIndexEntryForUpdate> {
    const MAX_BIND_ATTEMPTS: usize = 8;

    for _ in 0..MAX_BIND_ATTEMPTS {
        let Some(snapshot) = read_index_entry_snapshot_from_store(op, index_key).await? else {
            ensure_atomic_absent_create_supported(op, index_key)?;
            return Ok(ResolvedIndexEntryForUpdate {
                current: None,
                guard: IndexEntryWriteGuard::Absent,
            });
        };
        // Fail closed before staging anything when this backend cannot perform
        // the eventual compare-and-swap.
        let _ = write_guard_for_snapshot(op, index_key, &snapshot)?;

        let mut no_recovery_hook = || std::future::ready(Ok(()));
        let current = resolve_visible_parsed_entry(
            op,
            index_key,
            manifest_prefix,
            &snapshot,
            &mut no_recovery_hook,
        )
        .await?;

        let Some(rebound) = read_index_entry_snapshot_from_store(op, index_key).await? else {
            continue;
        };
        if snapshots_have_same_identity(op, &snapshot, &rebound) {
            return Ok(ResolvedIndexEntryForUpdate {
                current,
                guard: write_guard_for_snapshot(op, index_key, &rebound)?,
            });
        }
    }

    bail!("index entry kept changing while binding publish baseline; retry required: {index_key}")
}

pub(crate) async fn write_preparing_index_entry_conditionally(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    current: Option<RemoteIndexEntry>,
    pending: PendingIndexEntry,
    guard: IndexEntryWriteGuard,
) -> Result<IndexEntryWriteGuard> {
    let bytes = VersionedIndexEntry::preparing(current, pending).to_json_bytes()?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        bytes,
        guard,
        "preparing publish",
    )
    .await
}

pub(crate) async fn write_committed_index_entry_conditionally(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    entry: &RemoteIndexEntry,
    guard: IndexEntryWriteGuard,
) -> Result<()> {
    let bytes = VersionedIndexEntry::committed(entry.clone()).to_json_bytes()?;
    write_index_entry_conditionally(
        op,
        remote_prefix,
        index_key,
        bytes,
        guard,
        "committing publish",
    )
    .await
    .map(|_| ())
}

fn ensure_recovery_commit_supported(
    _op: &Operator,
    index_key: &str,
    snapshot: &IndexEntrySnapshot,
) -> Result<()> {
    if snapshot.cas_etag.is_some() {
        return Ok(());
    }

    // Registered in-memory test accessors have neither ETags nor conditional
    // writes, so recovery uses their process-local exact-snapshot emulation.
    if memory_index_emulation_is_registered(_op) {
        return Ok(());
    }

    bail!(
        "index recovery requires atomic conditional read/write with a usable ETag; refusing unsafe recovery for {index_key}"
    )
}

async fn commit_recovered_index_entry(
    op: &Operator,
    remote_prefix: &str,
    index_key: &str,
    snapshot: &IndexEntrySnapshot,
    entry: &RemoteIndexEntry,
) -> Result<()> {
    ensure_index_write_semantics(op, remote_prefix, index_key).await?;
    ensure_recovery_commit_supported(op, index_key, snapshot)?;
    let committed_bytes = VersionedIndexEntry::committed(entry.clone()).to_json_bytes()?;

    if let Some(etag) = snapshot.cas_etag.as_deref() {
        return match op
            .write_with(index_key, committed_bytes)
            .if_match(etag)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {
                bail!(
                    "index entry changed during crash recovery; refusing to overwrite a newer publisher: {index_key}"
                )
            }
            Err(error) => Err(anyhow::anyhow!(error)).with_context(|| {
                format!("conditionally committing recovered index entry: {index_key}")
            }),
        };
    }

    if memory_index_emulation_is_registered(op) {
        let _guard = memory_index_write_lock().lock().await;
        let observed = op
            .read(index_key)
            .await
            .with_context(|| format!("revalidating test index entry: {index_key}"))?
            .to_vec();
        anyhow::ensure!(
            observed == snapshot.raw_bytes,
            "index entry changed during crash recovery; refusing to overwrite a newer publisher: {index_key}"
        );
        op.write(index_key, committed_bytes)
            .await
            .with_context(|| format!("committing recovered test index entry: {index_key}"))?;
        return Ok(());
    }

    bail!("index recovery reached commit without atomic conditional write support: {index_key}")
}

/// Install a staged immutable manifest without ever overwriting an object that
/// appeared after recovery observed the final key as absent. A same-key object
/// is acceptable only when its bytes are exactly the staged bytes; legacy
/// file-hash-addressed manifests can otherwise share a key while carrying
/// different clocks, encryption wraps, or other authority metadata.
async fn materialize_pending_manifest_if_absent(
    op: &Operator,
    remote_prefix: &str,
    manifest_key: &str,
    staged_bytes: &[u8],
) -> Result<()> {
    let prefix = validate_manifest_key_for_remote_prefix(remote_prefix, manifest_key)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .with_context(|| {
            format!("verifying conditional-write semantics for recovered manifest: {manifest_key}")
        })?;
    write_immutable_object_if_absent_exact(
        op,
        manifest_key,
        staged_bytes,
        "pending immutable manifest",
        "existing immutable manifest differs from staged recovery bytes",
    )
    .await
    .map(|_| ())
}

pub async fn resolve_visible_index_entry(
    op: &Operator,
    index_key: &str,
    manifest_prefix: &str,
) -> Result<Option<RemoteIndexEntry>> {
    resolve_visible_index_entry_with_hook(op, index_key, manifest_prefix, || {
        std::future::ready(Ok(()))
    })
    .await
}

async fn resolve_visible_index_entry_with_hook<F, Fut>(
    op: &Operator,
    index_key: &str,
    manifest_prefix: &str,
    mut before_recovery_commit: F,
) -> Result<Option<RemoteIndexEntry>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let snapshot = match read_index_entry_snapshot_from_store(op, index_key).await? {
        Some(snapshot) => snapshot,
        None => return Ok(None),
    };

    resolve_visible_parsed_entry(
        op,
        index_key,
        manifest_prefix,
        &snapshot,
        &mut before_recovery_commit,
    )
    .await
}

async fn resolve_visible_parsed_entry<F, Fut>(
    op: &Operator,
    index_key: &str,
    manifest_prefix: &str,
    snapshot: &IndexEntrySnapshot,
    before_recovery_commit: &mut F,
) -> Result<Option<RemoteIndexEntry>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let remote_prefix = remote_prefix_from_manifest_prefix(manifest_prefix)?;
    if let Some(pending) = snapshot.parsed.pending_entry() {
        validate_staged_manifest_key(manifest_prefix, pending)?;
        let pending_manifest_key = manifest_key(manifest_prefix, &pending.manifest_hash);
        if op
            .exists(&pending_manifest_key)
            .await
            .with_context(|| format!("checking pending manifest: {pending_manifest_key}"))?
        {
            let final_bytes = op
                .read(&pending_manifest_key)
                .await
                .with_context(|| format!("reading pending manifest: {pending_manifest_key}"))?
                .to_vec();
            let exact_identity = verify_pending_manifest_bytes(
                pending,
                index_key,
                manifest_prefix,
                &pending_manifest_key,
                &final_bytes,
            )?;
            if !exact_identity {
                let staged_bytes = match op.read(&pending.staged_manifest_key).await {
                    Ok(bytes) => bytes.to_vec(),
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        bail!(
                            "legacy pending manifest has no staged bytes for exact recovery comparison: {}",
                            pending.staged_manifest_key
                        )
                    }
                    Err(error) => {
                        return Err(anyhow::anyhow!(error)).with_context(|| {
                            format!(
                                "reading staged legacy manifest for exact comparison: {}",
                                pending.staged_manifest_key
                            )
                        })
                    }
                };
                verify_pending_manifest_bytes(
                    pending,
                    index_key,
                    manifest_prefix,
                    &pending.staged_manifest_key,
                    &staged_bytes,
                )?;
                anyhow::ensure!(
                    staged_bytes == final_bytes,
                    "existing immutable manifest differs from staged recovery bytes: {pending_manifest_key}"
                );
            }
            let committed = pending.as_remote_entry();
            before_recovery_commit().await?;
            commit_recovered_index_entry(op, remote_prefix, index_key, snapshot, &committed)
                .await?;
            return Ok(Some(committed));
        }

        if op
            .exists(&pending.staged_manifest_key)
            .await
            .with_context(|| {
                format!(
                    "checking staged manifest for recovery: {}",
                    pending.staged_manifest_key
                )
            })?
        {
            let staged_bytes = op
                .read(&pending.staged_manifest_key)
                .await
                .with_context(|| {
                    format!(
                        "reading staged manifest for recovery: {}",
                        pending.staged_manifest_key
                    )
                })?
                .to_vec();
            verify_pending_manifest_bytes(
                pending,
                index_key,
                manifest_prefix,
                &pending.staged_manifest_key,
                &staged_bytes,
            )?;
            ensure_recovery_commit_supported(op, index_key, snapshot)?;
            materialize_pending_manifest_if_absent(
                op,
                remote_prefix,
                &pending_manifest_key,
                &staged_bytes,
            )
            .await?;

            let committed = pending.as_remote_entry();
            before_recovery_commit().await?;
            commit_recovered_index_entry(op, remote_prefix, index_key, snapshot, &committed)
                .await?;
            return Ok(Some(committed));
        }
    }

    if let Some(current) = snapshot.parsed.visible_entry() {
        let current_manifest_key = manifest_key(manifest_prefix, &current.manifest_hash);
        if op
            .exists(&current_manifest_key)
            .await
            .with_context(|| format!("checking current manifest: {current_manifest_key}"))?
        {
            return Ok(Some(current.clone()));
        }

        bail!("index entry points to missing manifest: {current_manifest_key}");
    }

    Ok(None)
}

fn parse_legacy_index_entry(text: &str) -> Result<RemoteIndexEntry> {
    let mut manifest_hash = None;
    let mut size = 0u64;
    let mut chunks = 0usize;

    for line in text.lines() {
        if let Some(v) = line.strip_prefix("manifest_hash=") {
            manifest_hash = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("size=") {
            size = v.parse().context("invalid size in index entry")?;
        } else if let Some(v) = line.strip_prefix("chunks=") {
            chunks = v.parse().context("invalid chunk count in index entry")?;
        }
    }

    let entry = RemoteIndexEntry {
        manifest_hash: manifest_hash.context("index entry missing manifest_hash")?,
        size,
        chunks,
        kind: RemoteEntryKind::RegularFile,
        symlink_target: None,
    };
    validate_remote_entry(&entry, "legacy index entry")?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::{
        legacy_symlink_target_id, manifest_key, manifest_object_id, parse_index_entry,
        parse_index_entry_record, portable_casefold_path, resolve_visible_index_entry,
        resolve_visible_index_entry_with_hook, validate_canonical_rel_path, IndexEntryState,
        ParsedIndexEntry, PendingIndexEntry, RemoteIndexEntry, VersionedIndexEntry,
    };
    use opendal::raw::{Access, AccessorInfo, OpRead, OpStat, RpRead, RpStat};
    use opendal::services::Memory;
    use opendal::{Buffer, Capability, EntryMode, ErrorKind, Metadata, Operator, OperatorBuilder};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        super::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    #[derive(Clone, Debug)]
    struct SnapshotTestObject {
        bytes: Vec<u8>,
        etag: Option<String>,
        version: Option<String>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct SnapshotTestRead {
        offset: u64,
        size: Option<u64>,
        if_match: Option<String>,
        version: Option<String>,
    }

    #[derive(Debug)]
    struct SnapshotTestState {
        current: SnapshotTestObject,
        versions: BTreeMap<String, SnapshotTestObject>,
        replace_after_next_stat: Option<SnapshotTestObject>,
        reads: Vec<SnapshotTestRead>,
    }

    #[derive(Clone, Debug)]
    struct SnapshotTestBackend {
        state: Arc<Mutex<SnapshotTestState>>,
        info: Arc<AccessorInfo>,
    }

    impl SnapshotTestBackend {
        fn reads(&self) -> Vec<SnapshotTestRead> {
            self.state.lock().unwrap().reads.clone()
        }
    }

    impl Access for SnapshotTestBackend {
        type Reader = Buffer;
        type Writer = ();
        type Lister = ();
        type Deleter = ();

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn stat(&self, _path: &str, _: OpStat) -> opendal::Result<RpStat> {
            let mut state = self.state.lock().unwrap();
            let observed = state.current.clone();
            if let Some(replacement) = state.replace_after_next_stat.take() {
                state.current = replacement;
            }
            let mut metadata =
                Metadata::new(EntryMode::FILE).with_content_length(observed.bytes.len() as u64);
            if let Some(etag) = observed.etag {
                metadata = metadata.with_etag(etag);
            }
            if let Some(version) = observed.version {
                metadata = metadata.with_version(version);
            }
            Ok(RpStat::new(metadata))
        }

        async fn read(&self, _path: &str, args: OpRead) -> opendal::Result<(RpRead, Buffer)> {
            let mut state = self.state.lock().unwrap();
            let range = args.range();
            state.reads.push(SnapshotTestRead {
                offset: range.offset(),
                size: range.size(),
                if_match: args.if_match().map(str::to_owned),
                version: args.version().map(str::to_owned),
            });
            let selected = match args.version() {
                Some(version) => state.versions.get(version).cloned(),
                None => Some(state.current.clone()),
            }
            .ok_or_else(|| {
                opendal::Error::new(ErrorKind::NotFound, "snapshot-test version is missing")
            })?;
            if args
                .if_match()
                .is_some_and(|expected| selected.etag.as_deref() != Some(expected))
            {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "snapshot-test rejected stale ETag",
                ));
            }

            let start = usize::try_from(range.offset()).unwrap_or(usize::MAX);
            let requested = range
                .size()
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(usize::MAX);
            let end = start.saturating_add(requested).min(selected.bytes.len());
            let bytes = if start <= end {
                selected.bytes[start..end].to_vec()
            } else {
                Vec::new()
            };
            Ok((
                RpRead::new().with_size(Some(bytes.len() as u64)),
                Buffer::from(bytes),
            ))
        }
    }

    fn snapshot_test_operator(
        current: SnapshotTestObject,
        replacement: Option<SnapshotTestObject>,
        advertise_version: bool,
        advertise_etag: bool,
    ) -> (Operator, SnapshotTestBackend) {
        let mut versions = BTreeMap::new();
        for object in std::iter::once(&current).chain(replacement.as_ref()) {
            if let Some(version) = object.version.as_ref() {
                versions.insert(version.clone(), object.clone());
            }
        }
        let info = AccessorInfo::default();
        info.set_scheme("snapshot-test")
            .set_root("/")
            .set_name("snapshot-test")
            .set_native_capability(Capability {
                stat: true,
                read: true,
                read_with_if_match: advertise_etag,
                read_with_version: advertise_version,
                ..Default::default()
            });
        let backend = SnapshotTestBackend {
            state: Arc::new(Mutex::new(SnapshotTestState {
                current,
                versions,
                replace_after_next_stat: replacement,
                reads: Vec::new(),
            })),
            info: Arc::new(info),
        };
        (OperatorBuilder::new(backend.clone()).finish(), backend)
    }

    async fn write_committed_index_entry(
        op: &Operator,
        index_key: &str,
        entry: &RemoteIndexEntry,
    ) -> anyhow::Result<()> {
        op.write(
            index_key,
            VersionedIndexEntry::committed(entry.clone()).to_json_bytes()?,
        )
        .await?;
        Ok(())
    }

    async fn write_preparing_index_entry(
        op: &Operator,
        index_key: &str,
        current: Option<RemoteIndexEntry>,
        pending: PendingIndexEntry,
    ) -> anyhow::Result<()> {
        op.write(
            index_key,
            VersionedIndexEntry::preparing(current, pending).to_json_bytes()?,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn memory_emulation_requires_exact_accessor_registration() {
        let _registered_but_distinct = memory_op();
        let unregistered = Operator::new(Memory::default()).unwrap().finish();
        let error = super::write_committed_index_entry(
            &unregistered,
            "data",
            "data/index/doc.txt",
            &RemoteIndexEntry::new("manifest-a", 0, 0),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("verifying conditional-write semantics"));
        assert!(!unregistered.exists("data/index/doc.txt").await.unwrap());
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_is_exact_unbound_and_non_mutating() {
        let op = memory_op();
        assert!(super::read_raw_object_snapshot_v1(&op, "object", 1024)
            .await
            .unwrap()
            .is_none());

        let expected = b"raw-object-sentinel";
        op.write("object", expected.to_vec()).await.unwrap();
        let before = op.read("object").await.unwrap().to_vec();
        assert_eq!(
            super::read_raw_object_snapshot_v1(&op, "object", 1024)
                .await
                .unwrap(),
            Some(super::RawObjectReadV1::Unbound)
        );
        assert_eq!(op.read("object").await.unwrap().to_vec(), before);
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_rejects_oversized_metadata_before_read() {
        let op = memory_op();
        op.write("object", b"five".to_vec()).await.unwrap();
        let error = super::read_raw_object_snapshot_v1(&op, "object", 3)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("exceeds 3 bytes"));
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_enforces_key_and_binding_token_bounds() {
        let remote_contract =
            tcfs_core::config::RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let overlong_key =
            "k".repeat(usize::try_from(remote_contract.max_storage_key_bytes() + 1).unwrap());
        let error = super::read_raw_object_snapshot_v1(&memory_op(), &overlong_key, 16)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("storage-key bound"),
            "{error:#}"
        );

        let oversized_etag =
            "e".repeat(usize::try_from(remote_contract.max_binding_token_bytes() + 1).unwrap());
        let (op, backend) = snapshot_test_operator(
            SnapshotTestObject {
                bytes: b"body".to_vec(),
                etag: Some(oversized_etag),
                version: None,
            },
            None,
            false,
            true,
        );
        let error = super::read_raw_object_snapshot_v1(&op, "object", 16)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("binding-token bound"),
            "{error:#}"
        );
        assert!(backend.reads().is_empty());
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_prefers_version_and_pins_stat_generation() {
        let (op, backend) = snapshot_test_operator(
            SnapshotTestObject {
                bytes: b"old".to_vec(),
                etag: Some("etag-1".to_owned()),
                version: Some("version-1".to_owned()),
            },
            Some(SnapshotTestObject {
                bytes: b"newer".to_vec(),
                etag: Some("etag-2".to_owned()),
                version: Some("version-2".to_owned()),
            }),
            true,
            true,
        );

        let snapshot = match super::read_raw_object_snapshot_v1(&op, "object", 32)
            .await
            .unwrap()
            .unwrap()
        {
            super::RawObjectReadV1::Bound(snapshot) => snapshot,
            super::RawObjectReadV1::Unbound => panic!("expected version-bound snapshot"),
        };
        assert_eq!(snapshot.raw_bytes(), b"old");
        assert_eq!(
            snapshot.binding(),
            &super::RawObjectReadBindingV1::Version {
                version: "version-1".to_owned(),
                etag: Some("etag-1".to_owned()),
            }
        );
        assert_eq!(
            backend.reads(),
            vec![SnapshotTestRead {
                offset: 0,
                size: None,
                if_match: Some("etag-1".to_owned()),
                version: Some("version-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_uses_etag_and_rejects_stat_read_mutation() {
        let stable = SnapshotTestObject {
            bytes: b"same".to_vec(),
            etag: Some("etag-1".to_owned()),
            version: None,
        };
        let (op, _) = snapshot_test_operator(stable.clone(), None, false, true);
        let snapshot = match super::read_raw_object_snapshot_v1(&op, "object", 16)
            .await
            .unwrap()
            .unwrap()
        {
            super::RawObjectReadV1::Bound(snapshot) => snapshot,
            super::RawObjectReadV1::Unbound => panic!("expected ETag-bound snapshot"),
        };
        assert_eq!(
            snapshot.binding(),
            &super::RawObjectReadBindingV1::Etag {
                etag: "etag-1".to_owned()
            }
        );

        let (mutated_op, _) = snapshot_test_operator(
            stable,
            Some(SnapshotTestObject {
                bytes: b"diff".to_vec(),
                etag: Some("etag-2".to_owned()),
                version: None,
            }),
            false,
            true,
        );
        let error = super::read_raw_object_snapshot_v1(&mutated_op, "object", 16)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("changed after read-only snapshot stat"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn read_only_raw_object_snapshot_rejects_oversize_and_same_identity_growth() {
        let (oversized_op, oversized_backend) = snapshot_test_operator(
            SnapshotTestObject {
                bytes: vec![0; 5],
                etag: Some("etag-1".to_owned()),
                version: None,
            },
            None,
            false,
            true,
        );
        assert!(
            super::read_raw_object_snapshot_v1(&oversized_op, "object", 4)
                .await
                .is_err()
        );
        assert!(oversized_backend.reads().is_empty());

        let (bounded_op, bounded_backend) = snapshot_test_operator(
            SnapshotTestObject {
                bytes: vec![0; 4],
                etag: Some("same-etag".to_owned()),
                version: None,
            },
            None,
            false,
            true,
        );
        let snapshot = super::read_raw_object_snapshot_v1(&bounded_op, "object", 4)
            .await
            .unwrap();
        assert!(matches!(snapshot, Some(super::RawObjectReadV1::Bound(_))));
        assert_eq!(
            bounded_backend.reads(),
            vec![SnapshotTestRead {
                offset: 0,
                size: None,
                if_match: Some("same-etag".to_owned()),
                version: None,
            }]
        );

        let (grown_op, grown_backend) = snapshot_test_operator(
            SnapshotTestObject {
                bytes: vec![0; 4],
                etag: Some("reused-etag".to_owned()),
                version: None,
            },
            Some(SnapshotTestObject {
                bytes: vec![0; 128],
                etag: Some("reused-etag".to_owned()),
                version: None,
            }),
            false,
            true,
        );
        let error = super::read_raw_object_snapshot_v1(&grown_op, "object", 4)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("exceeded read-only snapshot bound"),
            "{error:#}"
        );
        assert_eq!(
            grown_backend.reads(),
            vec![SnapshotTestRead {
                offset: 0,
                size: None,
                if_match: Some("reused-etag".to_owned()),
                version: None,
            }]
        );
    }

    #[tokio::test]
    async fn public_committed_writer_is_atomic_absent_create_not_overwrite() {
        let op = memory_op();
        let first = RemoteIndexEntry::new("manifest-a", 0, 0);
        let second = RemoteIndexEntry::new("manifest-b", 0, 0);
        super::write_committed_index_entry(&op, "data", "data/index/doc.txt", &first)
            .await
            .unwrap();
        let error = super::write_committed_index_entry(&op, "data", "data/index/doc.txt", &second)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("index entry appeared"));
        assert_eq!(
            parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
                .unwrap()
                .visible_entry(),
            Some(&first)
        );
    }

    #[tokio::test]
    async fn concurrent_casefold_contenders_create_exactly_one_namespace() {
        let op = memory_op();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

        let first = super::admit_portable_namespace_entry_with_hook(
            &op,
            "data",
            "Straße/file.txt",
            super::PortableNamespaceRole::File,
            {
                let barrier = barrier.clone();
                move || async move {
                    barrier.wait().await;
                    Ok(())
                }
            },
        );
        let second = super::admit_portable_namespace_entry_with_hook(
            &op,
            "data",
            "STRASSE/file.txt",
            super::PortableNamespaceRole::File,
            {
                let barrier = barrier.clone();
                move || async move {
                    barrier.wait().await;
                    Ok(())
                }
            },
        );

        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.is_ok() as usize + second.is_ok() as usize, 1);
        let error = first.err().or_else(|| second.err()).unwrap();
        assert!(format!("{error:#}").contains("namespace spelling collision"));
    }

    #[tokio::test]
    async fn concurrent_file_and_ancestor_contenders_create_exactly_one_namespace() {
        let op = memory_op();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

        let file = super::admit_portable_namespace_entry_with_hook(
            &op,
            "data",
            "node",
            super::PortableNamespaceRole::File,
            {
                let barrier = barrier.clone();
                move || async move {
                    barrier.wait().await;
                    Ok(())
                }
            },
        );
        let child = super::admit_portable_namespace_entry_with_hook(
            &op,
            "data",
            "node/child.txt",
            super::PortableNamespaceRole::File,
            {
                let barrier = barrier.clone();
                move || async move {
                    barrier.wait().await;
                    Ok(())
                }
            },
        );

        let (file, child) = tokio::join!(file, child);
        assert_eq!(file.is_ok() as usize + child.is_ok() as usize, 1);
        let error = file.err().or_else(|| child.err()).unwrap();
        assert!(format!("{error:#}").contains("file/ancestor collision"));
    }

    #[tokio::test]
    async fn legacy_collision_creates_no_namespace_reservations() {
        let op = memory_op();
        op.write("data/index/Straße/old.txt", b"legacy".to_vec())
            .await
            .unwrap();

        let error = super::admit_portable_namespace_entry(
            &op,
            "data",
            "STRASSE/new.txt",
            super::PortableNamespaceRole::File,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("namespace spelling collision"));
        assert!(op
            .list("data/.tcfs-namespace/v1/")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn exact_namespace_admission_is_idempotent() {
        let op = memory_op();
        for _ in 0..2 {
            super::admit_portable_namespace_entry(
                &op,
                "data",
                "folder/file.txt",
                super::PortableNamespaceRole::File,
            )
            .await
            .unwrap();
        }

        assert_eq!(op.list("data/.tcfs-namespace/v1/").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn directory_marker_claim_and_child_file_are_compatible() {
        let op = memory_op();
        super::admit_portable_namespace_entry(
            &op,
            "data",
            "empty",
            super::PortableNamespaceRole::Directory,
        )
        .await
        .unwrap();
        super::admit_portable_namespace_entry(
            &op,
            "data",
            "empty/child.txt",
            super::PortableNamespaceRole::File,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn immutable_manifest_create_is_exact_idempotent_and_race_safe() {
        let op = memory_op();
        let key = "data/manifests/shared-object";

        let first =
            super::write_immutable_manifest_object_if_absent(&op, "data", key, b"identical")
                .await
                .unwrap();
        let second =
            super::write_immutable_manifest_object_if_absent(&op, "data", key, b"identical")
                .await
                .unwrap();
        assert!(first);
        assert!(!second);

        let race_key = "data/manifests/raced-object";
        let left = super::write_immutable_manifest_object_if_absent(&op, "data", race_key, b"left");
        let right =
            super::write_immutable_manifest_object_if_absent(&op, "data", race_key, b"right");
        let (left, right) = tokio::join!(left, right);
        assert_eq!(left.is_ok() as usize + right.is_ok() as usize, 1);
        let stored = op.read(race_key).await.unwrap().to_vec();
        assert!(stored == b"left" || stored == b"right");
        let error = left.err().or_else(|| right.err()).unwrap();
        assert!(format!("{error:#}")
            .contains("existing immutable manifest object does not match its byte address"));
    }

    #[tokio::test]
    async fn corrupt_or_wrong_version_namespace_reservation_fails_closed() {
        for bytes in [
            b"{".as_slice(),
            br#"{"version":2,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file"}"#
                .as_slice(),
            br#"{"version":1,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file","unexpected":true}"#
                .as_slice(),
        ] {
            let op = memory_op();
            let claim = super::namespace_claims_for_path(
                "doc.txt",
                super::PortableNamespaceRole::File,
            )
            .unwrap()
            .pop()
            .unwrap();
            let key = super::namespace_reservation_key("data", &claim);
            op.write(&key, bytes.to_vec()).await.unwrap();

            let error = super::admit_portable_namespace_entry(
                &op,
                "data",
                "doc.txt",
                super::PortableNamespaceRole::File,
            )
            .await
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("invalid portable namespace reservation"),
                "unexpected error: {error:#}"
            );
        }
    }

    #[test]
    fn canonical_relative_path_boundary_is_cross_platform_and_unicode_stable() {
        assert!(validate_canonical_rel_path("docs/Résumé.txt").is_ok());
        for rejected in [
            "",
            "/absolute",
            "../escape",
            "a//b",
            "C:/windows",
            "docs/e\u{301}.txt",
            ".GIT/HEAD",
            ".git/Refs/heads/main",
        ] {
            assert!(
                validate_canonical_rel_path(rejected).is_err(),
                "unexpectedly accepted {rejected:?}"
            );
        }
    }

    #[test]
    fn canonical_relative_path_rejects_windows_aliases_and_device_names() {
        for rejected in [
            "docs/report.",
            "docs/report ",
            "docs/report.txt:secret",
            "docs/what?.txt",
            "docs/a<b.txt",
            "CON",
            "con.txt",
            "dir/PRN.md",
            "dir/AUX",
            "dir/NUL.json",
            "devices/COM1",
            "devices/com9.log",
            "devices/LPT1",
            "devices/lpt9.any",
            "devices/COM¹.log",
            "git~1/config",
            "docs/GIT~1/config",
            ".git./config",
            ".git /config",
            ".git::$DATA/config",
        ] {
            assert!(
                validate_canonical_rel_path(rejected).is_err(),
                "unexpectedly accepted Windows-unsafe path {rejected:?}"
            );
        }
    }

    #[test]
    fn canonical_relative_path_keeps_valid_windows_near_misses() {
        for accepted in [
            "docs/console.txt",
            "docs/auxiliary.txt",
            "docs/nullability",
            "devices/COM0",
            "devices/COM10",
            "devices/LPT0",
            "devices/LPT10",
            "docs/com1-backup.txt",
            "docs/git~0/config",
            "docs/git~2/config",
            "docs/git~9/config",
            "docs/git~10/config",
            "docs/report .txt",
            ".git/refs/heads/main",
        ] {
            assert!(
                validate_canonical_rel_path(accepted).is_ok(),
                "unexpectedly rejected valid portable path {accepted:?}"
            );
        }
    }

    #[test]
    fn portable_casefold_expands_unicode_aliases() {
        assert_eq!(
            portable_casefold_path("Straße/File").unwrap(),
            portable_casefold_path("STRASSE/file").unwrap()
        );
    }

    #[test]
    fn parse_legacy_index_entry() {
        let data = b"manifest_hash=abc123\nsize=1024\nchunks=2\n";
        let entry = parse_index_entry(data).unwrap();
        assert_eq!(entry.manifest_hash, "abc123");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.chunks, 2);
    }

    #[test]
    fn parse_committed_json_index_entry() {
        let data = br#"{
            "version": 2,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1024,
                "chunks": 2
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert_eq!(parsed.state(), IndexEntryState::Committed);
        let visible = parsed.visible_entry().unwrap();
        assert_eq!(visible.manifest_hash, "abc123");
        assert_eq!(visible.size, 1024);
        assert_eq!(visible.chunks, 2);
        assert!(parsed.pending_entry().is_none());
    }

    #[test]
    fn parse_preparing_json_index_entry() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "current": {
                "manifest_hash": "old123",
                "size": 10,
                "chunks": 1
            },
            "pending": {
                "manifest_hash": "new456",
                "size": 11,
                "chunks": 1,
                "staged_manifest_key": "data/staging/manifests/txn-1.json"
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert_eq!(parsed.state(), IndexEntryState::Preparing);
        let visible = parsed.visible_entry().unwrap();
        assert_eq!(visible.manifest_hash, "old123");

        let pending = parsed.pending_entry().unwrap();
        assert_eq!(pending.manifest_hash, "new456");
        assert_eq!(
            pending.staged_manifest_key,
            "data/staging/manifests/txn-1.json"
        );
    }

    #[test]
    fn legacy_serializer_roundtrips() {
        let entry = super::RemoteIndexEntry::new("abc123", 1024, 2);
        let bytes = entry.to_legacy_bytes();
        let reparsed = parse_index_entry(&bytes).unwrap();
        assert_eq!(reparsed, entry);
    }

    #[test]
    fn versioned_serializer_roundtrips() {
        let entry = super::VersionedIndexEntry::preparing(
            Some(super::RemoteIndexEntry::new("old123", 10, 1)),
            super::PendingIndexEntry::new("new456", 11, 1, "data/staging/manifests/txn-1.json"),
        );

        let bytes = entry.to_json_bytes().unwrap();
        match parse_index_entry_record(&bytes).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected v2 entry"),
            ParsedIndexEntry::V2(reparsed) => assert_eq!(reparsed, entry),
        }
    }

    #[test]
    fn deleted_serializer_uses_v4_and_has_no_visible_entry() {
        let entry = super::VersionedIndexEntry::deleted();
        let bytes = entry.to_json_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(r#""version": 4"#));
        assert!(text.contains(r#""state": "deleted""#));

        let parsed = parse_index_entry_record(&bytes).unwrap();
        assert_eq!(parsed.state(), IndexEntryState::Deleted);
        assert!(parsed.visible_entry().is_none());
        assert!(parsed.pending_entry().is_none());
        assert!(parse_index_entry(&bytes).is_err());

        let invalid_v4 = br#"{
            "version": 4,
            "state": "committed",
            "current": {"manifest_hash": "abc123", "size": 1, "chunks": 1}
        }"#;
        assert!(parse_index_entry_record(invalid_v4).is_err());
    }

    #[test]
    fn deleted_v4_roundtrips_exact_trash_evidence() {
        let evidence_bytes = b"manifest_hash=abc123\nsize=10\nchunks=1\n";
        let evidence = super::DeletionEvidence::for_trash_generation(
            "data",
            "doc.txt",
            "data/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt",
            evidence_bytes,
        )
        .unwrap();
        let entry = super::VersionedIndexEntry::deleted_with_evidence(evidence.clone());
        let bytes = entry.to_json_bytes().unwrap();
        let parsed = parse_index_entry_record(&bytes).unwrap();

        assert_eq!(parsed.state(), IndexEntryState::Deleted);
        assert_eq!(parsed.deletion_evidence(), Some(&evidence));
        assert!(evidence
            .matches_trash_generation(
                "data",
                "doc.txt",
                "data/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/doc.txt",
                evidence_bytes,
            )
            .unwrap());

        let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        tampered["deletion_evidence"]["safety_copy_blake3"] = serde_json::json!("NOT-A-DIGEST");
        assert!(parse_index_entry_record(&serde_json::to_vec(&tampered).unwrap()).is_err());
    }

    #[tokio::test]
    async fn exact_path_state_distinguishes_missing_deleted_and_live() {
        let op = memory_op();
        let key = "data/index/doc.txt";

        assert_eq!(
            super::read_exact_index_path_state(&op, "data", "doc.txt")
                .await
                .unwrap(),
            super::ExactIndexPathState::Missing
        );

        op.write(
            key,
            VersionedIndexEntry::committed(RemoteIndexEntry::new("abc123", 10, 1))
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            super::read_exact_index_path_state(&op, "data", "doc.txt")
                .await
                .unwrap(),
            super::ExactIndexPathState::Live
        );

        op.write(key, VersionedIndexEntry::deleted().to_json_bytes().unwrap())
            .await
            .unwrap();
        assert_eq!(
            super::read_exact_index_path_state(&op, "data", "doc.txt")
                .await
                .unwrap(),
            super::ExactIndexPathState::Deleted
        );
    }

    #[tokio::test]
    async fn exact_tombstone_and_safety_copy_restore_roundtrip() {
        let op = memory_op();
        let key = "data/index/doc.txt";
        let original = RemoteIndexEntry::new("abc123", 10, 1).to_legacy_bytes();
        op.write(key, original.clone()).await.unwrap();

        let previous = super::tombstone_index_entry_if_exact(&op, "data", key, &original)
            .await
            .unwrap();
        assert_eq!(previous.visible_entry().unwrap().manifest_hash, "abc123");
        let deleted = super::read_index_entry_record_from_store(&op, key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(deleted.state(), IndexEntryState::Deleted);

        super::restore_index_entry_from_safety_copy(&op, "data", key, &original)
            .await
            .unwrap();
        assert_eq!(op.read(key).await.unwrap().to_vec(), original);
    }

    #[tokio::test]
    async fn exact_tombstone_preserves_a_concurrent_replacement() {
        let op = memory_op();
        let key = "data/index/doc.txt";
        let snapshot = RemoteIndexEntry::new("old123", 10, 1).to_legacy_bytes();
        let concurrent = RemoteIndexEntry::new("new456", 11, 1).to_legacy_bytes();
        op.write(key, snapshot.clone()).await.unwrap();
        op.write(key, concurrent.clone()).await.unwrap();

        let error = super::tombstone_index_entry_if_exact(&op, "data", key, &snapshot)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("changed before logical delete"));
        assert_eq!(op.read(key).await.unwrap().to_vec(), concurrent);
    }

    #[tokio::test]
    async fn directory_marker_tombstone_restore_and_retry_are_logically_visible() {
        let op = memory_op();
        let key = "data/index/empty/.tcfs_dir";
        op.write(key, super::DIRECTORY_MARKER_BYTES.to_vec())
            .await
            .unwrap();
        assert!(super::directory_marker_is_visible(&op, key).await.unwrap());

        super::tombstone_directory_marker_if_exact(&op, "data", key, super::DIRECTORY_MARKER_BYTES)
            .await
            .unwrap();
        assert!(!super::directory_marker_is_visible(&op, key).await.unwrap());

        super::restore_index_entry_from_safety_copy(
            &op,
            "data",
            key,
            super::DIRECTORY_MARKER_BYTES,
        )
        .await
        .unwrap();
        assert!(super::directory_marker_is_visible(&op, key).await.unwrap());

        // Model a crash after the live marker write but before the caller's
        // lifecycle-completion record. Retrying against the exact raw marker
        // must be an idempotent conditional write, not a record-parse error.
        super::restore_index_entry_from_safety_copy(
            &op,
            "data",
            key,
            super::DIRECTORY_MARKER_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(
            op.read(key).await.unwrap().to_vec(),
            super::DIRECTORY_MARKER_BYTES
        );
    }

    #[test]
    fn symlink_index_entry_uses_v3_and_roundtrips() {
        let entry = super::VersionedIndexEntry::committed(super::RemoteIndexEntry::new_symlink(
            "linkhash",
            "../target.txt",
        ));

        let bytes = entry.to_json_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(r#""version": 3"#));
        assert!(text.contains(r#""kind": "symlink""#));

        match parse_index_entry_record(&bytes).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected v3 entry"),
            ParsedIndexEntry::V2(reparsed) => assert_eq!(reparsed, entry),
        }

        let visible = parse_index_entry(&bytes).unwrap();
        assert!(visible.is_symlink());
        assert_eq!(visible.symlink_target.as_deref(), Some("../target.txt"));
    }

    #[test]
    fn symlink_index_entry_requires_v3_and_target() {
        let v2_symlink = br#"{
            "version": 2,
            "state": "committed",
            "current": {
                "manifest_hash": "linkhash",
                "size": 13,
                "chunks": 0,
                "kind": "symlink",
                "symlink_target": "../target.txt"
            }
        }"#;
        assert!(parse_index_entry_record(v2_symlink).is_err());

        let missing_target = br#"{
            "version": 3,
            "state": "committed",
            "current": {
                "manifest_hash": "linkhash",
                "size": 13,
                "chunks": 0,
                "kind": "symlink"
            }
        }"#;
        assert!(parse_index_entry_record(missing_target).is_err());
    }

    #[test]
    fn preparing_entry_without_current_is_not_visible() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "pending": {
                "manifest_hash": "new456",
                "size": 11,
                "chunks": 1,
                "staged_manifest_key": "data/staging/manifests/txn-1.json"
            }
        }"#;

        let parsed = parse_index_entry_record(data).unwrap();
        assert!(parsed.visible_entry().is_none());
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn committed_entry_missing_current_errors() {
        let data = br#"{
            "version": 2,
            "state": "committed"
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn preparing_entry_missing_pending_errors() {
        let data = br#"{
            "version": 2,
            "state": "preparing",
            "current": {
                "manifest_hash": "old123",
                "size": 10,
                "chunks": 1
            }
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn unsupported_version_errors() {
        let data = br#"{
            "version": 4,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1,
                "chunks": 1
            }
        }"#;

        assert!(parse_index_entry_record(data).is_err());
    }

    #[test]
    fn malformed_legacy_size_errors() {
        let data = b"manifest_hash=abc123\nsize=notanumber\nchunks=5\n";
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn malformed_legacy_chunks_errors() {
        let data = b"manifest_hash=abc123\nsize=1024\nchunks=xyz\n";
        assert!(parse_index_entry(data).is_err());
    }

    #[test]
    fn parsed_entry_keeps_v2_shape() {
        let data = br#"{
            "version": 2,
            "state": "committed",
            "current": {
                "manifest_hash": "abc123",
                "size": 1,
                "chunks": 1
            }
        }"#;

        match parse_index_entry_record(data).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected v2 entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert!(entry.current.is_some());
            }
        }
    }

    #[tokio::test]
    async fn resolve_preparing_entry_rolls_forward_from_staged_manifest() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let manifest_prefix = "data/manifests";
        let staged_bytes = br#"{"version":2,"file_hash":"new456","file_size":11,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0,"rel_path":"doc.txt"}"#.to_vec();
        let pending_hash = manifest_object_id(&staged_bytes);
        let staged_key = format!(
            "data/staging/manifests/00000000-0000-4000-8000-000000000001-{pending_hash}.json"
        );

        op.write(&staged_key, staged_bytes).await.unwrap();

        write_preparing_index_entry(
            &op,
            index_key,
            Some(RemoteIndexEntry::new("old123", 10, 1)),
            PendingIndexEntry::new(&pending_hash, 11, 0, &staged_key),
        )
        .await
        .unwrap();

        let visible = resolve_visible_index_entry(&op, index_key, manifest_prefix)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.manifest_hash, pending_hash);
        assert!(op
            .exists(&manifest_key(manifest_prefix, &visible.manifest_hash))
            .await
            .unwrap());
        assert!(op.exists(&staged_key).await.unwrap());

        match parse_index_entry_record(&op.read(index_key).await.unwrap().to_vec()).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected v2 committed entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry, VersionedIndexEntry::committed(visible));
            }
        }
    }

    #[tokio::test]
    async fn resolve_preparing_entry_keeps_current_when_pending_is_missing() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let manifest_prefix = "data/manifests";

        op.write(
            &manifest_key(manifest_prefix, "old123"),
            br#"{"version":2,"file_hash":"old123","file_size":10,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        write_preparing_index_entry(
            &op,
            index_key,
            Some(RemoteIndexEntry::new("old123", 10, 1)),
            PendingIndexEntry::new(
                "new456",
                11,
                1,
                "data/staging/manifests/00000000-0000-4000-8000-000000000002-new456.json",
            ),
        )
        .await
        .unwrap();

        let visible = resolve_visible_index_entry(&op, index_key, manifest_prefix)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.manifest_hash, "old123");
    }

    #[test]
    fn parsed_index_rejects_unsafe_manifest_storage_components() {
        for manifest_hash in ["", ".", "..", "../other", "nested/hash", "nested\\hash"] {
            let legacy = format!("manifest_hash={manifest_hash}\nsize=1\nchunks=1\n");
            assert!(
                parse_index_entry(legacy.as_bytes()).is_err(),
                "unsafe legacy manifest hash must fail: {manifest_hash:?}"
            );

            let json = format!(
                r#"{{"version":2,"state":"committed","current":{{"manifest_hash":"{manifest_hash}","size":1,"chunks":1}}}}"#
            );
            assert!(
                parse_index_entry_record(json.as_bytes()).is_err(),
                "unsafe JSON manifest hash must fail: {manifest_hash:?}"
            );
        }
    }

    #[test]
    fn index_writer_rejects_incoherent_or_log_injecting_kind_metadata() {
        let mut regular = RemoteIndexEntry::new("manifest", 1, 0);
        regular.symlink_target = Some("unexpected".into());
        assert!(VersionedIndexEntry::committed(regular)
            .to_json_bytes()
            .is_err());

        let symlink = RemoteIndexEntry::new_symlink("manifest", "safe\nforged-log");
        assert!(VersionedIndexEntry::committed(symlink)
            .to_json_bytes()
            .is_err());
    }

    #[tokio::test]
    async fn pending_recovery_rejects_cross_prefix_staging_without_touching_victim() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let victim_key = "other/staging/manifests/00000000-0000-4000-8000-000000000003-new456.json";
        let victim_bytes = b"other root private manifest".to_vec();
        op.write(victim_key, victim_bytes.clone()).await.unwrap();
        write_preparing_index_entry(
            &op,
            index_key,
            None,
            PendingIndexEntry::new("new456", 11, 1, victim_key),
        )
        .await
        .unwrap();
        let original_index = op.read(index_key).await.unwrap().to_vec();

        let error = resolve_visible_index_entry(&op, index_key, "data/manifests")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("escapes its root staging namespace"));
        assert_eq!(op.read(victim_key).await.unwrap().to_vec(), victim_bytes);
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original_index);
        assert!(!op.exists("data/manifests/new456").await.unwrap());
    }

    #[tokio::test]
    async fn pending_recovery_rejects_staged_content_id_mismatch_without_committing() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let pending_hash = "new456";
        let staged_key = format!(
            "data/staging/manifests/00000000-0000-4000-8000-000000000004-{pending_hash}.json"
        );
        let staged_bytes = br#"{"version":2,"file_hash":"different-content","file_size":11,"chunks":[],"vclock":{"clocks":{}},"written_by":"peer","written_at":0,"rel_path":"doc.txt"}"#.to_vec();
        let staged_object_id = manifest_object_id(&staged_bytes);
        assert_ne!(staged_object_id, pending_hash);
        op.write(&staged_key, staged_bytes.clone()).await.unwrap();
        write_preparing_index_entry(
            &op,
            index_key,
            None,
            PendingIndexEntry::new(pending_hash, 11, 0, &staged_key),
        )
        .await
        .unwrap();
        let original_index = op.read(index_key).await.unwrap().to_vec();

        let error = resolve_visible_index_entry(&op, index_key, "data/manifests")
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("pending manifest content id mismatch")
                && error.contains(pending_hash)
                && error.contains(&staged_object_id),
            "unexpected recovery error: {error}"
        );
        assert_eq!(op.read(&staged_key).await.unwrap().to_vec(), staged_bytes);
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original_index);
        assert!(!op
            .exists(&manifest_key("data/manifests", pending_hash))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn pending_recovery_accepts_exactly_bound_legacy_symlink_addressing() {
        let op = memory_op();
        let index_key = "data/index/link";
        let target = "sibling.txt";
        let legacy_id = legacy_symlink_target_id(target);
        let staged_key =
            format!("data/staging/manifests/00000000-0000-4000-8000-000000000005-{legacy_id}.json");
        let manifest_bytes = format!(
            r#"{{"version":3,"kind":"symlink","symlink_target":"{target}","vclock":{{"clocks":{{}}}},"written_by":"peer","written_at":0,"rel_path":"link"}}"#
        )
        .into_bytes();
        op.write(&staged_key, manifest_bytes).await.unwrap();
        let entry = RemoteIndexEntry::new_symlink(&legacy_id, target);
        write_preparing_index_entry(
            &op,
            index_key,
            None,
            PendingIndexEntry::from_remote_entry(&entry, &staged_key),
        )
        .await
        .unwrap();

        let visible = resolve_visible_index_entry(&op, index_key, "data/manifests")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(visible, entry);
        assert!(op
            .exists(&manifest_key("data/manifests", &legacy_id))
            .await
            .unwrap());
        assert!(op.exists(&staged_key).await.unwrap());
    }

    #[tokio::test]
    async fn pending_recovery_accepts_exactly_bound_legacy_regular_addressing() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let legacy_id = "legacy-file-hash";
        let staged_key =
            format!("data/staging/manifests/00000000-0000-4000-8000-000000000006-{legacy_id}.json");
        let manifest_bytes = format!(
            r#"{{"version":2,"file_hash":"{legacy_id}","file_size":0,"chunks":[],"vclock":{{"clocks":{{}}}},"written_by":"peer","written_at":0,"rel_path":"doc.txt"}}"#
        )
        .into_bytes();
        assert_ne!(manifest_object_id(&manifest_bytes), legacy_id);
        op.write(&staged_key, manifest_bytes).await.unwrap();
        let entry = RemoteIndexEntry::new(legacy_id, 0, 0);
        write_preparing_index_entry(
            &op,
            index_key,
            None,
            PendingIndexEntry::from_remote_entry(&entry, &staged_key),
        )
        .await
        .unwrap();

        let visible = resolve_visible_index_entry(&op, index_key, "data/manifests")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(visible, entry);
        assert!(op
            .exists(&manifest_key("data/manifests", legacy_id))
            .await
            .unwrap());
        assert!(op.exists(&staged_key).await.unwrap());
    }

    #[tokio::test]
    async fn legacy_recovery_rejects_distinct_envelopes_with_same_file_hash() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let legacy_id = "shared-file-hash";
        let staged_key =
            format!("data/staging/manifests/00000000-0000-4000-8000-000000000008-{legacy_id}.json");
        let staged_bytes = format!(
            r#"{{"version":2,"file_hash":"{legacy_id}","file_size":0,"chunks":[],"vclock":{{"clocks":{{"peer-a":1}}}},"written_by":"peer-a","written_at":1,"rel_path":"doc.txt"}}"#
        )
        .into_bytes();
        let collided_bytes = format!(
            r#"{{"version":2,"file_hash":"{legacy_id}","file_size":0,"chunks":[],"vclock":{{"clocks":{{"peer-b":1}}}},"written_by":"peer-b","written_at":2,"rel_path":"doc.txt"}}"#
        )
        .into_bytes();
        assert_ne!(staged_bytes, collided_bytes);
        assert_ne!(manifest_object_id(&staged_bytes), legacy_id);
        assert_ne!(manifest_object_id(&collided_bytes), legacy_id);

        op.write(&staged_key, staged_bytes.clone()).await.unwrap();
        op.write(
            &manifest_key("data/manifests", legacy_id),
            collided_bytes.clone(),
        )
        .await
        .unwrap();
        write_preparing_index_entry(
            &op,
            index_key,
            None,
            PendingIndexEntry::new(legacy_id, 0, 0, &staged_key),
        )
        .await
        .unwrap();

        let error = resolve_visible_index_entry(&op, index_key, "data/manifests")
            .await
            .unwrap_err();
        assert!(format!("{error:#}")
            .contains("existing immutable manifest differs from staged recovery bytes"));
        assert_eq!(op.read(&staged_key).await.unwrap().to_vec(), staged_bytes);
        assert_eq!(
            op.read(&manifest_key("data/manifests", legacy_id))
                .await
                .unwrap()
                .to_vec(),
            collided_bytes
        );
        match parse_index_entry_record(&op.read(index_key).await.unwrap().to_vec()).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Preparing);
                assert_eq!(entry.pending.unwrap().manifest_hash, legacy_id);
            }
        }
    }

    #[tokio::test]
    async fn preparing_recovery_does_not_overwrite_concurrent_newer_index() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let manifest_prefix = "data/manifests";
        let pending_bytes = br#"{"version":2,"file_hash":"pending-file","file_size":11,"chunks":[],"vclock":{"clocks":{}},"written_by":"peer-a","written_at":0,"rel_path":"doc.txt"}"#.to_vec();
        let pending_hash = manifest_object_id(&pending_bytes);
        let staged_key = format!(
            "data/staging/manifests/00000000-0000-4000-8000-000000000007-{pending_hash}.json"
        );
        op.write(
            &manifest_key(manifest_prefix, &pending_hash),
            pending_bytes.clone(),
        )
        .await
        .unwrap();
        op.write(&staged_key, pending_bytes).await.unwrap();
        write_preparing_index_entry(
            &op,
            index_key,
            Some(RemoteIndexEntry::new("old123", 10, 1)),
            PendingIndexEntry::new(&pending_hash, 11, 0, &staged_key),
        )
        .await
        .unwrap();

        let newer = RemoteIndexEntry::new("newer789", 12, 1);
        let race_op = op.clone();
        let race_entry = newer.clone();
        let error =
            resolve_visible_index_entry_with_hook(&op, index_key, manifest_prefix, move || {
                let op = race_op.clone();
                let entry = race_entry.clone();
                async move { write_committed_index_entry(&op, index_key, &entry).await }
            })
            .await
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("refusing to overwrite a newer publisher"),
            "unexpected recovery error: {error:#}"
        );
        match parse_index_entry_record(&op.read(index_key).await.unwrap().to_vec()).unwrap() {
            ParsedIndexEntry::Legacy(_) => panic!("expected newer versioned index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry, VersionedIndexEntry::committed(newer));
            }
        }
        assert!(
            op.exists(&staged_key).await.unwrap(),
            "failed recovery must preserve its staging evidence"
        );
    }
}
