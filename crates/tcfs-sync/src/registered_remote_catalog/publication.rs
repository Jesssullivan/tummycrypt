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
//! can validate and compose their exact bindings and publish the exact immutable
//! predecessor-HEAD archive. A separately namespaced journal draft exercises
//! bounded artifact mechanics but cannot become authoritative recovery evidence
//! or enter a publishing fence. This module still cannot mint authority, write
//! a live `HEAD`, mutate the namespace, produce a plan digest, or authorize an
//! action.

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
use crate::index_entry::{read_raw_object_snapshot_v1, RawObjectReadBindingV1, RawObjectReadV1};
use crate::registered_reconcile::{
    validate_registered_remote_storage_key_bounds_v1, RegisteredRootRemoteObjectBindingV1,
};
use crate::registered_source_composition::ValidatedSelectedRegisteredRootRemoteContextV1;
use tcfs_storage::ConditionalWriteSemanticsReceipt;

const ARCHIVED_HEAD_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-archived-head-object.b3v1";
const MUTATION_JOURNAL_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-mutation-journal-object.b3v1";
const UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_DOMAIN_V1: &str =
    "tinyland.tcfs.remote-catalog-mutation-journal-draft-object.b3v1";
const ARCHIVED_HEAD_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/archived-heads";
const MUTATION_JOURNAL_OBJECT_SUFFIX_V1: &str = ".tcfs-catalog/v1/publications/mutation-journals";
const UNTRUSTED_MUTATION_JOURNAL_DRAFT_OBJECT_SUFFIX_V1: &str =
    ".tcfs-catalog/v1/publications/mutation-journal-drafts";
const CATALOG_MUTATION_JOURNAL_DRAFT_SCHEMA_VERSION_V1: u32 = 1;

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
    PredecessorArchiveMismatch,
    MutationJournalMismatch,
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
/// A future production constructor must prove the complete intended mutation
/// set against the exact semantic predecessor corpus, prove create-key absence,
/// validate every typed successor and cross-object closure, and bind immutable
/// successor payload references. No such constructor or conversion from the
/// untrusted draft exists in this checkpoint. Without those payload references,
/// classification can only remain fenced; it cannot roll forward.
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
