//! Held, bounded Git topology evidence for one `git-raw-v1` root.
//!
//! This module proves only that one already-authorized local root exposed the
//! same ordinary standalone Git topology and canonical ref set in two bounded
//! observations around the external state/remote read window. It does not
//! prove remote Git object semantics, fast-forward ancestry, catalog
//! completeness/currentness, bootstrap safety, continuous stability, or
//! writer fencing against same-principal swap/restore. The opaque artifact
//! therefore has no digest, serialization, clone, plan, or action conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::ExitStatus;

use tcfs_core::config::{RootGitObservationContractV1, RootGitPolicyV1};

use crate::conflict_git::{GitRepoAnchor, HeldGitReadErrorV1, HeldGitReadQueryV1};
use crate::index_entry::portable_casefold_path;
use crate::registered_local_snapshot::{
    PendingStrictLocalSnapshotV1, StrictInitialLocalEntryKindV1,
    StrictLocalSnapshotIncompleteKindV1, StrictLocalSnapshotIncompleteV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrictGitRawTopologyIncompleteKindV1 {
    UnsupportedProfile,
    CapabilityOpenFailed,
    TopologyRejected,
    ActivityDetected,
    ReferenceReadFailed,
    ReferenceMalformed,
    ReferenceResourceLimit,
    ReferenceSetChanged,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StrictGitRawTopologyIncompleteV1 {
    kind: StrictGitRawTopologyIncompleteKindV1,
    operation: &'static str,
}

impl StrictGitRawTopologyIncompleteV1 {
    pub(crate) const fn kind(&self) -> StrictGitRawTopologyIncompleteKindV1 {
        self.kind
    }

    pub(crate) const fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Debug for StrictGitRawTopologyIncompleteV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictGitRawTopologyIncompleteV1")
            .field("kind", &self.kind)
            .field("operation", &self.operation)
            .finish()
    }
}

fn incomplete(
    kind: StrictGitRawTopologyIncompleteKindV1,
    operation: &'static str,
) -> StrictGitRawTopologyIncompleteV1 {
    StrictGitRawTopologyIncompleteV1 { kind, operation }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrictGitObjectFormatV1 {
    Sha1,
    Sha256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StrictGitObjectKindV1 {
    Commit,
    Tree,
    Blob,
    Tag,
}

#[derive(PartialEq, Eq)]
pub(crate) struct StrictGitRefPinV1 {
    ref_name: String,
    object_id: String,
    object_kind: StrictGitObjectKindV1,
    symref_target: Option<String>,
}

impl StrictGitRefPinV1 {
    pub(crate) fn ref_name(&self) -> &str {
        &self.ref_name
    }

    pub(crate) fn object_id(&self) -> &str {
        &self.object_id
    }

    pub(crate) fn symref_target(&self) -> Option<&str> {
        self.symref_target.as_deref()
    }
}

impl fmt::Debug for StrictGitRefPinV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictGitRefPinV1")
            .field("ref_name", &self.ref_name)
            .field("object_id", &self.object_id)
            .field("object_kind", &self.object_kind)
            .field("symref_target", &self.symref_target)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum StrictGitHeadPinV1 {
    Symbolic {
        target: String,
        resolved_object_id: Option<String>,
    },
    Detached {
        object_id: String,
    },
}

impl StrictGitHeadPinV1 {
    pub(crate) fn symbolic_target(&self) -> Option<&str> {
        match self {
            Self::Symbolic { target, .. } => Some(target),
            Self::Detached { .. } => None,
        }
    }

    pub(crate) fn resolved_object_id(&self) -> Option<&str> {
        match self {
            Self::Symbolic {
                resolved_object_id, ..
            } => resolved_object_id.as_deref(),
            Self::Detached { object_id } => Some(object_id),
        }
    }
}

impl fmt::Debug for StrictGitHeadPinV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbolic {
                target,
                resolved_object_id,
            } => formatter
                .debug_struct("Symbolic")
                .field("target", target)
                .field("resolved_object_id", resolved_object_id)
                .finish(),
            Self::Detached { object_id } => formatter
                .debug_struct("Detached")
                .field("object_id", object_id)
                .finish(),
        }
    }
}

#[derive(PartialEq, Eq)]
struct StrictGitRawTopologySnapshotV1 {
    object_format: StrictGitObjectFormatV1,
    head: StrictGitHeadPinV1,
    refs: Vec<StrictGitRefPinV1>,
}

/// First local Git observation plus the descriptor capabilities that performed
/// it. Only an exact second pass can promote this to held local evidence.
pub(crate) struct PendingStrictGitRawTopologyV1 {
    anchor: GitRepoAnchor,
    contract: RootGitObservationContractV1,
    metadata: GitMetadataInventoryV1,
    first: StrictGitRawTopologySnapshotV1,
}

/// Exact two-pass local Git topology evidence retained under the same root and
/// `.git` descriptors.
///
/// "Complete" is intentionally not in this type name: this is one local input
/// prerequisite, not complete registered-root truth.
pub(crate) struct HeldStrictGitRawTopologyV1 {
    anchor: GitRepoAnchor,
    contract: RootGitObservationContractV1,
    snapshot: StrictGitRawTopologySnapshotV1,
}

impl HeldStrictGitRawTopologyV1 {
    pub(crate) const fn object_format(&self) -> StrictGitObjectFormatV1 {
        self.snapshot.object_format
    }

    pub(crate) fn head(&self) -> &StrictGitHeadPinV1 {
        &self.snapshot.head
    }

    pub(crate) fn refs(&self) -> impl ExactSizeIterator<Item = &StrictGitRefPinV1> {
        self.snapshot.refs.iter()
    }

    pub(crate) const fn contract(&self) -> RootGitObservationContractV1 {
        self.contract
    }

    /// Recheck only the held root and `.git` identities after inventory C.
    ///
    /// Ref equality is intentionally a two-pass contract. The local inventory
    /// C bracket separately checks the raw `.git` identities/content
    /// acquisition; this final check ensures the retained descriptors were not
    /// replaced while it ran.
    pub(crate) fn revalidate_capabilities(&self) -> Result<(), StrictGitRawTopologyIncompleteV1> {
        self.anchor.revalidate().map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                "revalidate-git-capabilities-after-local-inventory",
            )
        })
    }
}

impl fmt::Debug for HeldStrictGitRawTopologyV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeldStrictGitRawTopologyV1")
            .field("canonical_root", &self.anchor.canonical_root())
            .field("contract", &self.contract)
            .field("object_format", &self.snapshot.object_format)
            .field("head", &self.snapshot.head)
            .field("ref_count", &self.snapshot.refs.len())
            .finish_non_exhaustive()
    }
}

pub(crate) enum StrictGitRawTopologyBeginV1 {
    Pending(Box<PendingStrictGitRawTopologyV1>),
    Incomplete(StrictGitRawTopologyIncompleteV1),
}

pub(crate) enum StrictGitRawTopologyFinishV1 {
    Held(HeldStrictGitRawTopologyV1),
    Incomplete(StrictGitRawTopologyIncompleteV1),
}

