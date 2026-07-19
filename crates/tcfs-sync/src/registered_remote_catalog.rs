//! Immutable registered-root remote catalog closure.
//!
//! Generic S3-compatible LIST operations cannot prove that they did not omit
//! an object. The strict registered-root ceremony therefore requires a
//! conditionally updated current HEAD whose exact bytes select an immutable,
//! content-addressed catalog root and ordered immutable pages. This module
//! validates only that catalog closure:
//!
//! `current HEAD A -> catalog root -> every catalog page -> current HEAD B`
//!
//! It deliberately stops before binding every namespace object named by the
//! pages. It also cannot prove that legacy/direct writers have been fenced or
//! that the first catalog was bootstrapped from externally complete truth.
//! The inventory is the registered-root reconcile metadata corpus (indices,
//! namespace reservations, and manifests), not chunks, staging objects,
//! probes, or catalog objects. A later semantic gate must still prove that
//! every index and reservation is present and the manifest entries are exactly
//! the referenced manifest set.
//! Without linearizable current-HEAD reads or a trusted monotonic high-water
//! mark, it also proves closure only at the observed revision, not that the
//! revision is the latest published namespace.
//! Consequently the artifact below is not `CompleteOrNoDigestV1` authority and
//! has no digest, action, serialization, or planner conversion.

use anyhow::Result;
use opendal::Operator;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
#[cfg(test)]
use tcfs_core::config::RootSpecV1Config;
use tcfs_core::config::{
    validate_registered_root_id, RegisteredRootPlanContractFingerprintV1,
    RegisteredRootPlanContractV1, RootProfileSettingsFingerprintV1, RootProfileV1,
};
use tcfs_storage::ConditionalWriteSemanticsReceipt;

use crate::index_entry::{
    namespace_index_prefix, namespace_logical_entry_from_index_path, namespace_reservation_prefix,
    read_current_raw_object_snapshot_v1, read_raw_object_snapshot_v1,
    validate_canonical_namespace_remote_prefix, RawObjectChangedDuringReadV1,
    RawObjectReadBindingV1, RawObjectReadV1, RawObjectSnapshotTooLargeV1, RawObjectSnapshotV1,
};
use crate::registered_reconcile::{
    bind_remote_object_v1, BoundRemoteObjectSnapshotV1, RegisteredRootRemoteObjectBindingV1,
};
use crate::registered_source_composition::ValidatedSelectedRegisteredRootRemoteContextV1;

