//! Registered-root catalog publication contract.
//!
//! A final `HEAD` compare-and-swap is not a writer fence. Two writers can
//! mutate different namespace objects from the same predecessor and only one
//! can win the final CAS, leaving the winning catalog incomplete. The protocol
//! modeled here therefore makes the mutable catalog `HEAD` visibly
//! `publishing` *before* the first index, reservation, or manifest mutation.
//! Readers reject that state instead of falling back to the predecessor.
//!
//! This checkpoint deliberately stops before production authority. Bootstrap
//! completeness, exact-current high-water, and the all-writer credential epoch
//! are opaque external receipts with no production constructors. The module
//! can validate and compose their exact bindings, but cannot mint them, write a
//! live `HEAD`, produce a plan digest, or authorize an action.

use std::num::NonZeroU64;

use opendal::Operator;
use serde::{Deserialize, Serialize};
use tcfs_core::config::{
    RegisteredRootPlanContractFingerprintV1, RootProfileSettingsFingerprintV1, RootProfileV1,
};

use super::{
    lower_hex, validate_catalog_context_v1, InvalidRemoteCatalogReasonV1,
    RemoteCatalogContextWireV1, RemoteCatalogObjectBindingWireV1,
    SemanticallyBoundRemoteCatalogCorpusV1, CATALOG_SCHEMA_VERSION_V1,
};
use crate::registered_reconcile::{
    validate_registered_remote_storage_key_bounds_v1, RegisteredRootRemoteObjectBindingV1,
};
use crate::registered_source_composition::ValidatedSelectedRegisteredRootRemoteContextV1;
use tcfs_storage::ConditionalWriteSemanticsReceipt;

const ARCHIVED_HEAD_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-archived-head-object.b3v1";
const MUTATION_JOURNAL_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-mutation-journal-object.b3v1";
const ARCHIVED_HEAD_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/archived-heads";
const MUTATION_JOURNAL_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/mutation-journals";

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogAuthorityContextV1 {
    remote_prefix: String,
    root_id: String,
    root_identity_fingerprint: String,
    root_generation: NonZeroU64,
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
}

impl CatalogAuthorityContextV1 {
    fn from_corpus(corpus: &SemanticallyBoundRemoteCatalogCorpusV1) -> Self {
        Self {
            remote_prefix: corpus.remote_prefix().to_owned(),
            root_id: corpus.root_id().to_owned(),
            root_identity_fingerprint: corpus.root_identity_fingerprint().to_owned(),
            root_generation: corpus.root_generation(),
            profile: corpus.profile(),
            profile_settings_fingerprint: corpus.profile_settings_fingerprint(),
            plan_contract_fingerprint: corpus.plan_contract_fingerprint(),
        }
    }

    fn to_wire(&self) -> RemoteCatalogContextWireV1 {
        RemoteCatalogContextWireV1 {
            root_id: self.root_id.clone(),
            root_identity_fingerprint: self.root_identity_fingerprint.clone(),
            root_generation: self.root_generation.get(),
            profile: self.profile,
            profile_settings_fingerprint: self.profile_settings_fingerprint.to_string(),
            plan_contract_fingerprint: self.plan_contract_fingerprint.to_string(),
        }
    }
}

/// Exact current `HEAD` selected by one semantically verified catalog corpus.
///
/// This is predecessor evidence only. It says nothing about bootstrap,
/// currentness beyond this observation, or whether every writer participates.
pub(crate) struct ObservedPublishedCatalogHeadV1 {
    context: CatalogAuthorityContextV1,
    sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: Option<[u8; 32]>,
    head_revision: [u8; 32],
    committed_head_bytes: Vec<u8>,
    current_head_etag: String,
}

impl std::fmt::Debug for ObservedPublishedCatalogHeadV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservedPublishedCatalogHeadV1")
            .field("remote_prefix", &self.context.remote_prefix)
            .field("root_id", &self.context.root_id)
            .field("root_generation", &self.context.root_generation)
            .field("profile", &self.context.profile)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogPublicationContractErrorV1 {
    CurrentHeadWithoutEtag,
    StorageSemanticsUnverified,
    SequenceOverflow,
    SequenceMismatch,
    ParentRevisionMismatch,
    ZeroPublicationNonce,
    ReusedPublicationNonce,
    ContextMismatch,
    InvalidBootstrapReceipt,
    BootstrapMismatch,
    HighWaterMismatch,
    WriterFenceMismatch,
    StorageAuthorityMismatch,
    ControlAuthorityMismatch,
    PredecessorArchiveMismatch,
    MutationJournalMismatch,
}

/// Extract the exact mutable-HEAD ETag needed by a future pre-mutation CAS.
fn mutable_head_etag_v1(binding: &RegisteredRootRemoteObjectBindingV1) -> Option<String> {
    match binding {
        RegisteredRootRemoteObjectBindingV1::Etag { etag }
        | RegisteredRootRemoteObjectBindingV1::Version {
            etag: Some(etag), ..
        } if !etag.is_empty() => Some(etag.clone()),
        RegisteredRootRemoteObjectBindingV1::Etag { .. }
        | RegisteredRootRemoteObjectBindingV1::Version { .. } => None,
    }
}

/// Retain the exact mutable-HEAD ETag needed by a future pre-mutation CAS.
pub(crate) fn observe_published_catalog_head_v1(
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
) -> Result<ObservedPublishedCatalogHeadV1, CatalogPublicationContractErrorV1> {
    let current_head_etag = mutable_head_etag_v1(corpus.closure.head_object.binding())
        .ok_or(CatalogPublicationContractErrorV1::CurrentHeadWithoutEtag)?;
    Ok(ObservedPublishedCatalogHeadV1 {
        context: CatalogAuthorityContextV1::from_corpus(corpus),
        sequence: corpus.closure.catalog_sequence,
        publication_nonce: corpus.closure.publication_nonce,
        parent_head_revision: corpus.closure.parent_head_revision,
        head_revision: corpus.closure.head_revision,
        committed_head_bytes: corpus.closure.head_raw_bytes.clone(),
        current_head_etag,
    })
}

/// Opaque binding between an external storage-authority identity and the exact
/// live accessor/prefix conditional-semantics receipt used for this attempt.
///
/// A production constructor must authenticate the endpoint/bucket authority;
/// deriving this fingerprint from the same replayable catalog prefix is not
/// sufficient. No such constructor exists in this checkpoint.
pub(crate) struct TrustedCatalogStorageAuthorityV1<'a> {
    operator: &'a Operator,
    conditional_write_receipt: &'a ConditionalWriteSemanticsReceipt,
    authority_fingerprint: [u8; 32],
}