pub(crate) fn begin_strict_git_raw_topology_v1(
    local: &PendingStrictLocalSnapshotV1,
) -> StrictGitRawTopologyBeginV1 {
    let profile = local.profile();
    let policy = profile.policy().settings().git_policy();
    let contract = policy.observation_contract();
    if policy != RootGitPolicyV1::StandaloneRawWithFastForwardProofV1
        || !contract.is_applicable()
        || contract.pass_count() != 2
        || contract.retry_count() != 0
    {
        return StrictGitRawTopologyBeginV1::Incomplete(incomplete(
            StrictGitRawTopologyIncompleteKindV1::UnsupportedProfile,
            "select-git-observation-contract",
        ));
    }

    let directory = match local.try_clone_root_descriptor() {
        Ok(directory) => directory,
        Err(_) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::CapabilityOpenFailed,
                "clone-held-root-descriptor",
            ));
        }
    };
    let anchor = match GitRepoAnchor::capture_from_authorized_root(
        local.canonical_local_root(),
        directory,
    ) {
        Ok(anchor) => anchor,
        Err(_) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::CapabilityOpenFailed,
                "capture-held-git-descriptors",
            ));
        }
    };
    let limits = GitObservationLimitsV1::from(contract);
    let metadata = match capture_initial_git_metadata_v1(local, &anchor, limits) {
        Ok(metadata) => metadata,
        Err(incomplete) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete);
        }
    };
    let first = match read_topology_snapshot_v1(&anchor, &metadata, contract) {
        Ok(snapshot) => snapshot,
        Err(incomplete) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete);
        }
    };
    StrictGitRawTopologyBeginV1::Pending(Box::new(PendingStrictGitRawTopologyV1 {
        anchor,
        contract,
        metadata,
        first,
    }))
}

impl PendingStrictGitRawTopologyV1 {
    pub(crate) fn revalidate_after_external_reads(self) -> StrictGitRawTopologyFinishV1 {
        let second = match read_topology_snapshot_v1(&self.anchor, &self.metadata, self.contract) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                    "repeat-git-topology-observation",
                ));
            }
        };
        if self.first != second {
            return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                "compare-git-topology-passes",
            ));
        }
        StrictGitRawTopologyFinishV1::Held(HeldStrictGitRawTopologyV1 {
            anchor: self.anchor,
            contract: self.contract,
            snapshot: second,
        })
    }
}

#[derive(Clone, Copy)]
struct GitObservationLimitsV1 {
    max_refs: u64,
    max_ref_or_symref_bytes: u64,
    max_retained_bytes: u64,
    max_command_stdout_bytes: u64,
    max_config_file_bytes: u64,
    max_head_file_bytes: u64,
    sha1_oid_hex_bytes: u8,
    sha256_oid_hex_bytes: u8,
}

impl From<RootGitObservationContractV1> for GitObservationLimitsV1 {
    fn from(contract: RootGitObservationContractV1) -> Self {
        Self {
            max_refs: contract.max_refs(),
            max_ref_or_symref_bytes: contract.max_ref_or_symref_bytes(),
            max_retained_bytes: contract.max_retained_bytes(),
            max_command_stdout_bytes: contract.max_command_stdout_bytes(),
            max_config_file_bytes: contract.max_config_file_bytes(),
            max_head_file_bytes: contract.max_head_file_bytes(),
            sha1_oid_hex_bytes: contract.sha1_oid_hex_bytes(),
            sha256_oid_hex_bytes: contract.sha256_oid_hex_bytes(),
        }
    }
}

#[derive(PartialEq, Eq)]
enum RawLooseRefValueV1 {
    DirectObjectId(Vec<u8>),
    SymbolicTarget(String),
}

struct GitMetadataInventoryV1 {
    loose_refs: BTreeMap<String, RawLooseRefValueV1>,
    packed_refs: Option<Vec<u8>>,
    raw_head: Vec<u8>,
}