const CATALOG_SCHEMA_VERSION_V1: u32 = 1;
const CATALOG_HEAD_SUFFIX_V1: &str = ".tcfs-catalog/v1/head";
const CATALOG_ROOT_SUFFIX_V1: &str = ".tcfs-catalog/v1/roots";
const CATALOG_PAGE_SUFFIX_V1: &str = ".tcfs-catalog/v1/pages";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RemoteCatalogObjectKindV1 {
    Index,
    Reservation,
    Manifest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteCatalogClosureObjectKindV1 {
    Head,
    Root,
    Page,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteCatalogResourceV1 {
    HeadBytes,
    RootBytes,
    PageBytes,
    ClosureBytes,
    Pages,
    EntriesPerPage,
    Entries,
    EntryKeyBytes,
    BindingBytes,
    IndexEntries,
    ReservationEntries,
    ManifestEntries,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvalidRemoteCatalogReasonV1 {
    CanonicalEncoding,
    Context,
    Lineage,
    ObjectAddress,
    ObjectIdentity,
    ObjectBinding,
    PageOrder,
    EntryOrder,
    EntryRoute,
    EntryIdentity,
    Totals,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StrictRemoteCatalogIncompleteV1 {
    InvalidRemotePrefix,
    StorageSemanticsUnverified,
    HeadMissing,
    HeadUnboundCurrentEtag,
    HeadChanged,
    ClosureObjectMissing {
        kind: RemoteCatalogClosureObjectKindV1,
    },
    ClosureObjectUnbound {
        kind: RemoteCatalogClosureObjectKindV1,
    },
    ClosureObjectChanged {
        kind: RemoteCatalogClosureObjectKindV1,
    },
    Invalid {
        kind: RemoteCatalogClosureObjectKindV1,
        reason: InvalidRemoteCatalogReasonV1,
    },
    ResourceLimit(RemoteCatalogResourceV1),
}

pub(crate) enum StrictRemoteCatalogClosureReadV1 {
    Verified(Box<VerifiedRemoteCatalogClosureV1>),
    Incomplete(StrictRemoteCatalogIncompleteV1),
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogContextWireV1 {
    root_id: String,
    root_identity_fingerprint: String,
    root_generation: u64,
    profile: RootProfileV1,
    profile_settings_fingerprint: String,
    plan_contract_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogObjectBindingWireV1 {
    version: Option<String>,
    etag: Option<String>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogRootReferenceWireV1 {
    object_id: String,
    raw_bytes_len: u64,
    binding: RemoteCatalogObjectBindingWireV1,
    page_count: u64,
    entry_count: u64,
    entry_key_bytes: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogHeadWireV1 {
    version: u32,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    parent_head_revision: Option<String>,
    catalog_root: RemoteCatalogRootReferenceWireV1,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogPageReferenceWireV1 {
    ordinal: u64,
    object_id: String,
    raw_bytes_len: u64,
    binding: RemoteCatalogObjectBindingWireV1,
    entry_count: u64,
    entry_key_bytes: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogRootWireV1 {
    version: u32,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    parent_head_revision: Option<String>,
    page_count: u64,
    entry_count: u64,
    entry_key_bytes: u64,
    pages: Vec<RemoteCatalogPageReferenceWireV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogEntryWireV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    raw_bytes_len: u64,
    raw_blake3: String,
    binding: RemoteCatalogObjectBindingWireV1,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogPageWireV1 {
    version: u32,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    ordinal: u64,
    entry_count: u64,
    entry_key_bytes: u64,
    entries: Vec<RemoteCatalogEntryWireV1>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct VerifiedRemoteCatalogEntryV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    raw_bytes_len: u64,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

impl VerifiedRemoteCatalogEntryV1 {
    pub(crate) const fn kind(&self) -> RemoteCatalogObjectKindV1 {
        self.kind
    }

    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }

    pub(crate) const fn raw_bytes_len(&self) -> u64 {
        self.raw_bytes_len
    }

    pub(crate) const fn raw_blake3(&self) -> &[u8; 32] {
        &self.raw_blake3
    }

    pub(crate) const fn binding(&self) -> &RegisteredRootRemoteObjectBindingV1 {
        &self.binding
    }
}

/// Internally consistent immutable catalog inventory selected by one unchanged
/// current HEAD.
///
/// This remains weaker than complete remote truth until all named namespace
/// objects are bound and every legacy/direct writer plus bootstrap path is
/// fenced by the catalog publication protocol.
pub(crate) struct VerifiedRemoteCatalogClosureV1 {
    remote_prefix: String,
    root_id: String,
    root_identity_fingerprint: String,
    root_generation: NonZeroU64,
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
    catalog_sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: Option<[u8; 32]>,
    head_revision: [u8; 32],
    head_object: BoundRemoteObjectSnapshotV1,
    root_object: BoundRemoteObjectSnapshotV1,
    page_objects: Vec<BoundRemoteObjectSnapshotV1>,
    entries: Vec<VerifiedRemoteCatalogEntryV1>,
}

impl std::fmt::Debug for VerifiedRemoteCatalogClosureV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedRemoteCatalogClosureV1")
            .field("remote_prefix", &self.remote_prefix)
            .field("root_id", &self.root_id)
            .field("root_identity_fingerprint", &self.root_identity_fingerprint)
            .field("root_generation", &self.root_generation)
            .field("profile", &self.profile)
            .field("catalog_sequence", &self.catalog_sequence)
            .field("page_count", &self.page_objects.len())
            .field("entry_count", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl VerifiedRemoteCatalogClosureV1 {
    pub(crate) fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) fn root_identity_fingerprint(&self) -> &str {
        &self.root_identity_fingerprint
    }

    pub(crate) const fn root_generation(&self) -> NonZeroU64 {
        self.root_generation
    }

    pub(crate) const fn profile(&self) -> RootProfileV1 {
        self.profile
    }

    pub(crate) const fn profile_settings_fingerprint(&self) -> RootProfileSettingsFingerprintV1 {
        self.profile_settings_fingerprint
    }

    pub(crate) const fn plan_contract_fingerprint(
        &self,
    ) -> RegisteredRootPlanContractFingerprintV1 {
        self.plan_contract_fingerprint
    }

    pub(crate) const fn catalog_sequence(&self) -> NonZeroU64 {
        self.catalog_sequence
    }

    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = &VerifiedRemoteCatalogEntryV1> {
        self.entries.iter()
    }
}

fn join_remote_key_v1(remote_prefix: &str, suffix: &str) -> String {
    format!("{remote_prefix}/{suffix}")
}

fn catalog_head_key_v1(remote_prefix: &str) -> String {
    join_remote_key_v1(remote_prefix, CATALOG_HEAD_SUFFIX_V1)
}

fn catalog_root_key_v1(remote_prefix: &str, object_id: &str) -> String {
    format!("{}/{}/{}", remote_prefix, CATALOG_ROOT_SUFFIX_V1, object_id)
}

fn catalog_page_key_v1(remote_prefix: &str, object_id: &str) -> String {
    format!("{}/{}/{}", remote_prefix, CATALOG_PAGE_SUFFIX_V1, object_id)
}

fn domain_object_id_v1(domain: &str, raw_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(raw_bytes);
    *hasher.finalize().as_bytes()
}

fn catalog_head_revision_v1(raw_bytes: &[u8]) -> [u8; 32] {
    domain_object_id_v1("tinyland.tcfs.remote-catalog-head-revision.b3v1", raw_bytes)
}

fn catalog_root_object_id_v1(raw_bytes: &[u8]) -> [u8; 32] {
    domain_object_id_v1("tinyland.tcfs.remote-catalog-root-object.b3v1", raw_bytes)
}

fn catalog_page_object_id_v1(raw_bytes: &[u8]) -> [u8; 32] {
    domain_object_id_v1("tinyland.tcfs.remote-catalog-page-object.b3v1", raw_bytes)
}

fn lower_hex(bytes: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
    }
    text
}

fn parse_lower_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    let mut result = [0_u8; 32];
    for (index, output) in result.iter_mut().enumerate() {
        let start = index.checked_mul(2)?;
        *output = u8::from_str_radix(value.get(start..start + 2)?, 16).ok()?;
    }
    Some(result)
}

fn valid_b3v1_fingerprint(value: &str) -> bool {
    value
        .strip_prefix("b3v1:")
        .and_then(parse_lower_hex_32)
        .is_some()
}

fn invalid(
    kind: RemoteCatalogClosureObjectKindV1,
    reason: InvalidRemoteCatalogReasonV1,
) -> StrictRemoteCatalogIncompleteV1 {
    StrictRemoteCatalogIncompleteV1::Invalid { kind, reason }
}

fn validate_storage_key_bound_v1(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && !key.ends_with('/')
        && !key.contains('\\')
        && !key.chars().any(char::is_control)
        && !key
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        && u64::try_from(key.len()).is_ok_and(|length| {
            length
                <= RegisteredRootPlanContractV1::strict_v1()
                    .remote_contract()
                    .max_storage_key_bytes()
        })
}

fn validate_catalog_context_v1(
    context: &RemoteCatalogContextWireV1,
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
) -> Option<(
    NonZeroU64,
    RootProfileSettingsFingerprintV1,
    RegisteredRootPlanContractFingerprintV1,
)> {
    validate_registered_root_id(&context.root_id).ok()?;
    let generation = NonZeroU64::new(context.root_generation)?;
    let expected_spec = selected.spec();
    if context.root_id != selected.root_id()
        || context.root_identity_fingerprint != selected.spec_identity_fingerprint()
        || generation != expected_spec.generation
        || context.profile != expected_spec.profile
        || context.profile_settings_fingerprint
            != selected.profile_settings_fingerprint().to_string()
        || context.plan_contract_fingerprint != selected.plan_contract_fingerprint().to_string()
    {
        return None;
    }
    if !valid_b3v1_fingerprint(&context.root_identity_fingerprint)
        || expected_spec.identity_fingerprint(selected.root_id())
            != context.root_identity_fingerprint
        || expected_spec.profile.policy().settings_fingerprint()
            != selected.profile_settings_fingerprint()
        || RegisteredRootPlanContractV1::strict_v1().fingerprint()
            != selected.plan_contract_fingerprint()
    {
        return None;
    }
    Some((
        generation,
        selected.profile_settings_fingerprint(),
        selected.plan_contract_fingerprint(),
    ))
}

fn validate_catalog_lineage_v1(
    catalog_sequence: u64,
    publication_nonce: &str,
    parent_head_revision: Option<&str>,
) -> Option<(NonZeroU64, [u8; 32], Option<[u8; 32]>)> {
    let sequence = NonZeroU64::new(catalog_sequence)?;
    let nonce = parse_lower_hex_32(publication_nonce)?;
    if nonce == [0_u8; 32] {
        return None;
    }
    let parent = match parent_head_revision {
        Some(parent) => Some(parse_lower_hex_32(parent)?),
        None => None,
    };
    if (sequence.get() == 1) != parent.is_none() {
        return None;
    }
    Some((sequence, nonce, parent))
}

fn canonical_wire_v1<T>(raw_bytes: &[u8]) -> Option<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let parsed = serde_json::from_slice::<T>(raw_bytes).ok()?;
    (serde_json::to_vec(&parsed).ok()?.as_slice() == raw_bytes).then_some(parsed)
}

fn validate_binding_wire_v1(
    binding: &RemoteCatalogObjectBindingWireV1,
) -> Option<RegisteredRootRemoteObjectBindingV1> {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let valid_token = |value: &str| {
        !value.is_empty()
            && value != "null"
            && u64::try_from(value.len())
                .is_ok_and(|length| length <= remote.max_binding_token_bytes())
    };
    match (binding.version.as_deref(), binding.etag.as_deref()) {
        (Some(version), etag) if valid_token(version) => {
            if etag.is_some_and(|etag| !valid_token(etag)) {
                return None;
            }
            Some(RegisteredRootRemoteObjectBindingV1::Version {
                version: version.to_owned(),
                etag: etag.map(str::to_owned),
            })
        }
        (None, Some(etag)) if valid_token(etag) => {
            Some(RegisteredRootRemoteObjectBindingV1::Etag {
                etag: etag.to_owned(),
            })
        }
        _ => None,
    }
}

fn binding_wire_bytes_v1(binding: &RemoteCatalogObjectBindingWireV1) -> Option<u64> {
    binding
        .version
        .as_ref()
        .into_iter()
        .chain(binding.etag.as_ref())
        .try_fold(0_u64, |total, token| {
            total.checked_add(u64::try_from(token.len()).ok()?)
        })
}

fn observed_binding_matches_v1(
    raw: &RawObjectSnapshotV1,
    expected: &RemoteCatalogObjectBindingWireV1,
) -> bool {
    match (
        raw.binding(),
        expected.version.as_deref(),
        expected.etag.as_deref(),
    ) {
        (
            RawObjectReadBindingV1::Version { version, etag },
            Some(expected_version),
            expected_etag,
        ) => version == expected_version && etag.as_deref() == expected_etag,
        (RawObjectReadBindingV1::Etag { etag }, None, Some(expected_etag)) => etag == expected_etag,
        _ => false,
    }
}

fn validate_catalog_entry_route_v1(remote_prefix: &str, entry: &RemoteCatalogEntryWireV1) -> bool {
    if !validate_storage_key_bound_v1(&entry.object_key) {
        return false;
    }
    match entry.kind {
        RemoteCatalogObjectKindV1::Index => entry
            .object_key
            .strip_prefix(&namespace_index_prefix(remote_prefix))
            .filter(|relative| !relative.is_empty())
            .is_some_and(|relative| namespace_logical_entry_from_index_path(relative).is_ok()),
        RemoteCatalogObjectKindV1::Reservation => entry
            .object_key
            .strip_prefix(&namespace_reservation_prefix(remote_prefix))
            .and_then(parse_lower_hex_32)
            .is_some(),
        RemoteCatalogObjectKindV1::Manifest => entry
            .object_key
            .strip_prefix(&format!("{remote_prefix}/manifests/"))
            .and_then(parse_lower_hex_32)
            .is_some(),
    }
}

fn validate_entry_size_v1(kind: RemoteCatalogObjectKindV1, raw_bytes_len: u64) -> bool {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    raw_bytes_len > 0
        && raw_bytes_len
            <= match kind {
                RemoteCatalogObjectKindV1::Index => remote.max_index_object_bytes(),
                RemoteCatalogObjectKindV1::Reservation => remote.max_reservation_object_bytes(),
                RemoteCatalogObjectKindV1::Manifest => remote.max_manifest_object_bytes(),
            }
}

#[derive(Default)]
struct RemoteCatalogBudgetV1 {
    closure_bytes: u64,
    pages: u64,
    entries: u64,
    entry_key_bytes: u64,
    binding_bytes: u64,
    index_entries: u64,
    reservation_entries: u64,
    manifest_entries: u64,
}

impl RemoteCatalogBudgetV1 {
    fn checked_add(
        value: &mut u64,
        increment: u64,
        maximum: u64,
        resource: RemoteCatalogResourceV1,
    ) -> std::result::Result<(), StrictRemoteCatalogIncompleteV1> {
        *value = value
            .checked_add(increment)
            .ok_or(StrictRemoteCatalogIncompleteV1::ResourceLimit(resource))?;
        if *value > maximum {
            return Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(resource));
        }
        Ok(())
    }

    fn observe_root(
        &mut self,
        raw_bytes_len: u64,
        binding: &RemoteCatalogObjectBindingWireV1,
    ) -> std::result::Result<(), StrictRemoteCatalogIncompleteV1> {
        let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        if raw_bytes_len > remote.max_catalog_root_object_bytes() {
            return Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::RootBytes,
            ));
        }
        Self::checked_add(
            &mut self.closure_bytes,
            raw_bytes_len,
            remote.max_catalog_closure_object_bytes(),
            RemoteCatalogResourceV1::ClosureBytes,
        )?;
        self.observe_binding(binding)
    }

    fn observe_page(
        &mut self,
        raw_bytes_len: u64,
        binding: &RemoteCatalogObjectBindingWireV1,
    ) -> std::result::Result<(), StrictRemoteCatalogIncompleteV1> {
        let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        if raw_bytes_len > remote.max_catalog_page_object_bytes() {
            return Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::PageBytes,
            ));
        }
        Self::checked_add(
            &mut self.pages,
            1,
            remote.max_catalog_pages(),
            RemoteCatalogResourceV1::Pages,
        )?;
        Self::checked_add(
            &mut self.closure_bytes,
            raw_bytes_len,
            remote.max_catalog_closure_object_bytes(),
            RemoteCatalogResourceV1::ClosureBytes,
        )?;
        self.observe_binding(binding)
    }

    fn observe_binding(
        &mut self,
        binding: &RemoteCatalogObjectBindingWireV1,
    ) -> std::result::Result<(), StrictRemoteCatalogIncompleteV1> {
        let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        Self::checked_add(
            &mut self.binding_bytes,
            binding_wire_bytes_v1(binding).ok_or(
                StrictRemoteCatalogIncompleteV1::ResourceLimit(
                    RemoteCatalogResourceV1::BindingBytes,
                ),
            )?,
            remote.max_catalog_binding_bytes(),
            RemoteCatalogResourceV1::BindingBytes,
        )
    }

    fn observe_entry(
        &mut self,
        entry: &RemoteCatalogEntryWireV1,
    ) -> std::result::Result<(), StrictRemoteCatalogIncompleteV1> {
        let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        Self::checked_add(
            &mut self.entries,
            1,
            remote.max_catalog_entries(),
            RemoteCatalogResourceV1::Entries,
        )?;
        Self::checked_add(
            &mut self.entry_key_bytes,
            u64::try_from(entry.object_key.len()).map_err(|_| {
                StrictRemoteCatalogIncompleteV1::ResourceLimit(
                    RemoteCatalogResourceV1::EntryKeyBytes,
                )
            })?,
            remote.max_catalog_entry_key_bytes(),
            RemoteCatalogResourceV1::EntryKeyBytes,
        )?;
        self.observe_binding(&entry.binding)?;
        let (value, maximum, resource) = match entry.kind {
            RemoteCatalogObjectKindV1::Index => (
                &mut self.index_entries,
                remote.max_index_observations_per_pass(),
                RemoteCatalogResourceV1::IndexEntries,
            ),
            RemoteCatalogObjectKindV1::Reservation => (
                &mut self.reservation_entries,
                remote.max_reservation_observations_per_pass(),
                RemoteCatalogResourceV1::ReservationEntries,
            ),
            RemoteCatalogObjectKindV1::Manifest => (
                &mut self.manifest_entries,
                remote.max_index_observations_per_pass(),
                RemoteCatalogResourceV1::ManifestEntries,
            ),
        };
        Self::checked_add(value, 1, maximum, resource)
    }
}

fn read_changed(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RawObjectChangedDuringReadV1>()
        .is_some()
}

fn read_too_large(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RawObjectSnapshotTooLargeV1>()
        .is_some()
}

async fn read_current_head_v1(
    op: &Operator,
    head_key: &str,
) -> Result<std::result::Result<RawObjectSnapshotV1, StrictRemoteCatalogIncompleteV1>> {
    let maximum = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_catalog_head_object_bytes();
    let read = match read_current_raw_object_snapshot_v1(op, head_key, maximum).await {
        Ok(read) => read,
        Err(error) if read_changed(&error) => {
            return Ok(Err(StrictRemoteCatalogIncompleteV1::HeadChanged));
        }
        Err(error) if read_too_large(&error) => {
            return Ok(Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::HeadBytes,
            )));
        }
        Err(error) => return Err(error),
    };
    Ok(match read {
        None => Err(StrictRemoteCatalogIncompleteV1::HeadMissing),
        Some(RawObjectReadV1::Unbound) => {
            Err(StrictRemoteCatalogIncompleteV1::HeadUnboundCurrentEtag)
        }
        Some(RawObjectReadV1::Bound(snapshot)) => Ok(snapshot),
    })
}

async fn read_immutable_closure_object_v1(
    op: &Operator,
    key: &str,
    maximum: u64,
    kind: RemoteCatalogClosureObjectKindV1,
) -> Result<std::result::Result<RawObjectSnapshotV1, StrictRemoteCatalogIncompleteV1>> {
    let read = match read_raw_object_snapshot_v1(op, key, maximum).await {
        Ok(read) => read,
        Err(error) if read_changed(&error) => {
            return Ok(Err(StrictRemoteCatalogIncompleteV1::ClosureObjectChanged {
                kind,
            }));
        }
        Err(error) if read_too_large(&error) => {
            let resource = match kind {
                RemoteCatalogClosureObjectKindV1::Head => RemoteCatalogResourceV1::HeadBytes,
                RemoteCatalogClosureObjectKindV1::Root => RemoteCatalogResourceV1::RootBytes,
                RemoteCatalogClosureObjectKindV1::Page => RemoteCatalogResourceV1::PageBytes,
            };
            return Ok(Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                resource,
            )));
        }
        Err(error) => return Err(error),
    };
    Ok(match read {
        None => Err(StrictRemoteCatalogIncompleteV1::ClosureObjectMissing { kind }),
        Some(RawObjectReadV1::Unbound) => {
            Err(StrictRemoteCatalogIncompleteV1::ClosureObjectUnbound { kind })
        }
        Some(RawObjectReadV1::Bound(snapshot)) => Ok(snapshot),
    })
}

