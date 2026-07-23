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
//! are opaque external receipts with no production constructors. One external
//! control acquisition binds those receipts and must move monotonically from
//! exact-current `Ready` to one exact `PublicationPending` successor before a
//! future visible `HEAD` CAS. A terminal matcher also specifies the required
//! `PublicationPending` to exact-current `Ready(n+1)` advance, but cannot mint a
//! fresh guard. The module can publish the exact immutable predecessor-HEAD
//! archive. Fact-bound preparation derives predecessors from the semantic
//! corpus, proves create-key absence through the held accessor, reconstructs
//! successor semantics, and publishes exact immutable payloads plus an
//! authoritative journal. A separately namespaced diagnostic draft remains
//! non-authoritative and cannot convert into that journal. The namespace
//! mutation gateway accepts only the complete authoritative journal after an
//! exact visible publishing-HEAD receipt and rechecks a retained backend lease
//! before and after each conditional write. That receipt and lease still have
//! no production constructors, so this module cannot mint production
//! authority, write a live `HEAD`, reach a live namespace mutation, produce a
//! plan digest, or authorize an action.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use anyhow::{Context, Result as AnyhowResult};
use opendal::Operator;
use serde::{Deserialize, Serialize};
use tcfs_core::config::{
    RegisteredRootPlanContractFingerprintV1, RegisteredRootPlanContractV1,
    RootProfileSettingsFingerprintV1, RootProfileV1,
};

use super::{
    lower_hex, validate_catalog_context_v1, validate_catalog_object_route_v1,
    validate_entry_size_v1, InvalidRemoteCatalogReasonV1, RemoteCatalogContextWireV1,
    RemoteCatalogObjectBindingWireV1, RemoteCatalogObjectKindV1,
    SemanticallyBoundRemoteCatalogCorpusV1, CATALOG_SCHEMA_VERSION_V1,
};
use crate::blacklist::Blacklist;
use crate::index_entry::{
    read_raw_object_snapshot_v1, PortableNamespaceRole, RawObjectReadBindingV1, RawObjectReadV1,
};
use crate::registered_reconcile::{
    validate_bound_catalog_manifest_references_v1, validate_catalog_index_payload_v1,
    validate_catalog_manifest_payload_v1, validate_catalog_reservation_payload_v1,
    validate_registered_remote_storage_key_bounds_v1, CatalogManifestReferenceV1,
    CatalogValidatedIndexPayloadV1, CatalogValidatedIndexStateV1,
    CatalogValidatedReservationPayloadV1, RegisteredRootRemoteObjectBindingV1,
};
use crate::registered_remote_observation::{
    RemoteNamespaceClaimAccumulatorV1, RemoteNamespaceClaimOriginsV1,
};
use crate::registered_source_composition::ValidatedSelectedRegisteredRootRemoteContextV1;
use tcfs_storage::ConditionalWriteSemanticsReceipt;

const ARCHIVED_HEAD_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-archived-head-object.b3v1";
const MUTATION_JOURNAL_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-mutation-journal-object.b3v1";
const UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-mutation-journal-draft-object.b3v1";
const CATALOG_SUCCESSOR_PAYLOAD_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-successor-payload-object.b3v1";
const PUBLISHING_HEAD_RESERVATION_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-publishing-head-reservation.b3v1";
const PREDECESSOR_HEAD_STORAGE_BINDING_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-predecessor-head-storage-binding.b3v1";
const ARCHIVED_HEAD_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/archived-heads";
const MUTATION_JOURNAL_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/mutation-journals";
const UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_SUFFIX_V1: &str =
    ".tcfs-catalog/v1/publications/mutation-journal-drafts";
const CATALOG_SUCCESSOR_PAYLOAD_OBJECT_SUFFIX_V1: &str =
    ".tcfs-catalog/v1/publications/successor-payloads";
const CATALOG_MUTATION_JOURNAL_DRAFT_SCHEMA_VERSION_V1: u32 = 1;
const CATALOG_MUTATION_JOURNAL_SCHEMA_VERSION_V1: u32 = 1;

/// One draft is intentionally bounded to one catalog page worth of physical
/// transitions. Both factors are already committed by the registered-root plan
/// contract fingerprint, while the draft schema version fixes this derivation.
fn max_catalog_mutation_draft_key_bytes_v1() -> u64 {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    remote
        .max_catalog_entries_per_page()
        .checked_mul(remote.max_storage_key_bytes())
        .expect("strict catalog mutation draft bounds must multiply without overflow")
}

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
pub(crate) enum InvalidCatalogMutationJournalReasonV1 {
    CanonicalEncoding,
    Context,
    Lineage,
    Totals,
    Order,
    Route,
    Operation,
    ObjectIdentity,
    ObjectBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogMutationJournalResourceV1 {
    Bytes,
    Mutations,
    KeyBytes,
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
    ControlTransitionMismatch,
    PublishingHeadTooLarge,
    HighWaterAdvanceMismatch,
    PredecessorArchiveMismatch,
    MutationJournalMismatch,
    MutationCorpusMismatch,
    MutationPredecessorMissing,
    MutationPredecessorKindMismatch,
    MutationAbsenceMismatch,
    MutationAbsenceStale,
    ControlLeaseNotLive,
    InvalidSuccessorPayload,
    InvalidSuccessorClosure,
    InvalidMutationJournal(InvalidCatalogMutationJournalReasonV1),
    MutationJournalResource(CatalogMutationJournalResourceV1),
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
/// epoch. V1 does not model credential-epoch rotation.
///
/// There is intentionally no production constructor in this checkpoint.
pub(crate) struct TrustedCatalogBootstrapReceiptV1 {
    context: CatalogAuthorityContextV1,
    bootstrap: CatalogBootstrapIdentityV1,
    storage_authority_fingerprint: [u8; 32],
    control_authority_fingerprint: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogBootstrapIdentityV1 {
    head_revision: [u8; 32],
    publication_nonce: [u8; 32],
    complete_corpus_attestation: [u8; 32],
    writer_epoch: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogHighWaterPointV1 {
    sequence: NonZeroU64,
    head_revision: [u8; 32],
    publication_nonce: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogControlAuthorityRevisionV1 {
    generation: NonZeroU64,
    /// Preselected non-secret logical state identifier. This is neither a
    /// backend-generated ETag nor the content fingerprint of the record that
    /// embeds it; the latter is computed separately after canonical encoding.
    fingerprint: [u8; 32],
}

/// A non-secret identifier suitable for logs and serialized recovery state.
/// Bearer lease material, renewal credentials, and signing keys must never be
/// stored in this value or in any catalog object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NonSecretLeasePublicFingerprintV1([u8; 32]);

/// Non-cloneable retained backend lease for one exact control acquisition.
///
/// The public fingerprint serialized into catalog/control records is only a
/// correlation identifier. This value is the separate live backend handle.
/// There is deliberately no production constructor in this checkpoint.
trait CatalogControlLeaseLivenessV1: Send + Sync {
    fn is_live_v1(&self) -> bool;
}

struct RetainedCatalogControlLeaseV1 {
    control_binding: CatalogControlAcquisitionBindingV1,
    writer_fence_authority_revision_fingerprint: [u8; 32],
    writer_fence_lease_public_fingerprint: NonSecretLeasePublicFingerprintV1,
    liveness: Box<dyn CatalogControlLeaseLivenessV1>,
    #[cfg(test)]
    test_live: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RetainedCatalogControlLeaseV1 {
    fn matches_writer_fence_v1(&self, writers: &AllNamespaceWritersFencedLeaseV1) -> bool {
        self.control_binding == writers.control_binding
            && self.writer_fence_authority_revision_fingerprint
                == writers.authority_revision_fingerprint
            && self.writer_fence_lease_public_fingerprint == writers.lease_public_fingerprint
    }

    fn is_live_v1(&self) -> bool {
        self.liveness.is_live_v1()
    }
}

/// Exact `Ready` state held by one exclusive external control acquisition.
///
/// V1 deliberately freezes the writer epoch to the bootstrap epoch. A future
/// credential rotation must be its own monotonic, revocation-backed protocol;
/// copied current-epoch fields are not continuity evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogControlAcquisitionBindingV1 {
    context: CatalogAuthorityContextV1,
    bootstrap: CatalogBootstrapIdentityV1,
    current: CatalogHighWaterPointV1,
    storage_authority_fingerprint: [u8; 32],
    control_authority_fingerprint: [u8; 32],
    ready_revision: CatalogControlAuthorityRevisionV1,
    lease_public_fingerprint: NonSecretLeasePublicFingerprintV1,
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
    binding: CatalogControlAcquisitionBindingV1,
}

/// External proof that every credential and code path able to mutate the
/// catalog corpus is fenced by the publishing-HEAD protocol.
///
/// This is broader than an in-process mutex or storage CAS receipt: old
/// binaries and direct object-store credentials are part of the epoch. A
/// future production constructor must keep this lease live through the visible
/// HEAD CAS, every namespace mutation, committed-HEAD finalization, and
/// high-water advancement. The retained handle is nominally separate from the
/// serialized public fingerprints and cannot be reconstructed from them.
pub(crate) struct AllNamespaceWritersFencedLeaseV1 {
    control_binding: CatalogControlAcquisitionBindingV1,
    authority_revision_fingerprint: [u8; 32],
    lease_public_fingerprint: NonSecretLeasePublicFingerprintV1,
    retained_control_lease: RetainedCatalogControlLeaseV1,
}

/// One held external control acquisition. High-water and all-writer fencing
/// are inseparable views of this capability; callers cannot submit two
/// independently acquired field bags to the publication matcher.
///
/// There is intentionally no production constructor in this checkpoint.
pub(crate) struct HeldReadyCatalogControlGuardV1 {
    high_water: TrustedCatalogHighWaterGuardV1,
    all_writers: AllNamespaceWritersFencedLeaseV1,
}

fn require_held_control_lease_live_v1(
    control: &HeldReadyCatalogControlGuardV1,
) -> Result<(), CatalogPublicationContractErrorV1> {
    if control.high_water.binding != control.all_writers.control_binding
        || !control
            .all_writers
            .retained_control_lease
            .matches_writer_fence_v1(&control.all_writers)
    {
        return Err(CatalogPublicationContractErrorV1::WriterFenceMismatch);
    }
    if !control.all_writers.retained_control_lease.is_live_v1() {
        return Err(CatalogPublicationContractErrorV1::ControlLeaseNotLive);
    }
    Ok(())
}

/// Exact receipt match for one observed predecessor. This still is not remote
/// completeness authority and has no planner/action conversion.
pub(crate) struct MatchedCatalogPublicationPrerequisitesV1<'a> {
    storage_authority: TrustedCatalogStorageAuthorityV1<'a>,
    control_guard: HeldReadyCatalogControlGuardV1,
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
    control: HeldReadyCatalogControlGuardV1,
) -> Result<MatchedCatalogPublicationPrerequisitesV1<'a>, CatalogPublicationContractErrorV1> {
    let high_water = &control.high_water;
    let writers = &control.all_writers;
    if !storage_authority
        .conditional_write_receipt
        .authorizes(storage_authority.operator, &observed.context.remote_prefix)
        .unwrap_or(false)
    {
        return Err(CatalogPublicationContractErrorV1::StorageSemanticsUnverified);
    }
    let bootstrap_fields_nonzero = bootstrap.bootstrap.head_revision != [0; 32]
        && bootstrap.bootstrap.publication_nonce != [0; 32]
        && bootstrap.bootstrap.complete_corpus_attestation != [0; 32]
        && bootstrap.bootstrap.writer_epoch != [0; 32]
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
        || high_water.binding.storage_authority_fingerprint
            != storage_authority.authority_fingerprint
        || writers.control_binding.storage_authority_fingerprint
            != storage_authority.authority_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::StorageAuthorityMismatch);
    }
    if high_water.binding.control_authority_fingerprint != bootstrap.control_authority_fingerprint
        || writers.control_binding.control_authority_fingerprint
            != bootstrap.control_authority_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::ControlAuthorityMismatch);
    }
    if high_water.binding.context != observed.context
        || high_water.binding.bootstrap != bootstrap.bootstrap
        || high_water.binding.current.sequence != observed.sequence
        || high_water.binding.current.head_revision != observed.head_revision
        || high_water.binding.current.publication_nonce != observed.publication_nonce
        || high_water.binding.ready_revision.fingerprint == [0; 32]
        || high_water.binding.lease_public_fingerprint.0 == [0; 32]
    {
        return Err(CatalogPublicationContractErrorV1::HighWaterMismatch);
    }
    if writers.control_binding != high_water.binding
        || writers.authority_revision_fingerprint == [0; 32]
        || writers.lease_public_fingerprint.0 == [0; 32]
    {
        return Err(CatalogPublicationContractErrorV1::WriterFenceMismatch);
    }
    if observed.sequence.get() == 1
        && (bootstrap.bootstrap.head_revision != observed.head_revision
            || bootstrap.bootstrap.publication_nonce != observed.publication_nonce)
    {
        return Err(CatalogPublicationContractErrorV1::BootstrapMismatch);
    }
    require_held_control_lease_live_v1(&control)?;
    Ok(MatchedCatalogPublicationPrerequisitesV1 {
        storage_authority,
        control_guard: control,
        context: observed.context.clone(),
        sequence: observed.sequence,
        publication_nonce: observed.publication_nonce,
        head_revision: observed.head_revision,
        committed_head_bytes: observed.committed_head_bytes.clone(),
        current_head_etag: observed.current_head_etag.clone(),
        bootstrap_head_revision: bootstrap.bootstrap.head_revision,
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RemoteCatalogMutationOperationDraftWireV1 {
    CreateIfAbsent,
    ReplaceIfMatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum RemoteCatalogMutationPredecessorDraftWireV1 {
    Absent,
    Present {
        raw_bytes_len: u64,
        raw_blake3: String,
        binding: RemoteCatalogObjectBindingWireV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogMutationSuccessorDraftWireV1 {
    raw_bytes_len: u64,
    raw_blake3: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogMutationDraftWireV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    operation: RemoteCatalogMutationOperationDraftWireV1,
    predecessor: RemoteCatalogMutationPredecessorDraftWireV1,
    successor: RemoteCatalogMutationSuccessorDraftWireV1,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogMutationJournalDraftWireV1 {
    version: u32,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    parent_head_revision: String,
    mutation_count: u64,
    mutation_key_bytes: u64,
    mutations: Vec<RemoteCatalogMutationDraftWireV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogMutationDraftObjectIdentityV1 {
    raw_bytes_len: NonZeroU64,
    raw_blake3: [u8; 32],
}

impl CatalogMutationDraftObjectIdentityV1 {
    fn from_bytes(raw_bytes: &[u8]) -> Option<Self> {
        let raw_bytes_len = NonZeroU64::new(u64::try_from(raw_bytes.len()).ok()?)?;
        Some(Self {
            raw_bytes_len,
            raw_blake3: *blake3::hash(raw_bytes).as_bytes(),
        })
    }

    fn to_wire(&self) -> RemoteCatalogMutationSuccessorDraftWireV1 {
        RemoteCatalogMutationSuccessorDraftWireV1 {
            raw_bytes_len: self.raw_bytes_len.get(),
            raw_blake3: lower_hex(&self.raw_blake3),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CatalogMutationDraftPredecessorV1 {
    Absent,
    Present {
        identity: CatalogMutationDraftObjectIdentityV1,
        binding: RegisteredRootRemoteObjectBindingV1,
    },
}

/// One untrusted draft of a final catalog-visible physical-key transition.
///
/// These fields are deliberately caller claims. Shape validation cannot prove
/// predecessor membership, create-key absence, successor semantics, or
/// cross-object closure. Consequently this private draft cannot become the
/// authoritative `BoundCatalogMutationJournalV1` accepted by a publishing
/// fence. A later planner must derive that value from fact-bound corpus
/// evidence and immutable successor payloads.
struct UntrustedCatalogMutationIntentDraftV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    operation: RemoteCatalogMutationOperationDraftWireV1,
    predecessor: CatalogMutationDraftPredecessorV1,
    successor: CatalogMutationDraftObjectIdentityV1,
}

impl UntrustedCatalogMutationIntentDraftV1 {
    fn create_if_absent(
        kind: RemoteCatalogObjectKindV1,
        object_key: String,
        successor_bytes: &[u8],
    ) -> Option<Self> {
        Some(Self {
            kind,
            object_key,
            operation: RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
            predecessor: CatalogMutationDraftPredecessorV1::Absent,
            successor: CatalogMutationDraftObjectIdentityV1::from_bytes(successor_bytes)?,
        })
    }

    fn replace_if_match(
        kind: RemoteCatalogObjectKindV1,
        object_key: String,
        predecessor_bytes: &[u8],
        predecessor_binding: RegisteredRootRemoteObjectBindingV1,
        successor_bytes: &[u8],
    ) -> Option<Self> {
        Some(Self {
            kind,
            object_key,
            operation: RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
            predecessor: CatalogMutationDraftPredecessorV1::Present {
                identity: CatalogMutationDraftObjectIdentityV1::from_bytes(predecessor_bytes)?,
                binding: predecessor_binding,
            },
            successor: CatalogMutationDraftObjectIdentityV1::from_bytes(successor_bytes)?,
        })
    }

    fn into_wire(self) -> RemoteCatalogMutationDraftWireV1 {
        let predecessor = match self.predecessor {
            CatalogMutationDraftPredecessorV1::Absent => {
                RemoteCatalogMutationPredecessorDraftWireV1::Absent
            }
            CatalogMutationDraftPredecessorV1::Present { identity, binding } => {
                RemoteCatalogMutationPredecessorDraftWireV1::Present {
                    raw_bytes_len: identity.raw_bytes_len.get(),
                    raw_blake3: lower_hex(&identity.raw_blake3),
                    binding: binding_wire_v1(&binding),
                }
            }
        };
        RemoteCatalogMutationDraftWireV1 {
            kind: self.kind,
            object_key: self.object_key,
            operation: self.operation,
            predecessor,
            successor: self.successor.to_wire(),
        }
    }
}

/// Canonical, bounded, untrusted journal draft prepared without touching
/// storage.
///
/// Successors retain identities, not payload bytes. This is enough for
/// diagnostic classification, but not for authoritative recovery evidence or
/// autonomous roll-forward. There is intentionally no conversion from this
/// type to `BoundCatalogMutationJournalV1` or a prepared publishing fence.
struct PreparedUntrustedCatalogMutationJournalDraftV1 {
    context: CatalogAuthorityContextV1,
    catalog_sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
    object_id: [u8; 32],
    raw_bytes: Vec<u8>,
    raw_bytes_len: NonZeroU64,
}

impl std::fmt::Debug for PreparedUntrustedCatalogMutationJournalDraftV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedUntrustedCatalogMutationJournalDraftV1")
            .field("remote_prefix", &self.context.remote_prefix)
            .field("root_id", &self.context.root_id)
            .field("catalog_sequence", &self.catalog_sequence)
            .field("mutation_journal_bytes", &self.raw_bytes_len)
            .finish_non_exhaustive()
    }
}

/// Exact immutable publication of an untrusted mutation-journal draft.
///
/// The bytes and storage binding are real; the mutation claims are not yet
/// fact-bound. This type is deliberately distinct from
/// `BoundCatalogMutationJournalV1`, has no conversion to it, and cannot enter a
/// prepared publishing fence.
struct PublishedUntrustedCatalogMutationJournalDraftV1 {
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

fn invalid_mutation_journal(
    reason: InvalidCatalogMutationJournalReasonV1,
) -> CatalogPublicationContractErrorV1 {
    CatalogPublicationContractErrorV1::InvalidMutationJournal(reason)
}

fn mutation_journal_resource(
    resource: CatalogMutationJournalResourceV1,
) -> CatalogPublicationContractErrorV1 {
    CatalogPublicationContractErrorV1::MutationJournalResource(resource)
}

fn binding_has_usable_etag_v1(binding: &RemoteCatalogObjectBindingWireV1) -> bool {
    binding
        .etag
        .as_deref()
        .is_some_and(|etag| !etag.is_empty() && etag != "null")
}

fn validate_catalog_mutation_journal_draft_bytes_v1(
    raw_bytes: &[u8],
    expected_context: &CatalogAuthorityContextV1,
    expected_sequence: NonZeroU64,
    expected_publication_nonce: [u8; 32],
    expected_parent_head_revision: [u8; 32],
) -> Result<RemoteCatalogMutationJournalDraftWireV1, CatalogPublicationContractErrorV1> {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let raw_bytes_len = u64::try_from(raw_bytes.len())
        .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Bytes))?;
    if raw_bytes_len == 0 || raw_bytes_len > remote.max_catalog_page_object_bytes() {
        return Err(mutation_journal_resource(
            CatalogMutationJournalResourceV1::Bytes,
        ));
    }
    let wire = serde_json::from_slice::<RemoteCatalogMutationJournalDraftWireV1>(raw_bytes)
        .map_err(|_| {
            invalid_mutation_journal(InvalidCatalogMutationJournalReasonV1::CanonicalEncoding)
        })?;
    if serde_json::to_vec(&wire).ok().as_deref() != Some(raw_bytes)
        || wire.version != CATALOG_MUTATION_JOURNAL_DRAFT_SCHEMA_VERSION_V1
    {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::CanonicalEncoding,
        ));
    }
    if wire.context != expected_context.to_wire() {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Context,
        ));
    }
    if wire.catalog_sequence != expected_sequence.get()
        || super::parse_lower_hex_32(&wire.publication_nonce) != Some(expected_publication_nonce)
        || expected_publication_nonce == [0; 32]
        || super::parse_lower_hex_32(&wire.parent_head_revision)
            != Some(expected_parent_head_revision)
        || expected_parent_head_revision == [0; 32]
    {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Lineage,
        ));
    }
    let mutation_count = u64::try_from(wire.mutations.len())
        .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Mutations))?;
    if mutation_count > remote.max_catalog_entries_per_page() {
        return Err(mutation_journal_resource(
            CatalogMutationJournalResourceV1::Mutations,
        ));
    }
    if wire.mutation_count != mutation_count {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Totals,
        ));
    }

    let mut mutation_key_bytes = 0_u64;
    let mut previous_key: Option<&str> = None;
    for mutation in &wire.mutations {
        let object_key_bytes = u64::try_from(mutation.object_key.len())
            .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::KeyBytes))?;
        mutation_key_bytes = mutation_key_bytes
            .checked_add(object_key_bytes)
            .ok_or_else(|| mutation_journal_resource(CatalogMutationJournalResourceV1::KeyBytes))?;
        if mutation_key_bytes > max_catalog_mutation_draft_key_bytes_v1() {
            return Err(mutation_journal_resource(
                CatalogMutationJournalResourceV1::KeyBytes,
            ));
        }
        if previous_key.is_some_and(|previous| previous >= mutation.object_key.as_str()) {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::Order,
            ));
        }
        previous_key = Some(&mutation.object_key);
        if !validate_catalog_object_route_v1(
            &expected_context.remote_prefix,
            mutation.kind,
            &mutation.object_key,
        ) {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::Route,
            ));
        }
        let successor_hash = super::parse_lower_hex_32(&mutation.successor.raw_blake3);
        if successor_hash.is_none()
            || !validate_entry_size_v1(mutation.kind, mutation.successor.raw_bytes_len)
        {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::ObjectIdentity,
            ));
        }
        match (&mutation.operation, &mutation.predecessor) {
            (
                RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
                RemoteCatalogMutationPredecessorDraftWireV1::Absent,
            ) => {}
            (
                RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
                RemoteCatalogMutationPredecessorDraftWireV1::Present {
                    raw_bytes_len,
                    raw_blake3,
                    binding,
                },
            ) if mutation.kind == RemoteCatalogObjectKindV1::Index => {
                if super::parse_lower_hex_32(raw_blake3).is_none()
                    || !validate_entry_size_v1(mutation.kind, *raw_bytes_len)
                {
                    return Err(invalid_mutation_journal(
                        InvalidCatalogMutationJournalReasonV1::ObjectIdentity,
                    ));
                }
                if super::validate_binding_wire_v1(binding).is_none()
                    || !binding_has_usable_etag_v1(binding)
                {
                    return Err(invalid_mutation_journal(
                        InvalidCatalogMutationJournalReasonV1::ObjectBinding,
                    ));
                }
            }
            _ => {
                return Err(invalid_mutation_journal(
                    InvalidCatalogMutationJournalReasonV1::Operation,
                ))
            }
        }
    }
    if wire.mutation_key_bytes != mutation_key_bytes {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Totals,
        ));
    }
    Ok(wire)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UntrustedCatalogMutationRecoveryClassificationV1 {
    NotApplied,
    Applied,
    Diverged,
}