fn path_is_or_descends_v1(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn map_held_local_read_failure_v1(
    failure: StrictLocalSnapshotIncompleteV1,
    operation: &'static str,
) -> StrictGitRawTopologyIncompleteV1 {
    let kind = match failure.kind() {
        StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded => {
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        }
        StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed => {
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed
        }
        StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead => {
            StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged
        }
        _ => StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
    };
    incomplete(kind, operation)
}

#[cfg(unix)]
fn effective_principal_uid_v1() -> Option<u32> {
    // SAFETY: `geteuid` has no preconditions and only reads process identity.
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_principal_uid_v1() -> Option<u32> {
    None
}

fn capture_initial_git_metadata_v1(
    local: &PendingStrictLocalSnapshotV1,
    anchor: &GitRepoAnchor,
    limits: GitObservationLimitsV1,
) -> Result<GitMetadataInventoryV1, StrictGitRawTopologyIncompleteV1> {
    anchor.revalidate().map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
            "revalidate-before-raw-git-inventory",
        )
    })?;

    let effective_uid = effective_principal_uid_v1().ok_or_else(|| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::CapabilityOpenFailed,
            "unsupported-git-metadata-principal-platform",
        )
    })?;
    let mut saw_git_directory = false;
    let mut saw_refs_directory = false;
    let mut saw_objects_directory = false;
    let mut loose_refs = BTreeMap::new();
    let mut folded_refs = BTreeSet::new();
    let mut retained_bytes = 0_u64;
    let mut packed_refs = None;
    let mut raw_head = None;
    let mut saw_config = false;

    for entry in local.initial_inventory_entries() {
        let raw_path = entry.rel_path();
        if raw_path != b".git" && !raw_path.starts_with(b".git/") {
            continue;
        }
        let path = std::str::from_utf8(raw_path).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "decode-raw-git-metadata-path",
            )
        })?;
        let trusted_metadata = path == ".git"
            || path_is_or_descends_v1(path, ".git/refs")
            || path_is_or_descends_v1(path, ".git/objects")
            || path_is_or_descends_v1(path, ".git/logs")
            || matches!(
                path,
                ".git/HEAD" | ".git/config" | ".git/index" | ".git/packed-refs"
            );
        if trusted_metadata
            && (matches!(
                entry.kind(),
                StrictInitialLocalEntryKindV1::Symlink | StrictInitialLocalEntryKindV1::Special
            ) || entry.uid() != effective_uid
                || entry.mode() & 0o022 != 0
                || crate::path_acl::reject_write_grant_acl(&anchor.canonical_root().join(path))
                    .is_err())
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "validate-bounded-git-metadata-trust",
            ));
        }

        match path {
            ".git" => {
                saw_git_directory = entry.kind() == StrictInitialLocalEntryKindV1::Directory;
            }
            ".git/refs" => {
                saw_refs_directory = entry.kind() == StrictInitialLocalEntryKindV1::Directory;
            }
            ".git/objects" => {
                saw_objects_directory = entry.kind() == StrictInitialLocalEntryKindV1::Directory;
            }
            ".git/config" => {
                if entry.kind() != StrictInitialLocalEntryKindV1::Regular || entry.size() < 0 {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                        "require-regular-git-config",
                    ));
                }
                let size = u64::try_from(entry.size()).map_err(|_| {
                    incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "validate-git-config-size",
                    )
                })?;
                if size > limits.max_config_file_bytes {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                        "maximum-git-config-file-bytes",
                    ));
                }
                saw_config = true;
            }
            ".git/HEAD" => {
                if entry.kind() != StrictInitialLocalEntryKindV1::Regular
                    || entry.size() < 0
                    || raw_head.is_some()
                {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                        "require-regular-git-head",
                    ));
                }
                let size = u64::try_from(entry.size()).map_err(|_| {
                    incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "validate-git-head-size",
                    )
                })?;
                if size > limits.max_head_file_bytes {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                        "maximum-git-head-file-bytes",
                    ));
                }
                raw_head = Some(
                    local
                        .read_initial_regular_file_bounded(raw_path, limits.max_head_file_bytes)
                        .map_err(|failure| {
                            map_held_local_read_failure_v1(failure, "read-held-git-head")
                        })?,
                );
            }
            _ => {}
        }

        if matches!(
            path,
            ".git/commondir"
                | ".git/config.worktree"
                | ".git/shallow"
                | ".git/objects/info/alternates"
                | ".git/objects/info/http-alternates"
        ) || path_is_or_descends_v1(path, ".git/worktrees")
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "reject-git-topology-extension",
            ));
        }
        if matches!(
            path,
            ".git/gc.pid"
                | ".git/shallow.lock"
                | ".git/MERGE_HEAD"
                | ".git/CHERRY_PICK_HEAD"
                | ".git/BISECT_LOG"
                | ".git/REVERT_HEAD"
        ) || path_is_or_descends_v1(path, ".git/rebase-merge")
            || path_is_or_descends_v1(path, ".git/rebase-apply")
            || path_is_or_descends_v1(path, ".git/sequencer")
            || (path != ".git/tcfs.lock"
                && path.starts_with(".git/")
                && path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.ends_with(".lock")))
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
                "reject-native-git-activity",
            ));
        }

        if path == ".git/packed-refs" {
            if entry.kind() != StrictInitialLocalEntryKindV1::Regular || packed_refs.is_some() {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                    "require-regular-packed-refs",
                ));
            }
            let size = u64::try_from(entry.size()).map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "validate-packed-refs-size",
                )
            })?;
            if size > limits.max_command_stdout_bytes {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                    "maximum-packed-refs-bytes",
                ));
            }
            packed_refs = Some(
                local
                    .read_initial_regular_file_bounded(raw_path, limits.max_command_stdout_bytes)
                    .map_err(|failure| {
                        map_held_local_read_failure_v1(failure, "read-held-packed-refs")
                    })?,
            );
        }

        let Some(ref_name_bytes) = raw_path.strip_prefix(b".git/refs/") else {
            continue;
        };
        if entry.kind() == StrictInitialLocalEntryKindV1::Directory {
            continue;
        }
        if entry.kind() != StrictInitialLocalEntryKindV1::Regular {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "require-regular-loose-reference",
            ));
        }
        if u64::try_from(loose_refs.len()).unwrap_or(u64::MAX) >= limits.max_refs {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-loose-reference-count",
            ));
        }
        let full_ref_name_len = b"refs/"
            .len()
            .checked_add(ref_name_bytes.len())
            .ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                    "count-loose-reference-name-bytes",
                )
            })?;
        if full_ref_name_len > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX)
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-loose-reference-name-bytes",
            ));
        }
        let ref_name = std::str::from_utf8(ref_name_bytes).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "decode-loose-reference-name",
            )
        })?;
        let mut full_ref_name = String::new();
        full_ref_name
            .try_reserve_exact(full_ref_name_len)
            .map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                    "allocate-loose-reference-name",
                )
            })?;
        full_ref_name.push_str("refs/");
        full_ref_name.push_str(ref_name);
        let folded_ref = validate_ref_name_v1(&full_ref_name, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-loose-reference-name",
            )
        })?;
        if !folded_refs.insert(folded_ref.clone()) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-loose-reference-portable-alias",
            ));
        }
        let raw_value_limit = limits
            .max_ref_or_symref_bytes
            .max(u64::from(limits.sha256_oid_hex_bytes))
            .saturating_add(6);
        let size = u64::try_from(entry.size()).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-loose-reference-size",
            )
        })?;
        if size > raw_value_limit {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-loose-reference-value-bytes",
            ));
        }
        let bytes = local
            .read_initial_regular_file_bounded(raw_path, raw_value_limit)
            .map_err(|failure| {
                map_held_local_read_failure_v1(failure, "read-held-loose-reference")
            })?;
        let line = exact_single_line_v1(&bytes).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-loose-reference-line",
            )
        })?;
        let value = if let Some(target_bytes) = line.strip_prefix(b"ref: ") {
            if target_bytes.len()
                > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX)
            {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                    "maximum-loose-symref-target-bytes",
                ));
            }
            let target = std::str::from_utf8(target_bytes)
                .ok()
                .and_then(|target| validate_ref_name_v1(target, limits).map(|_| target.to_owned()))
                .ok_or_else(|| {
                    incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "validate-loose-symref-target",
                    )
                })?;
            RawLooseRefValueV1::SymbolicTarget(target)
        } else {
            if line.len() > usize::from(limits.sha256_oid_hex_bytes) {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                    "maximum-loose-object-id-bytes",
                ));
            }
            RawLooseRefValueV1::DirectObjectId(line.to_vec())
        };
        checked_retain_v1(&mut retained_bytes, full_ref_name.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, folded_ref.len(), limits)?;
        match &value {
            RawLooseRefValueV1::DirectObjectId(object_id) => {
                checked_retain_v1(&mut retained_bytes, object_id.len(), limits)?;
            }
            RawLooseRefValueV1::SymbolicTarget(target) => {
                checked_retain_v1(&mut retained_bytes, target.len(), limits)?;
            }
        }
        if loose_refs.insert(full_ref_name, value).is_some() {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-duplicate-loose-reference",
            ));
        }
    }

    if !saw_git_directory
        || !saw_refs_directory
        || !saw_objects_directory
        || !saw_config
        || raw_head.is_none()
    {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "require-standalone-git-layout",
        ));
    }
    anchor.revalidate().map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
            "revalidate-after-raw-git-inventory",
        )
    })?;
    Ok(GitMetadataInventoryV1 {
        loose_refs,
        packed_refs,
        raw_head: raw_head.expect("validated raw HEAD presence"),
    })
}

struct BoundedCommandOutputV1 {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_held_git_query_v1(
    anchor: &GitRepoAnchor,
    query: HeldGitReadQueryV1,
    stdout_limit: u64,
    operation: &'static str,
) -> Result<BoundedCommandOutputV1, StrictGitRawTopologyIncompleteV1> {
    match anchor.run_held_read_query(query, stdout_limit) {
        Ok(output) => {
            let (status, stdout) = output.into_parts();
            Ok(BoundedCommandOutputV1 { status, stdout })
        }
        Err(HeldGitReadErrorV1::OutputLimit) => Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            operation,
        )),
        Err(HeldGitReadErrorV1::Failed) => Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
            operation,
        )),
    }
}

fn require_only_false_config_values_v1(
    output: &BoundedCommandOutputV1,
    operation: &'static str,
) -> Result<(), StrictGitRawTopologyIncompleteV1> {
    match output.status.code() {
        Some(0) => {
            let values = output.stdout.strip_suffix(&[0]).ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    operation,
                )
            })?;
            if values.is_empty()
                || values
                    .split(|byte| *byte == 0)
                    .any(|value| value != b"false")
            {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                    operation,
                ));
            }
            Ok(())
        }
        Some(1) if output.stdout.is_empty() => Ok(()),
        _ => Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            operation,
        )),
    }
}