/// External attestation that sequence one was built from complete truth while
/// every legacy/out-of-tree writer was quiesced under its bootstrap credential
/// epoch. Later publications may use a rotated current epoch.
///
/// There is intentionally no production constructor in this checkpoint.
pub(crate) struct TrustedCatalogBootstrapReceiptV1 {
    context: CatalogAuthorityContextV1,
    bootstrap_head_revision: [u8; 32],
    bootstrap_publication_nonce: [u8; 32],
    complete_corpus_attestation: [u8; 32],
    bootstrap_writer_epoch: [u8; 32],
    storage_authority_fingerprint: [u8; 32],
    control_authority_fingerprint: [u8; 32],
}

/// Exclusive external exact-current guard anchored to one trusted bootstrap.
///
/// A lower bound is insufficient: without immutable historical HEAD objects,
/// this source-only client cannot prove an unseen multi-hop lineage. The
/// receipt must therefore match the exact observed sequence, revision, and
/// nonce. Same-prefix remote state cannot mint this receipt because it could
/// be replayed together with `HEAD`. A future production constructor must
/// acquire the named external authority revision exclusively, keep the lease
/// live through publication, and advance that revision monotonically before
/// release; retaining this non-cloneable value alone is not a production
/// liveness proof.
pub(crate) struct TrustedCatalogHighWaterGuardV1 {
    context: CatalogAuthorityContextV1,
    bootstrap_head_revision: [u8; 32],
    bootstrap_publication_nonce: [u8; 32],
    complete_corpus_attestation: [u8; 32],
    current_writer_epoch: [u8; 32],
    current_sequence: NonZeroU64,
    current_head_revision: [u8; 32],
    current_publication_nonce: [u8; 32],
    storage_authority_fingerprint: [u8; 32],
    authority_revision: [u8; 32],
    lease_nonce: [u8; 32],
    control_authority_fingerprint: [u8; 32],
}

/// External proof that every credential and code path able to mutate the
/// catalog corpus is fenced by the publishing-HEAD protocol.
///
/// This is broader than an in-process mutex or storage CAS receipt: old
/// binaries and direct object-store credentials are part of the epoch. A
/// future production constructor must keep this lease live through the visible
/// HEAD CAS, every namespace mutation, committed-HEAD finalization, and
/// high-water advancement.
pub(crate) struct AllNamespaceWritersFencedLeaseV1 {
    context: CatalogAuthorityContextV1,
    bootstrap_head_revision: [u8; 32],
    current_writer_epoch: [u8; 32],
    authority_revision: [u8; 32],
    lease_nonce: [u8; 32],
    storage_authority_fingerprint: [u8; 32],
    control_authority_fingerprint: [u8; 32],
}

/// Exact receipt match for one observed predecessor. This still is not remote
/// completeness authority and has no planner/action conversion.
pub(crate) struct MatchedCatalogPublicationPrerequisitesV1<'a> {
    storage_authority: TrustedCatalogStorageAuthorityV1<'a>,
    high_water_guard: TrustedCatalogHighWaterGuardV1,
    writer_fence_lease: AllNamespaceWritersFencedLeaseV1,
    context: CatalogAuthorityContextV1,
    sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    head_revision: [u8; 32],
    committed_head_bytes: Vec<u8>,
    current_head_etag: String,
    bootstrap_head_revision: [u8; 32],
}

impl std::fmt::Debug for MatchedCatalogPublicationPrerequisitesV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MatchedCatalogPublicationPrerequisitesV1")
            .field("remote_prefix", &self.context.remote_prefix)
            .field("root_id", &self.context.root_id)
            .field("root_generation", &self.context.root_generation)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

pub(crate) fn match_catalog_publication_prerequisites_v1<'a>(
    storage_authority: TrustedCatalogStorageAuthorityV1<'a>,
    observed: &ObservedPublishedCatalogHeadV1,
    bootstrap: &TrustedCatalogBootstrapReceiptV1,
    high_water: TrustedCatalogHighWaterGuardV1,
    writers: AllNamespaceWritersFencedLeaseV1,
) -> Result<MatchedCatalogPublicationPrerequisitesV1<'a>, CatalogPublicationContractErrorV1> {
    if !storage_authority
        .conditional_write_receipt
        .authorizes(storage_authority.operator, &observed.context.remote_prefix)
        .unwrap_or(false)
    {
        return Err(CatalogPublicationContractErrorV1::StorageSemanticsUnverified);
    }
    let bootstrap_fields_nonzero = bootstrap.bootstrap_head_revision != [0; 32]
        && bootstrap.bootstrap_publication_nonce != [0; 32]
        && bootstrap.complete_corpus_attestation != [0; 32]
        && bootstrap.bootstrap_writer_epoch != [0; 32]
        && bootstrap.storage_authority_fingerprint != [0; 32]
        && bootstrap.control_authority_fingerprint != [0; 32]
        && storage_authority.authority_fingerprint != [0; 32];
    if !bootstrap_fields_nonzero {
        return Err(CatalogPublicationContractErrorV1::InvalidBootstrapReceipt);
    }
    if bootstrap.context != observed.context {
        return Err(CatalogPublicationContractErrorV1::BootstrapMismatch);
    }
    if bootstrap.storage_authority_fingerprint != storage_authority.authority_fingerprint
        || high_water.storage_authority_fingerprint != storage_authority.authority_fingerprint
        || writers.storage_authority_fingerprint != storage_authority.authority_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::StorageAuthorityMismatch);
    }
    if high_water.control_authority_fingerprint != bootstrap.control_authority_fingerprint
        || writers.control_authority_fingerprint != bootstrap.control_authority_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::ControlAuthorityMismatch);
    }
    if high_water.context != observed.context
        || high_water.bootstrap_head_revision != bootstrap.bootstrap_head_revision
        || high_water.bootstrap_publication_nonce != bootstrap.bootstrap_publication_nonce
        || high_water.complete_corpus_attestation != bootstrap.complete_corpus_attestation
        || high_water.current_writer_epoch == [0; 32]
        || high_water.current_sequence != observed.sequence
        || high_water.current_head_revision != observed.head_revision
        || high_water.current_publication_nonce != observed.publication_nonce
        || high_water.authority_revision == [0; 32]
        || high_water.lease_nonce == [0; 32]
    {
        return Err(CatalogPublicationContractErrorV1::HighWaterMismatch);
    }
    if writers.context != observed.context
        || writers.bootstrap_head_revision != bootstrap.bootstrap_head_revision
        || writers.current_writer_epoch != high_water.current_writer_epoch
        || writers.authority_revision == [0; 32]
        || writers.lease_nonce == [0; 32]
    {
        return Err(CatalogPublicationContractErrorV1::WriterFenceMismatch);
    }
    if observed.sequence.get() == 1
        && (bootstrap.bootstrap_head_revision != observed.head_revision
            || bootstrap.bootstrap_publication_nonce != observed.publication_nonce)
    {
        return Err(CatalogPublicationContractErrorV1::BootstrapMismatch);
    }
    Ok(MatchedCatalogPublicationPrerequisitesV1 {
        storage_authority,
        high_water_guard: high_water,
        writer_fence_lease: writers,
        context: observed.context.clone(),
        sequence: observed.sequence,
        publication_nonce: observed.publication_nonce,
        head_revision: observed.head_revision,
        committed_head_bytes: observed.committed_head_bytes.clone(),
        current_head_etag: observed.current_head_etag.clone(),
        bootstrap_head_revision: bootstrap.bootstrap_head_revision,
    })
}

