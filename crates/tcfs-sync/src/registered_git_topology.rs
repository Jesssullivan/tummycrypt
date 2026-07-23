//! Held, bounded Git topology evidence for one `git-raw-v1` root.
//!
//! This module proves only that one already-authorized local root exposed the
//! same ordinary standalone raw Git metadata topology derived twice from one
//! bounded, process-owned inventory-A shadow around the external state/remote
//! read window. No child reopens live config, HEAD, or refs. It does not prove
//! object presence/kind, remote Git semantics, fast-forward ancestry, catalog
//! completeness/currentness, bootstrap safety, continuous stability, or an
//! enduring writer lease. The opaque artifact therefore has no digest,
//! serialization, clone, plan, or action conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Write};
use std::process::{ExitStatus, Stdio};

use tcfs_core::config::{RootGitObservationContractV1, RootGitPolicyV1};

use crate::conflict_git::GitRepoAnchor;
use crate::git_safety;
use crate::index_entry::portable_casefold_path;
use crate::registered_local_snapshot::{
    PendingStrictLocalSnapshotV1, RevalidatedStrictLocalSnapshotV1, StrictInitialLocalEntryKindV1,
    StrictLocalGitShadowWitnessV1, StrictLocalSnapshotIncompleteKindV1,
    StrictLocalSnapshotIncompleteV1,
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
    ImmutableDerivationMismatch,
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

#[derive(PartialEq, Eq)]
pub(crate) struct StrictGitRefPinV1 {
    ref_name: String,
    object_id: String,
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
            .field("symref_target", &self.symref_target)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum StrictGitHeadPinV1 {
    Symbolic {
        target: String,
        raw_target_object_id: Option<String>,
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

    pub(crate) fn raw_target_object_id(&self) -> Option<&str> {
        match self {
            Self::Symbolic {
                raw_target_object_id,
                ..
            } => raw_target_object_id.as_deref(),
            Self::Detached { object_id } => Some(object_id),
        }
    }
}

impl fmt::Debug for StrictGitHeadPinV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbolic {
                target,
                raw_target_object_id,
            } => formatter
                .debug_struct("Symbolic")
                .field("target", target)
                .field("raw_target_object_id", raw_target_object_id)
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
    local_acquisition: StrictLocalGitShadowWitnessV1,
    shadow: GitMetadataShadowV1,
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
    Held {
        topology: HeldStrictGitRawTopologyV1,
        local: RevalidatedStrictLocalSnapshotV1,
    },
    Incomplete(StrictGitRawTopologyIncompleteV1),
}

pub(crate) fn begin_strict_git_raw_topology_v1(
    local: &mut PendingStrictLocalSnapshotV1,
) -> StrictGitRawTopologyBeginV1 {
    let profile = local.profile();
    let policy = profile.policy().settings().git_policy();
    let contract = policy.observation_contract();
    if policy != RootGitPolicyV1::StandaloneRawWithFastForwardProofV1
        || contract != RootGitObservationContractV1::TwoPassImmutableRawMetadataV1
        || contract.pass_count() != 2
        || contract.retry_count() != 0
    {
        return StrictGitRawTopologyBeginV1::Incomplete(incomplete(
            StrictGitRawTopologyIncompleteKindV1::UnsupportedProfile,
            "select-git-observation-contract",
        ));
    }

    let local_acquisition = match local.issue_git_shadow_witness() {
        Some(witness) => witness,
        None => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::CapabilityOpenFailed,
                "issue-one-shot-local-git-shadow-witness",
            ));
        }
    };

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
    let shadow = match capture_initial_git_metadata_v1(local, &anchor, limits) {
        Ok(shadow) => shadow,
        Err(incomplete) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete);
        }
    };
    let first = match read_topology_snapshot_v1(&shadow, contract) {
        Ok(snapshot) => snapshot,
        Err(incomplete) => {
            return StrictGitRawTopologyBeginV1::Incomplete(incomplete);
        }
    };
    StrictGitRawTopologyBeginV1::Pending(Box::new(PendingStrictGitRawTopologyV1 {
        anchor,
        contract,
        local_acquisition,
        shadow,
        first,
    }))
}