fn validate_bounded_git_routing_v1(
    anchor: &GitRepoAnchor,
    limits: GitObservationLimitsV1,
) -> Result<(), StrictGitRawTopologyIncompleteV1> {
    let config = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::EffectiveConfigNames,
        limits.max_command_stdout_bytes,
        "read-bounded-effective-git-config",
    )
    .map_err(|failure| {
        if failure.kind() == StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit {
            failure
        } else {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "read-bounded-effective-git-config",
            )
        }
    })?;
    if !config.status.success() || (!config.stdout.is_empty() && !config.stdout.ends_with(&[0])) {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "read-bounded-effective-git-config",
        ));
    }
    for raw_key in config
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        if raw_key.len() > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-effective-git-config-key-bytes",
            ));
        }
        let key = std::str::from_utf8(raw_key)
            .map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "decode-effective-git-config-key",
                )
            })?
            .to_ascii_lowercase();
        let remote_promisor = key.starts_with("remote.")
            && (key.ends_with(".promisor") || key.ends_with(".partialclonefilter"));
        if key == "extensions.partialclone"
            || key == "extensions.refstorage"
            || key == "extensions.worktreeconfig"
            || remote_promisor
            || key == "protocol.ext.allow"
            || key == "core.alternaterefscommand"
            || key == "core.worktree"
            || key == "include.path"
            || (key.starts_with("includeif.") && key.ends_with(".path"))
            || key.starts_with("fsck.")
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "reject-effective-git-config-key",
            ));
        }
    }

    let shared = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::SharedRepositoryValues,
        1024,
        "read-bounded-shared-repository-config",
    )
    .map_err(|failure| {
        if failure.kind() == StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit {
            failure
        } else {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "read-bounded-shared-repository-config",
            )
        }
    })?;
    require_only_false_config_values_v1(&shared, "reject-shared-repository-config")?;

    let bare = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::BareRepositoryValues,
        1024,
        "read-bounded-bare-repository-config",
    )
    .map_err(|failure| {
        if failure.kind() == StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit {
            failure
        } else {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "read-bounded-bare-repository-config",
            )
        }
    })?;
    require_only_false_config_values_v1(&bare, "reject-bare-repository-config")?;

    Ok(())
}

fn read_topology_snapshot_v1(
    anchor: &GitRepoAnchor,
    metadata: &GitMetadataInventoryV1,
    contract: RootGitObservationContractV1,
) -> Result<StrictGitRawTopologySnapshotV1, StrictGitRawTopologyIncompleteV1> {
    let limits = GitObservationLimitsV1::from(contract);
    validate_bounded_git_routing_v1(anchor, limits)?;
    let object_format = read_object_format_v1(anchor, limits)?;
    let refs = read_refs_v1(anchor, metadata, object_format, limits)?;
    let head = read_head_v1(anchor, metadata, object_format, &refs, limits)?;
    Ok(StrictGitRawTopologySnapshotV1 {
        object_format,
        head,
        refs,
    })
}

fn exact_single_line_v1(bytes: &[u8]) -> Option<&[u8]> {
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') || line.contains(&0) {
        None
    } else {
        Some(line)
    }
}

fn read_object_format_v1(
    anchor: &GitRepoAnchor,
    limits: GitObservationLimitsV1,
) -> Result<StrictGitObjectFormatV1, StrictGitRawTopologyIncompleteV1> {
    let output = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::ObjectFormat,
        limits.max_command_stdout_bytes.min(32),
        "read-git-object-format",
    )?;
    if !output.status.success() {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
            "read-git-object-format",
        ));
    }
    match exact_single_line_v1(&output.stdout) {
        Some(b"sha1") => Ok(StrictGitObjectFormatV1::Sha1),
        Some(b"sha256") => Ok(StrictGitObjectFormatV1::Sha256),
        _ => Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
            "parse-git-object-format",
        )),
    }
}

fn expected_oid_len_v1(format: StrictGitObjectFormatV1, limits: GitObservationLimitsV1) -> usize {
    match format {
        StrictGitObjectFormatV1::Sha1 => usize::from(limits.sha1_oid_hex_bytes),
        StrictGitObjectFormatV1::Sha256 => usize::from(limits.sha256_oid_hex_bytes),
    }
}

fn parse_object_id_v1(
    bytes: &[u8],
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Option<String> {
    (bytes.len() == expected_oid_len_v1(format, limits)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then(|| String::from_utf8(bytes.to_vec()).expect("validated ASCII object ID"))
}

fn parse_object_kind_v1(bytes: &[u8]) -> Option<StrictGitObjectKindV1> {
    match bytes {
        b"commit" => Some(StrictGitObjectKindV1::Commit),
        b"tree" => Some(StrictGitObjectKindV1::Tree),
        b"blob" => Some(StrictGitObjectKindV1::Blob),
        b"tag" => Some(StrictGitObjectKindV1::Tag),
        _ => None,
    }
}

fn validate_ref_name_v1(value: &str, limits: GitObservationLimitsV1) -> Option<String> {
    if !value.starts_with("refs/")
        || value.len() > usize::try_from(limits.max_ref_or_symref_bytes).ok()?
        || value == "refs/"
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.chars().any(|character| {
            character.is_ascii_control()
                || matches!(character, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        || value.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.starts_with('.')
                || component.ends_with(".lock")
        })
    {
        return None;
    }
    portable_casefold_path(&format!(".git/{value}")).ok()
}

fn checked_retain_v1(
    retained: &mut u64,
    additional: usize,
    limits: GitObservationLimitsV1,
) -> Result<(), StrictGitRawTopologyIncompleteV1> {
    *retained = retained
        .checked_add(u64::try_from(additional).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "count-retained-git-reference-bytes",
            )
        })?)
        .ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "count-retained-git-reference-bytes",
            )
        })?;
    if *retained > limits.max_retained_bytes {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "maximum-retained-git-reference-bytes",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ObservedGitRefV1 {
    ref_name: String,
    object_id: String,
    object_kind: StrictGitObjectKindV1,
    reported_symref_target: Option<String>,
}

fn parse_ref_output_v1(
    bytes: &[u8],
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<Vec<ObservedGitRefV1>, StrictGitRawTopologyIncompleteV1> {
    let mut refs = Vec::new();
    let mut previous_ref: Option<String> = None;
    let mut folded_refs = BTreeSet::new();
    let mut retained_bytes = 0_u64;
    let mut lines = bytes.split(|byte| *byte == b'\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() && lines.peek().is_none() {
            break;
        }
        if line.is_empty() || line.contains(&b'\r') {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-record",
            ));
        }
        let mut fields = line.splitn(5, |byte| *byte == 0);
        let Some(ref_name_bytes) = fields.next() else {
            unreachable!("splitn always yields one field");
        };
        let Some(object_id_bytes) = fields.next() else {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-fields",
            ));
        };
        let Some(object_kind_bytes) = fields.next() else {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-fields",
            ));
        };
        let Some(symref_target_bytes) = fields.next() else {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-fields",
            ));
        };
        if fields.next().is_some() {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-fields",
            ));
        }
        if u64::try_from(refs.len()).unwrap_or(u64::MAX) >= limits.max_refs {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-git-reference-count",
            ));
        }
        if ref_name_bytes.len()
            > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX)
            || symref_target_bytes.len()
                > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX)
            || object_id_bytes.len() > usize::from(limits.sha256_oid_hex_bytes)
            || object_kind_bytes.len() > 6
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-git-reference-field-bytes",
            ));
        }
        let ref_name_borrowed = std::str::from_utf8(ref_name_bytes).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "decode-git-reference-name",
            )
        })?;
        let folded_ref = validate_ref_name_v1(ref_name_borrowed, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-git-reference-name",
            )
        })?;
        let ref_name = ref_name_borrowed.to_owned();
        if previous_ref
            .as_ref()
            .is_some_and(|previous| previous >= &ref_name)
            || !folded_refs.insert(folded_ref.clone())
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "require-sorted-unique-git-references",
            ));
        }
        let object_id = parse_object_id_v1(object_id_bytes, format, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-object-id",
            )
        })?;
        let object_kind = parse_object_kind_v1(object_kind_bytes).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-reference-object-kind",
            )
        })?;
        if ref_name.starts_with("refs/heads/") && object_kind != StrictGitObjectKindV1::Commit {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "require-branch-reference-commit",
            ));
        }
        let reported_symref_target = if symref_target_bytes.is_empty() {
            None
        } else {
            let target = std::str::from_utf8(symref_target_bytes).map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "decode-git-symref-target",
                )
            })?;
            validate_ref_name_v1(target, limits).ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "validate-git-symref-target",
                )
            })?;
            Some(target.to_owned())
        };
        checked_retain_v1(&mut retained_bytes, ref_name.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, folded_ref.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, object_id.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, object_kind_bytes.len(), limits)?;
        if let Some(target) = &reported_symref_target {
            checked_retain_v1(&mut retained_bytes, target.len(), limits)?;
        }
        previous_ref = Some(ref_name.clone());
        refs.try_reserve(1).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "allocate-git-reference-record",
            )
        })?;
        refs.push(ObservedGitRefV1 {
            ref_name,
            object_id,
            object_kind,
            reported_symref_target,
        });
    }
    Ok(refs)
}