/// Exact immutable copy of the complete committed predecessor HEAD.
///
/// The object must be installed absent-only and byte-verified before the
/// mutable HEAD CAS. `object_id` is the domain-separated digest of the exact
/// committed HEAD bytes, so its fixed reference lets recovery find and verify
/// the previous catalog root at a canonical key without LIST. The binding is
/// tied to the exact storage authority used for HEAD. No production constructor
/// exists until the immutable publication primitive is implemented.
pub(crate) struct BoundArchivedCatalogHeadV1 {
    context: CatalogAuthorityContextV1,
    storage_authority_fingerprint: [u8; 32],
    predecessor_head_revision: [u8; 32],
    predecessor_head_bytes_blake3: [u8; 32],
    object_id: [u8; 32],
    raw_bytes_len: NonZeroU64,
    binding: RegisteredRootRemoteObjectBindingV1,
}

/// Exact immutable transaction journal written before the visible fence.
///
/// The journal must enumerate the complete intended index/reservation/manifest
/// mutation set plus each predecessor/new byte identity, so crash recovery can
/// roll forward or remain fenced without reconstructing truth through LIST.
/// `object_id` is the domain-separated digest of the retained canonical bytes.
/// Its binding is tied to the exact storage authority and canonical key used
/// for HEAD. No production constructor or journal-schema validator exists in
/// this checkpoint.
pub(crate) struct BoundCatalogMutationJournalV1 {
    context: CatalogAuthorityContextV1,
    storage_authority_fingerprint: [u8; 32],
    catalog_sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
    object_id: [u8; 32],
    raw_bytes: Vec<u8>,
    raw_bytes_len: NonZeroU64,
    binding: RegisteredRootRemoteObjectBindingV1,
}

fn publication_object_key_v1(
    context: &CatalogAuthorityContextV1,
    suffix: &str,
    object_id: &[u8; 32],
) -> Option<String> {
    let key = format!(
        "{}/{}/{}",
        context.remote_prefix,
        suffix,
        lower_hex(object_id)
    );
    validate_registered_remote_storage_key_bounds_v1(&key, "catalog publication object key")
        .ok()?;
    Some(key)
}

fn archived_head_object_key_v1(archive: &BoundArchivedCatalogHeadV1) -> Option<String> {
    publication_object_key_v1(
        &archive.context,
        ARCHIVED_HEAD_OBJECT_SUFFIX_V1,
        &archive.object_id,
    )
}

fn mutation_journal_object_key_v1(journal: &BoundCatalogMutationJournalV1) -> Option<String> {
    publication_object_key_v1(
        &journal.context,
        MUTATION_JOURNAL_OBJECT_SUFFIX_V1,
        &journal.object_id,
    )
}

fn binding_wire_v1(
    binding: &RegisteredRootRemoteObjectBindingV1,
) -> RemoteCatalogObjectBindingWireV1 {
    match binding {
        RegisteredRootRemoteObjectBindingV1::Version { version, etag } => {
            RemoteCatalogObjectBindingWireV1 {
                version: Some(version.clone()),
                etag: etag.clone(),
            }
        }
        RegisteredRootRemoteObjectBindingV1::Etag { etag } => RemoteCatalogObjectBindingWireV1 {
            version: None,
            etag: Some(etag.clone()),
        },
    }
}

#[derive(Clone)]
struct CatalogSuccessorClaimV1 {
    context: CatalogAuthorityContextV1,
    sequence: u64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
}

/// Structurally valid successor plus the exact predecessor CAS baseline.
///
/// Future publication starts by conditionally replacing the committed HEAD
/// with the canonical publishing wire. This type cannot itself mutate storage.
pub(crate) struct PreparedCatalogPublicationFenceV1<'a> {
    storage_authority: TrustedCatalogStorageAuthorityV1<'a>,
    high_water_guard: TrustedCatalogHighWaterGuardV1,
    writer_fence_lease: AllNamespaceWritersFencedLeaseV1,
    context: CatalogAuthorityContextV1,
    sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
    expected_parent_head_etag: String,
    bootstrap_head_revision: [u8; 32],
    predecessor_archive: BoundArchivedCatalogHeadV1,
    mutation_journal: BoundCatalogMutationJournalV1,
}

impl std::fmt::Debug for PreparedCatalogPublicationFenceV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCatalogPublicationFenceV1")
            .field("remote_prefix", &self.context.remote_prefix)
            .field("root_id", &self.context.root_id)
            .field("root_generation", &self.context.root_generation)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