impl PendingStrictGitRawTopologyV1 {
    /// Derive the exact topology again from the same process-owned bytes.
    ///
    /// The caller promotes only after inventory C. This method therefore does
    /// not reopen live config, HEAD, or refs after the local acquisition
    /// bracket has closed.
    pub(crate) fn revalidate_after_external_reads(
        self,
        local: RevalidatedStrictLocalSnapshotV1,
    ) -> StrictGitRawTopologyFinishV1 {
        let Some(local) = local.bind_git_shadow_witness(self.local_acquisition) else {
            return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                "bind-shadow-to-inventory-c-acquisition",
            ));
        };
        if local.canonical_local_root() != self.anchor.canonical_root() {
            return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                "bind-shadow-to-inventory-c-root",
            ));
        }
        if self.anchor.revalidate().is_err() {
            return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged,
                "revalidate-git-capabilities-before-shadow-promotion",
            ));
        }
        let second = match read_topology_snapshot_v1(&self.shadow, self.contract) {
            Ok(snapshot) => snapshot,
            Err(incomplete) => {
                return StrictGitRawTopologyFinishV1::Incomplete(incomplete);
            }
        };
        if self.first != second {
            return StrictGitRawTopologyFinishV1::Incomplete(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ImmutableDerivationMismatch,
                "compare-immutable-git-metadata-derivations",
            ));
        }
        StrictGitRawTopologyFinishV1::Held {
            topology: HeldStrictGitRawTopologyV1 {
                anchor: self.anchor,
                contract: self.contract,
                snapshot: second,
            },
            local,
        }
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

struct GitMetadataShadowV1 {
    raw_config: Vec<u8>,
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
) -> Result<GitMetadataShadowV1, StrictGitRawTopologyIncompleteV1> {
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
    let mut raw_config = None;

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
                if entry.kind() != StrictInitialLocalEntryKindV1::Regular
                    || entry.size() < 0
                    || raw_config.is_some()
                {
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
                raw_config = Some(
                    local
                        .read_initial_regular_file_bounded(raw_path, limits.max_config_file_bytes)
                        .map_err(|failure| {
                            map_held_local_read_failure_v1(failure, "read-held-git-config")
                        })?,
                );
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
        || raw_config.is_none()
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
    Ok(GitMetadataShadowV1 {
        raw_config: raw_config.expect("validated raw config presence"),
        loose_refs,
        packed_refs,
        raw_head: raw_head.expect("validated raw HEAD presence"),
    })
}

struct BoundedCommandOutputV1 {
    status: ExitStatus,
    stdout: Vec<u8>,
}

enum CapturedConfigQueryV1 {
    List,
    BooleanValues(&'static str),
}

fn terminate_captured_config_query_v1(
    child: &mut std::process::Child,
    input_writer: std::thread::JoinHandle<std::io::Result<()>>,
    stderr_reader: std::thread::JoinHandle<std::io::Result<bool>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = input_writer.join();
    let _ = stderr_reader.join();
}

fn run_captured_config_query_v1(
    raw_config: &[u8],
    stdout_limit: u64,
    query: CapturedConfigQueryV1,
) -> Result<BoundedCommandOutputV1, StrictGitRawTopologyIncompleteV1> {
    let stdout_limit = usize::try_from(stdout_limit).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "bound-captured-git-config-output",
        )
    })?;
    let mut command = git_safety::sanitized_git_command();
    command.args(["config", "--file", "-", "--no-includes", "--null"]);
    match query {
        CapturedConfigQueryV1::List => {
            command.arg("--list");
        }
        CapturedConfigQueryV1::BooleanValues(key) => {
            command.args(["--type=bool", "--get-all", key]);
        }
    }
    #[cfg(unix)]
    command.current_dir("/");
    #[cfg(not(unix))]
    command.current_dir(std::env::temp_dir());
    #[cfg(windows)]
    let null_config = "NUL";
    #[cfg(not(windows))]
    let null_config = "/dev/null";
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_config)
        .env("GIT_CONFIG_GLOBAL", null_config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
            "spawn-captured-git-config-query",
        )
    })?;
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "open-captured-git-config-input",
            ));
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "open-captured-git-config-output",
            ));
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "open-captured-git-config-errors",
            ));
        }
    };
    let config = raw_config.to_vec();
    let input_writer = std::thread::Builder::new()
        .name("tcfs-shadow-config-input".to_owned())
        .spawn(move || stdin.write_all(&config))
        .map_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
            incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "start-captured-git-config-input",
            )
        })?;
    let stderr_reader = match std::thread::Builder::new()
        .name("tcfs-shadow-config-stderr".to_owned())
        .spawn(move || {
            let mut saw_bytes = false;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = stderr.read(&mut buffer)?;
                if read == 0 {
                    return Ok(saw_bytes);
                }
                saw_bytes = true;
            }
        }) {
        Ok(reader) => reader,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = input_writer.join();
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "start-captured-git-config-error-reader",
            ));
        }
    };

    let mut retained = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match stdout.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => {
                terminate_captured_config_query_v1(&mut child, input_writer, stderr_reader);
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                    "read-captured-git-config-output",
                ));
            }
        };
        if read == 0 {
            break;
        }
        let Some(next_len) = retained.len().checked_add(read) else {
            terminate_captured_config_query_v1(&mut child, input_writer, stderr_reader);
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "bound-captured-git-config-output",
            ));
        };
        if next_len > stdout_limit || retained.try_reserve_exact(read).is_err() {
            terminate_captured_config_query_v1(&mut child, input_writer, stderr_reader);
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
                "bound-captured-git-config-output",
            ));
        }
        retained.extend_from_slice(&buffer[..read]);
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = input_writer.join();
            let _ = stderr_reader.join();
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceReadFailed,
                "wait-captured-git-config-query",
            ));
        }
    };
    let input_ok = matches!(input_writer.join(), Ok(Ok(())));
    let stderr_empty = matches!(stderr_reader.join(), Ok(Ok(false)));
    if !input_ok || !stderr_empty {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "run-captured-git-config-query",
        ));
    }
    Ok(BoundedCommandOutputV1 {
        status,
        stdout: retained,
    })
}