struct ObservedCatalogMutationObjectV1 {
    raw_bytes_len: NonZeroU64,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

fn mutation_identity_matches_observed_v1(
    raw_bytes_len: u64,
    raw_blake3: &str,
    observed: &ObservedCatalogMutationObjectV1,
) -> bool {
    raw_bytes_len == observed.raw_bytes_len.get()
        && super::parse_lower_hex_32(raw_blake3) == Some(observed.raw_blake3)
}

/// Classify one untrusted draft transition without writing or retrying.
///
/// This diagnostic helper does not authorize recovery. A third state is never
/// guessed through, and even `Applied`/`NotApplied` cannot promote the draft to
/// authoritative evidence.
fn classify_untrusted_catalog_mutation_draft_v1(
    mutation: &RemoteCatalogMutationDraftWireV1,
    observed: Option<&ObservedCatalogMutationObjectV1>,
) -> UntrustedCatalogMutationRecoveryClassificationV1 {
    if observed.is_some_and(|observed| {
        mutation_identity_matches_observed_v1(
            mutation.successor.raw_bytes_len,
            &mutation.successor.raw_blake3,
            observed,
        )
    }) {
        return UntrustedCatalogMutationRecoveryClassificationV1::Applied;
    }
    match (&mutation.operation, &mutation.predecessor, observed) {
        (
            RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
            RemoteCatalogMutationPredecessorDraftWireV1::Absent,
            None,
        ) => UntrustedCatalogMutationRecoveryClassificationV1::NotApplied,
        (
            RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
            RemoteCatalogMutationPredecessorDraftWireV1::Present {
                raw_bytes_len,
                raw_blake3,
                binding,
            },
            Some(observed),
        ) if mutation_identity_matches_observed_v1(*raw_bytes_len, raw_blake3, observed)
            && &binding_wire_v1(&observed.binding) == binding =>
        {
            UntrustedCatalogMutationRecoveryClassificationV1::NotApplied
        }
        _ => UntrustedCatalogMutationRecoveryClassificationV1::Diverged,
    }
}

/// Prepare one deterministic transaction journal without writing storage.
/// Empty journals are structurally valid; later execution policy decides
/// whether a no-op publication should be attempted.
fn prepare_untrusted_catalog_mutation_journal_draft_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
    publication_nonce: [u8; 32],
    mut mutations: Vec<UntrustedCatalogMutationIntentDraftV1>,
) -> Result<PreparedUntrustedCatalogMutationJournalDraftV1, CatalogPublicationContractErrorV1> {
    if publication_nonce == [0; 32] {
        return Err(CatalogPublicationContractErrorV1::ZeroPublicationNonce);
    }
    if publication_nonce == prerequisites.publication_nonce {
        return Err(CatalogPublicationContractErrorV1::ReusedPublicationNonce);
    }
    let sequence = prerequisites
        .sequence
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let mutation_count = u64::try_from(mutations.len())
        .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Mutations))?;
    if mutation_count > remote.max_catalog_entries_per_page() {
        return Err(mutation_journal_resource(
            CatalogMutationJournalResourceV1::Mutations,
        ));
    }
    let mutation_key_bytes = mutations.iter().try_fold(0_u64, |total, mutation| {
        total.checked_add(u64::try_from(mutation.object_key.len()).ok()?)
    });
    let mutation_key_bytes = mutation_key_bytes
        .filter(|bytes| *bytes <= max_catalog_mutation_draft_key_bytes_v1())
        .ok_or_else(|| mutation_journal_resource(CatalogMutationJournalResourceV1::KeyBytes))?;
    mutations.sort_unstable_by(|left, right| left.object_key.cmp(&right.object_key));
    let mutations = mutations
        .into_iter()
        .map(UntrustedCatalogMutationIntentDraftV1::into_wire)
        .collect::<Vec<_>>();
    let wire = RemoteCatalogMutationJournalDraftWireV1 {
        version: CATALOG_MUTATION_JOURNAL_DRAFT_SCHEMA_VERSION_V1,
        context: prerequisites.context.to_wire(),
        catalog_sequence: sequence.get(),
        publication_nonce: lower_hex(&publication_nonce),
        parent_head_revision: lower_hex(&prerequisites.head_revision),
        mutation_count,
        mutation_key_bytes,
        mutations,
    };
    let raw_bytes =
        serde_json::to_vec(&wire).expect("catalog mutation journal draft is serializable");
    validate_catalog_mutation_journal_draft_bytes_v1(
        &raw_bytes,
        &prerequisites.context,
        sequence,
        publication_nonce,
        prerequisites.head_revision,
    )?;
    let raw_bytes_len = NonZeroU64::new(
        u64::try_from(raw_bytes.len())
            .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Bytes))?,
    )
    .ok_or_else(|| mutation_journal_resource(CatalogMutationJournalResourceV1::Bytes))?;
    Ok(PreparedUntrustedCatalogMutationJournalDraftV1 {
        context: prerequisites.context.clone(),
        catalog_sequence: sequence,
        publication_nonce,
        parent_head_revision: prerequisites.head_revision,
        object_id: super::domain_object_id_v1(
            UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_DOMAIN_V1,
            &raw_bytes,
        ),
        raw_bytes,
        raw_bytes_len,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogFactObjectIdentityV1 {
    raw_bytes_len: NonZeroU64,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

impl CatalogFactObjectIdentityV1 {
    fn from_bound(
        object: &crate::registered_reconcile::BoundRemoteObjectSnapshotV1,
    ) -> Option<Self> {
        Some(Self {
            raw_bytes_len: NonZeroU64::new(object.raw_bytes_len())?,
            raw_blake3: *object.raw_blake3(),
            binding: object.binding().clone(),
        })
    }
}

fn corpus_object_fact_v1(
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    object_key: &str,
) -> Option<(RemoteCatalogObjectKindV1, CatalogFactObjectIdentityV1)> {
    if let Some(index) = corpus
        .index_objects
        .iter()
        .find(|index| index.physical_key() == object_key)
    {
        return Some((
            RemoteCatalogObjectKindV1::Index,
            CatalogFactObjectIdentityV1::from_bound(index.object())?,
        ));
    }
    if let Some(reservation) = corpus
        .reservations
        .iter()
        .find(|reservation| reservation.object_key() == object_key)
    {
        return Some((
            RemoteCatalogObjectKindV1::Reservation,
            CatalogFactObjectIdentityV1::from_bound(reservation.object())?,
        ));
    }
    for manifest in &corpus.manifests {
        let source = corpus
            .index_objects
            .get(manifest.source_index_ordinal)?
            .committed()?;
        let key = format!(
            "{}/manifests/{}",
            corpus.remote_prefix(),
            source.current().manifest_hash()
        );
        if key == object_key {
            return Some((
                RemoteCatalogObjectKindV1::Manifest,
                CatalogFactObjectIdentityV1::from_bound(manifest.manifest.object())?,
            ));
        }
    }
    None
}

fn catalog_named_object_max_bytes_v1(kind: RemoteCatalogObjectKindV1) -> u64 {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    match kind {
        RemoteCatalogObjectKindV1::Index => remote.max_index_object_bytes(),
        RemoteCatalogObjectKindV1::Reservation => remote.max_reservation_object_bytes(),
        RemoteCatalogObjectKindV1::Manifest => remote.max_manifest_object_bytes(),
    }
}

fn mutation_inputs_match_corpus_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
) -> bool {
    prerequisites.context == CatalogAuthorityContextV1::from_corpus(corpus)
        && prerequisites.sequence == corpus.closure.catalog_sequence
        && prerequisites.head_revision == corpus.closure.head_revision
}

/// Exact missing-object observation tied to the same accessor, storage
/// authority, semantic predecessor, and held control acquisition as one
/// publication attempt.
///
/// The witness borrows the non-cloneable prerequisite capability. It is
/// re-read and consumed during authoritative preparation, so an old absence
/// observation cannot be replayed after the key appears.
pub(crate) struct ProvenCatalogObjectAbsenceV1<'attempt, 'storage> {
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    predecessor_head_revision: [u8; 32],
}

/// Read one exact proposed create key through the publication's retained
/// accessor. No LIST result or caller assertion can construct this witness.
pub(crate) async fn prove_catalog_object_absence_v1<'attempt, 'storage>(
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
) -> AnyhowResult<ProvenCatalogObjectAbsenceV1<'attempt, 'storage>> {
    require_held_control_lease_live_v1(&prerequisites.control_guard)
        .map_err(|error| anyhow::anyhow!("catalog control lease is not live: {error:?}"))?;
    anyhow::ensure!(
        mutation_inputs_match_corpus_v1(prerequisites, corpus),
        "catalog absence proof does not match the exact semantic predecessor"
    );
    anyhow::ensure!(
        validate_catalog_object_route_v1(&prerequisites.context.remote_prefix, kind, &object_key),
        "catalog absence proof key has the wrong route for its kind"
    );
    anyhow::ensure!(
        corpus_object_fact_v1(corpus, &object_key).is_none(),
        "catalog absence proof key is already a predecessor member"
    );
    anyhow::ensure!(
        read_raw_object_snapshot_v1(
            prerequisites.storage_authority.operator,
            &object_key,
            catalog_named_object_max_bytes_v1(kind),
        )
        .await?
        .is_none(),
        "catalog create key is not absent"
    );
    Ok(ProvenCatalogObjectAbsenceV1 {
        prerequisites,
        kind,
        object_key,
        predecessor_head_revision: prerequisites.head_revision,
    })
}

#[derive(Debug)]
enum TypedCatalogSuccessorSemanticsV1 {
    Index(CatalogValidatedIndexPayloadV1),
    Reservation(CatalogValidatedReservationPayloadV1),
    Manifest,
}

/// Exact successor bytes parsed into their catalog kind before they can enter
/// a fact-bound mutation. Fields are private so a raw byte vector cannot
/// masquerade as a typed successor.
pub(crate) struct TypedCatalogSuccessorPayloadV1 {
    context: CatalogAuthorityContextV1,
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    raw_bytes: Vec<u8>,
    raw_bytes_len: NonZeroU64,
    raw_blake3: [u8; 32],
    semantics: TypedCatalogSuccessorSemanticsV1,
}

impl std::fmt::Debug for TypedCatalogSuccessorPayloadV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypedCatalogSuccessorPayloadV1")
            .field("kind", &self.kind)
            .field("object_key", &self.object_key)
            .field("raw_bytes_len", &self.raw_bytes_len)
            .finish_non_exhaustive()
    }
}

/// Parse one proposed successor through the strict registered-root validator
/// for its kind. Manifest reference closure is completed only after every
/// mutation has been applied to the in-memory successor catalog.
pub(crate) fn type_catalog_successor_payload_v1(
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    raw_bytes: Vec<u8>,
) -> Result<TypedCatalogSuccessorPayloadV1, CatalogPublicationContractErrorV1> {
    let context = CatalogAuthorityContextV1::from_corpus(corpus);
    if !validate_catalog_object_route_v1(&context.remote_prefix, kind, &object_key) {
        return Err(CatalogPublicationContractErrorV1::InvalidSuccessorPayload);
    }
    let raw_bytes_len = NonZeroU64::new(
        u64::try_from(raw_bytes.len())
            .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorPayload)?,
    )
    .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorPayload)?;
    if !validate_entry_size_v1(kind, raw_bytes_len.get()) {
        return Err(CatalogPublicationContractErrorV1::InvalidSuccessorPayload);
    }
    let semantics = match kind {
        RemoteCatalogObjectKindV1::Index => TypedCatalogSuccessorSemanticsV1::Index(
            validate_catalog_index_payload_v1(&context.remote_prefix, &object_key, &raw_bytes)
                .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorPayload)?,
        ),
        RemoteCatalogObjectKindV1::Reservation => TypedCatalogSuccessorSemanticsV1::Reservation(
            validate_catalog_reservation_payload_v1(
                &context.remote_prefix,
                &object_key,
                &raw_bytes,
            )
            .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorPayload)?,
        ),
        RemoteCatalogObjectKindV1::Manifest => {
            let manifest_prefix = format!("{}/manifests/", context.remote_prefix);
            let object_id = object_key
                .strip_prefix(&manifest_prefix)
                .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorPayload)?;
            if crate::index_entry::manifest_object_id(&raw_bytes) != object_id {
                return Err(CatalogPublicationContractErrorV1::InvalidSuccessorPayload);
            }
            TypedCatalogSuccessorSemanticsV1::Manifest
        }
    };
    Ok(TypedCatalogSuccessorPayloadV1 {
        context,
        kind,
        object_key,
        raw_blake3: *blake3::hash(&raw_bytes).as_bytes(),
        raw_bytes,
        raw_bytes_len,
        semantics,
    })
}

enum FactBoundCatalogMutationPredecessorV1<'attempt, 'storage> {
    Absent(ProvenCatalogObjectAbsenceV1<'attempt, 'storage>),
    Present(CatalogFactObjectIdentityV1),
}

/// One final-key mutation whose before fact comes only from the exact semantic
/// corpus or a same-attempt missing-object witness.
pub(crate) struct FactBoundCatalogMutationV1<'attempt, 'storage> {
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    predecessor: FactBoundCatalogMutationPredecessorV1<'attempt, 'storage>,
    successor: TypedCatalogSuccessorPayloadV1,
}

impl std::fmt::Debug for FactBoundCatalogMutationV1<'_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FactBoundCatalogMutationV1")
            .field("kind", &self.successor.kind)
            .field("object_key", &self.successor.object_key)
            .finish_non_exhaustive()
    }
}

/// Derive replacement predecessor identity and binding exclusively from the
/// semantic corpus. Callers provide only the already-typed successor.
pub(crate) fn fact_bind_catalog_replacement_v1<'attempt, 'storage>(
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    successor: TypedCatalogSuccessorPayloadV1,
) -> Result<FactBoundCatalogMutationV1<'attempt, 'storage>, CatalogPublicationContractErrorV1> {
    if !mutation_inputs_match_corpus_v1(prerequisites, corpus)
        || successor.context != prerequisites.context
    {
        return Err(CatalogPublicationContractErrorV1::MutationCorpusMismatch);
    }
    let Some((kind, predecessor)) = corpus_object_fact_v1(corpus, &successor.object_key) else {
        return Err(CatalogPublicationContractErrorV1::MutationPredecessorMissing);
    };
    if kind != successor.kind {
        return Err(CatalogPublicationContractErrorV1::MutationPredecessorKindMismatch);
    }
    if !binding_has_usable_etag_v1(&binding_wire_v1(&predecessor.binding)) {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::ObjectBinding,
        ));
    }
    Ok(FactBoundCatalogMutationV1 {
        prerequisites,
        predecessor: FactBoundCatalogMutationPredecessorV1::Present(predecessor),
        successor,
    })
}

/// Bind a create to one consumed same-attempt absence witness.
pub(crate) fn fact_bind_catalog_create_v1<'attempt, 'storage>(
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    absence: ProvenCatalogObjectAbsenceV1<'attempt, 'storage>,
    successor: TypedCatalogSuccessorPayloadV1,
) -> Result<FactBoundCatalogMutationV1<'attempt, 'storage>, CatalogPublicationContractErrorV1> {
    if !mutation_inputs_match_corpus_v1(prerequisites, corpus)
        || successor.context != prerequisites.context
        || !std::ptr::eq(absence.prerequisites, prerequisites)
        || absence.predecessor_head_revision != prerequisites.head_revision
        || absence.kind != successor.kind
        || absence.object_key != successor.object_key
    {
        return Err(CatalogPublicationContractErrorV1::MutationAbsenceMismatch);
    }
    if corpus_object_fact_v1(corpus, &successor.object_key).is_some() {
        return Err(CatalogPublicationContractErrorV1::MutationAbsenceStale);
    }
    Ok(FactBoundCatalogMutationV1 {
        prerequisites,
        predecessor: FactBoundCatalogMutationPredecessorV1::Absent(absence),
        successor,
    })
}

#[derive(Clone, Debug)]
struct CatalogSemanticIndexV1 {
    logical_path: String,
    role: PortableNamespaceRole,
    origins: RemoteNamespaceClaimOriginsV1,
    committed: Option<CatalogManifestReferenceV1>,
    raw_bytes_len: u64,
}

#[derive(Clone, Debug)]
struct CatalogSemanticReservationV1 {
    exact_path: String,
    folded_path: String,
    role: PortableNamespaceRole,
    raw_bytes_len: u64,
}

enum CatalogSemanticObjectV1<'a> {
    Index(CatalogSemanticIndexV1),
    Reservation(CatalogSemanticReservationV1),
    ExistingManifest {
        manifest: &'a crate::registered_reconcile::StrictRemoteManifestV1,
        raw_bytes_len: u64,
    },
    SuccessorManifest(&'a [u8]),
}

impl CatalogSemanticObjectV1<'_> {
    fn raw_bytes_len(&self) -> Option<u64> {
        match self {
            Self::Index(index) => Some(index.raw_bytes_len),
            Self::Reservation(reservation) => Some(reservation.raw_bytes_len),
            Self::ExistingManifest { raw_bytes_len, .. } => Some(*raw_bytes_len),
            Self::SuccessorManifest(raw_bytes) => u64::try_from(raw_bytes.len()).ok(),
        }
    }
}

fn semantic_index_from_predecessor_v1(
    remote_prefix: &str,
    index: &super::SemanticallyBoundCatalogIndexObjectV1,
) -> CatalogSemanticIndexV1 {
    CatalogSemanticIndexV1 {
        logical_path: index.logical_path().to_owned(),
        role: index.role(),
        origins: index.claim_origin(),
        committed: index.committed().map(|committed| {
            CatalogManifestReferenceV1::new(
                remote_prefix,
                committed.rel_path(),
                committed.current(),
            )
        }),
        raw_bytes_len: index.object().raw_bytes_len(),
    }
}

fn semantic_index_from_successor_v1(
    remote_prefix: &str,
    index: &CatalogValidatedIndexPayloadV1,
    raw_bytes_len: u64,
) -> CatalogSemanticIndexV1 {
    CatalogSemanticIndexV1 {
        logical_path: index.logical_path().to_owned(),
        role: index.role(),
        origins: match index.state() {
            CatalogValidatedIndexStateV1::Current => RemoteNamespaceClaimOriginsV1::CURRENT,
            CatalogValidatedIndexStateV1::Historical => RemoteNamespaceClaimOriginsV1::HISTORICAL,
        },
        committed: index.committed().map(|committed| {
            CatalogManifestReferenceV1::new(remote_prefix, index.logical_path(), committed)
        }),
        raw_bytes_len,
    }
}