fn parse_packed_refs_v1(
    bytes: &[u8],
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<BTreeMap<String, String>, StrictGitRawTopologyIncompleteV1> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
            "require-packed-refs-terminal-newline",
        ));
    }
    let mut refs = BTreeMap::new();
    let mut retained_bytes = 0_u64;
    let mut previous_was_ref = false;
    let mut previous_was_peeled = false;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.contains(&b'\r') || line.contains(&0) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-packed-reference-line",
            ));
        }
        if line.starts_with(b"#") {
            previous_was_ref = false;
            previous_was_peeled = false;
            continue;
        }
        if let Some(peeled) = line.strip_prefix(b"^") {
            if !previous_was_ref
                || previous_was_peeled
                || parse_object_id_v1(peeled, format, limits).is_none()
            {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "parse-packed-peeled-object-id",
                ));
            }
            previous_was_peeled = true;
            continue;
        }
        previous_was_peeled = false;
        if u64::try_from(refs.len()).unwrap_or(u64::MAX) >= limits.max_refs {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-packed-reference-count",
            ));
        }
        let Some(separator) = line.iter().position(|byte| *byte == b' ') else {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-packed-reference-fields",
            ));
        };
        let object_id_bytes = &line[..separator];
        let ref_name_bytes = &line[separator + 1..];
        if ref_name_bytes.len()
            > usize::try_from(limits.max_ref_or_symref_bytes).unwrap_or(usize::MAX)
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-packed-reference-field-bytes",
            ));
        }
        if ref_name_bytes.contains(&b' ') {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-packed-reference-name",
            ));
        }
        let object_id = parse_object_id_v1(object_id_bytes, format, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-packed-reference-object-id",
            )
        })?;
        let ref_name = std::str::from_utf8(ref_name_bytes)
            .map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "decode-packed-reference-name",
                )
            })?
            .to_owned();
        let folded = validate_ref_name_v1(&ref_name, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-packed-reference-name",
            )
        })?;
        checked_retain_v1(&mut retained_bytes, ref_name.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, folded.len(), limits)?;
        checked_retain_v1(&mut retained_bytes, object_id.len(), limits)?;
        if refs.insert(ref_name, object_id).is_some() {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-duplicate-packed-reference",
            ));
        }
        previous_was_ref = true;
    }
    Ok(refs)
}

enum EffectiveRawRefValueV1 {
    DirectObjectId(String),
    SymbolicTarget(String),
}

fn effective_raw_refs_v1(
    metadata: &GitMetadataInventoryV1,
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<BTreeMap<String, EffectiveRawRefValueV1>, StrictGitRawTopologyIncompleteV1> {
    let mut refs = match &metadata.packed_refs {
        Some(bytes) => parse_packed_refs_v1(bytes, format, limits)?
            .into_iter()
            .map(|(name, oid)| (name, EffectiveRawRefValueV1::DirectObjectId(oid)))
            .collect::<BTreeMap<_, _>>(),
        None => BTreeMap::new(),
    };
    for (name, value) in &metadata.loose_refs {
        if !refs.contains_key(name)
            && u64::try_from(refs.len()).unwrap_or(u64::MAX) >= limits.max_refs
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "maximum-effective-raw-reference-count",
            ));
        }
        let value = match value {
            RawLooseRefValueV1::DirectObjectId(raw) => EffectiveRawRefValueV1::DirectObjectId(
                parse_object_id_v1(raw, format, limits).ok_or_else(|| {
                    incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "parse-loose-reference-object-id",
                    )
                })?,
            ),
            RawLooseRefValueV1::SymbolicTarget(target) => {
                EffectiveRawRefValueV1::SymbolicTarget(target.clone())
            }
        };
        refs.insert(name.clone(), value);
    }
    let mut folded = BTreeSet::new();
    for (name, value) in &refs {
        let folded_name = validate_ref_name_v1(name, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-effective-raw-reference-name",
            )
        })?;
        if !folded.insert(folded_name) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-effective-raw-reference-portable-alias",
            ));
        }
        if let EffectiveRawRefValueV1::SymbolicTarget(target) = value {
            if !refs.contains_key(target) {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "reject-dangling-symbolic-reference",
                ));
            }
        }
    }
    for folded_name in &folded {
        for (separator, _) in folded_name.match_indices('/') {
            if folded.contains(&folded_name[..separator]) {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "reject-reference-file-directory-conflict",
                ));
            }
        }
    }
    Ok(refs)
}

fn resolve_symbolic_ref_terminals_v1(
    raw: &BTreeMap<String, EffectiveRawRefValueV1>,
) -> Result<Vec<usize>, StrictGitRawTopologyIncompleteV1> {
    let mut names = Vec::new();
    names.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-symbolic-reference-name-index",
        )
    })?;
    names.extend(raw.keys().map(String::as_str));

    let mut terminals = Vec::new();
    terminals.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-symbolic-reference-terminal-index",
        )
    })?;
    terminals.resize(raw.len(), usize::MAX);
    for (index, value) in raw.values().enumerate() {
        if matches!(value, EffectiveRawRefValueV1::DirectObjectId(_)) {
            terminals[index] = index;
        }
    }

    let mut visit_generation = Vec::new();
    visit_generation.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-symbolic-reference-cycle-index",
        )
    })?;
    visit_generation.resize(raw.len(), 0_usize);

    let mut path = Vec::new();
    path.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-symbolic-reference-resolution-path",
        )
    })?;

    for start in 0..raw.len() {
        if terminals[start] != usize::MAX {
            continue;
        }
        path.clear();
        let generation = start.checked_add(1).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "count-symbolic-reference-resolution-generation",
            )
        })?;
        let mut current = start;
        let terminal = loop {
            if terminals[current] != usize::MAX {
                break terminals[current];
            }
            if visit_generation[current] == generation {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "reject-symbolic-reference-cycle",
                ));
            }
            visit_generation[current] = generation;
            path.push(current);
            let target = match raw.get(names[current]) {
                Some(EffectiveRawRefValueV1::SymbolicTarget(target)) => target,
                Some(EffectiveRawRefValueV1::DirectObjectId(_)) => {
                    terminals[current] = current;
                    break current;
                }
                None => {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "resolve-symbolic-reference-source",
                    ));
                }
            };
            current = names.binary_search(&target.as_str()).map_err(|_| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "reject-dangling-symbolic-reference",
                )
            })?;
        };
        for index in path.iter().copied() {
            terminals[index] = terminal;
        }
    }

    Ok(terminals)
}