fn captured_config_boolean_values_are_false_v1(
    raw_config: &[u8],
    key: &'static str,
    limits: GitObservationLimitsV1,
) -> Result<bool, StrictGitRawTopologyIncompleteV1> {
    let output = run_captured_config_query_v1(
        raw_config,
        limits.max_config_file_bytes,
        CapturedConfigQueryV1::BooleanValues(key),
    )?;
    if !output.status.success() || output.stdout.is_empty() || !output.stdout.ends_with(&[0]) {
        return Ok(false);
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .all(|value| value == b"false"))
}

fn validate_bounded_git_routing_v1(
    raw_config: &[u8],
    limits: GitObservationLimitsV1,
) -> Result<StrictGitObjectFormatV1, StrictGitRawTopologyIncompleteV1> {
    let config = run_captured_config_query_v1(
        raw_config,
        limits.max_command_stdout_bytes,
        CapturedConfigQueryV1::List,
    )?;
    if !config.status.success() || (!config.stdout.is_empty() && !config.stdout.ends_with(&[0])) {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "parse-captured-git-config",
        ));
    }
    let mut repository_format_version: Option<Vec<u8>> = None;
    let mut object_format: Option<Vec<u8>> = None;
    let mut saw_core_bare = false;
    let mut saw_core_shared_repository = false;
    for record in config
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let (raw_key, value) = match record.iter().position(|byte| *byte == b'\n') {
            Some(separator) => (&record[..separator], Some(&record[separator + 1..])),
            None => (record, None),
        };
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
        if (key.starts_with("extensions.") && key != "extensions.objectformat")
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
        if key == "core.bare" {
            saw_core_bare = true;
        }
        if key == "core.sharedrepository" {
            saw_core_shared_repository = true;
        }
        if key == "core.repositoryformatversion" {
            if repository_format_version.is_some() || value.is_none() {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                    "require-one-repository-format-version",
                ));
            }
            repository_format_version = value.map(|value| value.to_vec());
        }
        if key == "extensions.objectformat" {
            if object_format.is_some() || value.is_none() {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                    "require-one-git-object-format",
                ));
            }
            object_format = value.map(|value| value.to_vec());
        }
    }
    if (saw_core_bare
        && !captured_config_boolean_values_are_false_v1(raw_config, "core.bare", limits)?)
        || (saw_core_shared_repository
            && !captured_config_boolean_values_are_false_v1(
                raw_config,
                "core.sharedRepository",
                limits,
            )?)
    {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "reject-shared-or-bare-repository-config",
        ));
    }

    match (
        repository_format_version.as_deref(),
        object_format.as_deref(),
    ) {
        (Some(b"0"), None) => Ok(StrictGitObjectFormatV1::Sha1),
        (Some(b"1"), Some(format)) if format.eq_ignore_ascii_case(b"sha256") => {
            Ok(StrictGitObjectFormatV1::Sha256)
        }
        _ => Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
            "validate-captured-git-object-format",
        )),
    }
}