fn validate_complete_successor_semantic_closure_v1<'a>(
    corpus: &'a SemanticallyBoundRemoteCatalogCorpusV1,
    mutations: &'a [FactBoundCatalogMutationV1<'_, '_>],
) -> Result<(), CatalogPublicationContractErrorV1> {
    let remote_prefix = corpus.remote_prefix();
    let mut objects = BTreeMap::<String, CatalogSemanticObjectV1<'a>>::new();
    for index in &corpus.index_objects {
        if objects
            .insert(
                index.physical_key().to_owned(),
                CatalogSemanticObjectV1::Index(semantic_index_from_predecessor_v1(
                    remote_prefix,
                    index,
                )),
            )
            .is_some()
        {
            return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
        }
    }
    for reservation in &corpus.reservations {
        if objects
            .insert(
                reservation.object_key().to_owned(),
                CatalogSemanticObjectV1::Reservation(CatalogSemanticReservationV1 {
                    exact_path: reservation.exact_path().to_owned(),
                    folded_path: reservation.folded_path().to_owned(),
                    role: reservation.role(),
                    raw_bytes_len: reservation.object().raw_bytes_len(),
                }),
            )
            .is_some()
        {
            return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
        }
    }
    for manifest in &corpus.manifests {
        let source = corpus
            .index_objects
            .get(manifest.source_index_ordinal)
            .and_then(super::SemanticallyBoundCatalogIndexObjectV1::committed)
            .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
        let object_key = format!(
            "{remote_prefix}/manifests/{}",
            source.current().manifest_hash()
        );
        if objects
            .insert(
                object_key,
                CatalogSemanticObjectV1::ExistingManifest {
                    manifest: &manifest.manifest,
                    raw_bytes_len: manifest.manifest.object().raw_bytes_len(),
                },
            )
            .is_some()
        {
            return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
        }
    }

    let mut previous_key: Option<&str> = None;
    for mutation in mutations {
        if previous_key.is_some_and(|previous| previous >= mutation.successor.object_key.as_str()) {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::Order,
            ));
        }
        previous_key = Some(&mutation.successor.object_key);
        match &mutation.predecessor {
            FactBoundCatalogMutationPredecessorV1::Absent(_) => {
                if objects.contains_key(&mutation.successor.object_key) {
                    return Err(CatalogPublicationContractErrorV1::MutationAbsenceStale);
                }
            }
            FactBoundCatalogMutationPredecessorV1::Present(_) => {
                if !objects.contains_key(&mutation.successor.object_key) {
                    return Err(CatalogPublicationContractErrorV1::MutationPredecessorMissing);
                }
            }
        }
        let semantic = match &mutation.successor.semantics {
            TypedCatalogSuccessorSemanticsV1::Index(index) => {
                CatalogSemanticObjectV1::Index(semantic_index_from_successor_v1(
                    remote_prefix,
                    index,
                    mutation.successor.raw_bytes_len.get(),
                ))
            }
            TypedCatalogSuccessorSemanticsV1::Reservation(reservation) => {
                CatalogSemanticObjectV1::Reservation(CatalogSemanticReservationV1 {
                    exact_path: reservation.exact_path().to_owned(),
                    folded_path: reservation.folded_path().to_owned(),
                    role: reservation.role(),
                    raw_bytes_len: mutation.successor.raw_bytes_len.get(),
                })
            }
            TypedCatalogSuccessorSemanticsV1::Manifest => {
                CatalogSemanticObjectV1::SuccessorManifest(&mutation.successor.raw_bytes)
            }
        };
        objects.insert(mutation.successor.object_key.clone(), semantic);
    }

    let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let object_count = u64::try_from(objects.len())
        .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
    if object_count > contract.max_catalog_entries() {
        return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
    }
    let mut object_key_bytes = 0_u64;
    let mut object_body_bytes = 0_u64;
    for (object_key, object) in &objects {
        object_key_bytes = object_key_bytes
            .checked_add(
                u64::try_from(object_key.len())
                    .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?,
            )
            .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
        object_body_bytes = object_body_bytes
            .checked_add(
                object
                    .raw_bytes_len()
                    .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?,
            )
            .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
    }
    if object_key_bytes > contract.max_catalog_entry_key_bytes()
        || object_body_bytes > contract.max_bound_object_bytes_per_pass()
    {
        return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
    }
    let mut claims = RemoteNamespaceClaimAccumulatorV1::new(contract);
    let mut references = BTreeMap::<String, Vec<CatalogManifestReferenceV1>>::new();
    let mut manifest_count = 0_usize;
    for object in objects.values() {
        match object {
            CatalogSemanticObjectV1::Index(index) => {
                if index.role == PortableNamespaceRole::Directory
                    && index.origins == RemoteNamespaceClaimOriginsV1::CURRENT
                    && Blacklist::default()
                        .check_fixed_ingress_path_components(std::path::Path::new(
                            &index.logical_path,
                        ))
                        .is_some()
                {
                    return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
                }
                claims
                    .observe_path(&index.logical_path, index.role, index.origins)
                    .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
                if let Some(reference) = &index.committed {
                    let manifest_key =
                        format!("{remote_prefix}/manifests/{}", reference.manifest_hash());
                    references
                        .entry(manifest_key)
                        .or_default()
                        .push(reference.clone());
                }
            }
            CatalogSemanticObjectV1::Reservation(reservation) => {
                let expected = crate::index_entry::portable_casefold_path(&reservation.exact_path)
                    .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
                if expected != reservation.folded_path {
                    return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
                }
                claims
                    .observe_path(
                        &reservation.exact_path,
                        reservation.role,
                        RemoteNamespaceClaimOriginsV1::RESERVATION,
                    )
                    .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
            }
            CatalogSemanticObjectV1::ExistingManifest { .. }
            | CatalogSemanticObjectV1::SuccessorManifest(_) => {
                manifest_count = manifest_count
                    .checked_add(1)
                    .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
            }
        }
    }
    if references.len() != manifest_count {
        return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure);
    }
    for (manifest_key, manifest_references) in &references {
        let manifest = objects
            .get(manifest_key)
            .ok_or(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
        match manifest {
            CatalogSemanticObjectV1::ExistingManifest { manifest, .. } => {
                validate_bound_catalog_manifest_references_v1(
                    manifest_key,
                    manifest,
                    manifest_references,
                )
                .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
            }
            CatalogSemanticObjectV1::SuccessorManifest(raw_bytes) => {
                validate_catalog_manifest_payload_v1(manifest_key, raw_bytes, manifest_references)
                    .map_err(|_| CatalogPublicationContractErrorV1::InvalidSuccessorClosure)?;
            }
            CatalogSemanticObjectV1::Index(_) | CatalogSemanticObjectV1::Reservation(_) => {
                return Err(CatalogPublicationContractErrorV1::InvalidSuccessorClosure)
            }
        }
    }
    let _retained_claims = claims.into_retained_claims();
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogSuccessorPayloadReferenceWireV1 {
    object_id: String,
    raw_bytes_len: u64,
    raw_blake3: String,
    binding: RemoteCatalogObjectBindingWireV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogFactBoundMutationWireV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    operation: RemoteCatalogMutationOperationDraftWireV1,
    predecessor: RemoteCatalogMutationPredecessorDraftWireV1,
    successor_payload: RemoteCatalogSuccessorPayloadReferenceWireV1,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteCatalogMutationJournalWireV1 {
    version: u32,
    context: RemoteCatalogContextWireV1,
    catalog_sequence: u64,
    publication_nonce: String,
    parent_head_revision: String,
    mutation_count: u64,
    mutation_key_bytes: u64,
    mutations: Vec<RemoteCatalogFactBoundMutationWireV1>,
}

struct BoundCatalogSuccessorPayloadV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    object_id: [u8; 32],
    raw_bytes: Vec<u8>,
    raw_bytes_len: NonZeroU64,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

/// Complete canonical authoritative journal with every successor bound to an
/// immutable absent-only payload object. Publishing the journal remains a
/// separate exact-reread step; this type cannot enter a visible fence.
pub(crate) struct PreparedAuthoritativeCatalogMutationJournalV1 {
    context: CatalogAuthorityContextV1,
    storage_authority_fingerprint: [u8; 32],
    catalog_sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
    object_id: [u8; 32],
    raw_bytes: Vec<u8>,
    raw_bytes_len: NonZeroU64,
    successor_payloads: Vec<BoundCatalogSuccessorPayloadV1>,
}

impl std::fmt::Debug for PreparedAuthoritativeCatalogMutationJournalV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAuthoritativeCatalogMutationJournalV1")
            .field("catalog_sequence", &self.catalog_sequence)
            .field("raw_bytes_len", &self.raw_bytes_len)
            .field("successor_payload_count", &self.successor_payloads.len())
            .finish_non_exhaustive()
    }
}

fn validate_authoritative_catalog_mutation_journal_bytes_v1(
    raw_bytes: &[u8],
    expected_context: &CatalogAuthorityContextV1,
    expected_sequence: NonZeroU64,
    expected_publication_nonce: [u8; 32],
    expected_parent_head_revision: [u8; 32],
) -> Result<RemoteCatalogMutationJournalWireV1, CatalogPublicationContractErrorV1> {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let raw_bytes_len = u64::try_from(raw_bytes.len())
        .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Bytes))?;
    if raw_bytes_len == 0 || raw_bytes_len > remote.max_catalog_page_object_bytes() {
        return Err(mutation_journal_resource(
            CatalogMutationJournalResourceV1::Bytes,
        ));
    }
    let wire =
        serde_json::from_slice::<RemoteCatalogMutationJournalWireV1>(raw_bytes).map_err(|_| {
            invalid_mutation_journal(InvalidCatalogMutationJournalReasonV1::CanonicalEncoding)
        })?;
    if serde_json::to_vec(&wire).ok().as_deref() != Some(raw_bytes)
        || wire.version != CATALOG_MUTATION_JOURNAL_SCHEMA_VERSION_V1
    {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::CanonicalEncoding,
        ));
    }
    if wire.context != expected_context.to_wire() {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Context,
        ));
    }
    if wire.catalog_sequence != expected_sequence.get()
        || super::parse_lower_hex_32(&wire.publication_nonce) != Some(expected_publication_nonce)
        || super::parse_lower_hex_32(&wire.parent_head_revision)
            != Some(expected_parent_head_revision)
        || expected_publication_nonce == [0; 32]
        || expected_parent_head_revision == [0; 32]
    {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Lineage,
        ));
    }
    let mutation_count = u64::try_from(wire.mutations.len())
        .map_err(|_| mutation_journal_resource(CatalogMutationJournalResourceV1::Mutations))?;
    if mutation_count > remote.max_catalog_entries_per_page() {
        return Err(mutation_journal_resource(
            CatalogMutationJournalResourceV1::Mutations,
        ));
    }
    if wire.mutation_count != mutation_count {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Totals,
        ));
    }
    let mut key_bytes = 0_u64;
    let mut previous_key: Option<&str> = None;
    for mutation in &wire.mutations {
        if previous_key.is_some_and(|previous| previous >= mutation.object_key.as_str()) {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::Order,
            ));
        }
        previous_key = Some(&mutation.object_key);
        key_bytes = key_bytes
            .checked_add(u64::try_from(mutation.object_key.len()).map_err(|_| {
                mutation_journal_resource(CatalogMutationJournalResourceV1::KeyBytes)
            })?)
            .ok_or_else(|| mutation_journal_resource(CatalogMutationJournalResourceV1::KeyBytes))?;
        if key_bytes > max_catalog_mutation_draft_key_bytes_v1() {
            return Err(mutation_journal_resource(
                CatalogMutationJournalResourceV1::KeyBytes,
            ));
        }
        let successor_object_id = super::parse_lower_hex_32(&mutation.successor_payload.object_id);
        let successor_raw_blake3 =
            super::parse_lower_hex_32(&mutation.successor_payload.raw_blake3);
        if !validate_catalog_object_route_v1(
            &expected_context.remote_prefix,
            mutation.kind,
            &mutation.object_key,
        ) || successor_object_id.is_none_or(|object_id| object_id == [0; 32])
            || successor_raw_blake3.is_none_or(|raw_blake3| raw_blake3 == [0; 32])
            || !validate_entry_size_v1(mutation.kind, mutation.successor_payload.raw_bytes_len)
            || super::validate_binding_wire_v1(&mutation.successor_payload.binding).is_none()
        {
            return Err(invalid_mutation_journal(
                InvalidCatalogMutationJournalReasonV1::ObjectIdentity,
            ));
        }
        match (&mutation.operation, &mutation.predecessor) {
            (
                RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
                RemoteCatalogMutationPredecessorDraftWireV1::Absent,
            ) => {}
            (
                RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
                RemoteCatalogMutationPredecessorDraftWireV1::Present {
                    raw_bytes_len,
                    raw_blake3,
                    binding,
                },
            ) if super::parse_lower_hex_32(raw_blake3).is_some()
                && validate_entry_size_v1(mutation.kind, *raw_bytes_len)
                && super::validate_binding_wire_v1(binding).is_some()
                && binding_has_usable_etag_v1(binding) => {}
            _ => {
                return Err(invalid_mutation_journal(
                    InvalidCatalogMutationJournalReasonV1::Operation,
                ))
            }
        }
    }
    if wire.mutation_key_bytes != key_bytes {
        return Err(invalid_mutation_journal(
            InvalidCatalogMutationJournalReasonV1::Totals,
        ));
    }
    Ok(wire)
}

/// Exact immutable copy of the complete committed predecessor HEAD.
///
/// The object must be installed absent-only and byte-verified before the
/// mutable HEAD CAS. `object_id` is the domain-separated digest of the exact
/// committed HEAD bytes, so its fixed reference lets recovery find and verify
/// the previous catalog root at a canonical key without LIST. The binding is
/// tied to the exact storage authority used for HEAD. The only source-level
/// constructor is the absent-only, exact-read-bound publication primitive
/// below; this value still cannot acquire a live HEAD fence.
pub(crate) struct BoundArchivedCatalogHeadV1 {
    context: CatalogAuthorityContextV1,
    storage_authority_fingerprint: [u8; 32],
    predecessor_head_revision: [u8; 32],
    predecessor_head_bytes_blake3: [u8; 32],
    object_id: [u8; 32],
    raw_bytes_len: NonZeroU64,
    binding: RegisteredRootRemoteObjectBindingV1,
}

/// Exact immutable authoritative transaction journal written before the
/// visible fence.
///
/// Its only constructor consumes the prepared fact-bound journal above. The
/// diagnostic draft remains nominally separate and cannot convert into this
/// type. This still authorizes no namespace write, visible HEAD transition, or
/// recovery action.
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
    successor_payloads: Vec<BoundCatalogSuccessorPayloadV1>,
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

fn successor_payload_object_key_v1(
    context: &CatalogAuthorityContextV1,
    raw_bytes: &[u8],
) -> Option<String> {
    publication_object_key_v1(
        context,
        CATALOG_SUCCESSOR_PAYLOAD_OBJECT_SUFFIX_V1,
        &super::domain_object_id_v1(CATALOG_SUCCESSOR_PAYLOAD_OBJECT_DOMAIN_V1, raw_bytes),
    )
}

fn untrusted_mutation_journal_draft_object_key_v1(
    journal: &PublishedUntrustedCatalogMutationJournalDraftV1,
) -> Option<String> {
    publication_object_key_v1(
        &journal.context,
        UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_SUFFIX_V1,
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

fn registered_binding_from_raw_v1(
    binding: RawObjectReadBindingV1,
) -> RegisteredRootRemoteObjectBindingV1 {
    match binding {
        RawObjectReadBindingV1::Version { version, etag } => {
            RegisteredRootRemoteObjectBindingV1::Version { version, etag }
        }
        RawObjectReadBindingV1::Etag { etag } => RegisteredRootRemoteObjectBindingV1::Etag { etag },
    }
}

async fn publish_immutable_catalog_artifact_if_absent_exact_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
    suffix: &str,
    object_domain: &str,
    expected_bytes: &[u8],
    max_bytes: u64,
) -> AnyhowResult<([u8; 32], NonZeroU64, RegisteredRootRemoteObjectBindingV1)> {
    require_held_control_lease_live_v1(&prerequisites.control_guard)
        .map_err(|error| anyhow::anyhow!("catalog control lease is not live: {error:?}"))?;
    anyhow::ensure!(
        prerequisites
            .storage_authority
            .conditional_write_receipt
            .authorizes(
                prerequisites.storage_authority.operator,
                &prerequisites.context.remote_prefix,
            )?,
        "catalog artifact publication lost its exact accessor/prefix conditional-semantics authority"
    );
    let raw_bytes_len = u64::try_from(expected_bytes.len())
        .context("catalog publication artifact length does not fit u64")?;
    let raw_bytes_len =
        NonZeroU64::new(raw_bytes_len).context("catalog publication artifact must not be empty")?;
    anyhow::ensure!(
        raw_bytes_len.get() <= max_bytes,
        "catalog publication artifact exceeds its bound of {max_bytes} bytes"
    );
    let object_id = super::domain_object_id_v1(object_domain, expected_bytes);
    let object_key = publication_object_key_v1(&prerequisites.context, suffix, &object_id)
        .context(
            "catalog publication artifact key exceeds the registered-root storage-key bound",
        )?;
    let write_error = prerequisites
        .storage_authority
        .operator
        .write_with(&object_key, expected_bytes.to_vec())
        .if_not_exists(true)
        .await
        .err();

    let observed = match read_raw_object_snapshot_v1(
        prerequisites.storage_authority.operator,
        &object_key,
        max_bytes,
    )
    .await
    {
        Ok(Some(RawObjectReadV1::Bound(snapshot))) => snapshot,
        Ok(Some(RawObjectReadV1::Unbound)) => {
            anyhow::bail!(
                "catalog publication artifact exists without a usable exact storage binding: {object_key}"
            )
        }
        Ok(None) => {
            if let Some(error) = write_error {
                return Err(anyhow::anyhow!(error)).with_context(|| {
                    format!(
                        "atomically creating catalog publication artifact and proving its exact bytes: {object_key}"
                    )
                });
            }
            anyhow::bail!(
                "catalog publication artifact disappeared after absent-only creation: {object_key}"
            )
        }
        Err(read_error) => {
            let write_context = write_error
                .as_ref()
                .map(|error| format!("; absent-only write also returned: {error}"))
                .unwrap_or_default();
            return Err(read_error).with_context(|| {
                format!(
                    "proving exact catalog publication artifact after absent-only create{write_context}: {object_key}"
                )
            });
        }
    };
    anyhow::ensure!(
        observed.raw_bytes() == expected_bytes,
        "catalog publication artifact key contains different bytes: {object_key}"
    );
    anyhow::ensure!(
        observed.raw_bytes().len() == expected_bytes.len(),
        "catalog publication artifact length changed during exact rebind: {object_key}"
    );
    let (observed_bytes, observed_blake3, binding) = observed.into_parts();
    anyhow::ensure!(
        observed_blake3 == blake3::hash(expected_bytes)
            && super::domain_object_id_v1(object_domain, &observed_bytes) == object_id,
        "catalog publication artifact identity changed during exact rebind: {object_key}"
    );
    Ok((
        object_id,
        raw_bytes_len,
        registered_binding_from_raw_v1(binding),
    ))
}

/// Recheck absence freshness, reconstruct the complete successor semantics,
/// and publish every exact successor body into the immutable payload
/// namespace. No live namespace key or mutable HEAD is written here.
pub(crate) async fn prepare_authoritative_catalog_mutation_journal_v1<'attempt, 'storage>(
    prerequisites: &'attempt MatchedCatalogPublicationPrerequisitesV1<'storage>,
    corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
    publication_nonce: [u8; 32],
    mut mutations: Vec<FactBoundCatalogMutationV1<'attempt, 'storage>>,
) -> AnyhowResult<PreparedAuthoritativeCatalogMutationJournalV1> {
    require_held_control_lease_live_v1(&prerequisites.control_guard)
        .map_err(|error| anyhow::anyhow!("catalog control lease is not live: {error:?}"))?;
    anyhow::ensure!(
        mutation_inputs_match_corpus_v1(prerequisites, corpus),
        "authoritative mutation preparation does not match the semantic predecessor"
    );
    anyhow::ensure!(
        publication_nonce != [0; 32] && publication_nonce != prerequisites.publication_nonce,
        "authoritative mutation publication nonce is zero or reused"
    );
    let catalog_sequence = prerequisites
        .sequence
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .context("catalog sequence overflow")?;
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    anyhow::ensure!(
        u64::try_from(mutations.len()).ok() <= Some(remote.max_catalog_entries_per_page()),
        "authoritative mutation count exceeds the catalog page bound"
    );
    mutations
        .sort_unstable_by(|left, right| left.successor.object_key.cmp(&right.successor.object_key));
    let mut mutation_key_bytes = 0_u64;
    let mut previous_key: Option<&str> = None;
    for mutation in &mutations {
        anyhow::ensure!(
            std::ptr::eq(mutation.prerequisites, prerequisites)
                && mutation.successor.context == prerequisites.context,
            "fact-bound mutation belongs to a different publication acquisition"
        );
        anyhow::ensure!(
            previous_key.is_none_or(|previous| previous < mutation.successor.object_key.as_str()),
            "authoritative mutation keys are duplicated"
        );
        previous_key = Some(&mutation.successor.object_key);
        mutation_key_bytes = mutation_key_bytes
            .checked_add(
                u64::try_from(mutation.successor.object_key.len())
                    .context("authoritative mutation key length does not fit u64")?,
            )
            .context("authoritative mutation key-byte total overflow")?;
        anyhow::ensure!(
            mutation_key_bytes <= max_catalog_mutation_draft_key_bytes_v1(),
            "authoritative mutation key-byte total exceeds the bound"
        );
        match &mutation.predecessor {
            FactBoundCatalogMutationPredecessorV1::Absent(absence) => {
                anyhow::ensure!(
                    std::ptr::eq(absence.prerequisites, prerequisites)
                        && absence.kind == mutation.successor.kind
                        && absence.object_key == mutation.successor.object_key
                        && absence.predecessor_head_revision == prerequisites.head_revision,
                    "catalog create absence witness does not match the held publication"
                );
                anyhow::ensure!(
                    read_raw_object_snapshot_v1(
                        prerequisites.storage_authority.operator,
                        &absence.object_key,
                        catalog_named_object_max_bytes_v1(absence.kind),
                    )
                    .await?
                    .is_none(),
                    "catalog create absence witness is stale"
                );
            }
            FactBoundCatalogMutationPredecessorV1::Present(predecessor) => {
                let (kind, corpus_predecessor) =
                    corpus_object_fact_v1(corpus, &mutation.successor.object_key)
                        .context("fact-bound replacement disappeared from the semantic corpus")?;
                anyhow::ensure!(
                    kind == mutation.successor.kind && &corpus_predecessor == predecessor,
                    "fact-bound replacement predecessor does not exactly match corpus membership"
                );
            }
        }
    }

    validate_complete_successor_semantic_closure_v1(corpus, &mutations)
        .map_err(|error| anyhow::anyhow!("successor semantic closure is invalid: {error:?}"))?;

    let mut successor_payloads = Vec::new();
    successor_payloads
        .try_reserve(mutations.len())
        .context("reserving authoritative successor payload bindings")?;
    for mutation in &mutations {
        let (object_id, raw_bytes_len, binding) =
            publish_immutable_catalog_artifact_if_absent_exact_v1(
                prerequisites,
                CATALOG_SUCCESSOR_PAYLOAD_OBJECT_SUFFIX_V1,
                CATALOG_SUCCESSOR_PAYLOAD_OBJECT_DOMAIN_V1,
                &mutation.successor.raw_bytes,
                catalog_named_object_max_bytes_v1(mutation.successor.kind),
            )
            .await?;
        anyhow::ensure!(
            raw_bytes_len == mutation.successor.raw_bytes_len,
            "immutable successor payload length does not match its typed bytes"
        );
        successor_payloads.push(BoundCatalogSuccessorPayloadV1 {
            kind: mutation.successor.kind,
            object_key: mutation.successor.object_key.clone(),
            object_id,
            raw_bytes: mutation.successor.raw_bytes.clone(),
            raw_bytes_len,
            raw_blake3: mutation.successor.raw_blake3,
            binding,
        });
    }

    let wire_mutations = mutations
        .iter()
        .zip(&successor_payloads)
        .map(|(mutation, payload)| {
            let (operation, predecessor) = match &mutation.predecessor {
                FactBoundCatalogMutationPredecessorV1::Absent(_) => (
                    RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
                    RemoteCatalogMutationPredecessorDraftWireV1::Absent,
                ),
                FactBoundCatalogMutationPredecessorV1::Present(predecessor) => (
                    RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
                    RemoteCatalogMutationPredecessorDraftWireV1::Present {
                        raw_bytes_len: predecessor.raw_bytes_len.get(),
                        raw_blake3: lower_hex(&predecessor.raw_blake3),
                        binding: binding_wire_v1(&predecessor.binding),
                    },
                ),
            };
            RemoteCatalogFactBoundMutationWireV1 {
                kind: mutation.successor.kind,
                object_key: mutation.successor.object_key.clone(),
                operation,
                predecessor,
                successor_payload: RemoteCatalogSuccessorPayloadReferenceWireV1 {
                    object_id: lower_hex(&payload.object_id),
                    raw_bytes_len: payload.raw_bytes_len.get(),
                    raw_blake3: lower_hex(&payload.raw_blake3),
                    binding: binding_wire_v1(&payload.binding),
                },
            }
        })
        .collect::<Vec<_>>();
    let wire = RemoteCatalogMutationJournalWireV1 {
        version: CATALOG_MUTATION_JOURNAL_SCHEMA_VERSION_V1,
        context: prerequisites.context.to_wire(),
        catalog_sequence: catalog_sequence.get(),
        publication_nonce: lower_hex(&publication_nonce),
        parent_head_revision: lower_hex(&prerequisites.head_revision),
        mutation_count: u64::try_from(wire_mutations.len())
            .context("authoritative mutation count does not fit u64")?,
        mutation_key_bytes,
        mutations: wire_mutations,
    };
    let raw_bytes =
        serde_json::to_vec(&wire).context("serializing authoritative catalog mutation journal")?;
    validate_authoritative_catalog_mutation_journal_bytes_v1(
        &raw_bytes,
        &prerequisites.context,
        catalog_sequence,
        publication_nonce,
        prerequisites.head_revision,
    )
    .map_err(|error| anyhow::anyhow!("authoritative mutation journal is invalid: {error:?}"))?;
    let raw_bytes_len = NonZeroU64::new(
        u64::try_from(raw_bytes.len())
            .context("authoritative mutation journal length does not fit u64")?,
    )
    .context("authoritative mutation journal cannot be empty")?;
    anyhow::ensure!(
        raw_bytes_len.get() <= remote.max_catalog_page_object_bytes(),
        "authoritative mutation journal exceeds the catalog page bound"
    );
    Ok(PreparedAuthoritativeCatalogMutationJournalV1 {
        context: prerequisites.context.clone(),
        storage_authority_fingerprint: prerequisites.storage_authority.authority_fingerprint,
        catalog_sequence,
        publication_nonce,
        parent_head_revision: prerequisites.head_revision,
        object_id: super::domain_object_id_v1(MUTATION_JOURNAL_OBJECT_DOMAIN_V1, &raw_bytes),
        raw_bytes,
        raw_bytes_len,
        successor_payloads,
    })
}

/// Publish the canonical authoritative journal absent-only and bind its exact
/// reread. This is the only constructor for `BoundCatalogMutationJournalV1`.
pub(crate) async fn publish_authoritative_catalog_mutation_journal_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
    prepared: PreparedAuthoritativeCatalogMutationJournalV1,
) -> AnyhowResult<BoundCatalogMutationJournalV1> {
    anyhow::ensure!(
        prepared.context == prerequisites.context
            && prepared.storage_authority_fingerprint
                == prerequisites.storage_authority.authority_fingerprint
            && prepared.catalog_sequence.get()
                == prerequisites
                    .sequence
                    .get()
                    .checked_add(1)
                    .context("catalog sequence overflow")?
            && prepared.parent_head_revision == prerequisites.head_revision
            && prepared.publication_nonce != [0; 32]
            && prepared.publication_nonce != prerequisites.publication_nonce
            && prepared.object_id
                == super::domain_object_id_v1(
                    MUTATION_JOURNAL_OBJECT_DOMAIN_V1,
                    &prepared.raw_bytes,
                )
            && prepared.raw_bytes_len.get()
                == u64::try_from(prepared.raw_bytes.len())
                    .context("authoritative mutation journal length does not fit u64")?,
        "prepared authoritative journal does not match publication prerequisites"
    );
    let wire = validate_authoritative_catalog_mutation_journal_bytes_v1(
        &prepared.raw_bytes,
        &prerequisites.context,
        prepared.catalog_sequence,
        prepared.publication_nonce,
        prerequisites.head_revision,
    )
    .map_err(|error| anyhow::anyhow!("prepared authoritative journal is invalid: {error:?}"))?;
    anyhow::ensure!(
        wire.mutations.len() == prepared.successor_payloads.len()
            && wire.mutations.iter().zip(&prepared.successor_payloads).all(
                |(mutation, payload)| {
                    mutation.kind == payload.kind
                        && mutation.object_key == payload.object_key
                        && mutation.successor_payload.object_id == lower_hex(&payload.object_id)
                        && u64::try_from(payload.raw_bytes.len()).ok()
                            == Some(payload.raw_bytes_len.get())
                        && *blake3::hash(&payload.raw_bytes).as_bytes() == payload.raw_blake3
                        && mutation.successor_payload.raw_bytes_len == payload.raw_bytes_len.get()
                        && mutation.successor_payload.raw_blake3 == lower_hex(&payload.raw_blake3)
                        && mutation.successor_payload.binding == binding_wire_v1(&payload.binding)
                }
            ),
        "prepared authoritative journal lost an immutable successor binding"
    );
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let (object_id, raw_bytes_len, binding) =
        publish_immutable_catalog_artifact_if_absent_exact_v1(
            prerequisites,
            MUTATION_JOURNAL_OBJECT_SUFFIX_V1,
            MUTATION_JOURNAL_OBJECT_DOMAIN_V1,
            &prepared.raw_bytes,
            remote.max_catalog_page_object_bytes(),
        )
        .await?;
    anyhow::ensure!(
        object_id == prepared.object_id && raw_bytes_len == prepared.raw_bytes_len,
        "published authoritative journal identity changed during exact rebind"
    );
    Ok(BoundCatalogMutationJournalV1 {
        context: prepared.context,
        storage_authority_fingerprint: prepared.storage_authority_fingerprint,
        catalog_sequence: prepared.catalog_sequence,
        publication_nonce: prepared.publication_nonce,
        parent_head_revision: prepared.parent_head_revision,
        object_id,
        raw_bytes: prepared.raw_bytes,
        raw_bytes_len,
        binding,
        successor_payloads: prepared.successor_payloads,
    })
}