struct ValidatedRemoteCatalogHeadV1 {
    wire: RemoteCatalogHeadWireV1,
    catalog_sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: Option<[u8; 32]>,
    root_generation: NonZeroU64,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
}

fn validate_head_v1(
    raw: &RawObjectSnapshotV1,
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
) -> std::result::Result<ValidatedRemoteCatalogHeadV1, StrictRemoteCatalogIncompleteV1> {
    let kind = RemoteCatalogClosureObjectKindV1::Head;
    let head = canonical_wire_v1::<RemoteCatalogHeadWireV1>(raw.raw_bytes())
        .ok_or_else(|| invalid(kind, InvalidRemoteCatalogReasonV1::CanonicalEncoding))?;
    if head.version != CATALOG_SCHEMA_VERSION_V1 {
        return Err(invalid(
            kind,
            InvalidRemoteCatalogReasonV1::CanonicalEncoding,
        ));
    }
    let (root_generation, profile_fingerprint, plan_fingerprint) =
        validate_catalog_context_v1(&head.context, selected)
            .ok_or_else(|| invalid(kind, InvalidRemoteCatalogReasonV1::Context))?;
    let (sequence, nonce, parent) = validate_catalog_lineage_v1(
        head.catalog_sequence,
        &head.publication_nonce,
        head.parent_head_revision.as_deref(),
    )
    .ok_or_else(|| invalid(kind, InvalidRemoteCatalogReasonV1::Lineage))?;
    if parse_lower_hex_32(&head.catalog_root.object_id).is_none()
        || head.catalog_root.raw_bytes_len == 0
        || validate_binding_wire_v1(&head.catalog_root.binding).is_none()
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::ObjectIdentity));
    }
    Ok(ValidatedRemoteCatalogHeadV1 {
        wire: head,
        catalog_sequence: sequence,
        publication_nonce: nonce,
        parent_head_revision: parent,
        root_generation,
        profile_settings_fingerprint: profile_fingerprint,
        plan_contract_fingerprint: plan_fingerprint,
    })
}

fn validate_root_v1(
    raw: &RawObjectSnapshotV1,
    head: &RemoteCatalogHeadWireV1,
) -> std::result::Result<RemoteCatalogRootWireV1, StrictRemoteCatalogIncompleteV1> {
    let kind = RemoteCatalogClosureObjectKindV1::Root;
    if lower_hex(&catalog_root_object_id_v1(raw.raw_bytes())) != head.catalog_root.object_id {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::ObjectAddress));
    }
    if u64::try_from(raw.raw_bytes().len()).ok() != Some(head.catalog_root.raw_bytes_len)
        || !observed_binding_matches_v1(raw, &head.catalog_root.binding)
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::ObjectBinding));
    }
    let root = canonical_wire_v1::<RemoteCatalogRootWireV1>(raw.raw_bytes())
        .ok_or_else(|| invalid(kind, InvalidRemoteCatalogReasonV1::CanonicalEncoding))?;
    if root.version != CATALOG_SCHEMA_VERSION_V1
        || root.context != head.context
        || root.catalog_sequence != head.catalog_sequence
        || root.publication_nonce != head.publication_nonce
        || root.parent_head_revision != head.parent_head_revision
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::Context));
    }
    if root.page_count != head.catalog_root.page_count
        || root.entry_count != head.catalog_root.entry_count
        || root.entry_key_bytes != head.catalog_root.entry_key_bytes
        || u64::try_from(root.pages.len()).ok() != Some(root.page_count)
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::Totals));
    }
    Ok(root)
}

fn validate_page_v1(
    raw: &RawObjectSnapshotV1,
    reference: &RemoteCatalogPageReferenceWireV1,
    root: &RemoteCatalogRootWireV1,
) -> std::result::Result<RemoteCatalogPageWireV1, StrictRemoteCatalogIncompleteV1> {
    let kind = RemoteCatalogClosureObjectKindV1::Page;
    if lower_hex(&catalog_page_object_id_v1(raw.raw_bytes())) != reference.object_id {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::ObjectAddress));
    }
    if u64::try_from(raw.raw_bytes().len()).ok() != Some(reference.raw_bytes_len)
        || !observed_binding_matches_v1(raw, &reference.binding)
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::ObjectBinding));
    }
    let page = canonical_wire_v1::<RemoteCatalogPageWireV1>(raw.raw_bytes())
        .ok_or_else(|| invalid(kind, InvalidRemoteCatalogReasonV1::CanonicalEncoding))?;
    if page.version != CATALOG_SCHEMA_VERSION_V1
        || page.context != root.context
        || page.catalog_sequence != root.catalog_sequence
        || page.publication_nonce != root.publication_nonce
        || page.ordinal != reference.ordinal
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::Context));
    }
    let entry_key_bytes = page.entries.iter().try_fold(0_u64, |total, entry| {
        total.checked_add(u64::try_from(entry.object_key.len()).ok()?)
    });
    if u64::try_from(page.entries.len()).ok() != Some(page.entry_count)
        || page.entry_count != reference.entry_count
        || entry_key_bytes != Some(page.entry_key_bytes)
        || page.entry_key_bytes != reference.entry_key_bytes
    {
        return Err(invalid(kind, InvalidRemoteCatalogReasonV1::Totals));
    }
    Ok(page)
}