fn validate_catalog_successor_claim_v1<'a>(
    prerequisites: MatchedCatalogPublicationPrerequisitesV1<'a>,
    claim: CatalogSuccessorClaimV1,
    predecessor_archive: BoundArchivedCatalogHeadV1,
    mutation_journal: BoundCatalogMutationJournalV1,
) -> Result<PreparedCatalogPublicationFenceV1<'a>, CatalogPublicationContractErrorV1> {
    let expected_sequence = prerequisites
        .sequence
        .get()
        .checked_add(1)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    if claim.sequence != expected_sequence {
        return Err(CatalogPublicationContractErrorV1::SequenceMismatch);
    }
    if claim.parent_head_revision != prerequisites.head_revision {
        return Err(CatalogPublicationContractErrorV1::ParentRevisionMismatch);
    }
    if claim.publication_nonce == [0; 32] {
        return Err(CatalogPublicationContractErrorV1::ZeroPublicationNonce);
    }
    if claim.publication_nonce == prerequisites.publication_nonce {
        return Err(CatalogPublicationContractErrorV1::ReusedPublicationNonce);
    }
    if claim.context != prerequisites.context {
        return Err(CatalogPublicationContractErrorV1::ContextMismatch);
    }
    let predecessor_bytes_blake3 = *blake3::hash(&prerequisites.committed_head_bytes).as_bytes();
    let predecessor_object_id = super::domain_object_id_v1(
        ARCHIVED_HEAD_OBJECT_DOMAIN_V1,
        &prerequisites.committed_head_bytes,
    );
    let Ok(predecessor_raw_bytes_len) = u64::try_from(prerequisites.committed_head_bytes.len())
    else {
        return Err(CatalogPublicationContractErrorV1::PredecessorArchiveMismatch);
    };
    if predecessor_archive.context != prerequisites.context
        || predecessor_archive.storage_authority_fingerprint
            != prerequisites.storage_authority.authority_fingerprint
        || predecessor_archive.predecessor_head_revision != prerequisites.head_revision
        || predecessor_archive.predecessor_head_bytes_blake3 != predecessor_bytes_blake3
        || predecessor_archive.object_id != predecessor_object_id
        || predecessor_archive.raw_bytes_len.get() != predecessor_raw_bytes_len
        || archived_head_object_key_v1(&predecessor_archive).is_none()
        || super::validate_binding_wire_v1(&binding_wire_v1(&predecessor_archive.binding)).is_none()
    {
        return Err(CatalogPublicationContractErrorV1::PredecessorArchiveMismatch);
    }
    let journal_object_id = super::domain_object_id_v1(
        MUTATION_JOURNAL_OBJECT_DOMAIN_V1,
        &mutation_journal.raw_bytes,
    );
    let Ok(journal_raw_bytes_len) = u64::try_from(mutation_journal.raw_bytes.len()) else {
        return Err(CatalogPublicationContractErrorV1::MutationJournalMismatch);
    };
    if mutation_journal.context != prerequisites.context
        || mutation_journal.storage_authority_fingerprint
            != prerequisites.storage_authority.authority_fingerprint
        || mutation_journal.catalog_sequence.get() != expected_sequence
        || mutation_journal.publication_nonce != claim.publication_nonce
        || mutation_journal.parent_head_revision != prerequisites.head_revision
        || mutation_journal.object_id != journal_object_id
        || mutation_journal.raw_bytes_len.get() != journal_raw_bytes_len
        || mutation_journal_object_key_v1(&mutation_journal).is_none()
        || super::validate_binding_wire_v1(&binding_wire_v1(&mutation_journal.binding)).is_none()
    {
        return Err(CatalogPublicationContractErrorV1::MutationJournalMismatch);
    }
    Ok(PreparedCatalogPublicationFenceV1 {
        storage_authority: prerequisites.storage_authority,
        high_water_guard: prerequisites.high_water_guard,
        writer_fence_lease: prerequisites.writer_fence_lease,
        context: prerequisites.context,
        sequence: NonZeroU64::new(expected_sequence)
            .expect("checked nonzero successor of a nonzero sequence"),
        publication_nonce: claim.publication_nonce,
        parent_head_revision: claim.parent_head_revision,
        expected_parent_head_etag: prerequisites.current_head_etag,
        bootstrap_head_revision: prerequisites.bootstrap_head_revision,
        predecessor_archive,
        mutation_journal,
    })
}

/// Prepare one exact successor. The future storage CAS is the cross-process
/// winner election; callers must never mutate the namespace before it wins.
pub(crate) fn prepare_catalog_publication_fence_v1<'a>(
    prerequisites: MatchedCatalogPublicationPrerequisitesV1<'a>,
    publication_nonce: [u8; 32],
    predecessor_archive: BoundArchivedCatalogHeadV1,
    mutation_journal: BoundCatalogMutationJournalV1,
) -> Result<PreparedCatalogPublicationFenceV1<'a>, CatalogPublicationContractErrorV1> {
    let sequence = prerequisites
        .sequence
        .get()
        .checked_add(1)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let claim = CatalogSuccessorClaimV1 {
        context: prerequisites.context.clone(),
        sequence,
        publication_nonce,
        parent_head_revision: prerequisites.head_revision,
    };
    validate_catalog_successor_claim_v1(prerequisites, claim, predecessor_archive, mutation_journal)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RemoteCatalogPublishingStateWireV1 {
    Publishing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogPublicationObjectReferenceWireV1 {
    object_id: String,
    raw_bytes_len: u64,
    binding: RemoteCatalogObjectBindingWireV1,
}

/// Visible root-wide fence installed before the first catalog-corpus write.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogPublishingHeadWireV1 {
    version: u32,
    state: RemoteCatalogPublishingStateWireV1,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    parent_head_revision: String,
    bootstrap_head_revision: String,
    storage_authority_fingerprint: String,
    control_authority_fingerprint: String,
    writer_epoch: String,
    high_water_authority_revision: String,
    high_water_lease_nonce: String,
    writer_fence_authority_revision: String,
    writer_fence_lease_nonce: String,
    predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1,
    mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1,
}

fn canonical_publishing_head_bytes_v1(
    successor: &PreparedCatalogPublicationFenceV1<'_>,
) -> Vec<u8> {
    debug_assert!(
        successor
            .storage_authority
            .conditional_write_receipt
            .authorizes(
                successor.storage_authority.operator,
                &successor.context.remote_prefix,
            )
            .unwrap_or(false),
        "prepared successor must retain its exact accessor/prefix receipt"
    );
    serde_json::to_vec(&RemoteCatalogPublishingHeadWireV1 {
        version: CATALOG_SCHEMA_VERSION_V1,
        state: RemoteCatalogPublishingStateWireV1::Publishing,
        context: successor.context.to_wire(),
        catalog_sequence: successor.sequence.get(),
        publication_nonce: lower_hex(&successor.publication_nonce),
        parent_head_revision: lower_hex(&successor.parent_head_revision),
        bootstrap_head_revision: lower_hex(&successor.bootstrap_head_revision),
        storage_authority_fingerprint: lower_hex(
            &successor.storage_authority.authority_fingerprint,
        ),
        control_authority_fingerprint: lower_hex(
            &successor.writer_fence_lease.control_authority_fingerprint,
        ),
        writer_epoch: lower_hex(&successor.writer_fence_lease.current_writer_epoch),
        high_water_authority_revision: lower_hex(&successor.high_water_guard.authority_revision),
        high_water_lease_nonce: lower_hex(&successor.high_water_guard.lease_nonce),
        writer_fence_authority_revision: lower_hex(
            &successor.writer_fence_lease.authority_revision,
        ),
        writer_fence_lease_nonce: lower_hex(&successor.writer_fence_lease.lease_nonce),
        predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1 {
            object_id: lower_hex(&successor.predecessor_archive.object_id),
            raw_bytes_len: successor.predecessor_archive.raw_bytes_len.get(),
            binding: binding_wire_v1(&successor.predecessor_archive.binding),
        },
        mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1 {
            object_id: lower_hex(&successor.mutation_journal.object_id),
            raw_bytes_len: successor.mutation_journal.raw_bytes_len.get(),
            binding: binding_wire_v1(&successor.mutation_journal.binding),
        },
    })
    .expect("catalog publishing wire is infallibly serializable")
}