fn read_topology_snapshot_v1(
    shadow: &GitMetadataShadowV1,
    contract: RootGitObservationContractV1,
) -> Result<StrictGitRawTopologySnapshotV1, StrictGitRawTopologyIncompleteV1> {
    let limits = GitObservationLimitsV1::from(contract);
    let object_format = validate_bounded_git_routing_v1(&shadow.raw_config, limits)?;
    let refs = read_refs_v1(shadow, object_format, limits)?;
    let head = read_head_v1(shadow, object_format, &refs, limits)?;
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
    let records = &bytes[..bytes.len() - 1];
    if records.is_empty() {
        return Err(incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
            "reject-empty-packed-reference-record",
        ));
    }
    for (index, line) in records.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-empty-packed-reference-record",
            ));
        }
        if line.contains(&b'\r') || line.contains(&0) {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "parse-packed-reference-line",
            ));
        }
        if let Some(traits) = line.strip_prefix(b"# pack-refs with: ") {
            let Some(traits) = traits.strip_suffix(b" ") else {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "validate-packed-reference-header-termination",
                ));
            };
            if index != 0 || traits.is_empty() {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "validate-packed-reference-header-position",
                ));
            }
            let mut seen_traits = BTreeSet::new();
            for feature in traits.split(|byte| *byte == b' ') {
                if feature.is_empty()
                    || !matches!(feature, b"peeled" | b"fully-peeled" | b"sorted")
                    || !seen_traits.insert(feature)
                {
                    return Err(incomplete(
                        StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                        "validate-packed-reference-header-features",
                    ));
                }
            }
            previous_was_ref = false;
            previous_was_peeled = false;
            continue;
        }
        if line.starts_with(b"#") {
            return Err(incomplete(
                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                "reject-packed-reference-comment",
            ));
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
    metadata: &GitMetadataShadowV1,
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
    let mut effective_payload_bytes = 0_u64;
    for (name, value) in &refs {
        checked_retain_v1(&mut effective_payload_bytes, name.len(), limits)?;
        match value {
            EffectiveRawRefValueV1::DirectObjectId(object_id) => {
                checked_retain_v1(&mut effective_payload_bytes, object_id.len(), limits)?;
            }
            EffectiveRawRefValueV1::SymbolicTarget(target) => {
                checked_retain_v1(&mut effective_payload_bytes, target.len(), limits)?;
            }
        }
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

fn pin_immutable_raw_refs_v1(
    raw: BTreeMap<String, EffectiveRawRefValueV1>,
    terminals: Vec<usize>,
    limits: GitObservationLimitsV1,
) -> Result<Vec<StrictGitRefPinV1>, StrictGitRawTopologyIncompleteV1> {
    let mut raw_names = Vec::new();
    raw_names.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-shadow-reference-name-index",
        )
    })?;
    raw_names.extend(raw.keys().map(String::as_str));
    let mut pins = Vec::new();
    pins.try_reserve_exact(raw.len()).map_err(|_| {
        incomplete(
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit,
            "allocate-shadow-git-reference-pins",
        )
    })?;
    let mut retained_payload_bytes = 0_u64;
    for (index, (raw_name, raw_value)) in raw.iter().enumerate() {
        let terminal_name = raw_names[terminals[index]];
        let object_id = match raw.get(terminal_name) {
            Some(EffectiveRawRefValueV1::DirectObjectId(object_id)) => object_id,
            _ => {
                return Err(incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "resolve-shadow-reference-terminal-object",
                ));
            }
        };
        let symref_target = match raw_value {
            EffectiveRawRefValueV1::DirectObjectId(_) => None,
            EffectiveRawRefValueV1::SymbolicTarget(target) => Some(target),
        };
        checked_retain_v1(&mut retained_payload_bytes, raw_name.len(), limits)?;
        checked_retain_v1(&mut retained_payload_bytes, object_id.len(), limits)?;
        if let Some(target) = &symref_target {
            checked_retain_v1(&mut retained_payload_bytes, target.len(), limits)?;
        }
        pins.push(StrictGitRefPinV1 {
            ref_name: raw_name.clone(),
            object_id: object_id.clone(),
            symref_target: symref_target.cloned(),
        });
    }
    Ok(pins)
}