/// Validate one exact immutable catalog root and every ordered page selected by
/// an unchanged, current, ETag-bound HEAD.
///
/// No LIST operation is issued. The expected context must come from the
/// daemon-authenticated selected-root route, never from the remote bytes. The
/// caller must also acquire a live conditional-semantics receipt for this
/// exact operator and prefix.
pub(crate) async fn read_verified_remote_catalog_closure_v1(
    op: &Operator,
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
    receipt: &ConditionalWriteSemanticsReceipt,
) -> Result<StrictRemoteCatalogClosureReadV1> {
    let remote_prefix =
        match validate_canonical_namespace_remote_prefix(&selected.spec().remote_prefix) {
            Ok(prefix) if !prefix.is_empty() => prefix,
            _ => {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
                    StrictRemoteCatalogIncompleteV1::InvalidRemotePrefix,
                ));
            }
        };
    if !receipt.authorizes(op, remote_prefix)? {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
            StrictRemoteCatalogIncompleteV1::StorageSemanticsUnverified,
        ));
    }
    let head_key = catalog_head_key_v1(remote_prefix);
    if !validate_storage_key_bound_v1(&head_key) {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
            StrictRemoteCatalogIncompleteV1::InvalidRemotePrefix,
        ));
    }
    let head_a_raw = match read_current_head_v1(op, &head_key).await? {
        Ok(raw) => raw,
        Err(incomplete) => {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
    };
    let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    if u64::try_from(head_a_raw.raw_bytes().len()).map_or(true, |length| {
        length > remote_contract.max_catalog_head_object_bytes()
    }) {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
            StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::HeadBytes),
        ));
    }
    let ValidatedRemoteCatalogHeadV1 {
        wire: head,
        catalog_sequence,
        publication_nonce,
        parent_head_revision,
        root_generation,
        profile_settings_fingerprint,
        plan_contract_fingerprint,
    } = match validate_head_v1(&head_a_raw, selected) {
        Ok(validated) => validated,
        Err(incomplete) => {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
    };

    let mut budget = RemoteCatalogBudgetV1::default();
    let root_key = catalog_root_key_v1(remote_prefix, &head.catalog_root.object_id);
    if !validate_storage_key_bound_v1(&root_key) {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
            RemoteCatalogClosureObjectKindV1::Root,
            InvalidRemoteCatalogReasonV1::ObjectAddress,
        )));
    }
    let root_raw = match read_immutable_closure_object_v1(
        op,
        &root_key,
        remote_contract.max_catalog_root_object_bytes(),
        RemoteCatalogClosureObjectKindV1::Root,
    )
    .await?
    {
        Ok(raw) => raw,
        Err(incomplete) => {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
    };
    if let Err(incomplete) = budget.observe_root(
        u64::try_from(root_raw.raw_bytes().len()).unwrap_or(u64::MAX),
        &head.catalog_root.binding,
    ) {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
    }
    let root = match validate_root_v1(&root_raw, &head) {
        Ok(root) => root,
        Err(incomplete) => {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
    };
    if root.page_count > remote_contract.max_catalog_pages()
        || root.entry_count > remote_contract.max_catalog_entries()
        || root.entry_key_bytes > remote_contract.max_catalog_entry_key_bytes()
    {
        let resource = if root.page_count > remote_contract.max_catalog_pages() {
            RemoteCatalogResourceV1::Pages
        } else if root.entry_count > remote_contract.max_catalog_entries() {
            RemoteCatalogResourceV1::Entries
        } else {
            RemoteCatalogResourceV1::EntryKeyBytes
        };
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
            StrictRemoteCatalogIncompleteV1::ResourceLimit(resource),
        ));
    }

    let mut page_objects = Vec::with_capacity(root.pages.len());
    // The aggregate root count is untrusted until every page is drained. Grow
    // only by each already-bounded page so a tiny contradictory root cannot
    // force the full multi-million-entry allocation before totals fail.
    let mut entries = Vec::new();
    let mut previous_key: Option<String> = None;
    for (expected_ordinal, reference) in root.pages.iter().enumerate() {
        if reference.ordinal != u64::try_from(expected_ordinal).unwrap_or(u64::MAX)
            || parse_lower_hex_32(&reference.object_id).is_none()
            || reference.raw_bytes_len == 0
            || validate_binding_wire_v1(&reference.binding).is_none()
        {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                RemoteCatalogClosureObjectKindV1::Root,
                InvalidRemoteCatalogReasonV1::PageOrder,
            )));
        }
        if reference.entry_count > remote_contract.max_catalog_entries_per_page() {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
                StrictRemoteCatalogIncompleteV1::ResourceLimit(
                    RemoteCatalogResourceV1::EntriesPerPage,
                ),
            ));
        }
        let page_key = catalog_page_key_v1(remote_prefix, &reference.object_id);
        if !validate_storage_key_bound_v1(&page_key) {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                RemoteCatalogClosureObjectKindV1::Page,
                InvalidRemoteCatalogReasonV1::ObjectAddress,
            )));
        }
        let page_raw = match read_immutable_closure_object_v1(
            op,
            &page_key,
            remote_contract.max_catalog_page_object_bytes(),
            RemoteCatalogClosureObjectKindV1::Page,
        )
        .await?
        {
            Ok(raw) => raw,
            Err(incomplete) => {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
            }
        };
        if let Err(incomplete) = budget.observe_page(
            u64::try_from(page_raw.raw_bytes().len()).unwrap_or(u64::MAX),
            &reference.binding,
        ) {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
        let page = match validate_page_v1(&page_raw, reference, &root) {
            Ok(page) => page,
            Err(incomplete) => {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
            }
        };
        if page.entry_count > remote_contract.max_catalog_entries_per_page() {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
                StrictRemoteCatalogIncompleteV1::ResourceLimit(
                    RemoteCatalogResourceV1::EntriesPerPage,
                ),
            ));
        }
        if entries.try_reserve(page.entries.len()).is_err() {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
                StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::Entries),
            ));
        }

        for entry in page.entries {
            if let Err(incomplete) = budget.observe_entry(&entry) {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
            }
            if previous_key
                .as_deref()
                .is_some_and(|previous| previous >= entry.object_key.as_str())
            {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                    RemoteCatalogClosureObjectKindV1::Page,
                    InvalidRemoteCatalogReasonV1::EntryOrder,
                )));
            }
            if !validate_catalog_entry_route_v1(remote_prefix, &entry) {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                    RemoteCatalogClosureObjectKindV1::Page,
                    InvalidRemoteCatalogReasonV1::EntryRoute,
                )));
            }
            let Some(raw_blake3) = parse_lower_hex_32(&entry.raw_blake3) else {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                    RemoteCatalogClosureObjectKindV1::Page,
                    InvalidRemoteCatalogReasonV1::EntryIdentity,
                )));
            };
            if !validate_entry_size_v1(entry.kind, entry.raw_bytes_len) {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                    RemoteCatalogClosureObjectKindV1::Page,
                    InvalidRemoteCatalogReasonV1::EntryIdentity,
                )));
            }
            let Some(binding) = validate_binding_wire_v1(&entry.binding) else {
                return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
                    RemoteCatalogClosureObjectKindV1::Page,
                    InvalidRemoteCatalogReasonV1::EntryIdentity,
                )));
            };
            previous_key = Some(entry.object_key.clone());
            entries.push(VerifiedRemoteCatalogEntryV1 {
                kind: entry.kind,
                object_key: entry.object_key,
                raw_bytes_len: entry.raw_bytes_len,
                raw_blake3,
                binding,
            });
        }
        page_objects.push(bind_remote_object_v1(page_raw));
    }
    if budget.pages != root.page_count
        || budget.entries != root.entry_count
        || budget.entry_key_bytes != root.entry_key_bytes
    {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(invalid(
            RemoteCatalogClosureObjectKindV1::Root,
            InvalidRemoteCatalogReasonV1::Totals,
        )));
    }

    let head_b_raw = match read_current_head_v1(op, &head_key).await? {
        Ok(raw) => raw,
        Err(incomplete) => {
            return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(incomplete));
        }
    };
    if head_a_raw != head_b_raw {
        return Ok(StrictRemoteCatalogClosureReadV1::Incomplete(
            StrictRemoteCatalogIncompleteV1::HeadChanged,
        ));
    }

    let head_revision = catalog_head_revision_v1(head_a_raw.raw_bytes());
    let head_object = bind_remote_object_v1(head_a_raw);
    let root_object = bind_remote_object_v1(root_raw);
    Ok(StrictRemoteCatalogClosureReadV1::Verified(Box::new(
        VerifiedRemoteCatalogClosureV1 {
            remote_prefix: remote_prefix.to_owned(),
            root_id: head.context.root_id,
            root_identity_fingerprint: head.context.root_identity_fingerprint,
            root_generation,
            profile: head.context.profile,
            profile_settings_fingerprint,
            plan_contract_fingerprint,
            catalog_sequence,
            publication_nonce,
            parent_head_revision,
            head_revision,
            head_object,
            root_object,
            page_objects,
            entries,
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::raw::{
        oio, Access, AccessorInfo, OpDelete, OpRead, OpStat, OpWrite, RpDelete, RpRead, RpStat,
        RpWrite,
    };
    use opendal::{Buffer, Capability, EntryMode, Error, ErrorKind, Metadata, OperatorBuilder};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static_assertions::assert_not_impl_any!(
        VerifiedRemoteCatalogClosureV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        ValidatedSelectedRegisteredRootRemoteContextV1: Clone,
        serde::Serialize
    );

    #[derive(Clone, Debug)]
    struct ScriptedObject {
        bytes: Vec<u8>,
        etag: Option<String>,
        version: Option<String>,
    }

    #[derive(Clone, Copy, Debug)]
    enum HeadMutationTiming {
        BeforeFirst,
        AfterFirst,
        BeforeSecond,
    }

    #[derive(Clone, Debug)]
    struct HeadMutation {
        timing: HeadMutationTiming,
        replacement: ScriptedObject,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ObservedRead {
        path: String,
        if_match: Option<String>,
        version: Option<String>,
    }

    #[derive(Clone, Debug)]
    struct ScriptedCatalogBackend {
        info: Arc<AccessorInfo>,
        objects: Arc<Mutex<BTreeMap<String, ScriptedObject>>>,
        reads: Arc<Mutex<Vec<ObservedRead>>>,
        stats: Arc<Mutex<Vec<String>>>,
        head_key: String,
        head_reads: Arc<AtomicUsize>,
        head_mutation: Arc<Mutex<Option<HeadMutation>>>,
        next_read_replacements: Arc<Mutex<BTreeMap<String, ScriptedObject>>>,
        next_etag: Arc<AtomicU64>,
    }

    impl ScriptedCatalogBackend {
        fn insert(&self, key: impl Into<String>, object: ScriptedObject) {
            self.objects.lock().unwrap().insert(key.into(), object);
        }

        fn remove(&self, key: &str) {
            self.objects.lock().unwrap().remove(key);
        }

        fn replace_head_on_first_read(
            &self,
            timing: HeadMutationTiming,
            replacement: ScriptedObject,
        ) {
            *self.head_mutation.lock().unwrap() = Some(HeadMutation {
                timing,
                replacement,
            });
        }

        fn replace_object_on_next_read(&self, key: impl Into<String>, replacement: ScriptedObject) {
            self.next_read_replacements
                .lock()
                .unwrap()
                .insert(key.into(), replacement);
        }
    }

    impl Access for ScriptedCatalogBackend {
        type Reader = Buffer;
        type Writer = oio::OneShotWriter<ScriptedCatalogWriter>;
        type Lister = ();
        type Deleter = oio::OneShotDeleter<ScriptedCatalogDeleter>;

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn stat(&self, path: &str, _: OpStat) -> opendal::Result<RpStat> {
            self.stats.lock().unwrap().push(path.to_owned());
            let object = self
                .objects
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "scripted object missing"))?;
            let mut metadata = Metadata::new(EntryMode::FILE)
                .with_content_length(u64::try_from(object.bytes.len()).unwrap());
            if let Some(etag) = object.etag {
                metadata = metadata.with_etag(etag);
            }
            if let Some(version) = object.version {
                metadata = metadata.with_version(version);
            }
            Ok(RpStat::new(metadata))
        }

        async fn read(&self, path: &str, args: OpRead) -> opendal::Result<(RpRead, Buffer)> {
            let head_read_index =
                (path == self.head_key).then(|| self.head_reads.fetch_add(1, Ordering::SeqCst));
            let mutation = head_read_index.and_then(|_| self.head_mutation.lock().unwrap().clone());
            if let Some(HeadMutation {
                timing,
                replacement,
            }) = mutation.as_ref()
            {
                let applies = matches!(
                    (*timing, head_read_index),
                    (HeadMutationTiming::BeforeFirst, Some(0))
                        | (HeadMutationTiming::BeforeSecond, Some(1))
                );
                if applies {
                    self.objects
                        .lock()
                        .unwrap()
                        .insert(path.to_owned(), replacement.clone());
                }
            }
            if let Some(replacement) = self.next_read_replacements.lock().unwrap().remove(path) {
                self.objects
                    .lock()
                    .unwrap()
                    .insert(path.to_owned(), replacement);
            }

            let object = self
                .objects
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "scripted object missing"))?;
            self.reads.lock().unwrap().push(ObservedRead {
                path: path.to_owned(),
                if_match: args.if_match().map(str::to_owned),
                version: args.version().map(str::to_owned),
            });
            if args
                .if_match()
                .is_some_and(|expected| object.etag.as_deref() != Some(expected))
                || args
                    .version()
                    .is_some_and(|expected| object.version.as_deref() != Some(expected))
            {
                return Err(Error::new(
                    ErrorKind::ConditionNotMatch,
                    "scripted object identity changed",
                ));
            }
            let range = args.range();
            let start = usize::try_from(range.offset()).unwrap_or(usize::MAX);
            let end = range
                .size()
                .and_then(|size| usize::try_from(size).ok())
                .map(|size| start.saturating_add(size))
                .unwrap_or(object.bytes.len())
                .min(object.bytes.len());
            let selected = if start <= end {
                object.bytes[start..end].to_vec()
            } else {
                Vec::new()
            };

            if let Some(HeadMutation {
                timing: HeadMutationTiming::AfterFirst,
                replacement,
            }) = mutation.filter(|_| head_read_index == Some(0))
            {
                self.objects
                    .lock()
                    .unwrap()
                    .insert(path.to_owned(), replacement);
            }
            Ok((
                RpRead::new().with_size(Some(u64::try_from(selected.len()).unwrap())),
                Buffer::from(selected),
            ))
        }

        async fn write(
            &self,
            path: &str,
            args: OpWrite,
        ) -> opendal::Result<(RpWrite, Self::Writer)> {
            Ok((
                RpWrite::new(),
                oio::OneShotWriter::new(ScriptedCatalogWriter {
                    path: path.to_owned(),
                    args,
                    objects: self.objects.clone(),
                    next_etag: self.next_etag.clone(),
                }),
            ))
        }

        async fn delete(&self) -> opendal::Result<(RpDelete, Self::Deleter)> {
            Ok((
                RpDelete::default(),
                oio::OneShotDeleter::new(ScriptedCatalogDeleter {
                    objects: self.objects.clone(),
                }),
            ))
        }
    }

    struct ScriptedCatalogWriter {
        path: String,
        args: OpWrite,
        objects: Arc<Mutex<BTreeMap<String, ScriptedObject>>>,
        next_etag: Arc<AtomicU64>,
    }

    impl oio::OneShotWrite for ScriptedCatalogWriter {
        async fn write_once(&self, bytes: Buffer) -> opendal::Result<Metadata> {
            let mut objects = self.objects.lock().unwrap();
            let current_etag = objects
                .get(&self.path)
                .and_then(|object| object.etag.as_deref());
            if self.args.if_not_exists() && current_etag.is_some() {
                return Err(Error::new(
                    ErrorKind::ConditionNotMatch,
                    "scripted create-if-absent rejected existing object",
                ));
            }
            if self
                .args
                .if_match()
                .is_some_and(|expected| current_etag != Some(expected))
            {
                return Err(Error::new(
                    ErrorKind::ConditionNotMatch,
                    "scripted conditional write rejected stale ETag",
                ));
            }
            let generation = self.next_etag.fetch_add(1, Ordering::SeqCst);
            let etag = format!("\"catalog-test-etag-{generation}\"");
            let bytes = bytes.to_vec();
            objects.insert(
                self.path.clone(),
                ScriptedObject {
                    bytes: bytes.clone(),
                    etag: Some(etag.clone()),
                    version: None,
                },
            );
            Ok(Metadata::new(EntryMode::FILE)
                .with_content_length(u64::try_from(bytes.len()).unwrap())
                .with_etag(etag))
        }
    }

    struct ScriptedCatalogDeleter {
        objects: Arc<Mutex<BTreeMap<String, ScriptedObject>>>,
    }

    impl oio::OneShotDelete for ScriptedCatalogDeleter {
        async fn delete_once(&self, path: String, _: OpDelete) -> opendal::Result<()> {
            self.objects.lock().unwrap().remove(&path);
            Ok(())
        }
    }

    fn scripted_operator(prefix: &str) -> (Operator, ScriptedCatalogBackend) {
        let info = AccessorInfo::default();
        info.set_scheme("catalog-scripted")
            .set_root("/")
            .set_name("registered-catalog-test")
            .set_native_capability(Capability {
                stat: true,
                read: true,
                read_with_if_match: true,
                read_with_version: true,
                write: true,
                write_can_empty: true,
                write_with_if_match: true,
                write_with_if_not_exists: true,
                delete: true,
                ..Default::default()
            });
        let backend = ScriptedCatalogBackend {
            info: Arc::new(info),
            objects: Arc::new(Mutex::new(BTreeMap::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            stats: Arc::new(Mutex::new(Vec::new())),
            head_key: catalog_head_key_v1(prefix),
            head_reads: Arc::new(AtomicUsize::new(0)),
            head_mutation: Arc::new(Mutex::new(None)),
            next_read_replacements: Arc::new(Mutex::new(BTreeMap::new())),
            next_etag: Arc::new(AtomicU64::new(1)),
        };
        (OperatorBuilder::new(backend.clone()).finish(), backend)
    }

    fn etag_binding(etag: &str) -> RemoteCatalogObjectBindingWireV1 {
        RemoteCatalogObjectBindingWireV1 {
            version: None,
            etag: Some(etag.to_owned()),
        }
    }

    fn version_binding(version: &str, etag: &str) -> RemoteCatalogObjectBindingWireV1 {
        RemoteCatalogObjectBindingWireV1 {
            version: Some(version.to_owned()),
            etag: Some(etag.to_owned()),
        }
    }

    fn scripted_object(bytes: Vec<u8>, etag: &str) -> ScriptedObject {
        ScriptedObject {
            bytes,
            etag: Some(etag.to_owned()),
            version: None,
        }
    }

    fn fixture_context(prefix: &str) -> RemoteCatalogContextWireV1 {
        let root_id = "fixture-root".to_owned();
        let root_generation = 1;
        let profile = RootProfileV1::AgentStaticV1;
        let spec = RootSpecV1Config {
            version: RootSpecV1Config::VERSION,
            remote_prefix: prefix.to_owned(),
            profile,
            generation: NonZeroU64::new(root_generation).unwrap(),
        };
        RemoteCatalogContextWireV1 {
            root_identity_fingerprint: spec.identity_fingerprint(&root_id),
            root_id,
            root_generation,
            profile,
            profile_settings_fingerprint: profile.policy().settings_fingerprint().to_string(),
            plan_contract_fingerprint: RegisteredRootPlanContractV1::strict_v1()
                .fingerprint()
                .to_string(),
        }
    }

    fn fixture_entry(
        kind: RemoteCatalogObjectKindV1,
        object_key: &str,
        body: &[u8],
        etag: &str,
    ) -> RemoteCatalogEntryWireV1 {
        RemoteCatalogEntryWireV1 {
            kind,
            object_key: object_key.to_owned(),
            raw_bytes_len: u64::try_from(body.len()).unwrap(),
            raw_blake3: lower_hex(blake3::hash(body).as_bytes()),
            binding: etag_binding(etag),
        }
    }

    struct CatalogFixture {
        op: Operator,
        backend: ScriptedCatalogBackend,
        selected: ValidatedSelectedRegisteredRootRemoteContextV1,
        remote_prefix: String,
        head_key: String,
        root_key: String,
        page_keys: Vec<String>,
        head_bytes: Vec<u8>,
        root_bytes: Vec<u8>,
    }

    impl CatalogFixture {
        async fn receipt(&self, prefix: &str) -> ConditionalWriteSemanticsReceipt {
            let receipt =
                tcfs_storage::acquire_conditional_write_semantics_receipt(&self.op, prefix)
                    .await
                    .unwrap();
            self.backend.reads.lock().unwrap().clear();
            self.backend.stats.lock().unwrap().clear();
            receipt
        }

        async fn read(
            &self,
            receipt: &ConditionalWriteSemanticsReceipt,
        ) -> Result<StrictRemoteCatalogClosureReadV1> {
            read_verified_remote_catalog_closure_v1(&self.op, &self.selected, receipt).await
        }

        fn rewrite_root(&mut self, root: &RemoteCatalogRootWireV1) {
            let root_bytes = serde_json::to_vec(root).unwrap();
            let root_id = lower_hex(&catalog_root_object_id_v1(&root_bytes));
            let root_key = catalog_root_key_v1(&self.remote_prefix, &root_id);
            let root_etag = "root-etag-rewritten";

            let mut head =
                serde_json::from_slice::<RemoteCatalogHeadWireV1>(&self.head_bytes).unwrap();
            head.catalog_root = RemoteCatalogRootReferenceWireV1 {
                object_id: root_id,
                raw_bytes_len: u64::try_from(root_bytes.len()).unwrap(),
                binding: etag_binding(root_etag),
                page_count: root.page_count,
                entry_count: root.entry_count,
                entry_key_bytes: root.entry_key_bytes,
            };
            let head_bytes = serde_json::to_vec(&head).unwrap();

            self.backend.insert(
                root_key.clone(),
                scripted_object(root_bytes.clone(), root_etag),
            );
            self.backend.insert(
                self.head_key.clone(),
                scripted_object(head_bytes.clone(), "head-etag-a"),
            );
            self.root_key = root_key;
            self.root_bytes = root_bytes;
            self.head_bytes = head_bytes;
        }

        fn make_immutable_objects_versioned(&mut self) {
            let mut root =
                serde_json::from_slice::<RemoteCatalogRootWireV1>(&self.root_bytes).unwrap();
            {
                let mut objects = self.backend.objects.lock().unwrap();
                for (ordinal, reference) in root.pages.iter_mut().enumerate() {
                    let version = format!("page-version-{ordinal}");
                    let etag = reference.binding.etag.as_deref().unwrap().to_owned();
                    reference.binding = version_binding(&version, &etag);
                    objects.get_mut(&self.page_keys[ordinal]).unwrap().version = Some(version);
                }
            }
            self.rewrite_root(&root);

            let root_version = "root-version";
            let root_etag = "root-etag-rewritten";
            self.backend
                .objects
                .lock()
                .unwrap()
                .get_mut(&self.root_key)
                .unwrap()
                .version = Some(root_version.to_owned());
            let mut head =
                serde_json::from_slice::<RemoteCatalogHeadWireV1>(&self.head_bytes).unwrap();
            head.catalog_root.binding = version_binding(root_version, root_etag);
            let head_bytes = serde_json::to_vec(&head).unwrap();
            self.backend.insert(
                self.head_key.clone(),
                ScriptedObject {
                    bytes: head_bytes.clone(),
                    etag: Some("head-etag-a".to_owned()),
                    version: Some("historical-head-version".to_owned()),
                },
            );
            self.head_bytes = head_bytes;
        }
    }

    fn fixture(prefix: &str, pages: Vec<Vec<RemoteCatalogEntryWireV1>>) -> CatalogFixture {
        let context = fixture_context(prefix);
        let publication_nonce = "11".repeat(32);
        let mut objects = Vec::new();
        let mut page_references = Vec::new();
        let mut page_keys = Vec::new();
        let mut total_entries = 0_u64;
        let mut total_key_bytes = 0_u64;

        for (ordinal, entries) in pages.into_iter().enumerate() {
            let entry_count = u64::try_from(entries.len()).unwrap();
            let entry_key_bytes = entries
                .iter()
                .map(|entry| u64::try_from(entry.object_key.len()).unwrap())
                .sum();
            total_entries += entry_count;
            total_key_bytes += entry_key_bytes;
            let page = RemoteCatalogPageWireV1 {
                version: CATALOG_SCHEMA_VERSION_V1,
                context: fixture_context(prefix),
                catalog_sequence: 1,
                publication_nonce: publication_nonce.clone(),
                ordinal: u64::try_from(ordinal).unwrap(),
                entry_count,
                entry_key_bytes,
                entries,
            };
            let page_bytes = serde_json::to_vec(&page).unwrap();
            let page_id = lower_hex(&catalog_page_object_id_v1(&page_bytes));
            let page_key = catalog_page_key_v1(prefix, &page_id);
            let etag = format!("page-etag-{ordinal}");
            page_references.push(RemoteCatalogPageReferenceWireV1 {
                ordinal: u64::try_from(ordinal).unwrap(),
                object_id: page_id,
                raw_bytes_len: u64::try_from(page_bytes.len()).unwrap(),
                binding: etag_binding(&etag),
                entry_count,
                entry_key_bytes,
            });
            page_keys.push(page_key.clone());
            objects.push((page_key, scripted_object(page_bytes, &etag)));
        }

        let root = RemoteCatalogRootWireV1 {
            version: CATALOG_SCHEMA_VERSION_V1,
            context,
            catalog_sequence: 1,
            publication_nonce,
            parent_head_revision: None,
            page_count: u64::try_from(page_references.len()).unwrap(),
            entry_count: total_entries,
            entry_key_bytes: total_key_bytes,
            pages: page_references,
        };
        let root_bytes = serde_json::to_vec(&root).unwrap();
        let root_id = lower_hex(&catalog_root_object_id_v1(&root_bytes));
        let root_key = catalog_root_key_v1(prefix, &root_id);
        let root_etag = "root-etag";
        objects.push((
            root_key.clone(),
            scripted_object(root_bytes.clone(), root_etag),
        ));
        let head = RemoteCatalogHeadWireV1 {
            version: CATALOG_SCHEMA_VERSION_V1,
            context: fixture_context(prefix),
            catalog_sequence: 1,
            publication_nonce: "11".repeat(32),
            parent_head_revision: None,
            catalog_root: RemoteCatalogRootReferenceWireV1 {
                object_id: root_id,
                raw_bytes_len: u64::try_from(root_bytes.len()).unwrap(),
                binding: etag_binding(root_etag),
                page_count: root.page_count,
                entry_count: root.entry_count,
                entry_key_bytes: root.entry_key_bytes,
            },
        };
        let head_bytes = serde_json::to_vec(&head).unwrap();
        let head_key = catalog_head_key_v1(prefix);
        objects.push((
            head_key.clone(),
            scripted_object(head_bytes.clone(), "head-etag-a"),
        ));

        let (op, backend) = scripted_operator(prefix);
        for (key, object) in objects {
            backend.insert(key, object);
        }
        let selected = crate::registered_source_composition::
            validated_selected_registered_root_remote_context_for_test_v1(
                "fixture-root",
                &RootSpecV1Config {
                    version: RootSpecV1Config::VERSION,
                    remote_prefix: prefix.to_owned(),
                    profile: RootProfileV1::AgentStaticV1,
                    generation: NonZeroU64::new(1).unwrap(),
                },
            )
            .unwrap();
        CatalogFixture {
            op,
            backend,
            selected,
            remote_prefix: prefix.to_owned(),
            head_key,
            root_key,
            page_keys,
            head_bytes,
            root_bytes,
        }
    }

    fn incomplete(read: StrictRemoteCatalogClosureReadV1) -> StrictRemoteCatalogIncompleteV1 {
        match read {
            StrictRemoteCatalogClosureReadV1::Incomplete(incomplete) => incomplete,
            StrictRemoteCatalogClosureReadV1::Verified(_) => {
                panic!("expected incomplete catalog closure")
            }
        }
    }

    #[tokio::test]
    async fn explicit_empty_catalog_is_verified_without_any_listing() {
        let fixture = fixture("roots", Vec::new());
        let receipt = fixture.receipt("roots").await;
        let verified = match fixture.read(&receipt).await.unwrap() {
            StrictRemoteCatalogClosureReadV1::Verified(verified) => verified,
            StrictRemoteCatalogClosureReadV1::Incomplete(incomplete) => {
                panic!("expected verified empty catalog, got {incomplete:?}")
            }
        };
        assert_eq!(verified.remote_prefix(), "roots");
        assert_eq!(verified.root_id(), "fixture-root");
        assert_eq!(verified.root_generation().get(), 1);
        assert_eq!(verified.profile(), RootProfileV1::AgentStaticV1);
        assert_eq!(verified.catalog_sequence().get(), 1);
        assert_eq!(verified.entries().len(), 0);
        assert_eq!(
            fixture.backend.reads.lock().unwrap().as_slice(),
            &[
                ObservedRead {
                    path: fixture.head_key.clone(),
                    if_match: Some("head-etag-a".to_owned()),
                    version: None,
                },
                ObservedRead {
                    path: fixture
                        .backend
                        .stats
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|key| key.contains("/roots/"))
                        .unwrap()
                        .clone(),
                    if_match: Some("root-etag".to_owned()),
                    version: None,
                },
                ObservedRead {
                    path: fixture.head_key,
                    if_match: Some("head-etag-a".to_owned()),
                    version: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn populated_pages_retain_one_sorted_catalog_inventory() {
        let index = fixture_entry(
            RemoteCatalogObjectKindV1::Index,
            "roots/index/doc.txt",
            b"index",
            "index-etag",
        );
        let manifest = fixture_entry(
            RemoteCatalogObjectKindV1::Manifest,
            &format!("roots/manifests/{}", "a".repeat(64)),
            b"manifest",
            "manifest-etag",
        );
        let reservation = fixture_entry(
            RemoteCatalogObjectKindV1::Reservation,
            &format!("roots/.tcfs-namespace/v1/{}", "b".repeat(64)),
            b"reservation",
            "reservation-etag",
        );
        let mut entries = vec![reservation, index, manifest];
        entries.sort_by(|left, right| left.object_key.cmp(&right.object_key));
        let fixture = fixture("roots", vec![entries]);
        let receipt = fixture.receipt("roots").await;
        let verified = match fixture.read(&receipt).await.unwrap() {
            StrictRemoteCatalogClosureReadV1::Verified(verified) => verified,
            StrictRemoteCatalogClosureReadV1::Incomplete(incomplete) => {
                panic!("expected verified catalog, got {incomplete:?}")
            }
        };
        let keys = verified
            .entries()
            .map(|entry| entry.object_key())
            .collect::<Vec<_>>();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(keys.len(), 3);
    }

    #[tokio::test]
    async fn immutable_root_and_pages_prefer_versions_but_head_uses_current_etag() {
        let entry = fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let mut fixture = fixture("roots", vec![vec![entry]]);
        fixture.make_immutable_objects_versioned();
        let receipt = fixture.receipt("roots").await;
        assert!(matches!(
            fixture.read(&receipt).await.unwrap(),
            StrictRemoteCatalogClosureReadV1::Verified(_)
        ));

        let reads = fixture.backend.reads.lock().unwrap();
        let head_reads = reads
            .iter()
            .filter(|read| read.path == fixture.head_key)
            .collect::<Vec<_>>();
        assert_eq!(head_reads.len(), 2);
        assert!(head_reads.iter().all(|read| {
            read.version.is_none() && read.if_match.as_deref() == Some("head-etag-a")
        }));
        assert!(reads.iter().any(|read| {
            read.path == fixture.root_key
                && read.version.as_deref() == Some("root-version")
                && read.if_match.as_deref() == Some("root-etag-rewritten")
        }));
        assert!(reads.iter().any(|read| {
            read.path == fixture.page_keys[0]
                && read.version.as_deref() == Some("page-version-0")
                && read.if_match.as_deref() == Some("page-etag-0")
        }));
    }

    #[tokio::test]
    async fn missing_head_or_page_is_typed_and_never_recovered_by_listing() {
        let no_head = fixture("roots", Vec::new());
        no_head.backend.remove(&no_head.head_key);
        let receipt = no_head.receipt("roots").await;
        assert_eq!(
            incomplete(no_head.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::HeadMissing
        );

        let page = fixture_entry(
            RemoteCatalogObjectKindV1::Index,
            "roots/index/doc.txt",
            b"index",
            "index-etag",
        );
        let missing_page = fixture("roots", vec![vec![page]]);
        missing_page.backend.remove(&missing_page.page_keys[0]);
        let receipt = missing_page.receipt("roots").await;
        assert_eq!(
            incomplete(missing_page.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectMissing {
                kind: RemoteCatalogClosureObjectKindV1::Page
            }
        );

        let missing_root = fixture("roots", Vec::new());
        missing_root.backend.remove(&missing_root.root_key);
        let receipt = missing_root.receipt("roots").await;
        assert_eq!(
            incomplete(missing_root.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectMissing {
                kind: RemoteCatalogClosureObjectKindV1::Root
            }
        );
    }

    #[tokio::test]
    async fn unbound_or_changed_closure_objects_are_typed_without_retry() {
        let head = fixture("roots", Vec::new());
        head.backend.insert(
            head.head_key.clone(),
            ScriptedObject {
                bytes: head.head_bytes.clone(),
                etag: None,
                version: None,
            },
        );
        let receipt = head.receipt("roots").await;
        assert_eq!(
            incomplete(head.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::HeadUnboundCurrentEtag
        );

        let root = fixture("roots", Vec::new());
        root.backend.insert(
            root.root_key.clone(),
            ScriptedObject {
                bytes: root.root_bytes.clone(),
                etag: None,
                version: None,
            },
        );
        let receipt = root.receipt("roots").await;
        assert_eq!(
            incomplete(root.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectUnbound {
                kind: RemoteCatalogClosureObjectKindV1::Root
            }
        );

        let page_entry =
            fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let page = fixture("roots", vec![vec![page_entry.clone()]]);
        let page_bytes = page
            .backend
            .objects
            .lock()
            .unwrap()
            .get(&page.page_keys[0])
            .unwrap()
            .bytes
            .clone();
        page.backend.insert(
            page.page_keys[0].clone(),
            ScriptedObject {
                bytes: page_bytes,
                etag: None,
                version: None,
            },
        );
        let receipt = page.receipt("roots").await;
        assert_eq!(
            incomplete(page.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectUnbound {
                kind: RemoteCatalogClosureObjectKindV1::Page
            }
        );

        let changed_root = fixture("roots", Vec::new());
        changed_root.backend.replace_object_on_next_read(
            changed_root.root_key.clone(),
            scripted_object(changed_root.root_bytes.clone(), "root-etag-changed"),
        );
        let receipt = changed_root.receipt("roots").await;
        assert_eq!(
            incomplete(changed_root.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectChanged {
                kind: RemoteCatalogClosureObjectKindV1::Root
            }
        );

        let changed_page = fixture("roots", vec![vec![page_entry]]);
        let page_bytes = changed_page
            .backend
            .objects
            .lock()
            .unwrap()
            .get(&changed_page.page_keys[0])
            .unwrap()
            .bytes
            .clone();
        changed_page.backend.replace_object_on_next_read(
            changed_page.page_keys[0].clone(),
            scripted_object(page_bytes, "page-etag-changed"),
        );
        let receipt = changed_page.receipt("roots").await;
        assert_eq!(
            incomplete(changed_page.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ClosureObjectChanged {
                kind: RemoteCatalogClosureObjectKindV1::Page
            }
        );
    }

    #[tokio::test]
    async fn every_current_head_change_interleaving_fails_without_retry() {
        for (timing, expected_head_reads) in [
            (HeadMutationTiming::BeforeFirst, 1),
            (HeadMutationTiming::AfterFirst, 2),
            (HeadMutationTiming::BeforeSecond, 2),
        ] {
            let fixture = fixture("roots", Vec::new());
            fixture.backend.replace_head_on_first_read(
                timing,
                scripted_object(fixture.head_bytes.clone(), "head-etag-b"),
            );
            let receipt = fixture.receipt("roots").await;
            assert_eq!(
                incomplete(fixture.read(&receipt).await.unwrap()),
                StrictRemoteCatalogIncompleteV1::HeadChanged
            );
            assert_eq!(
                fixture.backend.head_reads.load(Ordering::SeqCst),
                expected_head_reads,
                "reader retried instead of failing this head-movement interleaving"
            );
        }
    }

    #[tokio::test]
    async fn catalog_rejects_noncanonical_head_unsorted_entries_and_cross_prefix_routes() {
        let noncanonical = fixture("roots", Vec::new());
        let mut bytes = noncanonical.head_bytes.clone();
        bytes.push(b'\n');
        noncanonical.backend.insert(
            &noncanonical.head_key,
            scripted_object(bytes, "head-etag-a"),
        );
        let receipt = noncanonical.receipt("roots").await;
        assert_eq!(
            incomplete(noncanonical.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::CanonicalEncoding
            }
        );

        let z = fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/z", b"z", "z");
        let a = fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let unsorted = fixture("roots", vec![vec![z, a]]);
        let receipt = unsorted.receipt("roots").await;
        assert_eq!(
            incomplete(unsorted.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Page,
                reason: InvalidRemoteCatalogReasonV1::EntryOrder
            }
        );

        let escaped = fixture_entry(RemoteCatalogObjectKindV1::Index, "other/index/a", b"a", "a");
        let cross_prefix = fixture("roots", vec![vec![escaped]]);
        let receipt = cross_prefix.receipt("roots").await;
        assert_eq!(
            incomplete(cross_prefix.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Page,
                reason: InvalidRemoteCatalogReasonV1::EntryRoute
            }
        );
    }

    #[tokio::test]
    async fn catalog_rejects_wrong_context_lineage_page_identity_and_cross_page_duplicates() {
        let wrong_context = fixture("roots", Vec::new());
        let mut head =
            serde_json::from_slice::<RemoteCatalogHeadWireV1>(&wrong_context.head_bytes).unwrap();
        head.context.root_id = "other-root".to_owned();
        head.context.root_identity_fingerprint = RootSpecV1Config {
            version: RootSpecV1Config::VERSION,
            remote_prefix: "roots".to_owned(),
            profile: head.context.profile,
            generation: NonZeroU64::new(head.context.root_generation).unwrap(),
        }
        .identity_fingerprint(&head.context.root_id);
        wrong_context.backend.insert(
            wrong_context.head_key.clone(),
            scripted_object(serde_json::to_vec(&head).unwrap(), "head-etag-a"),
        );
        let receipt = wrong_context.receipt("roots").await;
        assert_eq!(
            incomplete(wrong_context.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::Context
            }
        );

        let wrong_plan = fixture("roots", Vec::new());
        let mut head =
            serde_json::from_slice::<RemoteCatalogHeadWireV1>(&wrong_plan.head_bytes).unwrap();
        head.context.plan_contract_fingerprint = format!("b3v1:{}", "0".repeat(64));
        wrong_plan.backend.insert(
            wrong_plan.head_key.clone(),
            scripted_object(serde_json::to_vec(&head).unwrap(), "head-etag-a"),
        );
        let receipt = wrong_plan.receipt("roots").await;
        assert_eq!(
            incomplete(wrong_plan.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::Context
            }
        );

        let wrong_lineage = fixture("roots", Vec::new());
        let mut head =
            serde_json::from_slice::<RemoteCatalogHeadWireV1>(&wrong_lineage.head_bytes).unwrap();
        head.catalog_sequence = 2;
        wrong_lineage.backend.insert(
            wrong_lineage.head_key.clone(),
            scripted_object(serde_json::to_vec(&head).unwrap(), "head-etag-a"),
        );
        let receipt = wrong_lineage.receipt("roots").await;
        assert_eq!(
            incomplete(wrong_lineage.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::Lineage
            }
        );

        let zero_nonce = fixture("roots", Vec::new());
        let mut head =
            serde_json::from_slice::<RemoteCatalogHeadWireV1>(&zero_nonce.head_bytes).unwrap();
        head.publication_nonce = "00".repeat(32);
        zero_nonce.backend.insert(
            zero_nonce.head_key.clone(),
            scripted_object(serde_json::to_vec(&head).unwrap(), "head-etag-a"),
        );
        let receipt = zero_nonce.receipt("roots").await;
        assert_eq!(
            incomplete(zero_nonce.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::Lineage
            }
        );

        let entry = fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let wrong_page_address = fixture("roots", vec![vec![entry.clone()]]);
        let mut wrong_page_bytes = wrong_page_address
            .backend
            .objects
            .lock()
            .unwrap()
            .get(&wrong_page_address.page_keys[0])
            .unwrap()
            .bytes
            .clone();
        wrong_page_bytes.push(b'\n');
        wrong_page_address.backend.insert(
            wrong_page_address.page_keys[0].clone(),
            scripted_object(wrong_page_bytes, "page-etag-0"),
        );
        let receipt = wrong_page_address.receipt("roots").await;
        assert_eq!(
            incomplete(wrong_page_address.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Page,
                reason: InvalidRemoteCatalogReasonV1::ObjectAddress
            }
        );

        let duplicate_across_pages = fixture("roots", vec![vec![entry.clone()], vec![entry]]);
        let receipt = duplicate_across_pages.receipt("roots").await;
        assert_eq!(
            incomplete(duplicate_across_pages.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Page,
                reason: InvalidRemoteCatalogReasonV1::EntryOrder
            }
        );
    }

    #[tokio::test]
    async fn catalog_rejects_binding_and_total_mismatches() {
        let entry = fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let page_binding = fixture("roots", vec![vec![entry.clone()]]);
        let page_bytes = page_binding
            .backend
            .objects
            .lock()
            .unwrap()
            .get(&page_binding.page_keys[0])
            .unwrap()
            .bytes
            .clone();
        page_binding.backend.insert(
            page_binding.page_keys[0].clone(),
            scripted_object(page_bytes, "different-page-etag"),
        );
        let receipt = page_binding.receipt("roots").await;
        assert_eq!(
            incomplete(page_binding.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Page,
                reason: InvalidRemoteCatalogReasonV1::ObjectBinding
            }
        );

        let mut totals = fixture("roots", vec![vec![entry]]);
        let mut root =
            serde_json::from_slice::<RemoteCatalogRootWireV1>(&totals.root_bytes).unwrap();
        root.entry_count = RegisteredRootPlanContractV1::strict_v1()
            .remote_contract()
            .max_catalog_entries();
        totals.rewrite_root(&root);
        let receipt = totals.receipt("roots").await;
        assert_eq!(
            incomplete(totals.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Root,
                reason: InvalidRemoteCatalogReasonV1::Totals
            }
        );
    }

    #[tokio::test]
    async fn catalog_size_limits_and_semantics_receipts_fail_closed() {
        let exact_head = fixture("roots", Vec::new());
        exact_head.backend.insert(
            exact_head.head_key.clone(),
            scripted_object(
                vec![
                    b'x';
                    usize::try_from(
                        RegisteredRootPlanContractV1::strict_v1()
                            .remote_contract()
                            .max_catalog_head_object_bytes()
                    )
                    .unwrap()
                ],
                "head-etag-a",
            ),
        );
        let receipt = exact_head.receipt("roots").await;
        assert_eq!(
            incomplete(exact_head.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::Invalid {
                kind: RemoteCatalogClosureObjectKindV1::Head,
                reason: InvalidRemoteCatalogReasonV1::CanonicalEncoding
            }
        );

        let head = fixture("roots", Vec::new());
        head.backend.insert(
            head.head_key.clone(),
            scripted_object(
                vec![
                    b'x';
                    usize::try_from(
                        RegisteredRootPlanContractV1::strict_v1()
                            .remote_contract()
                            .max_catalog_head_object_bytes()
                            + 1
                    )
                    .unwrap()
                ],
                "head-etag-a",
            ),
        );
        let receipt = head.receipt("roots").await;
        assert_eq!(
            incomplete(head.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::HeadBytes)
        );

        let root = fixture("roots", Vec::new());
        root.backend.insert(
            root.root_key.clone(),
            scripted_object(
                vec![
                    b'x';
                    usize::try_from(
                        RegisteredRootPlanContractV1::strict_v1()
                            .remote_contract()
                            .max_catalog_root_object_bytes()
                            + 1
                    )
                    .unwrap()
                ],
                "root-etag",
            ),
        );
        let receipt = root.receipt("roots").await;
        assert_eq!(
            incomplete(root.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::RootBytes)
        );

        let page_entry =
            fixture_entry(RemoteCatalogObjectKindV1::Index, "roots/index/a", b"a", "a");
        let page = fixture("roots", vec![vec![page_entry.clone()]]);
        page.backend.insert(
            page.page_keys[0].clone(),
            scripted_object(
                vec![
                    b'x';
                    usize::try_from(
                        RegisteredRootPlanContractV1::strict_v1()
                            .remote_contract()
                            .max_catalog_page_object_bytes()
                            + 1
                    )
                    .unwrap()
                ],
                "page-etag-0",
            ),
        );
        let receipt = page.receipt("roots").await;
        assert_eq!(
            incomplete(page.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::PageBytes)
        );

        let mut entries_per_page = fixture("roots", vec![vec![page_entry]]);
        let mut root =
            serde_json::from_slice::<RemoteCatalogRootWireV1>(&entries_per_page.root_bytes)
                .unwrap();
        root.pages[0].entry_count = RegisteredRootPlanContractV1::strict_v1()
            .remote_contract()
            .max_catalog_entries_per_page()
            + 1;
        entries_per_page.rewrite_root(&root);
        let receipt = entries_per_page.receipt("roots").await;
        assert_eq!(
            incomplete(entries_per_page.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::ResourceLimit(RemoteCatalogResourceV1::EntriesPerPage)
        );

        let exact_page_entries = (0..RegisteredRootPlanContractV1::strict_v1()
            .remote_contract()
            .max_catalog_entries_per_page())
            .map(|ordinal| {
                fixture_entry(
                    RemoteCatalogObjectKindV1::Index,
                    &format!("roots/index/{ordinal:04}"),
                    b"a",
                    &format!("entry-etag-{ordinal}"),
                )
            })
            .collect::<Vec<_>>();
        let exact_entries_per_page = fixture("roots", vec![exact_page_entries]);
        let receipt = exact_entries_per_page.receipt("roots").await;
        assert!(matches!(
            exact_entries_per_page.read(&receipt).await.unwrap(),
            StrictRemoteCatalogClosureReadV1::Verified(_)
        ));

        let wrong_scope = fixture("roots", Vec::new());
        let receipt = wrong_scope.receipt("other").await;
        assert_eq!(
            incomplete(wrong_scope.read(&receipt).await.unwrap()),
            StrictRemoteCatalogIncompleteV1::StorageSemanticsUnverified
        );
        assert!(wrong_scope.backend.stats.lock().unwrap().is_empty());
        assert!(wrong_scope.backend.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn catalog_resource_accounting_accepts_exact_limits_and_rejects_next_value() {
        let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let entry = fixture_entry(
            RemoteCatalogObjectKindV1::Index,
            "roots/index/a",
            b"a",
            "etag",
        );

        let mut entries = RemoteCatalogBudgetV1 {
            entries: remote.max_catalog_entries() - 1,
            ..Default::default()
        };
        entries.observe_entry(&entry).unwrap();
        assert_eq!(entries.entries, remote.max_catalog_entries());
        assert_eq!(
            entries.observe_entry(&entry),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::Entries
            ))
        );

        let key_bytes = u64::try_from(entry.object_key.len()).unwrap();
        let mut keys = RemoteCatalogBudgetV1 {
            entry_key_bytes: remote.max_catalog_entry_key_bytes() - key_bytes,
            ..Default::default()
        };
        keys.observe_entry(&entry).unwrap();
        assert_eq!(keys.entry_key_bytes, remote.max_catalog_entry_key_bytes());
        assert_eq!(
            keys.observe_entry(&entry),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::EntryKeyBytes
            ))
        );

        let mut pages = RemoteCatalogBudgetV1 {
            pages: remote.max_catalog_pages() - 1,
            ..Default::default()
        };
        pages.observe_page(1, &etag_binding("page")).unwrap();
        assert_eq!(pages.pages, remote.max_catalog_pages());
        assert_eq!(
            pages.observe_page(1, &etag_binding("page")),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::Pages
            ))
        );

        let mut closure = RemoteCatalogBudgetV1 {
            closure_bytes: remote.max_catalog_closure_object_bytes() - 1,
            ..Default::default()
        };
        closure.observe_page(1, &etag_binding("page")).unwrap();
        assert_eq!(
            closure.closure_bytes,
            remote.max_catalog_closure_object_bytes()
        );
        assert_eq!(
            closure.observe_page(1, &etag_binding("page")),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::ClosureBytes
            ))
        );

        let binding = etag_binding("etag");
        let binding_bytes = binding_wire_bytes_v1(&binding).unwrap();
        let mut bindings = RemoteCatalogBudgetV1 {
            binding_bytes: remote.max_catalog_binding_bytes() - binding_bytes,
            ..Default::default()
        };
        bindings.observe_binding(&binding).unwrap();
        assert_eq!(bindings.binding_bytes, remote.max_catalog_binding_bytes());
        assert_eq!(
            bindings.observe_binding(&binding),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::BindingBytes
            ))
        );

        let mut root_bytes = RemoteCatalogBudgetV1::default();
        root_bytes
            .observe_root(
                remote.max_catalog_root_object_bytes(),
                &etag_binding("root"),
            )
            .unwrap();
        assert_eq!(
            RemoteCatalogBudgetV1::default().observe_root(
                remote.max_catalog_root_object_bytes() + 1,
                &etag_binding("root")
            ),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::RootBytes
            ))
        );

        let mut page_bytes = RemoteCatalogBudgetV1::default();
        page_bytes
            .observe_page(
                remote.max_catalog_page_object_bytes(),
                &etag_binding("page"),
            )
            .unwrap();
        assert_eq!(
            RemoteCatalogBudgetV1::default().observe_page(
                remote.max_catalog_page_object_bytes() + 1,
                &etag_binding("page")
            ),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::PageBytes
            ))
        );

        let mut index_entries = RemoteCatalogBudgetV1 {
            index_entries: remote.max_index_observations_per_pass() - 1,
            ..Default::default()
        };
        index_entries.observe_entry(&entry).unwrap();
        assert_eq!(
            index_entries.observe_entry(&entry),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::IndexEntries
            ))
        );

        let reservation = fixture_entry(
            RemoteCatalogObjectKindV1::Reservation,
            &format!("roots/.tcfs-namespace/v1/{}", "b".repeat(64)),
            b"reservation",
            "reservation",
        );
        let mut reservation_entries = RemoteCatalogBudgetV1 {
            reservation_entries: remote.max_reservation_observations_per_pass() - 1,
            ..Default::default()
        };
        reservation_entries.observe_entry(&reservation).unwrap();
        assert_eq!(
            reservation_entries.observe_entry(&reservation),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::ReservationEntries
            ))
        );

        let manifest = fixture_entry(
            RemoteCatalogObjectKindV1::Manifest,
            &format!("roots/manifests/{}", "c".repeat(64)),
            b"manifest",
            "manifest",
        );
        let mut manifest_entries = RemoteCatalogBudgetV1 {
            manifest_entries: remote.max_index_observations_per_pass() - 1,
            ..Default::default()
        };
        manifest_entries.observe_entry(&manifest).unwrap();
        assert_eq!(
            manifest_entries.observe_entry(&manifest),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::ManifestEntries
            ))
        );

        let mut overflow = u64::MAX;
        assert_eq!(
            RemoteCatalogBudgetV1::checked_add(
                &mut overflow,
                1,
                u64::MAX,
                RemoteCatalogResourceV1::ClosureBytes
            ),
            Err(StrictRemoteCatalogIncompleteV1::ResourceLimit(
                RemoteCatalogResourceV1::ClosureBytes
            ))
        );
    }
}