pub(super) fn classify_publishing_head_v1(
    raw_bytes: &[u8],
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
) -> Option<Result<(), InvalidRemoteCatalogReasonV1>> {
    let Ok(wire) = serde_json::from_slice::<RemoteCatalogPublishingHeadWireV1>(raw_bytes) else {
        return None;
    };
    if serde_json::to_vec(&wire).ok().as_deref() != Some(raw_bytes) {
        return Some(Err(InvalidRemoteCatalogReasonV1::CanonicalEncoding));
    }
    if wire.version != CATALOG_SCHEMA_VERSION_V1 {
        return Some(Err(InvalidRemoteCatalogReasonV1::CanonicalEncoding));
    }
    if validate_catalog_context_v1(&wire.context, selected).is_none() {
        return Some(Err(InvalidRemoteCatalogReasonV1::Context));
    }
    let valid_nonzero_hex =
        |value: &str| super::parse_lower_hex_32(value).is_some_and(|bytes| bytes != [0; 32]);
    let valid_lineage = wire.catalog_sequence >= 2
        && valid_nonzero_hex(&wire.publication_nonce)
        && valid_nonzero_hex(&wire.parent_head_revision)
        && valid_nonzero_hex(&wire.bootstrap_head_revision)
        && valid_nonzero_hex(&wire.storage_authority_fingerprint)
        && valid_nonzero_hex(&wire.control_authority_fingerprint)
        && valid_nonzero_hex(&wire.writer_epoch)
        && valid_nonzero_hex(&wire.high_water_authority_revision)
        && valid_nonzero_hex(&wire.high_water_lease_nonce)
        && valid_nonzero_hex(&wire.writer_fence_authority_revision)
        && valid_nonzero_hex(&wire.writer_fence_lease_nonce)
        && valid_nonzero_hex(&wire.predecessor_head_archive.object_id)
        && wire.predecessor_head_archive.raw_bytes_len > 0
        && super::validate_binding_wire_v1(&wire.predecessor_head_archive.binding).is_some()
        && valid_nonzero_hex(&wire.mutation_journal.object_id)
        && wire.mutation_journal.raw_bytes_len > 0
        && super::validate_binding_wire_v1(&wire.mutation_journal.binding).is_some();
    Some(if valid_lineage {
        Ok(())
    } else {
        Err(InvalidRemoteCatalogReasonV1::Lineage)
    })
}

#[cfg(test)]
fn is_canonical_publishing_head_v1(
    raw_bytes: &[u8],
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
) -> bool {
    classify_publishing_head_v1(raw_bytes, selected) == Some(Ok(()))
}