fn read_refs_v1(
    metadata: &GitMetadataShadowV1,
    format: StrictGitObjectFormatV1,
    limits: GitObservationLimitsV1,
) -> Result<Vec<StrictGitRefPinV1>, StrictGitRawTopologyIncompleteV1> {
    let raw = effective_raw_refs_v1(metadata, format, limits)?;
    let terminals = resolve_symbolic_ref_terminals_v1(&raw)?;
    pin_immutable_raw_refs_v1(raw, terminals, limits)
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
    metadata: &GitMetadataShadowV1,
    format: StrictGitObjectFormatV1,
    refs: &[StrictGitRefPinV1],
    limits: GitObservationLimitsV1,
) -> Result<StrictGitHeadPinV1, StrictGitRawTopologyIncompleteV1> {
    let raw_head = parse_raw_head_v1(&metadata.raw_head, format, limits)?;
    match raw_head {
        ParsedRawHeadV1::Symbolic(target) => {
            let folded_target = validate_ref_name_v1(&target, limits).ok_or_else(|| {
                incomplete(
                    StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                    "validate-shadow-symbolic-head",
                )
            })?;
            let raw_target_object_id = refs
                .iter()
                .find(|pin| pin.ref_name == target)
                .map(|pin| pin.object_id.clone());
            if raw_target_object_id.is_none() {
                for pin in refs {
                    let folded_pin =
                        validate_ref_name_v1(&pin.ref_name, limits).ok_or_else(|| {
                            incomplete(
                                StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed,
                                "revalidate-shadow-reference-for-unborn-head",
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
                raw_target_object_id,
            })
        }
        ParsedRawHeadV1::Detached(object_id) => Ok(StrictGitHeadPinV1::Detached { object_id }),
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
    static_assertions::assert_not_impl_any!(
        StrictLocalGitShadowWitnessV1: Clone,
        Copy,
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

    fn begin_topology(pending: &mut PendingStrictLocalSnapshotV1) -> PendingStrictGitRawTopologyV1 {
        match begin_strict_git_raw_topology_v1(pending) {
            StrictGitRawTopologyBeginV1::Pending(topology) => *topology,
            StrictGitRawTopologyBeginV1::Incomplete(incomplete) => {
                panic!("expected pending GitRaw topology: {incomplete:?}")
            }
        }
    }

    fn finish_topology(
        local: PendingStrictLocalSnapshotV1,
        pending: PendingStrictGitRawTopologyV1,
    ) -> (HeldStrictGitRawTopologyV1, RevalidatedStrictLocalSnapshotV1) {
        let local = finish_local(local);
        match pending.revalidate_after_external_reads(local) {
            StrictGitRawTopologyFinishV1::Held { topology, local } => (topology, local),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete) => {
                panic!("expected held GitRaw topology: {incomplete:?}")
            }
        }
    }

    fn finish_local(local: PendingStrictLocalSnapshotV1) -> RevalidatedStrictLocalSnapshotV1 {
        match local.revalidate_inventory_c().unwrap() {
            StrictLocalSnapshotFinishV1::Complete(local) => local,
            StrictLocalSnapshotFinishV1::Incomplete(incomplete) => {
                panic!("local inventory C must remain stable: {incomplete:?}")
            }
        }
    }

    fn begin_incomplete(
        mut pending: PendingStrictLocalSnapshotV1,
    ) -> StrictGitRawTopologyIncompleteV1 {
        match begin_strict_git_raw_topology_v1(&mut pending) {
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
        let mut pending = pending_local(&root);
        let pending_topology = begin_topology(&mut pending);
        let (topology, _local) = finish_topology(pending, pending_topology);

        assert_eq!(topology.object_format(), StrictGitObjectFormatV1::Sha1);
        assert_eq!(topology.head().symbolic_target(), Some("refs/heads/main"));
        assert!(topology.head().raw_target_object_id().is_some());
        let refs = topology
            .refs()
            .map(|pin| pin.ref_name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            refs,
            vec!["refs/heads/a-first", "refs/heads/main", "refs/heads/z-last"]
        );
        assert_eq!(topology.contract().pass_count(), 2);

        topology.revalidate_capabilities().unwrap();
    }

    #[test]
    fn sha1_and_sha256_packed_refs_and_symbolic_chains_are_exact() {
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

            let mut pending = pending_local(&root);
            let pending_topology = begin_topology(&mut pending);
            let (topology, _local) = finish_topology(pending, pending_topology);
            assert_eq!(topology.object_format(), expected_format);
            assert!(root.join(".git/packed-refs").is_file());
            assert!(topology
                .refs()
                .all(|pin| pin.object_id().len() == object_id_len));

            assert!(topology
                .refs()
                .any(|pin| pin.ref_name() == "refs/tags/annotated"));
            assert!(topology
                .refs()
                .any(|pin| pin.ref_name() == "refs/tags/lightweight"));
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
        let mut pending = pending_local(&unborn);
        let pending_topology = begin_topology(&mut pending);
        let (topology, _local) = finish_topology(pending, pending_topology);
        assert_eq!(topology.head().symbolic_target(), Some("refs/heads/main"));
        assert_eq!(topology.head().raw_target_object_id(), None);
        assert_eq!(topology.refs().len(), 0);

        let (_detached_temporary, detached) = canonical_committed_repo();
        run_git(&detached, &["checkout", "--quiet", "--detach"]);
        let mut pending = pending_local(&detached);
        let pending_topology = begin_topology(&mut pending);
        let (topology, _local) = finish_topology(pending, pending_topology);
        assert_eq!(topology.head().symbolic_target(), None);
        assert!(topology.head().raw_target_object_id().is_some());
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
            begin_incomplete(pending_local(&dangling)).kind(),
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
            begin_incomplete(pending_local(&cycle)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );

        let (_alias_temporary, alias) = canonical_committed_repo();
        run_git(&alias, &["branch", "foo"]);
        std::fs::write(alias.join(".git/HEAD"), b"ref: refs/heads/Foo\n").unwrap();
        assert_eq!(
            begin_incomplete(pending_local(&alias)).kind(),
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
            begin_incomplete(pending_local(&df_conflict)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceMalformed
        );
    }

    #[test]
    fn detached_head_retains_an_opaque_raw_object_id_without_kind_claims() {
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
        let mut pending = pending_local(&root);
        let pending_topology = begin_topology(&mut pending);
        let (topology, _local) = finish_topology(pending, pending_topology);
        assert_eq!(
            topology.head().raw_target_object_id(),
            Some(tag_object.as_str())
        );
    }

    #[test]
    fn immutable_shadow_never_reinterprets_live_config_head_or_refs() {
        let (_temporary, root) = canonical_committed_repo();
        let original_oid = git_stdout(&root, &["rev-parse", "refs/heads/main"]);
        let mut pending = pending_local(&root);
        let topology = begin_topology(&mut pending);
        std::fs::write(
            root.join(".git/config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = true\n",
        )
        .unwrap();
        std::fs::write(root.join(".git/HEAD"), format!("{}\n", "b".repeat(40))).unwrap();
        std::fs::write(
            root.join(".git/refs/heads/main"),
            format!("{}\n", "c".repeat(40)),
        )
        .unwrap();

        let repeated = read_topology_snapshot_v1(&topology.shadow, topology.contract).unwrap();
        assert!(repeated == topology.first);
        assert_eq!(repeated.head.symbolic_target(), Some("refs/heads/main"));
        assert_eq!(
            repeated.head.raw_target_object_id(),
            Some(original_oid.as_str())
        );
        assert!(matches!(
            pending.revalidate_inventory_c().unwrap(),
            StrictLocalSnapshotFinishV1::Incomplete(_)
        ));
    }

    #[test]
    fn git_shadow_witness_is_one_shot_and_bound_to_one_local_acquisition() {
        let (_temporary, root) = canonical_committed_repo();
        let mut pending_a = pending_local(&root);
        let topology_a = begin_topology(&mut pending_a);
        assert!(matches!(
            begin_strict_git_raw_topology_v1(&mut pending_a),
            StrictGitRawTopologyBeginV1::Incomplete(ref incomplete)
                if incomplete.kind()
                    == StrictGitRawTopologyIncompleteKindV1::CapabilityOpenFailed
                    && incomplete.operation() == "issue-one-shot-local-git-shadow-witness"
        ));

        let mut pending_b = pending_local(&root);
        let topology_b = begin_topology(&mut pending_b);
        drop(topology_b);
        let local_b = finish_local(pending_b);
        assert!(matches!(
            topology_a.revalidate_after_external_reads(local_b),
            StrictGitRawTopologyFinishV1::Incomplete(ref incomplete)
                if incomplete.kind()
                    == StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged
                    && incomplete.operation() == "bind-shadow-to-inventory-c-acquisition"
        ));
    }

    #[test]
    fn root_and_git_directory_replacement_are_rejected_by_the_held_anchor() {
        let (_root_temporary, root) = canonical_committed_repo();
        let mut pending = pending_local(&root);
        let topology = begin_topology(&mut pending);
        let original_root = root.with_extension("original");
        std::fs::rename(&root, &original_root).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(topology.anchor.revalidate().is_err());

        let (_git_temporary, git_root) = canonical_committed_repo();
        let mut pending = pending_local(&git_root);
        let topology = begin_topology(&mut pending);
        let original_git = git_root.join(".git-original");
        std::fs::rename(git_root.join(".git"), &original_git).unwrap();
        std::fs::create_dir(git_root.join(".git")).unwrap();
        assert!(topology.anchor.revalidate().is_err());
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
                begin_incomplete(pending_local(&root)).kind(),
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
                begin_incomplete(pending_local(&root)).kind(),
                StrictGitRawTopologyIncompleteKindV1::TopologyRejected,
                "{key}"
            );
        }

        let (_config_temporary, config_root) = canonical_committed_repo();
        std::fs::write(config_root.join(".git/config"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert_eq!(
            begin_incomplete(pending_local(&config_root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );

        let (_head_temporary, head_root) = canonical_committed_repo();
        std::fs::write(head_root.join(".git/HEAD"), vec![b'x'; 1030]).unwrap();
        assert_eq!(
            begin_incomplete(pending_local(&head_root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );

        let (_malformed_temporary, malformed_root) = canonical_committed_repo();
        std::fs::write(
            malformed_root.join(".git/config"),
            b"[core\n\tbare = false\n",
        )
        .unwrap();
        assert_eq!(
            begin_incomplete(pending_local(&malformed_root)).kind(),
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected
        );

        for false_value in ["false", "no", "off", "0", "+0", "-0", "00", "0x0"] {
            let (_temporary, root) = canonical_committed_repo();
            run_git(&root, &["config", "core.bare", false_value]);
            run_git(&root, &["config", "core.sharedRepository", false_value]);
            let mut pending = pending_local(&root);
            let topology = begin_topology(&mut pending);
            let (_topology, _local) = finish_topology(pending, topology);
        }
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

    #[test]
    fn packed_ref_parser_rejects_malformed_records() {
        let oid = "a".repeat(40);
        for bytes in [
            format!("{oid} refs/heads/a extra\n").into_bytes(),
            format!("^{oid}\n").into_bytes(),
            format!("# pack-refs\n{oid} refs/tags/a\n^{oid}\n^{oid}\n").into_bytes(),
            format!("{oid} refs/heads/a\0bad\n").into_bytes(),
            format!("{oid} refs/heads/a\n\n").into_bytes(),
            format!("{oid} refs/heads/a\n# pack-refs with: sorted\n").into_bytes(),
            format!("# arbitrary comment\n{oid} refs/heads/a\n").into_bytes(),
            format!("{oid} refs/tags/a\n\n^{oid}\n").into_bytes(),
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

    #[test]
    fn captured_config_query_enforces_exact_output_limit_and_reaps_failures() {
        let raw_config = b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n";
        let observed =
            run_captured_config_query_v1(raw_config, 1024, CapturedConfigQueryV1::List).unwrap();
        assert!(observed.status.success());
        let exact = u64::try_from(observed.stdout.len()).unwrap();
        assert!(
            run_captured_config_query_v1(raw_config, exact, CapturedConfigQueryV1::List,).is_ok()
        );
        let limited = match run_captured_config_query_v1(
            raw_config,
            exact.saturating_sub(1),
            CapturedConfigQueryV1::List,
        ) {
            Err(incomplete) => incomplete,
            Ok(_) => panic!("one byte below the captured config output must fail"),
        };
        assert_eq!(
            limited.kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
        let malformed =
            match run_captured_config_query_v1(b"[core\n", 1024, CapturedConfigQueryV1::List) {
                Err(incomplete) => incomplete,
                Ok(_) => panic!("malformed captured config must fail"),
            };
        assert_eq!(
            malformed.kind(),
            StrictGitRawTopologyIncompleteKindV1::TopologyRejected
        );
    }

    #[test]
    fn effective_and_canonical_ref_payloads_have_aggregate_exact_limits() {
        let oid_a = "a".repeat(40);
        let oid_b = vec![b'b'; 40];
        let packed_name = "refs/heads/a";
        let loose_name = "refs/heads/b";
        let mut loose_refs = BTreeMap::new();
        loose_refs.insert(
            loose_name.to_owned(),
            RawLooseRefValueV1::DirectObjectId(oid_b),
        );
        let shadow = GitMetadataShadowV1 {
            raw_config: Vec::new(),
            loose_refs,
            packed_refs: Some(format!("{oid_a} {packed_name}\n").into_bytes()),
            raw_head: Vec::new(),
        };
        let exact_effective = u64::try_from(packed_name.len() + loose_name.len() + 80).unwrap();
        assert!(effective_raw_refs_v1(
            &shadow,
            StrictGitObjectFormatV1::Sha1,
            fixture_limits(8, 128, exact_effective),
        )
        .is_ok());
        let limited = match effective_raw_refs_v1(
            &shadow,
            StrictGitObjectFormatV1::Sha1,
            fixture_limits(8, 128, exact_effective - 1),
        ) {
            Err(incomplete) => incomplete,
            Ok(_) => panic!("one byte below the effective ref payload must fail"),
        };
        assert_eq!(
            limited.kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );

        let direct_name = "refs/heads/main";
        let alias_name = "refs/heads/alias";
        let mut symbolic = BTreeMap::new();
        symbolic.insert(
            direct_name.to_owned(),
            EffectiveRawRefValueV1::DirectObjectId(oid_a),
        );
        symbolic.insert(
            alias_name.to_owned(),
            EffectiveRawRefValueV1::SymbolicTarget(direct_name.to_owned()),
        );
        let terminals = resolve_symbolic_ref_terminals_v1(&symbolic).unwrap();
        let exact_canonical =
            u64::try_from(direct_name.len() + 40 + alias_name.len() + 40 + direct_name.len())
                .unwrap();
        assert!(pin_immutable_raw_refs_v1(
            symbolic,
            terminals,
            fixture_limits(8, 128, exact_canonical),
        )
        .is_ok());

        let mut symbolic = BTreeMap::new();
        symbolic.insert(
            direct_name.to_owned(),
            EffectiveRawRefValueV1::DirectObjectId("a".repeat(40)),
        );
        symbolic.insert(
            alias_name.to_owned(),
            EffectiveRawRefValueV1::SymbolicTarget(direct_name.to_owned()),
        );
        let terminals = resolve_symbolic_ref_terminals_v1(&symbolic).unwrap();
        assert_eq!(
            pin_immutable_raw_refs_v1(
                symbolic,
                terminals,
                fixture_limits(8, 128, exact_canonical - 1),
            )
            .unwrap_err()
            .kind(),
            StrictGitRawTopologyIncompleteKindV1::ReferenceResourceLimit
        );
    }
}