/// Publish and bind the complete predecessor HEAD at its deterministic archive
/// key. A collision is accepted only after an exact bounded identity-bound
/// reread proves byte equality; this function never overwrites or cleans up.
pub(crate) async fn publish_predecessor_head_archive_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
) -> AnyhowResult<BoundArchivedCatalogHeadV1> {
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let (object_id, raw_bytes_len, binding) =
        publish_immutable_catalog_artifact_if_absent_exact_v1(
            prerequisites,
            ARCHIVED_HEAD_OBJECT_SUFFIX_V1,
            ARCHIVED_HEAD_OBJECT_DOMAIN_V1,
            &prerequisites.committed_head_bytes,
            remote.max_catalog_head_object_bytes(),
        )
        .await?;
    Ok(BoundArchivedCatalogHeadV1 {
        context: prerequisites.context.clone(),
        storage_authority_fingerprint: prerequisites.storage_authority.authority_fingerprint,
        predecessor_head_revision: prerequisites.head_revision,
        predecessor_head_bytes_blake3: *blake3::hash(&prerequisites.committed_head_bytes)
            .as_bytes(),
        object_id,
        raw_bytes_len,
        binding,
    })
}

/// Publish one canonical bounded untrusted journal draft.
///
/// This proves only the artifact's exact bytes and storage binding. The return
/// type cannot satisfy the authoritative mutation-journal slot in a publishing
/// fence and does not authorize namespace mutation or recovery.
async fn publish_untrusted_catalog_mutation_journal_draft_v1(
    prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
    prepared: PreparedUntrustedCatalogMutationJournalDraftV1,
) -> AnyhowResult<PublishedUntrustedCatalogMutationJournalDraftV1> {
    anyhow::ensure!(
        prepared.context == prerequisites.context
            && prepared.catalog_sequence.get()
                == prerequisites
                    .sequence
                    .get()
                    .checked_add(1)
                    .context("catalog sequence overflow")?
            && prepared.parent_head_revision == prerequisites.head_revision
            && prepared.publication_nonce != [0; 32]
            && prepared.publication_nonce != prerequisites.publication_nonce
            && prepared.object_id
                == super::domain_object_id_v1(
                    UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_DOMAIN_V1,
                    &prepared.raw_bytes,
                )
            && prepared.raw_bytes_len.get()
                == u64::try_from(prepared.raw_bytes.len())
                    .context("catalog mutation journal draft length does not fit u64")?,
        "prepared catalog mutation journal draft does not match the exact publication prerequisites"
    );
    validate_catalog_mutation_journal_draft_bytes_v1(
        &prepared.raw_bytes,
        &prerequisites.context,
        prepared.catalog_sequence,
        prepared.publication_nonce,
        prerequisites.head_revision,
    )
    .map_err(|error| {
        anyhow::anyhow!("prepared catalog mutation journal draft is invalid: {error:?}")
    })?;
    let remote = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let (object_id, raw_bytes_len, binding) =
        publish_immutable_catalog_artifact_if_absent_exact_v1(
            prerequisites,
            UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_SUFFIX_V1,
            UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_DOMAIN_V1,
            &prepared.raw_bytes,
            remote.max_catalog_page_object_bytes(),
        )
        .await?;
    anyhow::ensure!(
        object_id == prepared.object_id && raw_bytes_len == prepared.raw_bytes_len,
        "published catalog mutation journal draft identity differs from its prepared bytes"
    );
    Ok(PublishedUntrustedCatalogMutationJournalDraftV1 {
        context: prepared.context,
        storage_authority_fingerprint: prerequisites.storage_authority.authority_fingerprint,
        catalog_sequence: prepared.catalog_sequence,
        publication_nonce: prepared.publication_nonce,
        parent_head_revision: prepared.parent_head_revision,
        object_id,
        raw_bytes: prepared.raw_bytes,
        raw_bytes_len,
        binding,
    })
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
    control_guard: HeldReadyCatalogControlGuardV1,
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
    require_held_control_lease_live_v1(&prerequisites.control_guard)?;
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
        control_guard: prerequisites.control_guard,
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

/// Untrusted proposal for the exact `Ready -> PublicationPending` control CAS.
/// It still carries no authority to write either control state or catalog
/// `HEAD`.
pub(crate) struct PreparedCatalogControlTransitionV1<'a> {
    publication: PreparedCatalogPublicationFenceV1<'a>,
    pending_revision: CatalogControlAuthorityRevisionV1,
    canonical_publishing_head_bytes: Vec<u8>,
    publishing_head_reservation_fingerprint: [u8; 32],
    canonical_pending_control_record_bytes: Vec<u8>,
    pending_control_record_fingerprint: [u8; 32],
}

/// Opaque external receipt proving that the exact predecessor `Ready` record
/// was atomically replaced by the exact `PublicationPending` proposal.
///
/// There is intentionally no production constructor in this checkpoint.
pub(crate) struct TrustedCatalogPublicationPendingReceiptV1 {
    control_binding: CatalogControlAcquisitionBindingV1,
    pending_revision: CatalogControlAuthorityRevisionV1,
    publishing_head_reservation_fingerprint: [u8; 32],
    pending_control_record_fingerprint: [u8; 32],
}

/// The only future input permitted to arm a visible publishing-HEAD CAS. This
/// checkpoint cannot construct one in production and cannot write `HEAD`.
pub(crate) struct BoundPendingCatalogControlV1<'a> {
    transition: PreparedCatalogControlTransitionV1<'a>,
    pending_receipt: TrustedCatalogPublicationPendingReceiptV1,
}

impl std::fmt::Debug for BoundPendingCatalogControlV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundPendingCatalogControlV1")
            .field(
                "remote_prefix",
                &self.transition.publication.context.remote_prefix,
            )
            .field("sequence", &self.transition.publication.sequence)
            .field(
                "control_generation",
                &self.transition.pending_revision.generation,
            )
            .finish_non_exhaustive()
    }
}

/// Opaque backend receipt for the future committed-HEAD -> publishing-HEAD
/// compare-and-swap. The receipt is not the writer capability: it must be
/// rebound to the exact visible bytes through the held accessor while the
/// retained control lease is still live.
///
/// There is deliberately no production constructor in this checkpoint.
pub(crate) struct TrustedCatalogPublishingHeadInstalledReceiptV1 {
    publishing_head_binding: RegisteredRootRemoteObjectBindingV1,
}

/// Non-forgeable permission to apply exactly the authoritative journal and no
/// caller-selected key or byte string.
///
/// The capability retains the pending control transition, exact visible
/// publishing-HEAD binding, storage authority, and live backend lease. It is
/// non-cloneable and has no plan/action conversion.
pub(crate) struct CatalogNamespaceMutationCapabilityV1<'a> {
    pending: BoundPendingCatalogControlV1<'a>,
    publishing_head_binding: RegisteredRootRemoteObjectBindingV1,
}

impl std::fmt::Debug for CatalogNamespaceMutationCapabilityV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogNamespaceMutationCapabilityV1")
            .field(
                "remote_prefix",
                &self.pending.transition.publication.context.remote_prefix,
            )
            .field("sequence", &self.pending.transition.publication.sequence)
            .finish_non_exhaustive()
    }
}

/// Failed capability binding that keeps the exact pending transition and its
/// retained lease owned for recovery instead of dropping authority on error.
pub(crate) struct CatalogNamespaceMutationCapabilityBindFailureV1<'a> {
    pending: BoundPendingCatalogControlV1<'a>,
    error: anyhow::Error,
}

impl std::fmt::Debug for CatalogNamespaceMutationCapabilityBindFailureV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogNamespaceMutationCapabilityBindFailureV1")
            .field("sequence", &self.pending.transition.publication.sequence)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for CatalogNamespaceMutationCapabilityBindFailureV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if formatter.alternate() {
            write!(formatter, "{:#}", self.error)
        } else {
            std::fmt::Display::fmt(&self.error, formatter)
        }
    }
}

impl std::error::Error for CatalogNamespaceMutationCapabilityBindFailureV1<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

struct AppliedCatalogNamespaceObjectV1 {
    kind: RemoteCatalogObjectKindV1,
    object_key: String,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

/// Terminal evidence that every canonical journal entry was observed at its
/// exact successor bytes in canonical order while the visible publishing HEAD
/// and retained backend lease remained live.
///
/// Future committed-HEAD finalization must consume this value; it cannot be
/// converted back into a reusable mutation capability.
pub(crate) struct AppliedCatalogNamespaceMutationsV1<'a> {
    capability: CatalogNamespaceMutationCapabilityV1<'a>,
    applied: Vec<AppliedCatalogNamespaceObjectV1>,
}

impl std::fmt::Debug for AppliedCatalogNamespaceMutationsV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppliedCatalogNamespaceMutationsV1")
            .field(
                "sequence",
                &self.capability.pending.transition.publication.sequence,
            )
            .field("applied_count", &self.applied.len())
            .finish_non_exhaustive()
    }
}

/// Partial writer failure retaining the live capability and exact applied
/// prefix. Recovery can inspect/rebind the authoritative journal without
/// silently releasing the only held control acquisition.
pub(crate) struct FailedCatalogNamespaceMutationsV1<'a> {
    capability: CatalogNamespaceMutationCapabilityV1<'a>,
    applied: Vec<AppliedCatalogNamespaceObjectV1>,
    error: anyhow::Error,
}

impl std::fmt::Debug for FailedCatalogNamespaceMutationsV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FailedCatalogNamespaceMutationsV1")
            .field(
                "sequence",
                &self.capability.pending.transition.publication.sequence,
            )
            .field("applied_count", &self.applied.len())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for FailedCatalogNamespaceMutationsV1<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if formatter.alternate() {
            write!(formatter, "{:#}", self.error)
        } else {
            std::fmt::Display::fmt(&self.error, formatter)
        }
    }
}

impl std::error::Error for FailedCatalogNamespaceMutationsV1<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

fn wire_binding_etag_v1(binding: &RemoteCatalogObjectBindingWireV1) -> Option<&str> {
    binding.etag.as_deref().filter(|etag| !etag.is_empty())
}

async fn require_namespace_mutation_capability_live_v1(
    capability: &CatalogNamespaceMutationCapabilityV1<'_>,
) -> AnyhowResult<()> {
    let publication = &capability.pending.transition.publication;
    require_held_control_lease_live_v1(&publication.control_guard)
        .map_err(|error| anyhow::anyhow!("catalog control lease is not live: {error:?}"))?;
    anyhow::ensure!(
        publication
            .storage_authority
            .conditional_write_receipt
            .authorizes(
                publication.storage_authority.operator,
                &publication.context.remote_prefix,
            )?,
        "catalog namespace mutation lost its exact accessor/prefix conditional-semantics authority"
    );
    let expected_bytes = &capability
        .pending
        .transition
        .canonical_publishing_head_bytes;
    let head_key = super::catalog_head_key_v1(&publication.context.remote_prefix);
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_catalog_head_object_bytes();
    let observed =
        read_raw_object_snapshot_v1(publication.storage_authority.operator, &head_key, max_bytes)
            .await
            .with_context(|| format!("revalidating visible catalog publishing HEAD: {head_key}"))?;
    let Some(RawObjectReadV1::Bound(snapshot)) = observed else {
        anyhow::bail!("visible catalog publishing HEAD is missing or unbound: {head_key}");
    };
    let (raw_bytes, _, binding) = snapshot.into_parts();
    anyhow::ensure!(
        raw_bytes == *expected_bytes
            && registered_binding_from_raw_v1(binding) == capability.publishing_head_binding,
        "visible catalog publishing HEAD changed before namespace mutation: {head_key}"
    );
    Ok(())
}

/// Rebind the future HEAD-CAS receipt through the same accessor and retain the
/// live lease. This is the only constructor for the namespace mutation
/// capability, and this checkpoint cannot produce its trusted receipt in
/// production.
pub(crate) async fn bind_catalog_namespace_mutation_capability_v1<'a>(
    pending: BoundPendingCatalogControlV1<'a>,
    receipt: TrustedCatalogPublishingHeadInstalledReceiptV1,
) -> Result<
    CatalogNamespaceMutationCapabilityV1<'a>,
    CatalogNamespaceMutationCapabilityBindFailureV1<'a>,
> {
    let capability = CatalogNamespaceMutationCapabilityV1 {
        pending,
        publishing_head_binding: receipt.publishing_head_binding,
    };
    let result = async {
        anyhow::ensure!(
            super::validate_binding_wire_v1(&binding_wire_v1(&capability.publishing_head_binding))
                .is_some()
                && mutable_head_etag_v1(&capability.publishing_head_binding).is_some(),
            "publishing HEAD receipt has no usable exact mutable-object binding"
        );
        require_namespace_mutation_capability_live_v1(&capability)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "publishing HEAD receipt does not authorize namespace mutation: {error}"
                )
            })
    }
    .await;
    match result {
        Ok(()) => Ok(capability),
        Err(error) => {
            let CatalogNamespaceMutationCapabilityV1 { pending, .. } = capability;
            Err(CatalogNamespaceMutationCapabilityBindFailureV1 { pending, error })
        }
    }
}

async fn read_exact_catalog_successor_v1(
    capability: &CatalogNamespaceMutationCapabilityV1<'_>,
    kind: RemoteCatalogObjectKindV1,
    object_key: &str,
    expected_bytes: &[u8],
) -> AnyhowResult<RegisteredRootRemoteObjectBindingV1> {
    let publication = &capability.pending.transition.publication;
    let observed = read_raw_object_snapshot_v1(
        publication.storage_authority.operator,
        object_key,
        catalog_named_object_max_bytes_v1(kind),
    )
    .await
    .with_context(|| format!("reading exact catalog namespace successor: {object_key}"))?;
    let Some(RawObjectReadV1::Bound(snapshot)) = observed else {
        anyhow::bail!("catalog namespace successor is missing or unbound: {object_key}");
    };
    let (raw_bytes, raw_blake3, binding) = snapshot.into_parts();
    anyhow::ensure!(
        raw_bytes == expected_bytes && raw_blake3 == blake3::hash(expected_bytes),
        "catalog namespace successor differs from its authoritative journal: {object_key}"
    );
    let binding = registered_binding_from_raw_v1(binding);
    anyhow::ensure!(
        kind != RemoteCatalogObjectKindV1::Index || mutable_head_etag_v1(&binding).is_some(),
        "mutable catalog index successor has no usable ETag: {object_key}"
    );
    Ok(binding)
}

async fn apply_one_catalog_namespace_mutation_v1(
    capability: &CatalogNamespaceMutationCapabilityV1<'_>,
    mutation: &RemoteCatalogFactBoundMutationWireV1,
    payload: &BoundCatalogSuccessorPayloadV1,
) -> AnyhowResult<AppliedCatalogNamespaceObjectV1> {
    anyhow::ensure!(
        mutation.kind == payload.kind
            && mutation.object_key == payload.object_key
            && mutation.successor_payload.object_id == lower_hex(&payload.object_id)
            && mutation.successor_payload.raw_bytes_len == payload.raw_bytes_len.get()
            && mutation.successor_payload.raw_blake3 == lower_hex(&payload.raw_blake3)
            && mutation.successor_payload.binding == binding_wire_v1(&payload.binding)
            && u64::try_from(payload.raw_bytes.len()).ok() == Some(payload.raw_bytes_len.get())
            && *blake3::hash(&payload.raw_bytes).as_bytes() == payload.raw_blake3,
        "catalog namespace mutation lost its exact immutable successor payload"
    );
    let payload_key = successor_payload_object_key_v1(
        &capability.pending.transition.publication.context,
        &payload.raw_bytes,
    )
    .context("catalog successor payload key exceeds the storage-key bound")?;
    let immutable_payload = read_raw_object_snapshot_v1(
        capability
            .pending
            .transition
            .publication
            .storage_authority
            .operator,
        &payload_key,
        catalog_named_object_max_bytes_v1(payload.kind),
    )
    .await
    .with_context(|| format!("revalidating immutable catalog successor payload: {payload_key}"))?;
    let Some(RawObjectReadV1::Bound(immutable_payload)) = immutable_payload else {
        anyhow::bail!("immutable catalog successor payload is missing or unbound: {payload_key}");
    };
    let (immutable_bytes, immutable_blake3, immutable_binding) = immutable_payload.into_parts();
    anyhow::ensure!(
        immutable_bytes == payload.raw_bytes
            && immutable_blake3 == blake3::hash(&payload.raw_bytes)
            && registered_binding_from_raw_v1(immutable_binding) == payload.binding,
        "immutable catalog successor payload changed before namespace mutation: {payload_key}"
    );

    require_namespace_mutation_capability_live_v1(capability).await?;
    let operator = capability
        .pending
        .transition
        .publication
        .storage_authority
        .operator;
    let write_result = match (&mutation.operation, &mutation.predecessor) {
        (
            RemoteCatalogMutationOperationDraftWireV1::CreateIfAbsent,
            RemoteCatalogMutationPredecessorDraftWireV1::Absent,
        ) => {
            operator
                .write_with(&mutation.object_key, payload.raw_bytes.clone())
                .if_not_exists(true)
                .await
        }
        (
            RemoteCatalogMutationOperationDraftWireV1::ReplaceIfMatch,
            RemoteCatalogMutationPredecessorDraftWireV1::Present { binding, .. },
        ) => {
            let etag = wire_binding_etag_v1(binding)
                .context("authoritative replacement predecessor has no usable ETag")?;
            operator
                .write_with(&mutation.object_key, payload.raw_bytes.clone())
                .if_match(etag)
                .await
        }
        _ => anyhow::bail!("authoritative catalog journal contains an invalid operation"),
    };

    let rebound = read_exact_catalog_successor_v1(
        capability,
        mutation.kind,
        &mutation.object_key,
        &payload.raw_bytes,
    )
    .await;
    let binding = match (write_result, rebound) {
        (_, Ok(binding)) => binding,
        (Err(write_error), Err(read_error)) => {
            return Err(read_error).with_context(|| {
                format!(
                    "catalog namespace conditional write also failed ({write_error}): {}",
                    mutation.object_key
                )
            })
        }
        (Ok(_), Err(read_error)) => return Err(read_error),
    };
    require_namespace_mutation_capability_live_v1(capability).await?;
    Ok(AppliedCatalogNamespaceObjectV1 {
        kind: mutation.kind,
        object_key: mutation.object_key.clone(),
        raw_blake3: payload.raw_blake3,
        binding,
    })
}