fn reconcile_raw_and_observed_refs_v1(
    raw: BTreeMap<String, EffectiveRawRefValueV1>,
    terminals: Vec<usize>,
    observed: Vec<ObservedGitRefV1>,
) -> Result<Vec<StrictGitRefPinV1>, StrictGitRawTopologyIncompleteV1> {
    if raw.len() != observed.len() {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
            "reconcile-raw-and-git-reference-counts",
        ));
    }
    let mut raw_names = Vec::new();
    raw_names.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-reconciled-reference-name-index",
        )
    })?;
    raw_names.extend(raw.keys().map(String::as_str));
    let mut pins = Vec::new();
    pins.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-reconciled-git-reference-pins",
        )
    })?;
    for (index, ((raw_name, raw_value), observed)) in raw.iter().zip(observed).enumerate() {
        if raw_name != &observed.ref_name {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reconcile-raw-and-git-reference-names",
            ));
        }
        let symref_target = match raw_value {
            EffectiveRawRefValueV1::DirectObjectId(raw_object_id) => {
                if raw_object_id != &observed.object_id || observed.reported_symref_target.is_some()
                {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "reconcile-direct-reference-value",
                    ));
                }
                None
            }
            EffectiveRawRefValueV1::SymbolicTarget(target) => {
                let terminal_name = raw_names[terminals[index]];
                let terminal_object_id = match raw.get(terminal_name) {
                    Some(EffectiveRawRefValueV1::DirectObjectId(object_id)) => object_id,
                    _ => {
                        return Err(incomplete(
                            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                            "resolve-symbolic-reference-terminal-object",
                        ));
                    }
                };
                if observed.reported_symref_target.as_deref() != Some(terminal_name)
                    || &observed.object_id != terminal_object_id
                {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "reconcile-symbolic-reference-value",
                    ));
                }
                Some(target.clone())
            }
        };
        pins.push(StrictGitRefPinV1 {
            ref_name: observed.ref_name,
            object_id: observed.object_id,
            object_kind: observed.object_kind,
            symref_target,
        });
    }
    Ok(pins)
}

fn read_refs_v1(
    anchor: &GitRepoAnchor,
    metadata: &GitMetadataInventoryV1,
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<Vec<StrictGitRefPinV1>, StrictGitRawTopologyIncompleteV1> {
    let raw = effective_raw_refs_v1(metadata, format, limits)?;
    let terminals = resolve_symbolic_ref_terminals_v1(&raw)?;
    let count = limits.max_refs.checked_add(1).ok_or_else(|| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "bound-git-reference-command-count",
        )
    })?;
    let output = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::ForEachRef { count },
        limits.max_command_stdout_bytes,
        "read-bounded-git-references",
    )?;
    if !output.status.success() {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
            "read-bounded-git-references",
        ));
    }
    let observed = parse_ref_output_v1(&output.stdout, format, limits)?;
    reconcile_raw_and_observed_refs_v1(raw, terminals, observed)
}

enum ParsedRawHeadV1 {
    Symbolic(String),
    Detached(String),
}

fn parse_raw_head_v1(
    bytes: &[u8],
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<ParsedRawHeadV1, StrictGitRawTopologyIncompleteV1> {
    let line = exact_single_line_v1(bytes).ok_or_else(|| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
            "parse-raw-git-head-line",
        )
    })?;
    if let Some(target_bytes) = line.strip_prefix(b"ref: ") {
        let target = std::str::from_utf8(target_bytes).map_err(|_| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "decode-raw-git-symbolic-head",
            )
        })?;
        validate_ref_name_v1(target, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-raw-git-symbolic-head",
            )
        })?;
        if !target.starts_with("refs/heads/") {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "require-raw-symbolic-head-branch-target",
            ));
        }
        Ok(ParsedRawHeadV1::Symbolic(target.to_owned()))
    } else {
        parse_object_id_v1(line, format, limits)
            .map(ParsedRawHeadV1::Detached)
            .ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "parse-raw-git-detached-head",
                )
            })
    }
}