#[cfg(test)]
pub(super) fn canonical_publishing_head_bytes_for_test_v1(
    context: RemoteCatalogContextWireV1,
) -> Vec<u8> {
    serde_json::to_vec(&RemoteCatalogPublishingHeadWireV1 {
        version: CATALOG_SCHEMA_VERSION_V1,
        state: RemoteCatalogPublishingStateWireV1::Publishing,
        context,
        catalog_sequence: 2,
        publication_nonce: "44".repeat(32),
        parent_head_revision: "55".repeat(32),
        bootstrap_head_revision: "66".repeat(32),
        storage_authority_fingerprint: "99".repeat(32),
        control_authority_fingerprint: "9a".repeat(32),
        writer_epoch: "77".repeat(32),
        high_water_authority_revision: "ab".repeat(32),
        high_water_lease_nonce: "aa".repeat(32),
        writer_fence_authority_revision: "89".repeat(32),
        writer_fence_lease_nonce: "88".repeat(32),
        predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1 {
            object_id: "bb".repeat(32),
            raw_bytes_len: 1,
            binding: RemoteCatalogObjectBindingWireV1 {
                version: None,
                etag: Some("archive-etag".to_owned()),
            },
        },
        mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1 {
            object_id: "cc".repeat(32),
            raw_bytes_len: 1,
            binding: RemoteCatalogObjectBindingWireV1 {
                version: None,
                etag: Some("journal-etag".to_owned()),
            },
        },
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registered_remote_catalog::tests::{
        semantic_remote_catalog_fixture_for_test_v1, SemanticRemoteCatalogFixtureRowV1,
        SemanticRemoteCatalogFixtureV1,
    };
    use crate::registered_remote_catalog::{
        read_semantically_bound_remote_catalog_corpus_v1,
        StrictSemanticallyBoundRemoteCatalogReadV1,
    };
    use crate::registered_source_composition::validated_selected_registered_root_remote_context_for_test_v1;
    use tcfs_core::config::RootSpecV1Config;

    fn test_spec() -> RootSpecV1Config {
        RootSpecV1Config {
            version: RootSpecV1Config::VERSION,
            remote_prefix: "roots".to_owned(),
            profile: RootProfileV1::AgentStaticV1,
            generation: NonZeroU64::new(1).unwrap(),
        }
    }

    fn test_selected() -> ValidatedSelectedRegisteredRootRemoteContextV1 {
        validated_selected_registered_root_remote_context_for_test_v1("fixture-root", &test_spec())
            .unwrap()
    }

    static_assertions::assert_not_impl_any!(
        ObservedPublishedCatalogHeadV1: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogStorageAuthorityV1<'static>: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogBootstrapReceiptV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogHighWaterGuardV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        AllNamespaceWritersFencedLeaseV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        BoundArchivedCatalogHeadV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        BoundCatalogMutationJournalV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        MatchedCatalogPublicationPrerequisitesV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        PreparedCatalogPublicationFenceV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );

    async fn observed_head() -> (
        SemanticRemoteCatalogFixtureV1,
        ObservedPublishedCatalogHeadV1,
    ) {
        let spec = test_spec();
        let selected = test_selected();
        let fixture = semantic_remote_catalog_fixture_for_test_v1(
            "fixture-root",
            &spec,
            &[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )],
        )
        .await;
        let corpus = match read_semantically_bound_remote_catalog_corpus_v1(
            fixture.operator(),
            &selected,
            fixture.receipt(),
        )
        .await
        .unwrap()
        {
            StrictSemanticallyBoundRemoteCatalogReadV1::Verified(corpus) => corpus,
            StrictSemanticallyBoundRemoteCatalogReadV1::Incomplete(incomplete) => {
                panic!("expected semantic catalog fixture, got {incomplete:?}")
            }
        };
        let observed = observe_published_catalog_head_v1(&corpus).unwrap();
        (fixture, observed)
    }

    fn trusted_receipts(
        observed: &ObservedPublishedCatalogHeadV1,
    ) -> (
        TrustedCatalogBootstrapReceiptV1,
        TrustedCatalogHighWaterGuardV1,
        AllNamespaceWritersFencedLeaseV1,
    ) {
        let bootstrap_head_revision = observed.head_revision;
        let bootstrap_writer_epoch = [0x21; 32];
        let current_writer_epoch = [0x22; 32];
        let storage_authority_fingerprint = [0x99; 32];
        let control_authority_fingerprint = [0x9a; 32];
        (
            TrustedCatalogBootstrapReceiptV1 {
                context: observed.context.clone(),
                bootstrap_head_revision,
                bootstrap_publication_nonce: observed.publication_nonce,
                complete_corpus_attestation: [0x33; 32],
                bootstrap_writer_epoch,
                storage_authority_fingerprint,
                control_authority_fingerprint,
            },
            TrustedCatalogHighWaterGuardV1 {
                context: observed.context.clone(),
                bootstrap_head_revision,
                bootstrap_publication_nonce: observed.publication_nonce,
                complete_corpus_attestation: [0x33; 32],
                current_writer_epoch,
                current_sequence: observed.sequence,
                current_head_revision: observed.head_revision,
                current_publication_nonce: observed.publication_nonce,
                storage_authority_fingerprint,
                authority_revision: [0xab; 32],
                lease_nonce: [0xaa; 32],
                control_authority_fingerprint,
            },
            AllNamespaceWritersFencedLeaseV1 {
                context: observed.context.clone(),
                bootstrap_head_revision,
                current_writer_epoch,
                authority_revision: [0x89; 32],
                lease_nonce: [0x88; 32],
                storage_authority_fingerprint,
                control_authority_fingerprint,
            },
        )
    }

    fn trusted_storage(
        fixture: &SemanticRemoteCatalogFixtureV1,
    ) -> TrustedCatalogStorageAuthorityV1<'_> {
        TrustedCatalogStorageAuthorityV1 {
            operator: fixture.operator(),
            conditional_write_receipt: fixture.receipt(),
            authority_fingerprint: [0x99; 32],
        }
    }

    fn bound_publication_objects(
        observed: &ObservedPublishedCatalogHeadV1,
        publication_nonce: [u8; 32],
    ) -> (BoundArchivedCatalogHeadV1, BoundCatalogMutationJournalV1) {
        let archive_object_id = super::super::domain_object_id_v1(
            ARCHIVED_HEAD_OBJECT_DOMAIN_V1,
            &observed.committed_head_bytes,
        );
        let journal_raw_bytes = br#"{"catalog_sequence":2,"mutations":[],"parent_head_revision":"fixture","publication_nonce":"fixture","version":1}"#.to_vec();
        let journal_object_id = super::super::domain_object_id_v1(
            MUTATION_JOURNAL_OBJECT_DOMAIN_V1,
            &journal_raw_bytes,
        );
        (
            BoundArchivedCatalogHeadV1 {
                context: observed.context.clone(),
                storage_authority_fingerprint: [0x99; 32],
                predecessor_head_revision: observed.head_revision,
                predecessor_head_bytes_blake3: *blake3::hash(&observed.committed_head_bytes)
                    .as_bytes(),
                object_id: archive_object_id,
                raw_bytes_len: NonZeroU64::new(
                    u64::try_from(observed.committed_head_bytes.len()).unwrap(),
                )
                .unwrap(),
                binding: RegisteredRootRemoteObjectBindingV1::Etag {
                    etag: "archive-etag".to_owned(),
                },
            },
            BoundCatalogMutationJournalV1 {
                context: observed.context.clone(),
                storage_authority_fingerprint: [0x99; 32],
                catalog_sequence: NonZeroU64::new(observed.sequence.get() + 1).unwrap(),
                publication_nonce,
                parent_head_revision: observed.head_revision,
                object_id: journal_object_id,
                raw_bytes_len: NonZeroU64::new(u64::try_from(journal_raw_bytes.len()).unwrap())
                    .unwrap(),
                raw_bytes: journal_raw_bytes,
                binding: RegisteredRootRemoteObjectBindingV1::Etag {
                    etag: "journal-etag".to_owned(),
                },
            },
        )
    }

    fn matched<'a>(
        fixture: &'a SemanticRemoteCatalogFixtureV1,
        observed: &ObservedPublishedCatalogHeadV1,
    ) -> MatchedCatalogPublicationPrerequisitesV1<'a> {
        let (bootstrap, high_water, writers) = trusted_receipts(observed);
        match_catalog_publication_prerequisites_v1(
            trusted_storage(fixture),
            observed,
            &bootstrap,
            high_water,
            writers,
        )
        .unwrap()
    }

    fn successor_claim(
        observed: &ObservedPublishedCatalogHeadV1,
        publication_nonce: [u8; 32],
    ) -> CatalogSuccessorClaimV1 {
        CatalogSuccessorClaimV1 {
            context: observed.context.clone(),
            sequence: observed.sequence.get() + 1,
            publication_nonce,
            parent_head_revision: observed.head_revision,
        }
    }

    #[tokio::test]
    async fn observed_head_retains_exact_current_etag_and_lineage() {
        let (_fixture, observed) = observed_head().await;
        assert_eq!(observed.sequence.get(), 1);
        assert_eq!(observed.parent_head_revision, None);
        assert_ne!(observed.publication_nonce, [0; 32]);
        assert_ne!(observed.head_revision, [0; 32]);
        assert_eq!(observed.current_head_etag, "head-etag-a");
    }

    #[test]
    fn mutable_head_cas_accepts_etag_and_version_plus_etag_but_not_version_only() {
        assert_eq!(
            mutable_head_etag_v1(&RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "head-etag".to_owned(),
            }),
            Some("head-etag".to_owned())
        );
        assert_eq!(
            mutable_head_etag_v1(&RegisteredRootRemoteObjectBindingV1::Version {
                version: "head-version".to_owned(),
                etag: Some("head-etag".to_owned()),
            }),
            Some("head-etag".to_owned())
        );
        assert_eq!(
            mutable_head_etag_v1(&RegisteredRootRemoteObjectBindingV1::Version {
                version: "head-version".to_owned(),
                etag: None,
            }),
            None
        );
    }

    #[tokio::test]
    async fn exact_external_receipts_prepare_one_visible_successor() {
        let (fixture, observed) = observed_head().await;
        let publication_nonce = [0x44; 32];
        let (archive, journal) = bound_publication_objects(&observed, publication_nonce);
        let fence = prepare_catalog_publication_fence_v1(
            matched(&fixture, &observed),
            publication_nonce,
            archive,
            journal,
        )
        .unwrap();
        assert_eq!(fence.sequence.get(), 2);
        assert_eq!(fence.parent_head_revision, observed.head_revision);
        assert_eq!(fence.expected_parent_head_etag, "head-etag-a");
        assert_eq!(
            archived_head_object_key_v1(&fence.predecessor_archive).unwrap(),
            format!(
                "roots/{}/{}",
                ARCHIVED_HEAD_OBJECT_SUFFIX_V1,
                lower_hex(&fence.predecessor_archive.object_id)
            )
        );
        assert_eq!(
            mutation_journal_object_key_v1(&fence.mutation_journal).unwrap(),
            format!(
                "roots/{}/{}",
                MUTATION_JOURNAL_OBJECT_SUFFIX_V1,
                lower_hex(&fence.mutation_journal.object_id)
            )
        );

        let bytes = canonical_publishing_head_bytes_v1(&fence);
        assert!(is_canonical_publishing_head_v1(&bytes, &test_selected()));
        assert!(
            super::super::canonical_wire_v1::<super::super::RemoteCatalogHeadWireV1>(&bytes)
                .is_none()
        );
        let wire = serde_json::from_slice::<RemoteCatalogPublishingHeadWireV1>(&bytes).unwrap();
        assert_eq!(wire.storage_authority_fingerprint, "99".repeat(32));
        assert_eq!(wire.control_authority_fingerprint, "9a".repeat(32));
        assert_eq!(wire.writer_epoch, "22".repeat(32));
        assert_eq!(wire.high_water_authority_revision, "ab".repeat(32));
        assert_eq!(wire.high_water_lease_nonce, "aa".repeat(32));
        assert_eq!(wire.writer_fence_authority_revision, "89".repeat(32));
        assert_eq!(wire.writer_fence_lease_nonce, "88".repeat(32));
        assert_eq!(
            wire.predecessor_head_archive.object_id,
            lower_hex(&super::super::domain_object_id_v1(
                ARCHIVED_HEAD_OBJECT_DOMAIN_V1,
                &observed.committed_head_bytes,
            ))
        );
        assert_eq!(
            wire.mutation_journal.object_id,
            lower_hex(&fence.mutation_journal.object_id)
        );
    }

    #[tokio::test]
    async fn successor_rejects_sequence_parent_nonce_and_context_drift() {
        let (fixture, observed) = observed_head().await;

        let mut prerequisites = matched(&fixture, &observed);
        prerequisites.sequence = NonZeroU64::new(u64::MAX).unwrap();
        let (archive, journal) = bound_publication_objects(&observed, [0x44; 32]);
        assert_eq!(
            prepare_catalog_publication_fence_v1(prerequisites, [0x44; 32], archive, journal,)
                .unwrap_err(),
            CatalogPublicationContractErrorV1::SequenceOverflow
        );

        let cases = [
            (
                CatalogSuccessorClaimV1 {
                    context: observed.context.clone(),
                    sequence: observed.sequence.get(),
                    publication_nonce: [0x44; 32],
                    parent_head_revision: observed.head_revision,
                },
                CatalogPublicationContractErrorV1::SequenceMismatch,
            ),
            (
                CatalogSuccessorClaimV1 {
                    context: observed.context.clone(),
                    sequence: observed.sequence.get() + 2,
                    publication_nonce: [0x44; 32],
                    parent_head_revision: observed.head_revision,
                },
                CatalogPublicationContractErrorV1::SequenceMismatch,
            ),
            (
                CatalogSuccessorClaimV1 {
                    context: observed.context.clone(),
                    sequence: observed.sequence.get() + 1,
                    publication_nonce: [0x44; 32],
                    parent_head_revision: [0x55; 32],
                },
                CatalogPublicationContractErrorV1::ParentRevisionMismatch,
            ),
            (
                CatalogSuccessorClaimV1 {
                    context: observed.context.clone(),
                    sequence: observed.sequence.get() + 1,
                    publication_nonce: [0; 32],
                    parent_head_revision: observed.head_revision,
                },
                CatalogPublicationContractErrorV1::ZeroPublicationNonce,
            ),
            (
                CatalogSuccessorClaimV1 {
                    context: observed.context.clone(),
                    sequence: observed.sequence.get() + 1,
                    publication_nonce: observed.publication_nonce,
                    parent_head_revision: observed.head_revision,
                },
                CatalogPublicationContractErrorV1::ReusedPublicationNonce,
            ),
        ];
        for (claim, expected) in cases {
            let (archive, journal) = bound_publication_objects(&observed, claim.publication_nonce);
            assert_eq!(
                validate_catalog_successor_claim_v1(
                    matched(&fixture, &observed),
                    claim,
                    archive,
                    journal,
                )
                .unwrap_err(),
                expected
            );
        }

        let mut wrong_context = observed.context.clone();
        wrong_context.root_id = "other-root".to_owned();
        let claim = CatalogSuccessorClaimV1 {
            context: wrong_context,
            sequence: observed.sequence.get() + 1,
            publication_nonce: [0x44; 32],
            parent_head_revision: observed.head_revision,
        };
        let (archive, journal) = bound_publication_objects(&observed, claim.publication_nonce);
        assert_eq!(
            validate_catalog_successor_claim_v1(
                matched(&fixture, &observed),
                claim,
                archive,
                journal,
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::ContextMismatch
        );
    }

    #[tokio::test]
    async fn publication_artifacts_are_exactly_bound_before_the_visible_fence() {
        let (fixture, observed) = observed_head().await;
        let publication_nonce = [0x44; 32];
        let reject = |archive, journal, expected| {
            assert_eq!(
                validate_catalog_successor_claim_v1(
                    matched(&fixture, &observed),
                    successor_claim(&observed, publication_nonce),
                    archive,
                    journal,
                )
                .unwrap_err(),
                expected
            );
        };

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.context.root_id = "other-root".to_owned();
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.storage_authority_fingerprint = [0x98; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.predecessor_head_revision = [0x55; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.predecessor_head_bytes_blake3 = [0x55; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.object_id = [0x55; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.raw_bytes_len = NonZeroU64::new(archive.raw_bytes_len.get() + 1).unwrap();
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (mut archive, journal) = bound_publication_objects(&observed, publication_nonce);
        archive.binding = RegisteredRootRemoteObjectBindingV1::Etag {
            etag: String::new(),
        };
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::PredecessorArchiveMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.context.root_id = "other-root".to_owned();
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.storage_authority_fingerprint = [0x98; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.catalog_sequence = NonZeroU64::new(journal.catalog_sequence.get() + 1).unwrap();
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.publication_nonce = [0x45; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.parent_head_revision = [0x55; 32];
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.raw_bytes.push(b' ');
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.raw_bytes_len = NonZeroU64::new(journal.raw_bytes_len.get() + 1).unwrap();
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );

        let (archive, mut journal) = bound_publication_objects(&observed, publication_nonce);
        journal.binding = RegisteredRootRemoteObjectBindingV1::Etag {
            etag: String::new(),
        };
        reject(
            archive,
            journal,
            CatalogPublicationContractErrorV1::MutationJournalMismatch,
        );
    }

    #[tokio::test]
    async fn receipt_matching_rejects_bootstrap_high_water_and_writer_epoch_drift() {
        let (fixture, observed) = observed_head().await;
        {
            let (mut bootstrap, high_water, writers) = trusted_receipts(&observed);
            bootstrap.complete_corpus_attestation = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::InvalidBootstrapReceipt
            );
        }
        {
            let (mut bootstrap, mut high_water, mut writers) = trusted_receipts(&observed);
            bootstrap.bootstrap_head_revision = [0x55; 32];
            high_water.bootstrap_head_revision = [0x55; 32];
            writers.bootstrap_head_revision = [0x55; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::BootstrapMismatch,
                "sequence one must be the exact externally attested bootstrap head"
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.current_head_revision = [0x66; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.current_publication_nonce = [0x66; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.current_sequence = NonZeroU64::new(observed.sequence.get() + 1).unwrap();
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch,
                "a guard ahead of the observed HEAD identifies observed replay or rollback"
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.authority_revision = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.current_writer_epoch = [0x77; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::WriterFenceMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.authority_revision = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::WriterFenceMismatch
            );
        }
    }

    #[tokio::test]
    async fn exact_current_guard_rejects_a_stale_guard_behind_the_observed_head() {
        let (fixture, mut observed) = observed_head().await;
        observed.sequence = NonZeroU64::new(2).unwrap();
        observed.parent_head_revision = Some([0x54; 32]);
        observed.head_revision = [0x55; 32];
        observed.publication_nonce = [0x56; 32];
        let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
        high_water.current_sequence = NonZeroU64::new(1).unwrap();
        assert_eq!(
            match_catalog_publication_prerequisites_v1(
                trusted_storage(&fixture),
                &observed,
                &bootstrap,
                high_water,
                writers,
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::HighWaterMismatch,
            "a guard behind the observed HEAD is stale currentness authority"
        );
    }

    #[tokio::test]
    async fn receipt_matching_rejects_storage_and_control_authority_drift() {
        let (fixture, observed) = observed_head().await;

        {
            let (mut bootstrap, high_water, writers) = trusted_receipts(&observed);
            bootstrap.storage_authority_fingerprint = [0x98; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.storage_authority_fingerprint = [0x98; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.storage_authority_fingerprint = [0x98; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.control_authority_fingerprint = [0x9b; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::ControlAuthorityMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.control_authority_fingerprint = [0x9b; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    high_water,
                    writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::ControlAuthorityMismatch
            );
        }
    }

    #[tokio::test]
    async fn receipt_matching_rejects_another_accessor_or_prefix() {
        let (fixture, observed) = observed_head().await;
        let (other_fixture, _other_observed) = observed_head().await;
        {
            let (bootstrap, high_water, writers) = trusted_receipts(&observed);
            let crossed = TrustedCatalogStorageAuthorityV1 {
                operator: fixture.operator(),
                conditional_write_receipt: other_fixture.receipt(),
                authority_fingerprint: [0x99; 32],
            };
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    crossed, &observed, &bootstrap, high_water, writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageSemanticsUnverified
            );
        }
        {
            let (bootstrap, high_water, writers) = trusted_receipts(&observed);
            let crossed = TrustedCatalogStorageAuthorityV1 {
                operator: other_fixture.operator(),
                conditional_write_receipt: fixture.receipt(),
                authority_fingerprint: [0x99; 32],
            };
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    crossed, &observed, &bootstrap, high_water, writers,
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageSemanticsUnverified
            );
        }
    }

    #[test]
    fn publishing_wire_rejects_noncanonical_zero_and_bootstrap_like_states() {
        let selected = test_selected();
        let context = CatalogAuthorityContextV1 {
            remote_prefix: test_spec().remote_prefix,
            root_id: selected.root_id().to_owned(),
            root_identity_fingerprint: selected.spec_identity_fingerprint().to_owned(),
            root_generation: selected.spec().generation,
            profile: selected.spec().profile,
            profile_settings_fingerprint: selected.profile_settings_fingerprint(),
            plan_contract_fingerprint: selected.plan_contract_fingerprint(),
        }
        .to_wire();
        let wire = RemoteCatalogPublishingHeadWireV1 {
            version: CATALOG_SCHEMA_VERSION_V1,
            state: RemoteCatalogPublishingStateWireV1::Publishing,
            context,
            catalog_sequence: 2,
            publication_nonce: "44".repeat(32),
            parent_head_revision: "55".repeat(32),
            bootstrap_head_revision: "66".repeat(32),
            storage_authority_fingerprint: "99".repeat(32),
            control_authority_fingerprint: "9a".repeat(32),
            writer_epoch: "77".repeat(32),
            high_water_authority_revision: "ab".repeat(32),
            high_water_lease_nonce: "aa".repeat(32),
            writer_fence_authority_revision: "89".repeat(32),
            writer_fence_lease_nonce: "88".repeat(32),
            predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1 {
                object_id: "bb".repeat(32),
                raw_bytes_len: 1,
                binding: RemoteCatalogObjectBindingWireV1 {
                    version: None,
                    etag: Some("archive-etag".to_owned()),
                },
            },
            mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1 {
                object_id: "cc".repeat(32),
                raw_bytes_len: 1,
                binding: RemoteCatalogObjectBindingWireV1 {
                    version: None,
                    etag: Some("journal-etag".to_owned()),
                },
            },
        };
        let canonical = serde_json::to_vec(&wire).unwrap();
        assert!(is_canonical_publishing_head_v1(&canonical, &selected));

        let mut noncanonical = canonical.clone();
        noncanonical.push(b'\n');
        assert!(!is_canonical_publishing_head_v1(&noncanonical, &selected));

        let mut zero_nonce = wire;
        zero_nonce.publication_nonce = "00".repeat(32);
        assert!(!is_canonical_publishing_head_v1(
            &serde_json::to_vec(&zero_nonce).unwrap(),
            &selected
        ));

        zero_nonce.publication_nonce = "44".repeat(32);
        zero_nonce.catalog_sequence = 1;
        assert!(!is_canonical_publishing_head_v1(
            &serde_json::to_vec(&zero_nonce).unwrap(),
            &selected
        ));
    }

    #[test]
    fn publishing_wire_rejects_unbound_authority_and_recovery_references() {
        let fresh_wire = || {
            let selected = test_selected();
            let context = CatalogAuthorityContextV1 {
                remote_prefix: test_spec().remote_prefix,
                root_id: selected.root_id().to_owned(),
                root_identity_fingerprint: selected.spec_identity_fingerprint().to_owned(),
                root_generation: selected.spec().generation,
                profile: selected.spec().profile,
                profile_settings_fingerprint: selected.profile_settings_fingerprint(),
                plan_contract_fingerprint: selected.plan_contract_fingerprint(),
            }
            .to_wire();
            serde_json::from_slice::<RemoteCatalogPublishingHeadWireV1>(
                &canonical_publishing_head_bytes_for_test_v1(context),
            )
            .unwrap()
        };
        let selected = test_selected();
        let reject = |wire: RemoteCatalogPublishingHeadWireV1| {
            assert!(!is_canonical_publishing_head_v1(
                &serde_json::to_vec(&wire).unwrap(),
                &selected,
            ));
        };

        let mut wire = fresh_wire();
        wire.storage_authority_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.control_authority_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.high_water_authority_revision = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.high_water_lease_nonce = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.writer_fence_authority_revision = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.writer_fence_lease_nonce = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.predecessor_head_archive.raw_bytes_len = 0;
        reject(wire);
        let mut wire = fresh_wire();
        wire.predecessor_head_archive.binding.etag = Some(String::new());
        reject(wire);
        let mut wire = fresh_wire();
        wire.mutation_journal.object_id = "AA".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.mutation_journal.raw_bytes_len = 0;
        reject(wire);

        let mut unknown = serde_json::to_value(fresh_wire()).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("future-field".to_owned(), serde_json::json!(true));
        assert!(!is_canonical_publishing_head_v1(
            &serde_json::to_vec(&unknown).unwrap(),
            &selected,
        ));
    }
}