/// Apply the complete authoritative journal in canonical order.
///
/// Callers cannot select a key, payload, operation, or predecessor. Creates
/// remain absent-only; replacements remain exact-ETag conditional. Exact
/// successor rereads make crash replay idempotent, while any different
/// collision, stale predecessor, lost publishing HEAD, or lost lease fails
/// closed. The returned terminal evidence retains the capability for future
/// committed-HEAD finalization.
pub(crate) async fn apply_authoritative_catalog_namespace_mutations_v1<'a>(
    capability: CatalogNamespaceMutationCapabilityV1<'a>,
) -> Result<AppliedCatalogNamespaceMutationsV1<'a>, FailedCatalogNamespaceMutationsV1<'a>> {
    let mut applied = Vec::new();
    if let Err(error) = require_namespace_mutation_capability_live_v1(&capability).await {
        return Err(FailedCatalogNamespaceMutationsV1 {
            capability,
            applied,
            error,
        });
    }

    let wire = {
        let publication = &capability.pending.transition.publication;
        let journal = &publication.mutation_journal;
        validate_authoritative_catalog_mutation_journal_bytes_v1(
            &journal.raw_bytes,
            &publication.context,
            publication.sequence,
            publication.publication_nonce,
            publication.parent_head_revision,
        )
        .map_err(|error| anyhow::anyhow!("authoritative mutation journal is invalid: {error:?}"))
        .and_then(|wire| {
            anyhow::ensure!(
                wire.mutations.len() == journal.successor_payloads.len(),
                "authoritative mutation journal lost its complete successor payload set"
            );
            Ok(wire)
        })
    };
    let wire = match wire {
        Ok(wire) => wire,
        Err(error) => {
            return Err(FailedCatalogNamespaceMutationsV1 {
                capability,
                applied,
                error,
            })
        }
    };
    if let Err(error) = applied
        .try_reserve(wire.mutations.len())
        .context("reserving applied catalog mutation evidence")
    {
        return Err(FailedCatalogNamespaceMutationsV1 {
            capability,
            applied,
            error,
        });
    }

    for ordinal in 0..wire.mutations.len() {
        let result = {
            let journal = &capability.pending.transition.publication.mutation_journal;
            apply_one_catalog_namespace_mutation_v1(
                &capability,
                &wire.mutations[ordinal],
                &journal.successor_payloads[ordinal],
            )
            .await
        };
        match result {
            Ok(evidence) => applied.push(evidence),
            Err(error) => {
                return Err(FailedCatalogNamespaceMutationsV1 {
                    capability,
                    applied,
                    error,
                })
            }
        }
    }
    let complete = applied.len() == wire.mutations.len()
        && applied
            .iter()
            .zip(&wire.mutations)
            .all(|(evidence, mutation)| {
                evidence.kind == mutation.kind
                    && evidence.object_key == mutation.object_key
                    && lower_hex(&evidence.raw_blake3) == mutation.successor_payload.raw_blake3
                    && super::validate_binding_wire_v1(&binding_wire_v1(&evidence.binding))
                        .is_some()
                    && (evidence.kind != RemoteCatalogObjectKindV1::Index
                        || mutable_head_etag_v1(&evidence.binding).is_some())
            });
    if !complete {
        return Err(FailedCatalogNamespaceMutationsV1 {
            capability,
            applied,
            error: anyhow::anyhow!(
                "applied catalog namespace evidence is incomplete or out of order"
            ),
        });
    }
    Ok(AppliedCatalogNamespaceMutationsV1 {
        capability,
        applied,
    })
}

/// Opaque future proof that the exact reserved successor became the canonical
/// committed `HEAD`. No production constructor exists until visible fencing,
/// fact-bound mutation, and committed-HEAD finalization are implemented.
pub(crate) struct BoundCommittedCatalogSuccessorV1 {
    control_guard: HeldReadyCatalogControlGuardV1,
    pending_revision: CatalogControlAuthorityRevisionV1,
    publishing_head_reservation_fingerprint: [u8; 32],
    pending_control_record_fingerprint: [u8; 32],
    context: CatalogAuthorityContextV1,
    sequence: NonZeroU64,
    publication_nonce: [u8; 32],
    parent_head_revision: [u8; 32],
    head_revision: [u8; 32],
    committed_head_bytes: Vec<u8>,
}

/// Opaque receipt for the external `PublicationPending -> Ready(n+1)` CAS.
/// It binds the exact committed successor and a strictly later control record.
/// There is intentionally no production constructor in this checkpoint.
pub(crate) struct TrustedCatalogHighWaterAdvanceReceiptV1 {
    control_binding: CatalogControlAcquisitionBindingV1,
    pending_revision: CatalogControlAuthorityRevisionV1,
    publishing_head_reservation_fingerprint: [u8; 32],
    pending_control_record_fingerprint: [u8; 32],
    successor: CatalogHighWaterPointV1,
    ready_revision: CatalogControlAuthorityRevisionV1,
    ready_control_record_fingerprint: [u8; 32],
}

/// Terminal proof that one exact successor advanced the monotonic high-water.
/// It intentionally cannot be converted back into a reusable guard; the next
/// publication must acquire a fresh exact-current external control guard.
pub(crate) struct AdvancedCatalogHighWaterV1 {
    context: CatalogAuthorityContextV1,
    bootstrap: CatalogBootstrapIdentityV1,
    successor: CatalogHighWaterPointV1,
    storage_authority_fingerprint: [u8; 32],
    control_authority_fingerprint: [u8; 32],
    ready_revision: CatalogControlAuthorityRevisionV1,
    ready_control_record_fingerprint: [u8; 32],
    retained_control_lease: RetainedCatalogControlLeaseV1,
}

impl std::fmt::Debug for AdvancedCatalogHighWaterV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdvancedCatalogHighWaterV1")
            .field("remote_prefix", &self.context.remote_prefix)
            .field("sequence", &self.successor.sequence)
            .field("control_generation", &self.ready_revision.generation)
            .field(
                "retained_control_lease_live",
                &self.retained_control_lease.is_live_v1(),
            )
            .finish_non_exhaustive()
    }
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

const CATALOG_CONTROL_RECORD_SCHEMA_VERSION_V1: u32 = 1;
const CATALOG_CONTROL_CONTRACT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-control-contract.b3v1";
const CATALOG_CONTROL_RECORD_DOMAIN_V1: &str = "tinyland.tcfs.remote-catalog-control-record.b3v1";

fn catalog_control_contract_fingerprint_v1() -> [u8; 32] {
    super::domain_object_id_v1(
        CATALOG_CONTROL_CONTRACT_DOMAIN_V1,
        b"ready-exact-current->publication-pending-exact-successor->ready-exact-current-v1",
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogControlBootstrapWireV1 {
    head_revision: String,
    publication_nonce: String,
    complete_corpus_attestation: String,
    writer_epoch: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogControlHeadPointWireV1 {
    catalog_sequence: u64,
    head_revision: String,
    publication_nonce: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogControlPendingStateWireV1 {
    ready_control_generation: u64,
    ready_control_revision_fingerprint: String,
    parent: CatalogControlHeadPointWireV1,
    successor_catalog_sequence: u64,
    successor_publication_nonce: String,
    publishing_head_reservation_fingerprint: String,
    predecessor_head_storage_binding_fingerprint: String,
    predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1,
    mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum CatalogControlStateWireV1 {
    Ready {
        current: CatalogControlHeadPointWireV1,
    },
    PublicationPending {
        pending: Box<CatalogControlPendingStateWireV1>,
    },
}

/// Canonical external control-record representation. Parsing this wire never
/// creates a trusted guard: only a future authenticated control backend may do
/// that. The serialized lease value is explicitly a non-secret correlation
/// fingerprint, never bearer authority.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogControlRecordWireV1 {
    version: u32,
    remote_prefix: String,
    context: RemoteCatalogContextWireV1,
    control_contract_fingerprint: String,
    control_authority_fingerprint: String,
    storage_authority_fingerprint: String,
    control_generation: u64,
    control_revision_fingerprint: String,
    lease_public_fingerprint: String,
    bootstrap: CatalogControlBootstrapWireV1,
    writer_epoch: String,
    control_state: CatalogControlStateWireV1,
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
    control_ready_generation: u64,
    control_ready_revision_fingerprint: String,
    control_pending_generation: u64,
    control_pending_revision_fingerprint: String,
    control_lease_public_fingerprint: String,
    writer_fence_authority_revision_fingerprint: String,
    writer_fence_lease_public_fingerprint: String,
    predecessor_head_storage_binding_fingerprint: String,
    predecessor_head_archive: RemoteCatalogPublicationObjectReferenceWireV1,
    mutation_journal: RemoteCatalogPublicationObjectReferenceWireV1,
}

fn predecessor_head_storage_binding_fingerprint_v1(etag: &str) -> [u8; 32] {
    super::domain_object_id_v1(PREDECESSOR_HEAD_STORAGE_BINDING_DOMAIN_V1, etag.as_bytes())
}

fn canonical_publishing_head_bytes_v1(
    successor: &PreparedCatalogPublicationFenceV1<'_>,
    pending_revision: CatalogControlAuthorityRevisionV1,
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
            &successor
                .control_guard
                .all_writers
                .control_binding
                .control_authority_fingerprint,
        ),
        writer_epoch: lower_hex(
            &successor
                .control_guard
                .all_writers
                .control_binding
                .bootstrap
                .writer_epoch,
        ),
        control_ready_generation: successor
            .control_guard
            .high_water
            .binding
            .ready_revision
            .generation
            .get(),
        control_ready_revision_fingerprint: lower_hex(
            &successor
                .control_guard
                .high_water
                .binding
                .ready_revision
                .fingerprint,
        ),
        control_pending_generation: pending_revision.generation.get(),
        control_pending_revision_fingerprint: lower_hex(&pending_revision.fingerprint),
        control_lease_public_fingerprint: lower_hex(
            &successor
                .control_guard
                .high_water
                .binding
                .lease_public_fingerprint
                .0,
        ),
        writer_fence_authority_revision_fingerprint: lower_hex(
            &successor
                .control_guard
                .all_writers
                .authority_revision_fingerprint,
        ),
        writer_fence_lease_public_fingerprint: lower_hex(
            &successor
                .control_guard
                .all_writers
                .lease_public_fingerprint
                .0,
        ),
        predecessor_head_storage_binding_fingerprint: lower_hex(
            &predecessor_head_storage_binding_fingerprint_v1(&successor.expected_parent_head_etag),
        ),
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

fn control_bootstrap_wire_v1(
    bootstrap: &CatalogBootstrapIdentityV1,
) -> CatalogControlBootstrapWireV1 {
    CatalogControlBootstrapWireV1 {
        head_revision: lower_hex(&bootstrap.head_revision),
        publication_nonce: lower_hex(&bootstrap.publication_nonce),
        complete_corpus_attestation: lower_hex(&bootstrap.complete_corpus_attestation),
        writer_epoch: lower_hex(&bootstrap.writer_epoch),
    }
}

fn control_head_point_wire_v1(point: CatalogHighWaterPointV1) -> CatalogControlHeadPointWireV1 {
    CatalogControlHeadPointWireV1 {
        catalog_sequence: point.sequence.get(),
        head_revision: lower_hex(&point.head_revision),
        publication_nonce: lower_hex(&point.publication_nonce),
    }
}

fn publication_reference_wire_v1(
    object_id: &[u8; 32],
    raw_bytes_len: NonZeroU64,
    binding: &RegisteredRootRemoteObjectBindingV1,
) -> RemoteCatalogPublicationObjectReferenceWireV1 {
    RemoteCatalogPublicationObjectReferenceWireV1 {
        object_id: lower_hex(object_id),
        raw_bytes_len: raw_bytes_len.get(),
        binding: binding_wire_v1(binding),
    }
}

fn canonical_pending_control_record_bytes_v1(
    publication: &PreparedCatalogPublicationFenceV1<'_>,
    pending_revision: CatalogControlAuthorityRevisionV1,
    publishing_head_reservation_fingerprint: [u8; 32],
) -> Vec<u8> {
    let binding = &publication.control_guard.high_water.binding;
    serde_json::to_vec(&CatalogControlRecordWireV1 {
        version: CATALOG_CONTROL_RECORD_SCHEMA_VERSION_V1,
        remote_prefix: binding.context.remote_prefix.clone(),
        context: binding.context.to_wire(),
        control_contract_fingerprint: lower_hex(&catalog_control_contract_fingerprint_v1()),
        control_authority_fingerprint: lower_hex(&binding.control_authority_fingerprint),
        storage_authority_fingerprint: lower_hex(&binding.storage_authority_fingerprint),
        control_generation: pending_revision.generation.get(),
        control_revision_fingerprint: lower_hex(&pending_revision.fingerprint),
        lease_public_fingerprint: lower_hex(&binding.lease_public_fingerprint.0),
        bootstrap: control_bootstrap_wire_v1(&binding.bootstrap),
        writer_epoch: lower_hex(&binding.bootstrap.writer_epoch),
        control_state: CatalogControlStateWireV1::PublicationPending {
            pending: Box::new(CatalogControlPendingStateWireV1 {
                ready_control_generation: binding.ready_revision.generation.get(),
                ready_control_revision_fingerprint: lower_hex(&binding.ready_revision.fingerprint),
                parent: control_head_point_wire_v1(binding.current),
                successor_catalog_sequence: publication.sequence.get(),
                successor_publication_nonce: lower_hex(&publication.publication_nonce),
                publishing_head_reservation_fingerprint: lower_hex(
                    &publishing_head_reservation_fingerprint,
                ),
                predecessor_head_storage_binding_fingerprint: lower_hex(
                    &predecessor_head_storage_binding_fingerprint_v1(
                        &publication.expected_parent_head_etag,
                    ),
                ),
                predecessor_head_archive: publication_reference_wire_v1(
                    &publication.predecessor_archive.object_id,
                    publication.predecessor_archive.raw_bytes_len,
                    &publication.predecessor_archive.binding,
                ),
                mutation_journal: publication_reference_wire_v1(
                    &publication.mutation_journal.object_id,
                    publication.mutation_journal.raw_bytes_len,
                    &publication.mutation_journal.binding,
                ),
            }),
        },
    })
    .expect("catalog control wire is infallibly serializable")
}

fn canonical_ready_control_record_bytes_v1(
    committed: &BoundCommittedCatalogSuccessorV1,
    ready_revision: CatalogControlAuthorityRevisionV1,
) -> Vec<u8> {
    let binding = &committed.control_guard.high_water.binding;
    serde_json::to_vec(&CatalogControlRecordWireV1 {
        version: CATALOG_CONTROL_RECORD_SCHEMA_VERSION_V1,
        remote_prefix: binding.context.remote_prefix.clone(),
        context: binding.context.to_wire(),
        control_contract_fingerprint: lower_hex(&catalog_control_contract_fingerprint_v1()),
        control_authority_fingerprint: lower_hex(&binding.control_authority_fingerprint),
        storage_authority_fingerprint: lower_hex(&binding.storage_authority_fingerprint),
        control_generation: ready_revision.generation.get(),
        control_revision_fingerprint: lower_hex(&ready_revision.fingerprint),
        lease_public_fingerprint: lower_hex(&binding.lease_public_fingerprint.0),
        bootstrap: control_bootstrap_wire_v1(&binding.bootstrap),
        writer_epoch: lower_hex(&binding.bootstrap.writer_epoch),
        control_state: CatalogControlStateWireV1::Ready {
            current: control_head_point_wire_v1(CatalogHighWaterPointV1 {
                sequence: committed.sequence,
                head_revision: committed.head_revision,
                publication_nonce: committed.publication_nonce,
            }),
        },
    })
    .expect("catalog control wire is infallibly serializable")
}

/// Prepare the exact external control transition without acquiring authority.
/// The future backend must CAS the authenticated predecessor `Ready` bytes to
/// these exact `PublicationPending` bytes before any catalog `HEAD` write.
pub(crate) fn prepare_catalog_control_transition_v1<'a>(
    publication: PreparedCatalogPublicationFenceV1<'a>,
    pending_revision_fingerprint: [u8; 32],
) -> Result<PreparedCatalogControlTransitionV1<'a>, CatalogPublicationContractErrorV1> {
    require_held_control_lease_live_v1(&publication.control_guard)?;
    let expected_successor_sequence = publication
        .control_guard
        .high_water
        .binding
        .current
        .sequence
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    if publication.control_guard.all_writers.control_binding
        != publication.control_guard.high_water.binding
        || publication.sequence != expected_successor_sequence
        || publication.parent_head_revision
            != publication
                .control_guard
                .high_water
                .binding
                .current
                .head_revision
        || publication.publication_nonce == [0; 32]
        || publication.publication_nonce
            == publication
                .control_guard
                .high_water
                .binding
                .current
                .publication_nonce
        || publication.bootstrap_head_revision
            != publication
                .control_guard
                .high_water
                .binding
                .bootstrap
                .head_revision
        || pending_revision_fingerprint == [0; 32]
        || pending_revision_fingerprint
            == publication
                .control_guard
                .high_water
                .binding
                .ready_revision
                .fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::ControlTransitionMismatch);
    }
    let pending_generation = publication
        .control_guard
        .high_water
        .binding
        .ready_revision
        .generation
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let pending_revision = CatalogControlAuthorityRevisionV1 {
        generation: pending_generation,
        fingerprint: pending_revision_fingerprint,
    };
    let canonical_publishing_head_bytes =
        canonical_publishing_head_bytes_v1(&publication, pending_revision);
    let publishing_head_bytes_len = u64::try_from(canonical_publishing_head_bytes.len())
        .map_err(|_| CatalogPublicationContractErrorV1::PublishingHeadTooLarge)?;
    if publishing_head_bytes_len
        > RegisteredRootPlanContractV1::strict_v1()
            .remote_contract()
            .max_catalog_head_object_bytes()
    {
        return Err(CatalogPublicationContractErrorV1::PublishingHeadTooLarge);
    }
    let publishing_head_reservation_fingerprint = super::domain_object_id_v1(
        PUBLISHING_HEAD_RESERVATION_DOMAIN_V1,
        &canonical_publishing_head_bytes,
    );
    let canonical_pending_control_record_bytes = canonical_pending_control_record_bytes_v1(
        &publication,
        pending_revision,
        publishing_head_reservation_fingerprint,
    );
    let pending_control_record_fingerprint = super::domain_object_id_v1(
        CATALOG_CONTROL_RECORD_DOMAIN_V1,
        &canonical_pending_control_record_bytes,
    );
    Ok(PreparedCatalogControlTransitionV1 {
        publication,
        pending_revision,
        canonical_publishing_head_bytes,
        publishing_head_reservation_fingerprint,
        canonical_pending_control_record_bytes,
        pending_control_record_fingerprint,
    })
}

/// Bind an externally authenticated `Ready -> PublicationPending` CAS receipt
/// to the exact proposed successor. Field equality is necessary but not a
/// production liveness proof; only the future backend may construct `receipt`.
pub(crate) fn match_catalog_publication_pending_receipt_v1<'a>(
    transition: PreparedCatalogControlTransitionV1<'a>,
    receipt: TrustedCatalogPublicationPendingReceiptV1,
) -> Result<BoundPendingCatalogControlV1<'a>, CatalogPublicationContractErrorV1> {
    require_held_control_lease_live_v1(&transition.publication.control_guard)?;
    let binding = &transition.publication.control_guard.high_water.binding;
    let expected_publishing_bytes =
        canonical_publishing_head_bytes_v1(&transition.publication, transition.pending_revision);
    let publishing_fingerprint = super::domain_object_id_v1(
        PUBLISHING_HEAD_RESERVATION_DOMAIN_V1,
        &expected_publishing_bytes,
    );
    let expected_control_bytes = canonical_pending_control_record_bytes_v1(
        &transition.publication,
        transition.pending_revision,
        publishing_fingerprint,
    );
    let control_fingerprint =
        super::domain_object_id_v1(CATALOG_CONTROL_RECORD_DOMAIN_V1, &expected_control_bytes);
    if receipt.control_binding != *binding
        || receipt.pending_revision != transition.pending_revision
        || receipt.publishing_head_reservation_fingerprint
            != transition.publishing_head_reservation_fingerprint
        || receipt.pending_control_record_fingerprint
            != transition.pending_control_record_fingerprint
        || transition.canonical_publishing_head_bytes != expected_publishing_bytes
        || transition.canonical_pending_control_record_bytes != expected_control_bytes
        || publishing_fingerprint != transition.publishing_head_reservation_fingerprint
        || control_fingerprint != transition.pending_control_record_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::ControlTransitionMismatch);
    }
    Ok(BoundPendingCatalogControlV1 {
        transition,
        pending_receipt: receipt,
    })
}