fn read_head_v1(
    anchor: &GitRepoAnchor,
    metadata: &GitMetadataInventoryV1,
    format: StrictGitObjectFormatV1,
    refs: &[StrictGitRefPinV1],
    limits: GitObservationLimitsV1,
) -> Result<StrictGitHeadPinV1, StrictGitRawTopologyIncompleteV1> {
    let raw_head = parse_raw_head_v1(&metadata.raw_head, format, limits)?;
    let symbolic = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::SymbolicHeadNoRecurse,
        limits.max_ref_or_symref_bytes.saturating_add(2),
        "read-git-symbolic-head",
    )?;
    let resolved = run_held_git_query_v1(
        anchor,
        HeldGitReadQueryV1::ResolvedHeadCommit,
        u64::from(limits.sha256_oid_hex_bytes).saturating_add(2),
        "read-git-resolved-head",
    )?;
    let resolved_object_id = if resolved.status.success() {
        let line = exact_single_line_v1(&resolved.stdout).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-resolved-head",
            )
        })?;
        Some(parse_object_id_v1(line, format, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-git-resolved-head",
            )
        })?)
    } else if resolved.status.code() == Some(1) && resolved.stdout.is_empty() {
        None
    } else {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
            "read-git-resolved-head",
        ));
    };

    if symbolic.status.success() {
        let target = exact_single_line_v1(&symbolic.stdout)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "parse-git-symbolic-head",
                )
            })?;
        let folded_target = validate_ref_name_v1(&target, limits).ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "validate-git-symbolic-head",
            )
        })?;
        if !target.starts_with("refs/heads/") {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "require-symbolic-head-branch-target",
            ));
        }
        if !matches!(&raw_head, ParsedRawHeadV1::Symbolic(raw_target) if raw_target == &target) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reconcile-raw-and-git-symbolic-head",
            ));
        }
        if let Some(oid) = &resolved_object_id {
            if !refs
                .iter()
                .any(|pin| pin.ref_name == target && pin.object_id == *oid)
            {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "bind-symbolic-head-to-reference",
                ));
            }
        } else {
            for pin in refs {
                let folded_pin = validate_ref_name_v1(&pin.ref_name, limits).ok_or_else(|| {
                    incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "revalidate-git-reference-for-unborn-head",
                    )
                })?;
                if folded_pin == folded_target
                    || folded_pin
                        .strip_prefix(&folded_target)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                    || folded_target
                        .strip_prefix(&folded_pin)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "reject-unborn-head-portable-alias",
                    ));
                }
            }
        }
        Ok(StrictGitHeadPinV1::Symbolic {
            target,
            resolved_object_id,
        })
    } else if symbolic.status.code() == Some(1) && symbolic.stdout.is_empty() {
        let object_id = resolved_object_id.ok_or_else(|| {
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "require-detached-head-object",
            )
        })?;
        if !matches!(&raw_head, ParsedRawHeadV1::Detached(raw_object_id) if raw_object_id == &object_id)
        {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reconcile-raw-and-git-detached-head",
            ));
        }
        Ok(StrictGitHeadPinV1::Detached { object_id })
    } else {
        Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
            "read-git-symbolic-head",
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::registered_local_snapshot::{
        begin_strict_local_snapshot_v1, StrictLocalSnapshotFinishV1, StrictLocalSnapshotHoldReadV1,
    };
    use std::path::Path;
    use std::process::Command;
    use tcfs_core::config::RootProfileV1;

    static_assertions::assert_not_impl_any!(
        HeldStrictGitRawTopologyV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        StrictGitRefPinV1: Clone,
        serde::Serialize
    );

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-C", repo.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-C", repo.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn canonical_unborn_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        canonical_unborn_repo_with_format("sha1")
    }

    fn canonical_unborn_repo_with_format(format: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run_git(
            &root,
            &[
                "init",
                "--quiet",
                "--initial-branch=main",
                &format!("--object-format={format}"),
            ],
        );
        (temporary, root.canonicalize().unwrap())
    }

    fn canonical_committed_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        canonical_committed_repo_with_format("sha1")
    }

    fn canonical_committed_repo_with_format(
        format: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let (temporary, root) = canonical_unborn_repo_with_format(format);
        std::fs::write(root.join("tracked"), b"payload").unwrap();
        run_git(&root, &["add", "tracked"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=TCFS Test",
                "-c",
                "user.email=tcfs@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        );
        (temporary, root)
    }

    fn pending_local(root: &Path) -> PendingStrictLocalSnapshotV1 {
        match begin_strict_local_snapshot_v1(root, RootProfileV1::GitRawV1).unwrap() {
            StrictLocalSnapshotHoldReadV1::Pending(pending) => pending,
            StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
                panic!("expected pending GitRaw local snapshot: {incomplete:?}")
            }
        }
    }

    fn begin_topology(pending: &PendingStrictLocalSnapshotV1) -> PendingStrictGitRawTopologyV1 {
        match begin_strict_git_raw_topology_v1(pending) {
            StrictGitRawTopologyBeginV1::Pending(topology) => *topology,
            StrictGitRawTopologyBeginV1::Incomplete(incomplete) => {
                panic!("expected pending GitRaw topology: {incomplete:?}")
            }
        }
    }

    fn finish_topology(pending: PendingStrictGitRawTopologyV1) -> HeldStrictGitRawTopologyV1 {
        match pending.revalidate_after_external_reads() {
            StrictGitRawTopologyFinishV1::Held(topology) => topology,
            StrictGitRawTopologyFinishV1::Incomplete(incomplete) => {
                panic!("expected held GitRaw topology: {incomplete:?}")
            }
        }
    }

    fn begin_incomplete(
        pending: &PendingStrictLocalSnapshotV1,
    ) -> StrictGitRawTopologyIncompleteV1 {
        match begin_strict_git_raw_topology_v1(pending) {
            StrictGitRawTopologyBeginV1::Incomplete(incomplete) => incomplete,
            StrictGitRawTopologyBeginV1::Pending(_) => {
                panic!("expected GitRaw topology to fail closed")
            }
        }
    }

    #[test]
    fn ordinary_standalone_repo_yields_sorted_opaque_ref_pins() {
        let (_temporary, root) = canonical_committed_repo();
        run_git(&root, &["branch", "z-last"]);
        run_git(&root, &["branch", "a-first"]);
        std::fs::write(root.join(".git/tcfs.lock"), b"").unwrap();
        let pending = pending_local(&root);
        let topology = finish_topology(begin_topology(&pending));

        assert_eq!(topology.object_format(), StrictGitObjectFormatV1::Sha1);
        assert_eq!(topology.head().symbolic_target(), Some("refs/heads/main"));
        assert!(topology.head().resolved_object_id().is_some());
        let refs = topology
            .refs()
            .map(|pin| pin.ref_name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            refs,
            vec!["refs/heads/a-first", "refs/heads/main", "refs/heads/z-last"]
        );
        assert_eq!(topology.contract().pass_count(), 2);

        match pending.revalidate_inventory_c().unwrap() {
            StrictLocalSnapshotFinishV1::Complete(_) => {}
            StrictLocalSnapshotFinishV1::Incomplete(incomplete) => {
                panic!("local inventory C must remain stable: {incomplete:?}")
            }
        }
        topology.revalidate_capabilities().unwrap();
    }

    #[test]
    fn sha1_and_sha256_packed_tags_and_symbolic_chains_are_exact() {
        for (format, expected_format, object_id_len) in [
            ("sha1", StrictGitObjectFormatV1::Sha1, 40),
            ("sha256", StrictGitObjectFormatV1::Sha256, 64),
        ] {
            let (_temporary, root) = canonical_committed_repo_with_format(format);
            run_git(&root, &["update-ref", "refs/tags/lightweight", "HEAD"]);
            run_git(
                &root,
                &[
                    "-c",
                    "user.name=TCFS Test",
                    "-c",
                    "user.email=tcfs@example.invalid",
                    "-c",
                    "tag.gpgsign=false",
                    "tag",
                    "--annotate",
                    "annotated",
                    "--message=tag",
                ],
            );
            run_git(&root, &["pack-refs", "--all", "--prune"]);
            run_git(
                &root,
                &["symbolic-ref", "refs/heads/middle", "refs/heads/main"],
            );
            run_git(
                &root,
                &["symbolic-ref", "refs/heads/alias", "refs/heads/middle"],
            );

            let pending = pending_local(&root);
            let topology = finish_topology(begin_topology(&pending));
            assert_eq!(topology.object_format(), expected_format);
            assert!(root.join(".git/packed-refs").is_file());
            assert!(topology
                .refs()
                .all(|pin| pin.object_id().len() == object_id_len));

            let annotated = topology
                .refs()
                .find(|pin| pin.ref_name() == "refs/tags/annotated")
                .unwrap();
            assert_eq!(annotated.object_kind, StrictGitObjectKindV1::Tag);
            let lightweight = topology
                .refs()
                .find(|pin| pin.ref_name() == "refs/tags/lightweight")
                .unwrap();
            assert_eq!(lightweight.object_kind, StrictGitObjectKindV1::Commit);
            let main = topology
                .refs()
                .find(|pin| pin.ref_name() == "refs/heads/main")
                .unwrap();
            let alias = topology
                .refs()
                .find(|pin| pin.ref_name() == "refs/heads/alias")
                .unwrap();
            assert_eq!(alias.symref_target(), Some("refs/heads/middle"));
            assert_eq!(alias.object_id(), main.object_id());
        }
    }

    #[test]
    fn unborn_and_detached_head_states_are_explicit() {
        let (_unborn_temporary, unborn) = canonical_unborn_repo();
        let pending = pending_local(&unborn);
        let topology = finish_topology(begin_topology(&pending));
        assert_eq!(topology.head().symbolic_target(), Some("refs/heads/main"));
        assert_eq!(topology.head().resolved_object_id(), None);
        assert_eq!(topology.refs().len(), 0);

        let (_detached_temporary, detached) = canonical_committed_repo();
        run_git(&detached, &["checkout", "--quiet", "--detach"]);
        let pending = pending_local(&detached);
        let topology = finish_topology(begin_topology(&pending));
        assert_eq!(topology.head().symbolic_target(), None);
        assert!(topology.head().resolved_object_id().is_some());
    }

    #[test]
    fn dangling_cyclic_and_nonportable_unborn_symrefs_are_rejected() {
        let (_dangling_temporary, dangling) = canonical_committed_repo();
        std::fs::write(
            dangling.join(".git/refs/heads/dangling"),
            b"ref: refs/heads/missing\n",
        )
        .unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&dangling)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );

        let (_cycle_temporary, cycle) = canonical_committed_repo();
        std::fs::write(
            cycle.join(".git/refs/heads/cycle-a"),
            b"ref: refs/heads/cycle-b\n",
        )
        .unwrap();
        std::fs::write(
            cycle.join(".git/refs/heads/cycle-b"),
            b"ref: refs/heads/cycle-a\n",
        )
        .unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&cycle)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );

        let (_alias_temporary, alias) = canonical_committed_repo();
        run_git(&alias, &["branch", "foo"]);
        std::fs::write(alias.join(".git/HEAD"), b"ref: refs/heads/Foo\n").unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&alias)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );

        let (_df_temporary, df_conflict) = canonical_committed_repo();
        run_git(&df_conflict, &["branch", "topic"]);
        std::fs::write(
            df_conflict.join(".git/HEAD"),
            b"ref: refs/heads/topic/child\n",
        )
        .unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&df_conflict)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );
    }

    #[test]
    fn detached_head_must_name_a_commit_object_directly() {
        let (_temporary, root) = canonical_committed_repo();
        run_git(
            &root,
            &[
                "-c",
                "user.name=TCFS Test",
                "-c",
                "user.email=tcfs@example.invalid",
                "-c",
                "tag.gpgsign=false",
                "tag",
                "--annotate",
                "annotated",
                "--message=tag",
            ],
        );
        let tag_object = git_stdout(&root, &["rev-parse", "refs/tags/annotated"]);
        std::fs::write(root.join(".git/HEAD"), format!("{tag_object}\n")).unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );
    }

    #[test]
    fn reference_mutation_between_passes_is_typed_incomplete() {
        let (_temporary, root) = canonical_committed_repo();
        let pending = pending_local(&root);
        let topology = begin_topology(&pending);
        run_git(&root, &["branch", "appeared-between-passes"]);
        assert!(matches!(
            topology.revalidate_after_external_reads(),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete)
                if incomplete.kind()
                    == StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged
        ));
    }

    #[test]
    fn root_and_git_directory_replacement_between_passes_are_typed_changed() {
        let (_root_temporary, root) = canonical_committed_repo();
        let pending = pending_local(&root);
        let topology = begin_topology(&pending);
        let original_root = root.with_extension("original");
        std::fs::rename(&root, &original_root).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(matches!(
            topology.revalidate_after_external_reads(),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete)
                if incomplete.kind()
                    == StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged
        ));

        let (_git_temporary, git_root) = canonical_committed_repo();
        let pending = pending_local(&git_root);
        let topology = begin_topology(&pending);
        let original_git = git_root.join(".git-original");
        std::fs::rename(git_root.join(".git"), &original_git).unwrap();
        std::fs::create_dir(git_root.join(".git")).unwrap();
        assert!(matches!(
            topology.revalidate_after_external_reads(),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete)
                if incomplete.kind()
                    == StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged
        ));
    }

    #[test]
    fn topology_extensions_and_native_activity_are_rejected() {
        for (relative, directory, bytes, expected) in [
            (
                "commondir",
                false,
                b"../shared\n".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "config.worktree",
                false,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "worktrees",
                true,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "shallow",
                false,
                b"0000000000000000000000000000000000000000\n".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "objects/info/alternates",
                false,
                b"../external\n".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "objects/info/http-alternates",
                false,
                b"https://example.invalid/\n".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            ),
            (
                "index.lock",
                false,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
            ),
            (
                "config.lock",
                false,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
            ),
            (
                "objects/pack/write.lock",
                false,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
            ),
            (
                "MERGE_HEAD",
                false,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
            ),
            (
                "sequencer",
                true,
                b"".as_slice(),
                StrictGitRawTopologyIncompleteKindV1::ActivityDetected,
            ),
        ] {
            let (_temporary, root) = canonical_committed_repo();
            let path = root.join(".git").join(relative);
            if directory {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, bytes).unwrap();
            }
            assert_eq!(
                begin_incomplete(&pending_local(&root)).kind(),
                expected,
                "{relative}"
            );
        }
    }

    #[test]
    fn repository_config_routing_and_oversized_inputs_are_rejected() {
        for (key, value) in [
            ("include.path", "/does/not-exist"),
            ("includeIf.onbranch:main.path", "/does/not-exist"),
            ("core.worktree", "/tmp/external-worktree"),
            ("core.bare", "true"),
            ("core.sharedRepository", "group"),
            ("extensions.worktreeConfig", "true"),
            ("extensions.refStorage", "reftable"),
            ("remote.origin.promisor", "true"),
        ] {
            let (_temporary, root) = canonical_committed_repo();
            run_git(&root, &["config", key, value]);
            assert_eq!(
                begin_incomplete(&pending_local(&root)).kind(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "{key}"
            );
        }

        let (_config_temporary, config_root) = canonical_committed_repo();
        std::fs::write(config_root.join(".git/config"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&config_root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );

        let (_head_temporary, head_root) = canonical_committed_repo();
        std::fs::write(head_root.join(".git/HEAD"), vec![b'x'; 1030]).unwrap();
        assert_eq!(
            begin_incomplete(&pending_local(&head_root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
    }

    fn fixture_limits(max_refs: u64, max_name: u64, max_retained: u64) -> GitObservationLimitsV1 {
        GitObservationLimitsV1 {
            max_refs,
            max_ref_or_symref_bytes: max_name,
            max_retained_bytes: max_retained,
            max_command_stdout_bytes: 1024,
            max_config_file_bytes: 1024,
            max_head_file_bytes: 1029,
            sha1_oid_hex_bytes: 40,
            sha256_oid_hex_bytes: 64,
        }
    }

    fn record(name: &str, oid: &str, kind: &str, symref: &str) -> Vec<u8> {
        format!("{name}\0{oid}\0{kind}\0{symref}\n").into_bytes()
    }

    #[test]
    fn parser_enforces_exact_count_name_and_retained_byte_limits() {
        let oid = "a".repeat(40);
        let one = record("refs/heads/a", &oid, "commit", "");
        let two = record("refs/heads/b", &oid, "commit", "");
        let exact = [one.clone(), two.clone()].concat();
        assert_eq!(
            parse_ref_output_v1(
                &exact,
                StrictGitObjectFormatV1::Sha1,
                fixture_limits(2, 64, 1024)
            )
            .unwrap()
            .len(),
            2
        );
        let error = parse_ref_output_v1(
            &exact,
            StrictGitObjectFormatV1::Sha1,
            fixture_limits(1, 64, 1024),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
        let error = parse_ref_output_v1(
            &one,
            StrictGitObjectFormatV1::Sha1,
            fixture_limits(1, 11, 1024),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
        let error = parse_ref_output_v1(
            &one,
            StrictGitObjectFormatV1::Sha1,
            fixture_limits(1, 64, 1),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
    }

    #[test]
    fn parser_rejects_unsorted_duplicate_alias_and_malformed_records() {
        let oid = "a".repeat(40);
        let unsorted = [
            record("refs/heads/b", &oid, "commit", ""),
            record("refs/heads/a", &oid, "commit", ""),
        ]
        .concat();
        let alias = [
            record("refs/heads/Foo", &oid, "commit", ""),
            record("refs/heads/foo", &oid, "commit", ""),
        ]
        .concat();
        for bytes in [
            unsorted,
            alias,
            record("refs/heads/a", &"A".repeat(40), "commit", ""),
            record("refs/heads/a", &oid, "blob", ""),
            record("refs/heads/a\tb", &oid, "commit", ""),
            record("refs/heads/a\u{7f}b", &oid, "commit", ""),
            b"refs/heads/a\0missing-fields\n".to_vec(),
            format!("refs/heads/a\0{oid}\0commit\0\0extra\n").into_bytes(),
            [
                b"refs/heads/".as_slice(),
                &[0xff],
                format!("\0{oid}\0commit\0\n").as_bytes(),
            ]
            .concat(),
        ] {
            assert_eq!(
                parse_ref_output_v1(
                    &bytes,
                    StrictGitObjectFormatV1::Sha1,
                    fixture_limits(8, 128, 4096),
                )
                .unwrap_err()
                .kind(),
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
            );
        }

        for bytes in [
            format!("{oid} refs/heads/a extra\n").into_bytes(),
            format!("^{oid}\n").into_bytes(),
            format!("# pack-refs\n{oid} refs/tags/a\n^{oid}\n^{oid}\n").into_bytes(),
            format!("{oid} refs/heads/a\0bad\n").into_bytes(),
        ] {
            assert_eq!(
                parse_packed_refs_v1(
                    &bytes,
                    StrictGitObjectFormatV1::Sha1,
                    fixture_limits(8, 128, 4096),
                )
                .unwrap_err()
                .kind(),
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
            );
        }
    }
}