/// Bind an exact committed successor to the external monotonic
/// `PublicationPending -> Ready(n+1)` advance. This returns terminal evidence,
/// never a reusable exact-current guard or action capability.
pub(crate) fn match_catalog_high_water_advance_v1(
    committed: BoundCommittedCatalogSuccessorV1,
    receipt: TrustedCatalogHighWaterAdvanceReceiptV1,
) -> Result<AdvancedCatalogHighWaterV1, CatalogPublicationContractErrorV1> {
    require_held_control_lease_live_v1(&committed.control_guard)?;
    let control_binding = committed.control_guard.high_water.binding.clone();
    let expected_pending_generation = committed
        .control_guard
        .high_water
        .binding
        .ready_revision
        .generation
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let expected_successor_sequence = committed
        .control_guard
        .high_water
        .binding
        .current
        .sequence
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let expected_ready_generation = committed
        .pending_revision
        .generation
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(CatalogPublicationContractErrorV1::SequenceOverflow)?;
    let exact_successor = CatalogHighWaterPointV1 {
        sequence: committed.sequence,
        head_revision: committed.head_revision,
        publication_nonce: committed.publication_nonce,
    };
    let committed_revision = super::catalog_head_revision_v1(&committed.committed_head_bytes);
    let ready_bytes = canonical_ready_control_record_bytes_v1(&committed, receipt.ready_revision);
    let ready_record_fingerprint =
        super::domain_object_id_v1(CATALOG_CONTROL_RECORD_DOMAIN_V1, &ready_bytes);
    if committed.context != control_binding.context
        || committed.pending_revision.generation != expected_pending_generation
        || committed.pending_revision.fingerprint == [0; 32]
        || committed.pending_revision.fingerprint == control_binding.ready_revision.fingerprint
        || committed.publishing_head_reservation_fingerprint == [0; 32]
        || committed.pending_control_record_fingerprint == [0; 32]
        || committed.sequence != expected_successor_sequence
        || committed.parent_head_revision != control_binding.current.head_revision
        || committed.publication_nonce == [0; 32]
        || committed.publication_nonce == control_binding.current.publication_nonce
        || committed.head_revision == [0; 32]
        || committed.head_revision == control_binding.current.head_revision
        || committed_revision != committed.head_revision
        || receipt.control_binding != control_binding
        || receipt.pending_revision != committed.pending_revision
        || receipt.publishing_head_reservation_fingerprint
            != committed.publishing_head_reservation_fingerprint
        || receipt.pending_control_record_fingerprint
            != committed.pending_control_record_fingerprint
        || receipt.successor != exact_successor
        || receipt.ready_revision.generation != expected_ready_generation
        || receipt.ready_revision.fingerprint == [0; 32]
        || receipt.ready_revision.fingerprint == committed.pending_revision.fingerprint
        || receipt.ready_revision.fingerprint == control_binding.ready_revision.fingerprint
        || receipt.ready_control_record_fingerprint == [0; 32]
        || receipt.ready_control_record_fingerprint == committed.pending_control_record_fingerprint
        || receipt.ready_control_record_fingerprint != ready_record_fingerprint
    {
        return Err(CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch);
    }
    Ok(AdvancedCatalogHighWaterV1 {
        context: control_binding.context,
        bootstrap: control_binding.bootstrap,
        successor: exact_successor,
        storage_authority_fingerprint: control_binding.storage_authority_fingerprint,
        control_authority_fingerprint: control_binding.control_authority_fingerprint,
        ready_revision: receipt.ready_revision,
        ready_control_record_fingerprint: ready_record_fingerprint,
        retained_control_lease: committed.control_guard.all_writers.retained_control_lease,
    })
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
        && wire.control_ready_generation > 0
        && valid_nonzero_hex(&wire.control_ready_revision_fingerprint)
        && wire
            .control_ready_generation
            .checked_add(1)
            .is_some_and(|next| next == wire.control_pending_generation)
        && valid_nonzero_hex(&wire.control_pending_revision_fingerprint)
        && wire.control_pending_revision_fingerprint != wire.control_ready_revision_fingerprint
        && valid_nonzero_hex(&wire.control_lease_public_fingerprint)
        && valid_nonzero_hex(&wire.writer_fence_authority_revision_fingerprint)
        && valid_nonzero_hex(&wire.writer_fence_lease_public_fingerprint)
        && valid_nonzero_hex(&wire.predecessor_head_storage_binding_fingerprint)
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
fn is_canonical_control_record_v1(
    raw_bytes: &[u8],
    selected: &ValidatedSelectedRegisteredRootRemoteContextV1,
) -> bool {
    let Ok(raw_bytes_len) = u64::try_from(raw_bytes.len()) else {
        return false;
    };
    if raw_bytes_len
        > RegisteredRootPlanContractV1::strict_v1()
            .remote_contract()
            .max_catalog_page_object_bytes()
    {
        return false;
    }
    let Ok(wire) = serde_json::from_slice::<CatalogControlRecordWireV1>(raw_bytes) else {
        return false;
    };
    if serde_json::to_vec(&wire).ok().as_deref() != Some(raw_bytes)
        || wire.version != CATALOG_CONTROL_RECORD_SCHEMA_VERSION_V1
        || wire.remote_prefix != selected.spec().remote_prefix
        || validate_catalog_context_v1(&wire.context, selected).is_none()
        || wire.control_contract_fingerprint
            != lower_hex(&catalog_control_contract_fingerprint_v1())
        || wire.control_generation == 0
    {
        return false;
    }
    let nonzero_hex = |value: &str| {
        super::parse_lower_hex_32(value).is_some_and(|fingerprint| fingerprint != [0; 32])
    };
    if !nonzero_hex(&wire.control_authority_fingerprint)
        || !nonzero_hex(&wire.storage_authority_fingerprint)
        || !nonzero_hex(&wire.control_revision_fingerprint)
        || !nonzero_hex(&wire.lease_public_fingerprint)
        || !nonzero_hex(&wire.bootstrap.head_revision)
        || !nonzero_hex(&wire.bootstrap.publication_nonce)
        || !nonzero_hex(&wire.bootstrap.complete_corpus_attestation)
        || !nonzero_hex(&wire.bootstrap.writer_epoch)
        || wire.writer_epoch != wire.bootstrap.writer_epoch
    {
        return false;
    }
    let valid_point = |point: &CatalogControlHeadPointWireV1| {
        point.catalog_sequence > 0
            && nonzero_hex(&point.head_revision)
            && nonzero_hex(&point.publication_nonce)
            && (point.catalog_sequence != 1
                || (point.head_revision == wire.bootstrap.head_revision
                    && point.publication_nonce == wire.bootstrap.publication_nonce))
    };
    match &wire.control_state {
        CatalogControlStateWireV1::Ready { current } => valid_point(current),
        CatalogControlStateWireV1::PublicationPending { pending } => {
            let valid_reference = |reference: &RemoteCatalogPublicationObjectReferenceWireV1| {
                nonzero_hex(&reference.object_id)
                    && reference.raw_bytes_len > 0
                    && super::validate_binding_wire_v1(&reference.binding).is_some()
            };
            valid_point(&pending.parent)
                && pending.ready_control_generation > 0
                && pending
                    .ready_control_generation
                    .checked_add(1)
                    .is_some_and(|next| next == wire.control_generation)
                && nonzero_hex(&pending.ready_control_revision_fingerprint)
                && pending.ready_control_revision_fingerprint != wire.control_revision_fingerprint
                && pending
                    .parent
                    .catalog_sequence
                    .checked_add(1)
                    .is_some_and(|next| next == pending.successor_catalog_sequence)
                && nonzero_hex(&pending.successor_publication_nonce)
                && pending.successor_publication_nonce != pending.parent.publication_nonce
                && nonzero_hex(&pending.publishing_head_reservation_fingerprint)
                && nonzero_hex(&pending.predecessor_head_storage_binding_fingerprint)
                && valid_reference(&pending.predecessor_head_archive)
                && valid_reference(&pending.mutation_journal)
        }
    }
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
        control_ready_generation: 7,
        control_ready_revision_fingerprint: "ab".repeat(32),
        control_pending_generation: 8,
        control_pending_revision_fingerprint: "ac".repeat(32),
        control_lease_public_fingerprint: "aa".repeat(32),
        writer_fence_authority_revision_fingerprint: "89".repeat(32),
        writer_fence_lease_public_fingerprint: "88".repeat(32),
        predecessor_head_storage_binding_fingerprint: "87".repeat(32),
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

    struct TestCatalogControlLeaseLivenessV1 {
        live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl CatalogControlLeaseLivenessV1 for TestCatalogControlLeaseLivenessV1 {
        fn is_live_v1(&self) -> bool {
            self.live.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

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
        RetainedCatalogControlLeaseV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        HeldReadyCatalogControlGuardV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        CatalogControlRecordWireV1:
        Into<TrustedCatalogBootstrapReceiptV1>,
        Into<HeldReadyCatalogControlGuardV1>,
        Into<TrustedCatalogHighWaterGuardV1>,
        Into<AllNamespaceWritersFencedLeaseV1>,
        Into<TrustedCatalogPublicationPendingReceiptV1>,
        Into<TrustedCatalogHighWaterAdvanceReceiptV1>,
        Into<BoundPendingCatalogControlV1<'static>>,
        Into<BoundCommittedCatalogSuccessorV1>
    );
    static_assertions::assert_not_impl_any!(
        RemoteCatalogPublishingHeadWireV1:
        Into<TrustedCatalogBootstrapReceiptV1>,
        Into<HeldReadyCatalogControlGuardV1>,
        Into<TrustedCatalogPublicationPendingReceiptV1>,
        Into<TrustedCatalogHighWaterAdvanceReceiptV1>,
        Into<BoundPendingCatalogControlV1<'static>>,
        Into<BoundCommittedCatalogSuccessorV1>
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
        ProvenCatalogObjectAbsenceV1<'static, 'static>: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        TypedCatalogSuccessorPayloadV1: Clone,
        serde::Serialize,
        Default,
        Into<BoundCatalogMutationJournalV1>
    );
    static_assertions::assert_not_impl_any!(
        FactBoundCatalogMutationV1<'static, 'static>: Clone,
        serde::Serialize,
        Default,
        Into<BoundCatalogMutationJournalV1>
    );
    static_assertions::assert_not_impl_any!(
        PreparedAuthoritativeCatalogMutationJournalV1: Clone,
        serde::Serialize,
        Default,
        Into<BoundCatalogMutationJournalV1>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        PreparedUntrustedCatalogMutationJournalDraftV1: Clone,
        serde::Serialize,
        Default,
        Into<BoundCatalogMutationJournalV1>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        PublishedUntrustedCatalogMutationJournalDraftV1: Clone,
        serde::Serialize,
        Default,
        Into<BoundCatalogMutationJournalV1>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
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
    static_assertions::assert_not_impl_any!(
        PreparedCatalogControlTransitionV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogPublicationPendingReceiptV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        BoundPendingCatalogControlV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogPublishingHeadInstalledReceiptV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        CatalogNamespaceMutationCapabilityV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        CatalogNamespaceMutationCapabilityBindFailureV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<CatalogNamespaceMutationCapabilityV1<'static>>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        AppliedCatalogNamespaceMutationsV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<CatalogNamespaceMutationCapabilityV1<'static>>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        FailedCatalogNamespaceMutationsV1<'static>: Clone,
        serde::Serialize,
        Default,
        Into<CatalogNamespaceMutationCapabilityV1<'static>>,
        Into<AppliedCatalogNamespaceMutationsV1<'static>>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        BoundCommittedCatalogSuccessorV1: Clone,
        serde::Serialize,
        Default,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );
    static_assertions::assert_not_impl_any!(
        TrustedCatalogHighWaterAdvanceReceiptV1: Clone,
        serde::Serialize,
        Default
    );
    static_assertions::assert_not_impl_any!(
        AdvancedCatalogHighWaterV1: Clone,
        serde::Serialize,
        Default,
        Into<TrustedCatalogHighWaterGuardV1>,
        Into<HeldReadyCatalogControlGuardV1>,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>
    );

    async fn observed_corpus(
        rows: &[SemanticRemoteCatalogFixtureRowV1],
    ) -> (
        SemanticRemoteCatalogFixtureV1,
        Box<SemanticallyBoundRemoteCatalogCorpusV1>,
        ObservedPublishedCatalogHeadV1,
    ) {
        let spec = test_spec();
        let selected = test_selected();
        let fixture =
            semantic_remote_catalog_fixture_for_test_v1("fixture-root", &spec, rows).await;
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
        (fixture, corpus, observed)
    }

    async fn observed_head() -> (
        SemanticRemoteCatalogFixtureV1,
        ObservedPublishedCatalogHeadV1,
    ) {
        let (fixture, _corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        (fixture, observed)
    }

    fn deleted_index_bytes() -> Vec<u8> {
        br#"{"version":4,"state":"deleted","current":null,"pending":null}"#.to_vec()
    }

    fn committed_index_bytes(manifest_hash: &str) -> Vec<u8> {
        format!(
            r#"{{"version":2,"state":"committed","current":{{"manifest_hash":"{manifest_hash}","size":4,"chunks":1}},"pending":null}}"#
        )
        .into_bytes()
    }

    fn regular_manifest_bytes(rel_path: &str) -> Vec<u8> {
        format!(
            r#"{{
  "version": 2,
  "file_hash": "{file_hash}",
  "file_size": 4,
  "chunks": ["{chunk_hash}"],
  "vclock": {{ "clocks": {{ "sting": 1 }} }},
  "written_by": "sting",
  "written_at": 7,
  "rel_path": "{rel_path}",
  "mode": 420,
  "mtime": [8, 9]
}}"#,
            file_hash = "d".repeat(64),
            chunk_hash = "e".repeat(64),
        )
        .into_bytes()
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
        let storage_authority_fingerprint = [0x99; 32];
        let control_authority_fingerprint = [0x9a; 32];
        let bootstrap = CatalogBootstrapIdentityV1 {
            head_revision: bootstrap_head_revision,
            publication_nonce: observed.publication_nonce,
            complete_corpus_attestation: [0x33; 32],
            writer_epoch: bootstrap_writer_epoch,
        };
        let control_binding = CatalogControlAcquisitionBindingV1 {
            context: observed.context.clone(),
            bootstrap: bootstrap.clone(),
            current: CatalogHighWaterPointV1 {
                sequence: observed.sequence,
                head_revision: observed.head_revision,
                publication_nonce: observed.publication_nonce,
            },
            storage_authority_fingerprint,
            control_authority_fingerprint,
            ready_revision: CatalogControlAuthorityRevisionV1 {
                generation: NonZeroU64::new(7).unwrap(),
                fingerprint: [0xab; 32],
            },
            lease_public_fingerprint: NonSecretLeasePublicFingerprintV1([0xaa; 32]),
        };
        (
            TrustedCatalogBootstrapReceiptV1 {
                context: observed.context.clone(),
                bootstrap,
                storage_authority_fingerprint,
                control_authority_fingerprint,
            },
            TrustedCatalogHighWaterGuardV1 {
                binding: control_binding.clone(),
            },
            AllNamespaceWritersFencedLeaseV1 {
                retained_control_lease: {
                    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
                    RetainedCatalogControlLeaseV1 {
                        control_binding: control_binding.clone(),
                        writer_fence_authority_revision_fingerprint: [0x89; 32],
                        writer_fence_lease_public_fingerprint: NonSecretLeasePublicFingerprintV1(
                            [0x88; 32],
                        ),
                        liveness: Box::new(TestCatalogControlLeaseLivenessV1 {
                            live: live.clone(),
                        }),
                        test_live: live,
                    }
                },
                control_binding,
                authority_revision_fingerprint: [0x89; 32],
                lease_public_fingerprint: NonSecretLeasePublicFingerprintV1([0x88; 32]),
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

    fn held_control(
        high_water: TrustedCatalogHighWaterGuardV1,
        all_writers: AllNamespaceWritersFencedLeaseV1,
    ) -> HeldReadyCatalogControlGuardV1 {
        HeldReadyCatalogControlGuardV1 {
            high_water,
            all_writers,
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
                successor_payloads: Vec::new(),
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
            held_control(high_water, writers),
        )
        .unwrap()
    }

    fn prepared_control_transition<'a>(
        fixture: &'a SemanticRemoteCatalogFixtureV1,
        observed: &ObservedPublishedCatalogHeadV1,
    ) -> PreparedCatalogControlTransitionV1<'a> {
        let publication_nonce = [0x44; 32];
        let (archive, journal) = bound_publication_objects(observed, publication_nonce);
        let publication = prepare_catalog_publication_fence_v1(
            matched(fixture, observed),
            publication_nonce,
            archive,
            journal,
        )
        .unwrap();
        prepare_catalog_control_transition_v1(publication, [0xac; 32]).unwrap()
    }

    fn pending_receipt(
        transition: &PreparedCatalogControlTransitionV1<'_>,
    ) -> TrustedCatalogPublicationPendingReceiptV1 {
        TrustedCatalogPublicationPendingReceiptV1 {
            control_binding: transition
                .publication
                .control_guard
                .high_water
                .binding
                .clone(),
            pending_revision: transition.pending_revision,
            publishing_head_reservation_fingerprint: transition
                .publishing_head_reservation_fingerprint,
            pending_control_record_fingerprint: transition.pending_control_record_fingerprint,
        }
    }

    fn bound_pending<'a>(
        fixture: &'a SemanticRemoteCatalogFixtureV1,
        observed: &ObservedPublishedCatalogHeadV1,
    ) -> BoundPendingCatalogControlV1<'a> {
        let transition = prepared_control_transition(fixture, observed);
        let receipt = pending_receipt(&transition);
        match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap()
    }

    async fn authoritative_bound_pending<'a>(
        fixture: &'a SemanticRemoteCatalogFixtureV1,
        corpus: &SemanticallyBoundRemoteCatalogCorpusV1,
        observed: &ObservedPublishedCatalogHeadV1,
    ) -> BoundPendingCatalogControlV1<'a> {
        let publication_nonce = [0x44; 32];
        let prerequisites = matched(fixture, observed);
        let successor_bytes = deleted_index_bytes();
        let replacement = fact_bind_catalog_replacement_v1(
            &prerequisites,
            corpus,
            type_catalog_successor_payload_v1(
                corpus,
                RemoteCatalogObjectKindV1::Index,
                "roots/index/retained.txt".to_owned(),
                successor_bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let create_key = "roots/index/added.txt".to_owned();
        let absence = prove_catalog_object_absence_v1(
            &prerequisites,
            corpus,
            RemoteCatalogObjectKindV1::Index,
            create_key.clone(),
        )
        .await
        .unwrap();
        let create = fact_bind_catalog_create_v1(
            &prerequisites,
            corpus,
            absence,
            type_catalog_successor_payload_v1(
                corpus,
                RemoteCatalogObjectKindV1::Index,
                create_key,
                successor_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        let prepared = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            corpus,
            publication_nonce,
            vec![replacement, create],
        )
        .await
        .unwrap();
        let journal = publish_authoritative_catalog_mutation_journal_v1(&prerequisites, prepared)
            .await
            .unwrap();
        let archive = publish_predecessor_head_archive_v1(&prerequisites)
            .await
            .unwrap();
        let publication = prepare_catalog_publication_fence_v1(
            prerequisites,
            publication_nonce,
            archive,
            journal,
        )
        .unwrap();
        let transition = prepare_catalog_control_transition_v1(publication, [0xac; 32]).unwrap();
        let receipt = pending_receipt(&transition);
        match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap()
    }

    async fn install_test_publishing_head_receipt(
        pending: &BoundPendingCatalogControlV1<'_>,
    ) -> TrustedCatalogPublishingHeadInstalledReceiptV1 {
        let publication = &pending.transition.publication;
        let head_key = super::super::catalog_head_key_v1(&publication.context.remote_prefix);
        publication
            .storage_authority
            .operator
            .write(
                &head_key,
                pending.transition.canonical_publishing_head_bytes.clone(),
            )
            .await
            .unwrap();
        let observed = read_raw_object_snapshot_v1(
            publication.storage_authority.operator,
            &head_key,
            RegisteredRootPlanContractV1::strict_v1()
                .remote_contract()
                .max_catalog_head_object_bytes(),
        )
        .await
        .unwrap()
        .unwrap();
        let RawObjectReadV1::Bound(snapshot) = observed else {
            panic!("test publishing HEAD must carry an exact binding");
        };
        let (_, _, binding) = snapshot.into_parts();
        TrustedCatalogPublishingHeadInstalledReceiptV1 {
            publishing_head_binding: registered_binding_from_raw_v1(binding),
        }
    }

    fn committed_successor(
        pending: BoundPendingCatalogControlV1<'_>,
    ) -> BoundCommittedCatalogSuccessorV1 {
        let committed_head_bytes = br#"{"catalog_sequence":2,"fixture":"committed"}"#.to_vec();
        let head_revision = super::super::catalog_head_revision_v1(&committed_head_bytes);
        let pending_revision = pending.transition.pending_revision;
        let publishing_head_reservation_fingerprint = pending
            .pending_receipt
            .publishing_head_reservation_fingerprint;
        let pending_control_record_fingerprint =
            pending.pending_receipt.pending_control_record_fingerprint;
        let publication = pending.transition.publication;
        BoundCommittedCatalogSuccessorV1 {
            control_guard: publication.control_guard,
            pending_revision,
            publishing_head_reservation_fingerprint,
            pending_control_record_fingerprint,
            context: publication.context,
            sequence: publication.sequence,
            publication_nonce: publication.publication_nonce,
            parent_head_revision: publication.parent_head_revision,
            head_revision,
            committed_head_bytes,
        }
    }

    fn high_water_advance_receipt(
        committed: &BoundCommittedCatalogSuccessorV1,
    ) -> TrustedCatalogHighWaterAdvanceReceiptV1 {
        let ready_revision = CatalogControlAuthorityRevisionV1 {
            generation: NonZeroU64::new(committed.pending_revision.generation.get() + 1).unwrap(),
            fingerprint: [0xad; 32],
        };
        let ready_bytes = canonical_ready_control_record_bytes_v1(committed, ready_revision);
        TrustedCatalogHighWaterAdvanceReceiptV1 {
            control_binding: committed.control_guard.high_water.binding.clone(),
            pending_revision: committed.pending_revision,
            publishing_head_reservation_fingerprint: committed
                .publishing_head_reservation_fingerprint,
            pending_control_record_fingerprint: committed.pending_control_record_fingerprint,
            successor: CatalogHighWaterPointV1 {
                sequence: committed.sequence,
                head_revision: committed.head_revision,
                publication_nonce: committed.publication_nonce,
            },
            ready_revision,
            ready_control_record_fingerprint: super::super::domain_object_id_v1(
                CATALOG_CONTROL_RECORD_DOMAIN_V1,
                &ready_bytes,
            ),
        }
    }

    fn created_index_mutation(
        object_key: impl Into<String>,
    ) -> UntrustedCatalogMutationIntentDraftV1 {
        UntrustedCatalogMutationIntentDraftV1::create_if_absent(
            RemoteCatalogObjectKindV1::Index,
            object_key.into(),
            br#"{"state":"deleted","version":4}"#,
        )
        .unwrap()
    }

    fn replaced_index_mutation(
        object_key: impl Into<String>,
        binding: RegisteredRootRemoteObjectBindingV1,
    ) -> UntrustedCatalogMutationIntentDraftV1 {
        UntrustedCatalogMutationIntentDraftV1::replace_if_match(
            RemoteCatalogObjectKindV1::Index,
            object_key.into(),
            br#"{"state":"committed","version":4}"#,
            binding,
            br#"{"state":"deleted","version":4}"#,
        )
        .unwrap()
    }

    fn prepared_journal(
        prerequisites: &MatchedCatalogPublicationPrerequisitesV1<'_>,
        publication_nonce: [u8; 32],
    ) -> PreparedUntrustedCatalogMutationJournalDraftV1 {
        prepare_untrusted_catalog_mutation_journal_draft_v1(
            prerequisites,
            publication_nonce,
            vec![replaced_index_mutation(
                "roots/index/retained.txt",
                RegisteredRootRemoteObjectBindingV1::Etag {
                    etag: "fixture-index-etag".to_owned(),
                },
            )],
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

    #[tokio::test]
    async fn fact_bound_journal_derives_predecessors_sorts_and_publishes_exact_payloads() {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let replacement_bytes = deleted_index_bytes();
        let replacement = fact_bind_catalog_replacement_v1(
            &prerequisites,
            &corpus,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                "roots/index/retained.txt".to_owned(),
                replacement_bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let create_key = "roots/index/added.txt".to_owned();
        let absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Index,
            create_key.clone(),
        )
        .await
        .unwrap();
        let create = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                create_key,
                replacement_bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();

        let prepared = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![replacement, create],
        )
        .await
        .unwrap();
        let wire =
            serde_json::from_slice::<RemoteCatalogMutationJournalWireV1>(&prepared.raw_bytes)
                .unwrap();
        assert_eq!(wire.mutation_count, 2);
        assert_eq!(wire.mutations[0].object_key, "roots/index/added.txt");
        assert_eq!(wire.mutations[1].object_key, "roots/index/retained.txt");
        assert!(matches!(
            wire.mutations[0].predecessor,
            RemoteCatalogMutationPredecessorDraftWireV1::Absent
        ));
        assert!(matches!(
            wire.mutations[1].predecessor,
            RemoteCatalogMutationPredecessorDraftWireV1::Present { .. }
        ));
        let payload_key =
            successor_payload_object_key_v1(&prerequisites.context, &replacement_bytes).unwrap();
        assert_eq!(
            fixture
                .operator()
                .read(&payload_key)
                .await
                .unwrap()
                .to_vec(),
            replacement_bytes
        );

        let bound = publish_authoritative_catalog_mutation_journal_v1(&prerequisites, prepared)
            .await
            .unwrap();
        let journal_key = mutation_journal_object_key_v1(&bound).unwrap();
        assert_eq!(
            fixture
                .operator()
                .read(&journal_key)
                .await
                .unwrap()
                .to_vec(),
            bound.raw_bytes
        );
        assert_eq!(bound.catalog_sequence.get(), 2);
        assert_eq!(bound.parent_head_revision, observed.head_revision);
    }

    #[tokio::test]
    async fn namespace_writer_applies_only_the_complete_authoritative_journal_with_replay() {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let pending = authoritative_bound_pending(&fixture, &corpus, &observed).await;
        let successor_bytes = deleted_index_bytes();

        fixture
            .operator()
            .write("roots/index/added.txt", successor_bytes.clone())
            .await
            .unwrap();
        let receipt = install_test_publishing_head_receipt(&pending).await;
        let capability = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
            .await
            .unwrap();
        let applied = apply_authoritative_catalog_namespace_mutations_v1(capability)
            .await
            .unwrap();

        assert_eq!(applied.applied.len(), 2);
        assert_eq!(applied.applied[0].object_key, "roots/index/added.txt");
        assert_eq!(applied.applied[1].object_key, "roots/index/retained.txt");
        assert_eq!(
            fixture
                .operator()
                .read("roots/index/added.txt")
                .await
                .unwrap()
                .to_vec(),
            successor_bytes
        );
        assert_eq!(
            fixture
                .operator()
                .read("roots/index/retained.txt")
                .await
                .unwrap()
                .to_vec(),
            deleted_index_bytes()
        );
        assert_eq!(
            applied
                .capability
                .pending
                .transition
                .publication
                .sequence
                .get(),
            2
        );
    }

    #[tokio::test]
    async fn namespace_writer_covers_index_reservation_and_manifest_planes() {
        let (fixture, corpus, observed) = observed_corpus(&[]).await;
        let prerequisites = matched(&fixture, &observed);
        let rel_path = "new.txt";
        let manifest_bytes = regular_manifest_bytes(rel_path);
        let manifest_id = crate::index_entry::manifest_object_id(&manifest_bytes);
        let index_key = format!("roots/index/{rel_path}");
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let folded_path = crate::index_entry::portable_casefold_path(rel_path).unwrap();
        let reservation_key = format!(
            "roots/.tcfs-namespace/v1/{}",
            crate::index_entry::namespace_reservation_object_id(&folded_path)
        );
        let reservation_bytes = format!(
            r#"{{"version":1,"exact_path":"{rel_path}","folded_path":"{folded_path}","role":"file"}}"#
        )
        .into_bytes();

        let index_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Index,
            index_key.clone(),
        )
        .await
        .unwrap();
        let reservation_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Reservation,
            reservation_key.clone(),
        )
        .await
        .unwrap();
        let manifest_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Manifest,
            manifest_key.clone(),
        )
        .await
        .unwrap();
        let index = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            index_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                index_key.clone(),
                committed_index_bytes(&manifest_id),
            )
            .unwrap(),
        )
        .unwrap();
        let reservation = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            reservation_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Reservation,
                reservation_key.clone(),
                reservation_bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let manifest = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            manifest_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Manifest,
                manifest_key.clone(),
                manifest_bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let prepared = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![manifest, reservation, index],
        )
        .await
        .unwrap();
        let journal = publish_authoritative_catalog_mutation_journal_v1(&prerequisites, prepared)
            .await
            .unwrap();
        let archive = publish_predecessor_head_archive_v1(&prerequisites)
            .await
            .unwrap();
        let publication =
            prepare_catalog_publication_fence_v1(prerequisites, [0x44; 32], archive, journal)
                .unwrap();
        let transition = prepare_catalog_control_transition_v1(publication, [0xac; 32]).unwrap();
        let receipt = pending_receipt(&transition);
        let pending = match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap();
        let receipt = install_test_publishing_head_receipt(&pending).await;
        let capability = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
            .await
            .unwrap();
        let applied = apply_authoritative_catalog_namespace_mutations_v1(capability)
            .await
            .unwrap();

        assert_eq!(applied.applied.len(), 3);
        assert!(applied
            .applied
            .iter()
            .any(|object| object.kind == RemoteCatalogObjectKindV1::Index));
        assert!(applied
            .applied
            .iter()
            .any(|object| object.kind == RemoteCatalogObjectKindV1::Reservation));
        assert!(applied
            .applied
            .iter()
            .any(|object| object.kind == RemoteCatalogObjectKindV1::Manifest));
        assert_eq!(
            fixture.operator().read(&index_key).await.unwrap().to_vec(),
            committed_index_bytes(&manifest_id)
        );
        assert_eq!(
            fixture
                .operator()
                .read(&reservation_key)
                .await
                .unwrap()
                .to_vec(),
            reservation_bytes
        );
        assert_eq!(
            fixture
                .operator()
                .read(&manifest_key)
                .await
                .unwrap()
                .to_vec(),
            manifest_bytes
        );
    }

    #[tokio::test]
    async fn namespace_writer_rejects_lost_lease_visible_head_drift_and_wrong_collision() {
        {
            let (fixture, corpus, observed) =
                observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                    "retained.txt".to_owned(),
                )])
                .await;
            let pending = authoritative_bound_pending(&fixture, &corpus, &observed).await;
            let receipt = install_test_publishing_head_receipt(&pending).await;
            pending
                .transition
                .publication
                .control_guard
                .all_writers
                .retained_control_lease
                .test_live
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let error = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
                .await
                .expect_err("a lost backend lease must not mint a writer capability");
            assert!(error.to_string().contains("control lease is not live"));
        }

        {
            let (fixture, corpus, observed) =
                observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                    "retained.txt".to_owned(),
                )])
                .await;
            let pending = authoritative_bound_pending(&fixture, &corpus, &observed).await;
            let receipt = install_test_publishing_head_receipt(&pending).await;
            let capability = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
                .await
                .unwrap();
            capability
                .pending
                .transition
                .publication
                .control_guard
                .all_writers
                .retained_control_lease
                .test_live
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let error = apply_authoritative_catalog_namespace_mutations_v1(capability)
                .await
                .expect_err("a lease lost after capability mint must stop the writer");
            assert!(error.to_string().contains("control lease is not live"));
            assert!(!fixture
                .operator()
                .exists("roots/index/added.txt")
                .await
                .unwrap());
        }

        {
            let (fixture, corpus, observed) =
                observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                    "retained.txt".to_owned(),
                )])
                .await;
            let pending = authoritative_bound_pending(&fixture, &corpus, &observed).await;
            let receipt = install_test_publishing_head_receipt(&pending).await;
            let capability = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
                .await
                .unwrap();
            fixture
                .operator()
                .write(
                    "roots/.tcfs-catalog/v1/head",
                    observed.committed_head_bytes.clone(),
                )
                .await
                .unwrap();
            let error = apply_authoritative_catalog_namespace_mutations_v1(capability)
                .await
                .expect_err("visible HEAD drift must stop the complete writer");
            assert!(error.to_string().contains("publishing HEAD changed"));
            assert_eq!(error.applied.len(), 0);
            assert!(!fixture
                .operator()
                .exists("roots/index/added.txt")
                .await
                .unwrap());
        }

        {
            let (fixture, corpus, observed) =
                observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                    "retained.txt".to_owned(),
                )])
                .await;
            let pending = authoritative_bound_pending(&fixture, &corpus, &observed).await;
            fixture
                .operator()
                .write("roots/index/added.txt", b"wrong collision".to_vec())
                .await
                .unwrap();
            let receipt = install_test_publishing_head_receipt(&pending).await;
            let capability = bind_catalog_namespace_mutation_capability_v1(pending, receipt)
                .await
                .unwrap();
            let error = apply_authoritative_catalog_namespace_mutations_v1(capability)
                .await
                .expect_err("a different create collision must fail closed");
            assert_eq!(error.applied.len(), 0);
            assert!(error
                .capability
                .pending
                .transition
                .publication
                .control_guard
                .all_writers
                .retained_control_lease
                .is_live_v1());
            assert!(
                format!("{error:#}").contains("differs from its authoritative journal"),
                "unexpected error: {error:#}"
            );
            assert_eq!(
                fixture
                    .operator()
                    .read("roots/index/added.txt")
                    .await
                    .unwrap()
                    .to_vec(),
                b"wrong collision"
            );
            assert_eq!(
                fixture
                    .operator()
                    .read("roots/index/retained.txt")
                    .await
                    .unwrap()
                    .to_vec(),
                deleted_index_bytes(),
                "canonical key ordering must stop before the later replacement"
            );
        }
    }

    #[tokio::test]
    async fn complete_successor_closure_accepts_exact_index_manifest_pair() {
        let (fixture, corpus, observed) = observed_corpus(&[]).await;
        let prerequisites = matched(&fixture, &observed);
        let manifest_bytes = regular_manifest_bytes("new.txt");
        let manifest_id = crate::index_entry::manifest_object_id(&manifest_bytes);
        let index_key = "roots/index/new.txt".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");

        let index_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Index,
            index_key.clone(),
        )
        .await
        .unwrap();
        let manifest_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Manifest,
            manifest_key.clone(),
        )
        .await
        .unwrap();
        let index = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            index_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                index_key,
                committed_index_bytes(&manifest_id),
            )
            .unwrap(),
        )
        .unwrap();
        let manifest = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            manifest_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Manifest,
                manifest_key,
                manifest_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        let prepared = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![manifest, index],
        )
        .await
        .unwrap();
        let wire =
            serde_json::from_slice::<RemoteCatalogMutationJournalWireV1>(&prepared.raw_bytes)
                .unwrap();
        assert_eq!(wire.mutation_count, 2);
        assert_eq!(wire.mutations[0].object_key, "roots/index/new.txt");
        assert!(wire.mutations[1].object_key.starts_with("roots/manifests/"));
    }

    #[tokio::test]
    async fn replacement_requires_exact_predecessor_membership_and_strict_payload_type() {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let missing = type_catalog_successor_payload_v1(
            &corpus,
            RemoteCatalogObjectKindV1::Index,
            "roots/index/missing.txt".to_owned(),
            deleted_index_bytes(),
        )
        .unwrap();
        assert_eq!(
            fact_bind_catalog_replacement_v1(&prerequisites, &corpus, missing).unwrap_err(),
            CatalogPublicationContractErrorV1::MutationPredecessorMissing
        );
        assert_eq!(
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                "roots/index/retained.txt".to_owned(),
                b"{}".to_vec(),
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidSuccessorPayload
        );
        assert_eq!(
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Reservation,
                "roots/index/retained.txt".to_owned(),
                deleted_index_bytes(),
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidSuccessorPayload
        );
    }

    #[tokio::test]
    async fn create_absence_is_rechecked_on_the_same_accessor() {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let key = "roots/index/raced.txt".to_owned();
        let absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Index,
            key.clone(),
        )
        .await
        .unwrap();
        let create = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                key.clone(),
                deleted_index_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
        fixture
            .operator()
            .write(&key, deleted_index_bytes())
            .await
            .unwrap();
        let error = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![create],
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("absence witness is stale"));
    }

    #[tokio::test]
    async fn complete_successor_closure_rejects_missing_or_orphaned_manifests_and_claim_collisions()
    {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::CurrentFile(
                "file.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let missing_manifest_index = fact_bind_catalog_replacement_v1(
            &prerequisites,
            &corpus,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                "roots/index/file.txt".to_owned(),
                committed_index_bytes(&"f".repeat(64)),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![missing_manifest_index],
        )
        .await
        .is_err());

        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let orphan_bytes = regular_manifest_bytes("orphan.txt");
        let orphan_key = format!(
            "roots/manifests/{}",
            crate::index_entry::manifest_object_id(&orphan_bytes)
        );
        let orphan_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Manifest,
            orphan_key.clone(),
        )
        .await
        .unwrap();
        let orphan = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            orphan_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Manifest,
                orphan_key,
                orphan_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x45; 32],
            vec![orphan],
        )
        .await
        .is_err());

        let exact_path = "retained.txt";
        let folded_path = crate::index_entry::portable_casefold_path(exact_path).unwrap();
        let reservation_bytes = format!(
            r#"{{"version":1,"exact_path":"{exact_path}","folded_path":"{folded_path}","role":"directory"}}"#
        )
        .into_bytes();
        let reservation_key = format!(
            "roots/.tcfs-namespace/v1/{}",
            crate::index_entry::namespace_reservation_object_id(&folded_path)
        );
        let reservation_absence = prove_catalog_object_absence_v1(
            &prerequisites,
            &corpus,
            RemoteCatalogObjectKindV1::Reservation,
            reservation_key.clone(),
        )
        .await
        .unwrap();
        let reservation = fact_bind_catalog_create_v1(
            &prerequisites,
            &corpus,
            reservation_absence,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Reservation,
                reservation_key,
                reservation_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x46; 32],
            vec![reservation],
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn immutable_successor_payload_collision_requires_exact_bytes() {
        let (fixture, corpus, observed) =
            observed_corpus(&[SemanticRemoteCatalogFixtureRowV1::DeletedFile(
                "retained.txt".to_owned(),
            )])
            .await;
        let prerequisites = matched(&fixture, &observed);
        let successor_bytes = deleted_index_bytes();
        let payload_key =
            successor_payload_object_key_v1(&prerequisites.context, &successor_bytes).unwrap();
        fixture
            .operator()
            .write(&payload_key, b"wrong".to_vec())
            .await
            .unwrap();
        let replacement = fact_bind_catalog_replacement_v1(
            &prerequisites,
            &corpus,
            type_catalog_successor_payload_v1(
                &corpus,
                RemoteCatalogObjectKindV1::Index,
                "roots/index/retained.txt".to_owned(),
                successor_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        let error = prepare_authoritative_catalog_mutation_journal_v1(
            &prerequisites,
            &corpus,
            [0x44; 32],
            vec![replacement],
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("different bytes"));
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
    async fn canonical_mutation_journal_sorts_unique_final_transitions_and_accepts_empty() {
        let (fixture, observed) = observed_head().await;
        let prerequisites = matched(&fixture, &observed);
        let prepared = prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x44; 32],
            vec![
                created_index_mutation("roots/index/z.txt"),
                created_index_mutation("roots/index/a.txt"),
            ],
        )
        .unwrap();
        let wire = validate_catalog_mutation_journal_draft_bytes_v1(
            &prepared.raw_bytes,
            &prerequisites.context,
            NonZeroU64::new(2).unwrap(),
            [0x44; 32],
            observed.head_revision,
        )
        .unwrap();
        assert_eq!(wire.mutation_count, 2);
        assert_eq!(wire.mutations[0].object_key, "roots/index/a.txt");
        assert_eq!(wire.mutations[1].object_key, "roots/index/z.txt");
        assert_eq!(
            wire.mutation_key_bytes,
            u64::try_from("roots/index/a.txt".len() + "roots/index/z.txt".len()).unwrap()
        );

        let empty = prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x45; 32],
            Vec::new(),
        )
        .unwrap();
        let empty_wire = validate_catalog_mutation_journal_draft_bytes_v1(
            &empty.raw_bytes,
            &prerequisites.context,
            NonZeroU64::new(2).unwrap(),
            [0x45; 32],
            observed.head_revision,
        )
        .unwrap();
        assert_eq!(empty_wire.mutation_count, 0);
        assert_eq!(empty_wire.mutation_key_bytes, 0);
        assert!(empty_wire.mutations.is_empty());
    }

    #[tokio::test]
    async fn mutation_recovery_classification_is_exact_and_third_states_fail_closed() {
        let (fixture, observed_head) = observed_head().await;
        let prerequisites = matched(&fixture, &observed_head);

        let create_successor = br#"{"state":"deleted","version":4}"#;
        let create = prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x44; 32],
            vec![UntrustedCatalogMutationIntentDraftV1::create_if_absent(
                RemoteCatalogObjectKindV1::Index,
                "roots/index/new.txt".to_owned(),
                create_successor,
            )
            .unwrap()],
        )
        .unwrap();
        let create_wire =
            serde_json::from_slice::<RemoteCatalogMutationJournalDraftWireV1>(&create.raw_bytes)
                .unwrap();
        let create_mutation = &create_wire.mutations[0];
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(create_mutation, None),
            UntrustedCatalogMutationRecoveryClassificationV1::NotApplied
        );
        let created = ObservedCatalogMutationObjectV1 {
            raw_bytes_len: NonZeroU64::new(u64::try_from(create_successor.len()).unwrap()).unwrap(),
            raw_blake3: *blake3::hash(create_successor).as_bytes(),
            binding: RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "created-etag".to_owned(),
            },
        };
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(create_mutation, Some(&created)),
            UntrustedCatalogMutationRecoveryClassificationV1::Applied
        );
        let wrong = ObservedCatalogMutationObjectV1 {
            raw_bytes_len: NonZeroU64::new(5).unwrap(),
            raw_blake3: *blake3::hash(b"wrong").as_bytes(),
            binding: RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "wrong-etag".to_owned(),
            },
        };
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(create_mutation, Some(&wrong)),
            UntrustedCatalogMutationRecoveryClassificationV1::Diverged
        );

        let predecessor = br#"{"state":"committed","version":4}"#;
        let successor = br#"{"state":"deleted","version":4}"#;
        let predecessor_binding = RegisteredRootRemoteObjectBindingV1::Etag {
            etag: "predecessor-etag".to_owned(),
        };
        let replace = prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x45; 32],
            vec![UntrustedCatalogMutationIntentDraftV1::replace_if_match(
                RemoteCatalogObjectKindV1::Index,
                "roots/index/current.txt".to_owned(),
                predecessor,
                predecessor_binding.clone(),
                successor,
            )
            .unwrap()],
        )
        .unwrap();
        let replace_wire =
            serde_json::from_slice::<RemoteCatalogMutationJournalDraftWireV1>(&replace.raw_bytes)
                .unwrap();
        let replace_mutation = &replace_wire.mutations[0];
        let unchanged = ObservedCatalogMutationObjectV1 {
            raw_bytes_len: NonZeroU64::new(u64::try_from(predecessor.len()).unwrap()).unwrap(),
            raw_blake3: *blake3::hash(predecessor).as_bytes(),
            binding: predecessor_binding,
        };
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(replace_mutation, Some(&unchanged)),
            UntrustedCatalogMutationRecoveryClassificationV1::NotApplied
        );
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(replace_mutation, None),
            UntrustedCatalogMutationRecoveryClassificationV1::Diverged
        );
        let successor_observed = ObservedCatalogMutationObjectV1 {
            raw_bytes_len: NonZeroU64::new(u64::try_from(successor.len()).unwrap()).unwrap(),
            raw_blake3: *blake3::hash(successor).as_bytes(),
            binding: RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "successor-etag".to_owned(),
            },
        };
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(
                replace_mutation,
                Some(&successor_observed)
            ),
            UntrustedCatalogMutationRecoveryClassificationV1::Applied
        );
        let wrong_predecessor_binding = ObservedCatalogMutationObjectV1 {
            binding: RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "different-etag".to_owned(),
            },
            ..unchanged
        };
        assert_eq!(
            classify_untrusted_catalog_mutation_draft_v1(
                replace_mutation,
                Some(&wrong_predecessor_binding)
            ),
            UntrustedCatalogMutationRecoveryClassificationV1::Diverged
        );
    }

    #[tokio::test]
    async fn mutation_journal_rejects_duplicate_routes_and_operation_binding_drift() {
        let (fixture, observed) = observed_head().await;
        let prerequisites = matched(&fixture, &observed);
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x44; 32],
                vec![
                    created_index_mutation("roots/index/same.txt"),
                    created_index_mutation("roots/index/same.txt"),
                ],
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidMutationJournal(
                InvalidCatalogMutationJournalReasonV1::Order
            )
        );
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x44; 32],
                vec![created_index_mutation("other/index/file.txt")],
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidMutationJournal(
                InvalidCatalogMutationJournalReasonV1::Route
            )
        );
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x44; 32],
                vec![replaced_index_mutation(
                    "roots/index/file.txt",
                    RegisteredRootRemoteObjectBindingV1::Version {
                        version: "version-only".to_owned(),
                        etag: None,
                    },
                )],
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidMutationJournal(
                InvalidCatalogMutationJournalReasonV1::ObjectBinding
            )
        );

        let manifest_key = format!("roots/manifests/{}", "11".repeat(32));
        let manifest_replace = UntrustedCatalogMutationIntentDraftV1::replace_if_match(
            RemoteCatalogObjectKindV1::Manifest,
            manifest_key,
            b"old",
            RegisteredRootRemoteObjectBindingV1::Etag {
                etag: "manifest-etag".to_owned(),
            },
            b"new",
        )
        .unwrap();
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x44; 32],
                vec![manifest_replace],
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidMutationJournal(
                InvalidCatalogMutationJournalReasonV1::Operation
            )
        );
    }

    #[tokio::test]
    async fn mutation_journal_enforces_lineage_canonical_encoding_and_page_bounds() {
        let (fixture, observed) = observed_head().await;
        let prerequisites = matched(&fixture, &observed);
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0; 32],
                Vec::new()
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::ZeroPublicationNonce
        );
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                observed.publication_nonce,
                Vec::new(),
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::ReusedPublicationNonce
        );

        let prepared = prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x44; 32],
            vec![created_index_mutation("roots/index/file.txt")],
        )
        .unwrap();
        let mut noncanonical = prepared.raw_bytes.clone();
        noncanonical.push(b'\n');
        assert_eq!(
            validate_catalog_mutation_journal_draft_bytes_v1(
                &noncanonical,
                &prerequisites.context,
                NonZeroU64::new(2).unwrap(),
                [0x44; 32],
                observed.head_revision,
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::InvalidMutationJournal(
                InvalidCatalogMutationJournalReasonV1::CanonicalEncoding
            )
        );

        let too_many = (0..=4096)
            .map(|ordinal| created_index_mutation(format!("roots/index/{ordinal:04}.txt")))
            .collect();
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x45; 32],
                too_many
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::MutationJournalResource(
                CatalogMutationJournalResourceV1::Mutations
            )
        );

        let at_mutation_limit = (0..4096)
            .map(|ordinal| created_index_mutation(format!("roots/index/{ordinal:04}.txt")))
            .collect();
        assert!(prepare_untrusted_catalog_mutation_journal_draft_v1(
            &prerequisites,
            [0x46; 32],
            at_mutation_limit,
        )
        .is_ok());

        let excessive_aggregate_key_bytes = (0..4096)
            .map(|_| created_index_mutation("x".repeat(1025)))
            .collect();
        assert_eq!(
            prepare_untrusted_catalog_mutation_journal_draft_v1(
                &prerequisites,
                [0x47; 32],
                excessive_aggregate_key_bytes,
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::MutationJournalResource(
                CatalogMutationJournalResourceV1::KeyBytes
            )
        );

        let oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
        assert_eq!(
            validate_catalog_mutation_journal_draft_bytes_v1(
                &oversized,
                &prerequisites.context,
                NonZeroU64::new(2).unwrap(),
                [0x44; 32],
                observed.head_revision,
            )
            .unwrap_err(),
            CatalogPublicationContractErrorV1::MutationJournalResource(
                CatalogMutationJournalResourceV1::Bytes
            )
        );
    }

    #[tokio::test]
    async fn immutable_archive_and_untrusted_journal_draft_leave_head_unchanged() {
        let (fixture, observed) = observed_head().await;
        let head_key = "roots/.tcfs-catalog/v1/head";
        let head_before = fixture.operator().read(head_key).await.unwrap().to_vec();
        let prerequisites = matched(&fixture, &observed);
        let prepared = prepared_journal(&prerequisites, [0x44; 32]);
        let archive = publish_predecessor_head_archive_v1(&prerequisites)
            .await
            .unwrap();
        let journal = publish_untrusted_catalog_mutation_journal_draft_v1(&prerequisites, prepared)
            .await
            .unwrap();
        let archive_key = archived_head_object_key_v1(&archive).unwrap();
        let journal_key = untrusted_mutation_journal_draft_object_key_v1(&journal).unwrap();
        assert_eq!(
            fixture
                .operator()
                .read(&archive_key)
                .await
                .unwrap()
                .to_vec(),
            observed.committed_head_bytes
        );
        assert_eq!(
            fixture
                .operator()
                .read(&journal_key)
                .await
                .unwrap()
                .to_vec(),
            journal.raw_bytes
        );
        assert_eq!(
            fixture.operator().read(head_key).await.unwrap().to_vec(),
            head_before,
            "immutable artifact publication must not mutate HEAD"
        );
        assert_eq!(journal.storage_authority_fingerprint, [0x99; 32]);
        assert_eq!(journal.catalog_sequence.get(), 2);
        assert_eq!(journal.publication_nonce, [0x44; 32]);
        assert_eq!(journal.parent_head_revision, observed.head_revision);
        assert_ne!(journal.raw_bytes_len.get(), 0);
        assert!(
            super::super::validate_binding_wire_v1(&binding_wire_v1(&journal.binding)).is_some()
        );
        let _archive_remains_authoritative = archive;
    }

    #[tokio::test]
    async fn immutable_artifact_publication_is_idempotent_but_rejects_wrong_collision_bytes() {
        let (fixture, observed) = observed_head().await;
        let first = matched(&fixture, &observed);
        let first_archive = publish_predecessor_head_archive_v1(&first).await.unwrap();
        let first_archive_key = archived_head_object_key_v1(&first_archive).unwrap();
        let first_binding = binding_wire_v1(&first_archive.binding);

        let second = matched(&fixture, &observed);
        let second_archive = publish_predecessor_head_archive_v1(&second).await.unwrap();
        assert_eq!(
            archived_head_object_key_v1(&second_archive).as_deref(),
            Some(first_archive_key.as_str())
        );
        assert_eq!(binding_wire_v1(&second_archive.binding), first_binding);

        let other_nonce = [0x45; 32];
        let prepared = prepared_journal(&second, other_nonce);
        let journal_key = publication_object_key_v1(
            &second.context,
            UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_SUFFIX_V1,
            &prepared.object_id,
        )
        .unwrap();
        fixture
            .operator()
            .write(&journal_key, b"wrong collision bytes".to_vec())
            .await
            .unwrap();
        let error = publish_untrusted_catalog_mutation_journal_draft_v1(&second, prepared)
            .await
            .err()
            .expect("wrong collision bytes must be rejected");
        assert!(
            error.to_string().contains("contains different bytes"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            fixture
                .operator()
                .read(&journal_key)
                .await
                .unwrap()
                .to_vec(),
            b"wrong collision bytes",
            "failed absent-only publication must preserve the existing collision bytes"
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

        let transition = prepare_catalog_control_transition_v1(fence, [0xac; 32]).unwrap();
        let pending_receipt = TrustedCatalogPublicationPendingReceiptV1 {
            control_binding: transition
                .publication
                .control_guard
                .high_water
                .binding
                .clone(),
            pending_revision: transition.pending_revision,
            publishing_head_reservation_fingerprint: transition
                .publishing_head_reservation_fingerprint,
            pending_control_record_fingerprint: transition.pending_control_record_fingerprint,
        };
        let bound =
            match_catalog_publication_pending_receipt_v1(transition, pending_receipt).unwrap();
        let bytes = &bound.transition.canonical_publishing_head_bytes;
        assert!(is_canonical_publishing_head_v1(bytes, &test_selected()));
        assert!(
            super::super::canonical_wire_v1::<super::super::RemoteCatalogHeadWireV1>(bytes)
                .is_none()
        );
        let wire = serde_json::from_slice::<RemoteCatalogPublishingHeadWireV1>(bytes).unwrap();
        assert_eq!(wire.storage_authority_fingerprint, "99".repeat(32));
        assert_eq!(wire.control_authority_fingerprint, "9a".repeat(32));
        assert_eq!(wire.writer_epoch, "21".repeat(32));
        assert_eq!(wire.control_ready_generation, 7);
        assert_eq!(wire.control_ready_revision_fingerprint, "ab".repeat(32));
        assert_eq!(wire.control_pending_generation, 8);
        assert_eq!(wire.control_pending_revision_fingerprint, "ac".repeat(32));
        assert_eq!(wire.control_lease_public_fingerprint, "aa".repeat(32));
        assert_eq!(
            wire.writer_fence_authority_revision_fingerprint,
            "89".repeat(32)
        );
        assert_eq!(wire.writer_fence_lease_public_fingerprint, "88".repeat(32));
        assert_eq!(
            wire.predecessor_head_storage_binding_fingerprint,
            lower_hex(&predecessor_head_storage_binding_fingerprint_v1(
                "head-etag-a"
            ))
        );
        assert_eq!(
            wire.predecessor_head_archive.object_id,
            lower_hex(&super::super::domain_object_id_v1(
                ARCHIVED_HEAD_OBJECT_DOMAIN_V1,
                &observed.committed_head_bytes,
            ))
        );
        assert_eq!(
            wire.mutation_journal.object_id,
            lower_hex(&bound.transition.publication.mutation_journal.object_id)
        );
        assert_eq!(
            bound.pending_receipt.pending_control_record_fingerprint,
            bound.transition.pending_control_record_fingerprint
        );
        assert!(is_canonical_control_record_v1(
            &bound.transition.canonical_pending_control_record_bytes,
            &test_selected()
        ));
        let pending_text =
            std::str::from_utf8(&bound.transition.canonical_pending_control_record_bytes).unwrap();
        assert!(pending_text.contains("lease_public_fingerprint"));
        assert!(!pending_text.contains("lease_nonce"));
        assert!(!pending_text.contains("bearer"));
    }

    #[tokio::test]
    async fn pending_control_record_is_canonical_bounded_and_fail_closed() {
        let (fixture, observed) = observed_head().await;
        let transition = prepared_control_transition(&fixture, &observed);
        let canonical = transition.canonical_pending_control_record_bytes.clone();
        assert!(is_canonical_control_record_v1(&canonical, &test_selected()));

        let mut newline = canonical.clone();
        newline.push(b'\n');
        assert!(!is_canonical_control_record_v1(&newline, &test_selected()));

        let mut unknown = serde_json::from_slice::<serde_json::Value>(&canonical).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("bearer_token".to_owned(), serde_json::json!("forbidden"));
        assert!(!is_canonical_control_record_v1(
            &serde_json::to_vec(&unknown).unwrap(),
            &test_selected()
        ));

        let mut wire = serde_json::from_slice::<CatalogControlRecordWireV1>(&canonical).unwrap();
        wire.writer_epoch = "22".repeat(32);
        assert!(!is_canonical_control_record_v1(
            &serde_json::to_vec(&wire).unwrap(),
            &test_selected()
        ));

        let mut wire = serde_json::from_slice::<CatalogControlRecordWireV1>(&canonical).unwrap();
        if let CatalogControlStateWireV1::PublicationPending { pending } = &mut wire.control_state {
            pending.successor_catalog_sequence += 1;
        }
        assert!(!is_canonical_control_record_v1(
            &serde_json::to_vec(&wire).unwrap(),
            &test_selected()
        ));
    }

    #[tokio::test]
    async fn exact_pending_receipt_rejects_crossed_acquisitions_and_proposals() {
        let (fixture, observed) = observed_head().await;

        {
            let transition = prepared_control_transition(&fixture, &observed);
            let mut receipt = pending_receipt(&transition);
            receipt.control_binding.lease_public_fingerprint =
                NonSecretLeasePublicFingerprintV1([0x45; 32]);
            assert_eq!(
                match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::ControlTransitionMismatch
            );
        }
        {
            let transition = prepared_control_transition(&fixture, &observed);
            let mut receipt = pending_receipt(&transition);
            receipt.pending_revision.fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::ControlTransitionMismatch
            );
        }
        {
            let transition = prepared_control_transition(&fixture, &observed);
            let mut receipt = pending_receipt(&transition);
            receipt.publishing_head_reservation_fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::ControlTransitionMismatch
            );
        }
        {
            let transition = prepared_control_transition(&fixture, &observed);
            let mut receipt = pending_receipt(&transition);
            receipt.pending_control_record_fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::ControlTransitionMismatch
            );
        }
    }

    #[tokio::test]
    async fn control_transition_rejects_revision_reuse_overflow_and_byte_tampering() {
        let (fixture, observed) = observed_head().await;
        let make_publication = || {
            let publication_nonce = [0x44; 32];
            let (archive, journal) = bound_publication_objects(&observed, publication_nonce);
            prepare_catalog_publication_fence_v1(
                matched(&fixture, &observed),
                publication_nonce,
                archive,
                journal,
            )
            .unwrap()
        };

        assert_eq!(
            prepare_catalog_control_transition_v1(make_publication(), [0; 32])
                .err()
                .expect("zero revision must fail"),
            CatalogPublicationContractErrorV1::ControlTransitionMismatch
        );
        assert_eq!(
            prepare_catalog_control_transition_v1(make_publication(), [0xab; 32])
                .err()
                .expect("ready revision reuse must fail"),
            CatalogPublicationContractErrorV1::ControlTransitionMismatch
        );

        let mut overflow = make_publication();
        overflow
            .control_guard
            .high_water
            .binding
            .ready_revision
            .generation = NonZeroU64::new(u64::MAX).unwrap();
        overflow
            .control_guard
            .all_writers
            .control_binding
            .ready_revision
            .generation = NonZeroU64::new(u64::MAX).unwrap();
        overflow
            .control_guard
            .all_writers
            .retained_control_lease
            .control_binding
            .ready_revision
            .generation = NonZeroU64::new(u64::MAX).unwrap();
        assert_eq!(
            prepare_catalog_control_transition_v1(overflow, [0xac; 32])
                .err()
                .expect("control generation overflow must fail"),
            CatalogPublicationContractErrorV1::SequenceOverflow
        );

        let mut oversized = make_publication();
        let maximum_token = "v".repeat(
            usize::try_from(
                RegisteredRootPlanContractV1::strict_v1()
                    .remote_contract()
                    .max_binding_token_bytes(),
            )
            .unwrap(),
        );
        oversized.predecessor_archive.binding = RegisteredRootRemoteObjectBindingV1::Version {
            version: maximum_token.clone(),
            etag: Some(maximum_token.clone()),
        };
        oversized.mutation_journal.binding = RegisteredRootRemoteObjectBindingV1::Version {
            version: maximum_token.clone(),
            etag: Some(maximum_token),
        };
        assert_eq!(
            prepare_catalog_control_transition_v1(oversized, [0xac; 32])
                .err()
                .expect("an unreadable publishing HEAD must fail before reservation"),
            CatalogPublicationContractErrorV1::PublishingHeadTooLarge
        );

        let first = prepare_catalog_control_transition_v1(make_publication(), [0xac; 32]).unwrap();
        let mut other_baseline = make_publication();
        other_baseline.expected_parent_head_etag = "head-etag-b".to_owned();
        let second = prepare_catalog_control_transition_v1(other_baseline, [0xac; 32]).unwrap();
        assert_ne!(
            first.publishing_head_reservation_fingerprint,
            second.publishing_head_reservation_fingerprint,
            "the durable reservation must bind the exact predecessor storage CAS baseline"
        );
        assert_ne!(
            first.pending_control_record_fingerprint,
            second.pending_control_record_fingerprint
        );

        let mut transition = prepared_control_transition(&fixture, &observed);
        let receipt = pending_receipt(&transition);
        transition.canonical_publishing_head_bytes.push(b' ');
        assert_eq!(
            match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
            CatalogPublicationContractErrorV1::ControlTransitionMismatch
        );

        let mut transition = prepared_control_transition(&fixture, &observed);
        let receipt = pending_receipt(&transition);
        transition.canonical_pending_control_record_bytes.push(b' ');
        assert_eq!(
            match_catalog_publication_pending_receipt_v1(transition, receipt).unwrap_err(),
            CatalogPublicationContractErrorV1::ControlTransitionMismatch
        );
    }

    #[tokio::test]
    async fn high_water_advance_accepts_only_exact_monotonic_successor() {
        let (fixture, observed) = observed_head().await;
        let make_committed = || committed_successor(bound_pending(&fixture, &observed));

        let committed = make_committed();
        let receipt = high_water_advance_receipt(&committed);
        let ready_bytes =
            canonical_ready_control_record_bytes_v1(&committed, receipt.ready_revision);
        assert!(is_canonical_control_record_v1(
            &ready_bytes,
            &test_selected()
        ));
        let ready_text = std::str::from_utf8(&ready_bytes).unwrap();
        assert!(ready_text.contains("lease_public_fingerprint"));
        assert!(!ready_text.contains("lease_nonce"));
        assert!(!ready_text.contains("bearer"));

        let mut malformed_ready =
            serde_json::from_slice::<CatalogControlRecordWireV1>(&ready_bytes).unwrap();
        malformed_ready.bootstrap.writer_epoch = "00".repeat(32);
        assert!(!is_canonical_control_record_v1(
            &serde_json::to_vec(&malformed_ready).unwrap(),
            &test_selected()
        ));

        let mut wrong_bootstrap =
            serde_json::from_slice::<CatalogControlRecordWireV1>(&ready_bytes).unwrap();
        if let CatalogControlStateWireV1::Ready { current } = &mut wrong_bootstrap.control_state {
            current.catalog_sequence = 1;
        }
        assert!(!is_canonical_control_record_v1(
            &serde_json::to_vec(&wrong_bootstrap).unwrap(),
            &test_selected()
        ));
        let advanced = match_catalog_high_water_advance_v1(committed, receipt).unwrap();
        assert_eq!(advanced.successor.sequence.get(), 2);
        assert_eq!(advanced.ready_revision.generation.get(), 9);
        assert_ne!(advanced.ready_control_record_fingerprint, [0; 32]);
        assert_eq!(advanced.bootstrap.writer_epoch, [0x21; 32]);
        assert_eq!(advanced.storage_authority_fingerprint, [0x99; 32]);
        assert_eq!(advanced.control_authority_fingerprint, [0x9a; 32]);
        assert_eq!(advanced.context, observed.context);
        assert!(advanced.retained_control_lease.is_live_v1());

        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.successor.sequence = NonZeroU64::new(3).unwrap();
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.ready_revision.generation = NonZeroU64::new(10).unwrap();
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let mut committed = make_committed();
            committed.pending_revision.generation = NonZeroU64::new(10).unwrap();
            let receipt = high_water_advance_receipt(&committed);
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let receipt = high_water_advance_receipt(&committed);
            committed
                .control_guard
                .all_writers
                .retained_control_lease
                .test_live
                .store(false, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::ControlLeaseNotLive
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.ready_revision.fingerprint = committed.pending_revision.fingerprint;
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.ready_revision.fingerprint = committed
                .control_guard
                .high_water
                .binding
                .ready_revision
                .fingerprint;
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let mut committed = make_committed();
            let receipt = high_water_advance_receipt(&committed);
            committed.committed_head_bytes.push(b' ');
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.ready_control_record_fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let mut committed = make_committed();
            let receipt = high_water_advance_receipt(&committed);
            committed.parent_head_revision = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.successor.publication_nonce = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.successor.head_revision = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.publishing_head_reservation_fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.pending_control_record_fingerprint = [0x45; 32];
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            receipt.control_binding.lease_public_fingerprint =
                NonSecretLeasePublicFingerprintV1([0x45; 32]);
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterAdvanceMismatch
            );
        }
        {
            let mut committed = make_committed();
            let mut receipt = high_water_advance_receipt(&committed);
            committed.pending_revision.generation = NonZeroU64::new(u64::MAX).unwrap();
            receipt.pending_revision = committed.pending_revision;
            assert_eq!(
                match_catalog_high_water_advance_v1(committed, receipt).unwrap_err(),
                CatalogPublicationContractErrorV1::SequenceOverflow
            );
        }
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
            bootstrap.bootstrap.complete_corpus_attestation = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::InvalidBootstrapReceipt
            );
        }
        {
            let (mut bootstrap, mut high_water, mut writers) = trusted_receipts(&observed);
            bootstrap.bootstrap.head_revision = [0x55; 32];
            high_water.binding.bootstrap.head_revision = [0x55; 32];
            writers.control_binding.bootstrap.head_revision = [0x55; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::BootstrapMismatch,
                "sequence one must be the exact externally attested bootstrap head"
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.current.head_revision = [0x66; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.current.publication_nonce = [0x66; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.current.sequence =
                NonZeroU64::new(observed.sequence.get() + 1).unwrap();
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch,
                "a guard ahead of the observed HEAD identifies observed replay or rollback"
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.ready_revision.fingerprint = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::HighWaterMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.control_binding.bootstrap.writer_epoch = [0x77; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::WriterFenceMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.authority_revision_fingerprint = [0; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
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
        high_water.binding.current.sequence = NonZeroU64::new(1).unwrap();
        assert_eq!(
            match_catalog_publication_prerequisites_v1(
                trusted_storage(&fixture),
                &observed,
                &bootstrap,
                held_control(high_water, writers),
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
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.storage_authority_fingerprint = [0x98; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.control_binding.storage_authority_fingerprint = [0x98; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::StorageAuthorityMismatch
            );
        }
        {
            let (bootstrap, mut high_water, writers) = trusted_receipts(&observed);
            high_water.binding.control_authority_fingerprint = [0x9b; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
                )
                .unwrap_err(),
                CatalogPublicationContractErrorV1::ControlAuthorityMismatch
            );
        }
        {
            let (bootstrap, high_water, mut writers) = trusted_receipts(&observed);
            writers.control_binding.control_authority_fingerprint = [0x9b; 32];
            assert_eq!(
                match_catalog_publication_prerequisites_v1(
                    trusted_storage(&fixture),
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
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
                    crossed,
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
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
                    crossed,
                    &observed,
                    &bootstrap,
                    held_control(high_water, writers),
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
            control_ready_generation: 7,
            control_ready_revision_fingerprint: "ab".repeat(32),
            control_pending_generation: 8,
            control_pending_revision_fingerprint: "ac".repeat(32),
            control_lease_public_fingerprint: "aa".repeat(32),
            writer_fence_authority_revision_fingerprint: "89".repeat(32),
            writer_fence_lease_public_fingerprint: "88".repeat(32),
            predecessor_head_storage_binding_fingerprint: "87".repeat(32),
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
        wire.control_ready_revision_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.control_lease_public_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.writer_fence_authority_revision_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.writer_fence_lease_public_fingerprint = "00".repeat(32);
        reject(wire);
        let mut wire = fresh_wire();
        wire.predecessor_head_storage_binding_fingerprint = "00".repeat(32);
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
