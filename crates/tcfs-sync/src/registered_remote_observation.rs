//! Strict registered-root remote-observation primitives.
//!
//! The key-only repeated-listing artifact remains diagnostic and must be
//! discarded: matching non-atomic keysets are not a namespace snapshot, do not
//! satisfy `CompleteOrNoDigestV1`, and cannot mint a digest or plan input. The
//! bound reader separately performs fresh sequential pass A list+bind and pass
//! B list+bind work for every index, marker, reservation, and referenced
//! manifest. Matching bound evidence is still non-atomic observation evidence,
//! not planner authority.

use futures::TryStreamExt;
use opendal::{EntryMode, Operator};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use tcfs_core::config::{RegisteredRootPlanContractV1, RootRemoteContractV1};

use crate::blacklist::Blacklist;
use crate::index_entry::{
    namespace_claims_for_path, namespace_index_prefix, namespace_logical_entry_from_index_path,
    namespace_reservation_prefix, validate_canonical_namespace_remote_prefix,
    PortableNamespaceReservationV1, PortableNamespaceRole,
};
use crate::registered_reconcile::{
    read_exact_observed_namespace_reservation_v1, read_exact_observed_raw_directory_marker_v1,
    read_exact_observed_raw_index_entry_v1, read_observed_strict_remote_manifest_for_references_v1,
    validate_registered_remote_logical_path_bounds_v1, BoundNamespaceReservationV1,
    BoundRemoteObjectSnapshotV1, ExactObservedNamespaceReservationReadV1,
    ExactObservedRawDirectoryMarkerReadV1, ExactObservedRawIndexEntryReadV1,
    RawCommittedIndexEntryV1, RawDeletedDirectoryMarkerV1, RawDeletedIndexEntryV1,
    RawLiveDirectoryMarkerV1, RegisteredRootRemoteObjectBindingV1,
    StrictNamespaceReservationIncompleteV1, StrictObservedRemoteManifestReadV1,
    StrictRemoteDirectoryMarkerIncompleteV1, StrictRemoteIndexIncompleteV1,
    StrictRemoteManifestIncompleteV1, StrictRemoteManifestV1,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteNamespaceCorpusV1 {
    Index,
    Reservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteNamespaceListingPassV1 {
    First,
    Second,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteNamespaceKeyResourceV1 {
    ListingRows,
    ListingKeyBytes,
    StorageKeyBytes,
    IndexObjects,
    IndexKeyBytes,
    ReservationObjects,
    ReservationKeyBytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvalidRemoteNamespaceKeyReasonV1 {
    DeletedListingEntry,
    NonCurrentListingEntry,
    InvalidEntryMode,
    ModePathMismatch,
    OutsideRequestedPrefix,
    EmptyObjectSuffix,
    InvalidIndexPath,
    InvalidReservationObjectId,
    UnexpectedReservationDirectory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StrictRemoteNamespaceKeyIncompleteV1 {
    InvalidRemotePrefix,
    UnsupportedListing {
        pass: RemoteNamespaceListingPassV1,
    },
    ListingFailed {
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
    },
    ResourceLimit {
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
        resource: RemoteNamespaceKeyResourceV1,
    },
    InvalidEntry {
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
        reason: InvalidRemoteNamespaceKeyReasonV1,
    },
    DuplicateObjectKey {
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
    },
    PassesDisagreed {
        index: bool,
        reservations: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ListedRemoteIndexKeyClassV1 {
    OrdinaryIndexObject,
    DirectoryMarkerCandidate,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ListedRemoteIndexKeyV1 {
    object_key: String,
    index_rel_offset: usize,
    logical_rel_len: usize,
    class: ListedRemoteIndexKeyClassV1,
}

impl ListedRemoteIndexKeyV1 {
    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }

    pub(crate) fn index_rel_path(&self) -> &str {
        self.object_key
            .get(self.index_rel_offset..)
            .expect("listed index-key offset is an internal invariant")
    }

    pub(crate) fn logical_path(&self) -> &str {
        let end = self
            .index_rel_offset
            .checked_add(self.logical_rel_len)
            .expect("listed logical-path length is an internal invariant");
        self.object_key
            .get(self.index_rel_offset..end)
            .expect("listed logical-path bounds are an internal invariant")
    }

    pub(crate) const fn class(&self) -> ListedRemoteIndexKeyClassV1 {
        self.class
    }
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ListedRemoteReservationKeyV1 {
    object_key: String,
    object_id_offset: usize,
}

impl ListedRemoteReservationKeyV1 {
    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }

    pub(crate) fn object_id(&self) -> &str {
        self.object_key
            .get(self.object_id_offset..)
            .expect("listed reservation-key offset is an internal invariant")
    }
}

/// Matching classified FILE-object keys from two fully drained listings only.
///
/// Synthetic DIR rows are validated in each pass but excluded from the object
/// comparison. This is intentionally not named an observation snapshot or
/// stable namespace. It carries no fingerprint, is not cloneable, and has no
/// conversion into a complete planner input.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct MatchingTwoPassListedRemoteKeysV1 {
    remote_prefix: String,
    index_objects: Vec<ListedRemoteIndexKeyV1>,
    reservation_objects: Vec<ListedRemoteReservationKeyV1>,
}

impl MatchingTwoPassListedRemoteKeysV1 {
    pub(crate) fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    pub(crate) fn index_objects(&self) -> &[ListedRemoteIndexKeyV1] {
        &self.index_objects
    }

    pub(crate) fn reservation_objects(&self) -> &[ListedRemoteReservationKeyV1] {
        &self.reservation_objects
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum StrictRemoteNamespaceKeyReadV1 {
    Matched(MatchingTwoPassListedRemoteKeysV1),
    Incomplete(StrictRemoteNamespaceKeyIncompleteV1),
}

#[derive(Debug, PartialEq, Eq)]
enum BoundRemoteIndexObjectV1 {
    Committed(Box<RawCommittedIndexEntryV1>),
    Deleted(Box<RawDeletedIndexEntryV1>),
    LiveMarker(Box<RawLiveDirectoryMarkerV1>),
    DeletedMarker(Box<RawDeletedDirectoryMarkerV1>),
}

impl BoundRemoteIndexObjectV1 {
    fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        match self {
            Self::Committed(index) => index.object(),
            Self::Deleted(index) => index.object(),
            Self::LiveMarker(marker) => marker.object(),
            Self::DeletedMarker(marker) => marker.object(),
        }
    }

    fn logical_path(&self) -> &str {
        match self {
            Self::Committed(index) => index.rel_path(),
            Self::Deleted(index) => index.route().rel_path(),
            Self::LiveMarker(marker) => marker.route().logical_dir(),
            Self::DeletedMarker(marker) => marker.route().logical_dir(),
        }
    }

    fn physical_key(&self) -> &str {
        match self {
            Self::Committed(index) => index.index_key(),
            Self::Deleted(index) => index.route().index_key(),
            Self::LiveMarker(marker) => marker.route().marker_key(),
            Self::DeletedMarker(marker) => marker.route().marker_key(),
        }
    }

    const fn role(&self) -> PortableNamespaceRole {
        match self {
            Self::Committed(_) | Self::Deleted(_) => PortableNamespaceRole::File,
            Self::LiveMarker(_) | Self::DeletedMarker(_) => PortableNamespaceRole::Directory,
        }
    }

    fn committed(&self) -> Option<&RawCommittedIndexEntryV1> {
        match self {
            Self::Committed(index) => Some(index),
            Self::Deleted(_) | Self::LiveMarker(_) | Self::DeletedMarker(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BoundRemoteManifestObservationV1 {
    /// Ordinal of the first committed index reference in the canonical
    /// physical-index ordering. The manifest ID is available from that index,
    /// so the full storage key is not duplicated per retained manifest.
    source_index_ordinal: usize,
    manifest: Box<StrictRemoteManifestV1>,
}

#[derive(Debug, PartialEq, Eq)]
struct RetainedRemoteNamespaceClaimV1 {
    folded_path: String,
    exact_path: String,
    role: PortableNamespaceRole,
}

/// One sequential list-and-bind pass over a registered remote namespace.
///
/// This is intentionally private, non-cloneable, and non-serializable. It is
/// observation evidence only and has no digest or planner conversion.
#[derive(Debug, PartialEq, Eq)]
struct FullyDrainedBoundRemotePassV1 {
    index_objects: Vec<BoundRemoteIndexObjectV1>,
    reservations: Vec<BoundNamespaceReservationV1>,
    manifests: Vec<BoundRemoteManifestObservationV1>,
    claims: Vec<RetainedRemoteNamespaceClaimV1>,
}

/// Matching evidence from two fresh, fully drained, identity-bound passes.
///
/// Equality still does not imply a transactionally complete remote snapshot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MatchingTwoPassBoundRemoteEvidenceV1 {
    remote_prefix: String,
    evidence: FullyDrainedBoundRemotePassV1,
}

impl MatchingTwoPassBoundRemoteEvidenceV1 {
    pub(crate) fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    pub(crate) fn index_object_count(&self) -> usize {
        self.evidence.index_objects.len()
    }

    pub(crate) fn reservation_count(&self) -> usize {
        self.evidence.reservations.len()
    }

    pub(crate) fn manifest_count(&self) -> usize {
        self.evidence.manifests.len()
    }

    pub(crate) fn claim_count(&self) -> usize {
        self.evidence.claims.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundRemoteObjectKindV1 {
    OrdinaryIndex,
    DirectoryMarker,
    NamespaceReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundRemotePassResourceV1 {
    BoundObjectBytes,
    RetainedBindingBytes,
    GeneratedClaims,
    GeneratedClaimBytes,
    RetainedClaims,
    RetainedClaimBytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteNamespaceClaimConflictV1 {
    InvalidPath,
    FoldedSpellingAlias,
    FileDirectoryRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StrictBoundRemoteObservationIncompleteV1 {
    InvalidRemotePrefix,
    Listing(StrictRemoteNamespaceKeyIncompleteV1),
    ListedObjectMissing {
        pass: RemoteNamespaceListingPassV1,
        kind: BoundRemoteObjectKindV1,
    },
    ListedRouteMismatch {
        pass: RemoteNamespaceListingPassV1,
        kind: BoundRemoteObjectKindV1,
    },
    Index {
        pass: RemoteNamespaceListingPassV1,
        reason: StrictRemoteIndexIncompleteV1,
    },
    Marker {
        pass: RemoteNamespaceListingPassV1,
        reason: StrictRemoteDirectoryMarkerIncompleteV1,
    },
    LiveMarkerExcluded {
        pass: RemoteNamespaceListingPassV1,
    },
    Reservation {
        pass: RemoteNamespaceListingPassV1,
        reason: StrictNamespaceReservationIncompleteV1,
    },
    Manifest {
        pass: RemoteNamespaceListingPassV1,
        reason: StrictRemoteManifestIncompleteV1,
    },
    Claim {
        pass: RemoteNamespaceListingPassV1,
        reason: RemoteNamespaceClaimConflictV1,
    },
    ResourceLimit {
        pass: RemoteNamespaceListingPassV1,
        resource: BoundRemotePassResourceV1,
    },
    PassesDisagreed {
        index_objects: bool,
        reservations: bool,
        manifests: bool,
        claims: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StrictBoundRemoteObservationReadV1 {
    Matched(MatchingTwoPassBoundRemoteEvidenceV1),
    Incomplete(StrictBoundRemoteObservationIncompleteV1),
}

#[derive(Debug, Default, Eq, PartialEq)]
struct RemoteNamespaceKeyPassV1 {
    index_objects: BTreeSet<ListedRemoteIndexKeyV1>,
    reservation_objects: BTreeSet<ListedRemoteReservationKeyV1>,
    index_directory_rows: BTreeSet<String>,
    reservation_directory_rows: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct RemoteListingBudgetV1 {
    listing_rows: u64,
    listing_key_bytes: u64,
    index_objects: u64,
    index_key_bytes: u64,
    reservation_objects: u64,
    reservation_key_bytes: u64,
}

impl RemoteListingBudgetV1 {
    fn checked_increment(
        value: &mut u64,
        increment: u64,
        maximum: u64,
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
        resource: RemoteNamespaceKeyResourceV1,
    ) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
        *value = value.checked_add(increment).ok_or(
            StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass,
                corpus,
                resource,
            },
        )?;
        if *value > maximum {
            return Err(StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass,
                corpus,
                resource,
            });
        }
        Ok(())
    }

    fn observe_raw_row(
        &mut self,
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
        key: &str,
        contract: RootRemoteContractV1,
    ) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
        Self::checked_increment(
            &mut self.listing_rows,
            1,
            contract.max_listing_rows_per_pass(),
            pass,
            corpus,
            RemoteNamespaceKeyResourceV1::ListingRows,
        )?;
        Self::checked_increment(
            &mut self.listing_key_bytes,
            u64::try_from(key.len()).map_err(|_| {
                StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                    pass,
                    corpus,
                    resource: RemoteNamespaceKeyResourceV1::ListingKeyBytes,
                }
            })?,
            contract.max_listing_key_bytes_per_pass(),
            pass,
            corpus,
            RemoteNamespaceKeyResourceV1::ListingKeyBytes,
        )?;
        if u64::try_from(key.len()).map_or(true, |length| length > contract.max_storage_key_bytes())
        {
            return Err(StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass,
                corpus,
                resource: RemoteNamespaceKeyResourceV1::StorageKeyBytes,
            });
        }
        Ok(())
    }

    fn observe_corpus_object(
        &mut self,
        pass: RemoteNamespaceListingPassV1,
        corpus: RemoteNamespaceCorpusV1,
        key: &str,
        contract: RootRemoteContractV1,
    ) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
        let key_bytes = u64::try_from(key.len()).map_err(|_| {
            StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass,
                corpus,
                resource: RemoteNamespaceKeyResourceV1::ListingKeyBytes,
            }
        })?;
        match corpus {
            RemoteNamespaceCorpusV1::Index => {
                Self::checked_increment(
                    &mut self.index_objects,
                    1,
                    contract.max_index_observations_per_pass(),
                    pass,
                    corpus,
                    RemoteNamespaceKeyResourceV1::IndexObjects,
                )?;
                Self::checked_increment(
                    &mut self.index_key_bytes,
                    key_bytes,
                    contract.max_retained_index_key_bytes_per_pass(),
                    pass,
                    corpus,
                    RemoteNamespaceKeyResourceV1::IndexKeyBytes,
                )
            }
            RemoteNamespaceCorpusV1::Reservation => {
                Self::checked_increment(
                    &mut self.reservation_objects,
                    1,
                    contract.max_reservation_observations_per_pass(),
                    pass,
                    corpus,
                    RemoteNamespaceKeyResourceV1::ReservationObjects,
                )?;
                Self::checked_increment(
                    &mut self.reservation_key_bytes,
                    key_bytes,
                    contract.max_retained_reservation_key_bytes_per_pass(),
                    pass,
                    corpus,
                    RemoteNamespaceKeyResourceV1::ReservationKeyBytes,
                )
            }
        }
    }
}

#[derive(Debug, Default)]
struct BoundRemotePassBudgetV1 {
    bound_object_bytes: u64,
    retained_binding_bytes: u64,
    generated_claims: u64,
    generated_claim_bytes: u64,
    retained_claims: u64,
    retained_claim_bytes: u64,
}

#[derive(Debug)]
struct RetainedRemoteNamespaceClaimValueV1 {
    exact_path: String,
    role: PortableNamespaceRole,
}

fn bound_pass_resource_limit(
    pass: RemoteNamespaceListingPassV1,
    resource: BoundRemotePassResourceV1,
) -> StrictBoundRemoteObservationIncompleteV1 {
    StrictBoundRemoteObservationIncompleteV1::ResourceLimit { pass, resource }
}

fn checked_bound_pass_increment_v1(
    value: &mut u64,
    increment: u64,
    maximum: u64,
    pass: RemoteNamespaceListingPassV1,
    resource: BoundRemotePassResourceV1,
) -> Result<(), StrictBoundRemoteObservationIncompleteV1> {
    *value = value
        .checked_add(increment)
        .ok_or_else(|| bound_pass_resource_limit(pass, resource))?;
    if *value > maximum {
        return Err(bound_pass_resource_limit(pass, resource));
    }
    Ok(())
}

fn binding_bytes_v1(
    object: &BoundRemoteObjectSnapshotV1,
    pass: RemoteNamespaceListingPassV1,
) -> Result<u64, StrictBoundRemoteObservationIncompleteV1> {
    let token_bytes = match object.binding() {
        RegisteredRootRemoteObjectBindingV1::Version { version, etag } => version
            .len()
            .checked_add(etag.as_ref().map_or(0, String::len)),
        RegisteredRootRemoteObjectBindingV1::Etag { etag } => Some(etag.len()),
    }
    .ok_or_else(|| {
        bound_pass_resource_limit(pass, BoundRemotePassResourceV1::RetainedBindingBytes)
    })?;
    u64::try_from(token_bytes).map_err(|_| {
        bound_pass_resource_limit(pass, BoundRemotePassResourceV1::RetainedBindingBytes)
    })
}

impl BoundRemotePassBudgetV1 {
    fn observe_object_accounting(
        &mut self,
        pass: RemoteNamespaceListingPassV1,
        raw_bytes: u64,
        binding_bytes: u64,
        contract: RootRemoteContractV1,
    ) -> Result<(), StrictBoundRemoteObservationIncompleteV1> {
        checked_bound_pass_increment_v1(
            &mut self.bound_object_bytes,
            raw_bytes,
            contract.max_bound_object_bytes_per_pass(),
            pass,
            BoundRemotePassResourceV1::BoundObjectBytes,
        )?;
        checked_bound_pass_increment_v1(
            &mut self.retained_binding_bytes,
            binding_bytes,
            contract.max_retained_binding_bytes_per_pass(),
            pass,
            BoundRemotePassResourceV1::RetainedBindingBytes,
        )
    }

    fn observe_object(
        &mut self,
        pass: RemoteNamespaceListingPassV1,
        object: &BoundRemoteObjectSnapshotV1,
        contract: RootRemoteContractV1,
    ) -> Result<(), StrictBoundRemoteObservationIncompleteV1> {
        self.observe_object_accounting(
            pass,
            object.raw_bytes_len(),
            binding_bytes_v1(object, pass)?,
            contract,
        )
    }

    fn observe_claim(
        &mut self,
        pass: RemoteNamespaceListingPassV1,
        claim: &PortableNamespaceReservationV1,
        claims: &mut BTreeMap<String, RetainedRemoteNamespaceClaimValueV1>,
        contract: RootRemoteContractV1,
    ) -> Result<(), StrictBoundRemoteObservationIncompleteV1> {
        let claim_bytes = claim
            .exact_path()
            .len()
            .checked_add(claim.folded_path().len())
            .and_then(|length| u64::try_from(length).ok())
            .ok_or_else(|| {
                bound_pass_resource_limit(pass, BoundRemotePassResourceV1::GeneratedClaimBytes)
            })?;
        checked_bound_pass_increment_v1(
            &mut self.generated_claims,
            1,
            contract.max_generated_claim_observations_per_pass(),
            pass,
            BoundRemotePassResourceV1::GeneratedClaims,
        )?;
        checked_bound_pass_increment_v1(
            &mut self.generated_claim_bytes,
            claim_bytes,
            contract.max_generated_claim_bytes_per_pass(),
            pass,
            BoundRemotePassResourceV1::GeneratedClaimBytes,
        )?;

        if let Some(existing) = claims.get(claim.folded_path()) {
            if existing.exact_path != claim.exact_path() {
                return Err(StrictBoundRemoteObservationIncompleteV1::Claim {
                    pass,
                    reason: RemoteNamespaceClaimConflictV1::FoldedSpellingAlias,
                });
            }
            if existing.role != claim.role() {
                return Err(StrictBoundRemoteObservationIncompleteV1::Claim {
                    pass,
                    reason: RemoteNamespaceClaimConflictV1::FileDirectoryRole,
                });
            }
            return Ok(());
        }

        checked_bound_pass_increment_v1(
            &mut self.retained_claims,
            1,
            contract.max_retained_unique_claims_per_pass(),
            pass,
            BoundRemotePassResourceV1::RetainedClaims,
        )?;
        checked_bound_pass_increment_v1(
            &mut self.retained_claim_bytes,
            claim_bytes,
            contract.max_retained_unique_claim_bytes_per_pass(),
            pass,
            BoundRemotePassResourceV1::RetainedClaimBytes,
        )?;
        claims.insert(
            claim.folded_path().to_owned(),
            RetainedRemoteNamespaceClaimValueV1 {
                exact_path: claim.exact_path().to_owned(),
                role: claim.role(),
            },
        );
        Ok(())
    }
}

fn observe_claim_chain_v1(
    budget: &mut BoundRemotePassBudgetV1,
    pass: RemoteNamespaceListingPassV1,
    claims: &mut BTreeMap<String, RetainedRemoteNamespaceClaimValueV1>,
    rel_path: &str,
    role: PortableNamespaceRole,
    contract: RootRemoteContractV1,
) -> Result<(), StrictBoundRemoteObservationIncompleteV1> {
    let generated = namespace_claims_for_path(rel_path, role).map_err(|_| {
        StrictBoundRemoteObservationIncompleteV1::Claim {
            pass,
            reason: RemoteNamespaceClaimConflictV1::InvalidPath,
        }
    })?;
    for claim in &generated {
        budget.observe_claim(pass, claim, claims, contract)?;
    }
    Ok(())
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn namespace_prefix_storage_bytes(remote_prefix: &str, suffix: &str) -> Option<u64> {
    let remote_bytes = u64::try_from(remote_prefix.len()).ok()?;
    let suffix_bytes = u64::try_from(suffix.len()).ok()?;
    remote_bytes
        .checked_add(u64::from(!remote_prefix.is_empty()))?
        .checked_add(suffix_bytes)?
        .checked_add(1)
}

fn invalid_entry(
    pass: RemoteNamespaceListingPassV1,
    corpus: RemoteNamespaceCorpusV1,
    reason: InvalidRemoteNamespaceKeyReasonV1,
) -> StrictRemoteNamespaceKeyIncompleteV1 {
    StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
        pass,
        corpus,
        reason,
    }
}

fn validate_listing_metadata_v1(
    listing_pass: RemoteNamespaceListingPassV1,
    corpus: RemoteNamespaceCorpusV1,
    is_deleted: bool,
    is_current: Option<bool>,
) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
    if is_deleted {
        return Err(invalid_entry(
            listing_pass,
            corpus,
            InvalidRemoteNamespaceKeyReasonV1::DeletedListingEntry,
        ));
    }
    if is_current == Some(false) {
        return Err(invalid_entry(
            listing_pass,
            corpus,
            InvalidRemoteNamespaceKeyReasonV1::NonCurrentListingEntry,
        ));
    }
    Ok(())
}

fn record_unique_directory_row_v1(
    key_pass: &mut RemoteNamespaceKeyPassV1,
    listing_pass: RemoteNamespaceListingPassV1,
    corpus: RemoteNamespaceCorpusV1,
    key: &str,
) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
    let seen_rows = match corpus {
        RemoteNamespaceCorpusV1::Index => &mut key_pass.index_directory_rows,
        RemoteNamespaceCorpusV1::Reservation => &mut key_pass.reservation_directory_rows,
    };
    if !seen_rows.insert(key.to_owned()) {
        return Err(StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
            pass: listing_pass,
            corpus,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_listing_row_v1(
    key_pass: &mut RemoteNamespaceKeyPassV1,
    budget: &mut RemoteListingBudgetV1,
    listing_pass: RemoteNamespaceListingPassV1,
    corpus: RemoteNamespaceCorpusV1,
    requested_prefix: &str,
    key: &str,
    mode: EntryMode,
    is_deleted: bool,
    is_current: Option<bool>,
    contract: RootRemoteContractV1,
) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
    // This is deliberately first: malformed, duplicate, synthetic-directory,
    // and out-of-prefix entries all consume the raw backend budget.
    budget.observe_raw_row(listing_pass, corpus, key, contract)?;

    match mode {
        EntryMode::DIR => {
            if !key.ends_with('/') {
                return Err(invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::ModePathMismatch,
                ));
            }
            validate_listing_metadata_v1(listing_pass, corpus, is_deleted, is_current)?;
            let suffix = key.strip_prefix(requested_prefix).ok_or_else(|| {
                invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::OutsideRequestedPrefix,
                )
            })?;
            if corpus == RemoteNamespaceCorpusV1::Reservation && !suffix.is_empty() {
                return Err(invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::UnexpectedReservationDirectory,
                ));
            }
            if corpus == RemoteNamespaceCorpusV1::Index && !suffix.is_empty() {
                let logical_dir = suffix.strip_suffix('/').ok_or_else(|| {
                    invalid_entry(
                        listing_pass,
                        corpus,
                        InvalidRemoteNamespaceKeyReasonV1::ModePathMismatch,
                    )
                })?;
                if logical_dir.is_empty()
                    || validate_registered_remote_logical_path_bounds_v1(logical_dir).is_err()
                {
                    return Err(invalid_entry(
                        listing_pass,
                        corpus,
                        InvalidRemoteNamespaceKeyReasonV1::InvalidIndexPath,
                    ));
                }
            }
            record_unique_directory_row_v1(key_pass, listing_pass, corpus, key)?;
            return Ok(());
        }
        EntryMode::FILE => {
            if key.ends_with('/') {
                return Err(invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::ModePathMismatch,
                ));
            }
        }
        EntryMode::Unknown => {
            return Err(invalid_entry(
                listing_pass,
                corpus,
                InvalidRemoteNamespaceKeyReasonV1::InvalidEntryMode,
            ));
        }
    }

    // Corpus-specific resources count every file-shaped row before prefix,
    // metadata, grammar, or duplicate rejection.
    budget.observe_corpus_object(listing_pass, corpus, key, contract)?;
    validate_listing_metadata_v1(listing_pass, corpus, is_deleted, is_current)?;
    let suffix = key.strip_prefix(requested_prefix).ok_or_else(|| {
        invalid_entry(
            listing_pass,
            corpus,
            InvalidRemoteNamespaceKeyReasonV1::OutsideRequestedPrefix,
        )
    })?;
    if suffix.is_empty() {
        return Err(invalid_entry(
            listing_pass,
            corpus,
            InvalidRemoteNamespaceKeyReasonV1::EmptyObjectSuffix,
        ));
    }

    match corpus {
        RemoteNamespaceCorpusV1::Index => {
            let (logical_path, logical_role) = namespace_logical_entry_from_index_path(suffix)
                .map_err(|_| {
                    invalid_entry(
                        listing_pass,
                        corpus,
                        InvalidRemoteNamespaceKeyReasonV1::InvalidIndexPath,
                    )
                })?;
            validate_registered_remote_logical_path_bounds_v1(&logical_path).map_err(|_| {
                invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::InvalidIndexPath,
                )
            })?;
            let index_rel_offset = requested_prefix.len();
            let logical_rel_len = logical_path.len();
            let listed_key = ListedRemoteIndexKeyV1 {
                object_key: key.to_owned(),
                index_rel_offset,
                logical_rel_len,
                class: match logical_role {
                    PortableNamespaceRole::File => ListedRemoteIndexKeyClassV1::OrdinaryIndexObject,
                    PortableNamespaceRole::Directory => {
                        ListedRemoteIndexKeyClassV1::DirectoryMarkerCandidate
                    }
                },
            };
            if !key_pass.index_objects.insert(listed_key) {
                return Err(StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
                    pass: listing_pass,
                    corpus,
                });
            }
        }
        RemoteNamespaceCorpusV1::Reservation => {
            if !is_lower_hex_64(suffix) {
                return Err(invalid_entry(
                    listing_pass,
                    corpus,
                    InvalidRemoteNamespaceKeyReasonV1::InvalidReservationObjectId,
                ));
            }
            let listed_key = ListedRemoteReservationKeyV1 {
                object_key: key.to_owned(),
                object_id_offset: requested_prefix.len(),
            };
            if !key_pass.reservation_objects.insert(listed_key) {
                return Err(StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
                    pass: listing_pass,
                    corpus,
                });
            }
        }
    }
    Ok(())
}

async fn list_corpus_v1(
    op: &Operator,
    pass: &mut RemoteNamespaceKeyPassV1,
    budget: &mut RemoteListingBudgetV1,
    listing_pass: RemoteNamespaceListingPassV1,
    corpus: RemoteNamespaceCorpusV1,
    requested_prefix: &str,
    contract: RootRemoteContractV1,
) -> Result<(), StrictRemoteNamespaceKeyIncompleteV1> {
    let limit = usize::try_from(contract.listing_page_request_limit()).map_err(|_| {
        StrictRemoteNamespaceKeyIncompleteV1::ListingFailed {
            pass: listing_pass,
            corpus,
        }
    })?;
    // `limit` is only a per-request backend hint. OpenDAL's CompleteLayer can
    // emulate recursive listing and a backend can ignore the hint; the manual
    // aggregate row/key ceilings above are the only safety boundary.
    let request = op
        .lister_with(requested_prefix)
        .recursive(true)
        .limit(limit)
        .versions(false)
        .deleted(false);
    let mut lister =
        request
            .await
            .map_err(|_| StrictRemoteNamespaceKeyIncompleteV1::ListingFailed {
                pass: listing_pass,
                corpus,
            })?;
    loop {
        let entry = match lister.try_next().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(_) => {
                return Err(StrictRemoteNamespaceKeyIncompleteV1::ListingFailed {
                    pass: listing_pass,
                    corpus,
                });
            }
        };
        observe_listing_row_v1(
            pass,
            budget,
            listing_pass,
            corpus,
            requested_prefix,
            entry.path(),
            entry.metadata().mode(),
            entry.metadata().is_deleted(),
            entry.metadata().is_current(),
            contract,
        )?;
    }
    Ok(())
}

async fn read_remote_namespace_key_pass_v1(
    op: &Operator,
    remote_prefix: &str,
    listing_pass: RemoteNamespaceListingPassV1,
    contract: RootRemoteContractV1,
) -> Result<RemoteNamespaceKeyPassV1, StrictRemoteNamespaceKeyIncompleteV1> {
    for (corpus, suffix) in [
        (RemoteNamespaceCorpusV1::Index, "index"),
        (RemoteNamespaceCorpusV1::Reservation, ".tcfs-namespace/v1"),
    ] {
        if namespace_prefix_storage_bytes(remote_prefix, suffix)
            .is_none_or(|length| length > contract.max_storage_key_bytes())
        {
            return Err(StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass: listing_pass,
                corpus,
                resource: RemoteNamespaceKeyResourceV1::StorageKeyBytes,
            });
        }
    }
    let capability = op.info().full_capability();
    if !capability.list {
        return Err(StrictRemoteNamespaceKeyIncompleteV1::UnsupportedListing {
            pass: listing_pass,
        });
    }
    // Lengths were checked before either allocation, so an oversized
    // caller-supplied prefix cannot be cloned into requested-prefix strings.
    let index_prefix = namespace_index_prefix(remote_prefix);
    let reservation_prefix = namespace_reservation_prefix(remote_prefix);

    let mut pass = RemoteNamespaceKeyPassV1::default();
    let mut budget = RemoteListingBudgetV1::default();
    list_corpus_v1(
        op,
        &mut pass,
        &mut budget,
        listing_pass,
        RemoteNamespaceCorpusV1::Index,
        &index_prefix,
        contract,
    )
    .await?;
    // Seen rows exist only to reject duplicates within this one lister. Drop
    // their strings before opening the next corpus or retaining this pass.
    pass.index_directory_rows.clear();
    list_corpus_v1(
        op,
        &mut pass,
        &mut budget,
        listing_pass,
        RemoteNamespaceCorpusV1::Reservation,
        &reservation_prefix,
        contract,
    )
    .await?;
    pass.reservation_directory_rows.clear();
    Ok(pass)
}

async fn read_fully_drained_bound_remote_pass_v1(
    op: &Operator,
    remote_prefix: &str,
    listing_pass: RemoteNamespaceListingPassV1,
    contract: RootRemoteContractV1,
) -> anyhow::Result<Result<FullyDrainedBoundRemotePassV1, StrictBoundRemoteObservationIncompleteV1>>
{
    let listed =
        match read_remote_namespace_key_pass_v1(op, remote_prefix, listing_pass, contract).await {
            Ok(listed) => listed,
            Err(incomplete) => {
                return Ok(Err(StrictBoundRemoteObservationIncompleteV1::Listing(
                    incomplete,
                )));
            }
        };
    let RemoteNamespaceKeyPassV1 {
        index_objects: listed_index_objects,
        reservation_objects: listed_reservation_objects,
        index_directory_rows: _,
        reservation_directory_rows: _,
    } = listed;

    let mut budget = BoundRemotePassBudgetV1::default();
    let mut claims = BTreeMap::<String, RetainedRemoteNamespaceClaimValueV1>::new();
    let mut index_objects = Vec::with_capacity(listed_index_objects.len());

    for listed in listed_index_objects {
        let (kind, observed) = match listed.class() {
            ListedRemoteIndexKeyClassV1::OrdinaryIndexObject => {
                let read = read_exact_observed_raw_index_entry_v1(
                    op,
                    remote_prefix,
                    listed.logical_path(),
                )
                .await?;
                let observed = match read {
                    ExactObservedRawIndexEntryReadV1::Missing => {
                        return Ok(Err(
                            StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                                pass: listing_pass,
                                kind: BoundRemoteObjectKindV1::OrdinaryIndex,
                            },
                        ));
                    }
                    ExactObservedRawIndexEntryReadV1::Incomplete {
                        reason,
                        observed_object,
                    } => {
                        if let Some(object) = observed_object {
                            if let Err(incomplete) =
                                budget.observe_object(listing_pass, &object, contract)
                            {
                                return Ok(Err(incomplete));
                            }
                        }
                        return Ok(Err(StrictBoundRemoteObservationIncompleteV1::Index {
                            pass: listing_pass,
                            reason,
                        }));
                    }
                    ExactObservedRawIndexEntryReadV1::Deleted(index) => {
                        BoundRemoteIndexObjectV1::Deleted(Box::new(index))
                    }
                    ExactObservedRawIndexEntryReadV1::Committed(index) => {
                        BoundRemoteIndexObjectV1::Committed(Box::new(index))
                    }
                };
                (BoundRemoteObjectKindV1::OrdinaryIndex, observed)
            }
            ListedRemoteIndexKeyClassV1::DirectoryMarkerCandidate => {
                let read = read_exact_observed_raw_directory_marker_v1(
                    op,
                    remote_prefix,
                    listed.logical_path(),
                )
                .await?;
                let observed = match read {
                    ExactObservedRawDirectoryMarkerReadV1::Missing => {
                        return Ok(Err(
                            StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                                pass: listing_pass,
                                kind: BoundRemoteObjectKindV1::DirectoryMarker,
                            },
                        ));
                    }
                    ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                        reason,
                        observed_object,
                    } => {
                        if let Some(object) = observed_object {
                            if let Err(incomplete) =
                                budget.observe_object(listing_pass, &object, contract)
                            {
                                return Ok(Err(incomplete));
                            }
                        }
                        return Ok(Err(StrictBoundRemoteObservationIncompleteV1::Marker {
                            pass: listing_pass,
                            reason,
                        }));
                    }
                    ExactObservedRawDirectoryMarkerReadV1::Live(marker) => {
                        BoundRemoteIndexObjectV1::LiveMarker(Box::new(marker))
                    }
                    ExactObservedRawDirectoryMarkerReadV1::Deleted(marker) => {
                        BoundRemoteIndexObjectV1::DeletedMarker(Box::new(marker))
                    }
                };
                (BoundRemoteObjectKindV1::DirectoryMarker, observed)
            }
        };
        if observed.physical_key() != listed.object_key() {
            return Ok(Err(
                StrictBoundRemoteObservationIncompleteV1::ListedRouteMismatch {
                    pass: listing_pass,
                    kind,
                },
            ));
        }
        if let Err(incomplete) = budget.observe_object(listing_pass, observed.object(), contract) {
            return Ok(Err(incomplete));
        }
        if matches!(&observed, BoundRemoteIndexObjectV1::LiveMarker(_))
            && Blacklist::default()
                .check_fixed_ingress_path_components(Path::new(observed.logical_path()))
                .is_some()
        {
            return Ok(Err(
                StrictBoundRemoteObservationIncompleteV1::LiveMarkerExcluded { pass: listing_pass },
            ));
        }
        if let Err(incomplete) = observe_claim_chain_v1(
            &mut budget,
            listing_pass,
            &mut claims,
            observed.logical_path(),
            observed.role(),
            contract,
        ) {
            return Ok(Err(incomplete));
        }
        index_objects.push(observed);
    }

    let mut reservations = Vec::with_capacity(listed_reservation_objects.len());
    for listed in listed_reservation_objects {
        let reservation = match read_exact_observed_namespace_reservation_v1(
            op,
            remote_prefix,
            listed.object_id(),
        )
        .await?
        {
            ExactObservedNamespaceReservationReadV1::Missing => {
                return Ok(Err(
                    StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                        pass: listing_pass,
                        kind: BoundRemoteObjectKindV1::NamespaceReservation,
                    },
                ));
            }
            ExactObservedNamespaceReservationReadV1::Incomplete {
                reason,
                observed_object,
            } => {
                if let Some(object) = observed_object {
                    if let Err(incomplete) = budget.observe_object(listing_pass, &object, contract)
                    {
                        return Ok(Err(incomplete));
                    }
                }
                return Ok(Err(StrictBoundRemoteObservationIncompleteV1::Reservation {
                    pass: listing_pass,
                    reason,
                }));
            }
            ExactObservedNamespaceReservationReadV1::Bound(reservation) => reservation,
        };
        if reservation.object_key() != listed.object_key() {
            return Ok(Err(
                StrictBoundRemoteObservationIncompleteV1::ListedRouteMismatch {
                    pass: listing_pass,
                    kind: BoundRemoteObjectKindV1::NamespaceReservation,
                },
            ));
        }
        if let Err(incomplete) = budget.observe_object(listing_pass, reservation.object(), contract)
        {
            return Ok(Err(incomplete));
        }
        if let Err(incomplete) = observe_claim_chain_v1(
            &mut budget,
            listing_pass,
            &mut claims,
            reservation.exact_path(),
            reservation.role(),
            contract,
        ) {
            return Ok(Err(incomplete));
        }
        reservations.push(reservation);
    }

    // Borrow manifest IDs from the now-immobile canonical index vector. The
    // retained observation stores only a source ordinal, not a duplicate full
    // manifest storage key for every object.
    let mut manifest_references = BTreeMap::<&str, Vec<usize>>::new();
    for (index_ordinal, observed) in index_objects.iter().enumerate() {
        if let Some(committed) = observed.committed() {
            manifest_references
                .entry(committed.current().manifest_hash())
                .or_default()
                .push(index_ordinal);
        }
    }
    let mut manifests = Vec::with_capacity(manifest_references.len());
    for (_manifest_id, index_ordinals) in manifest_references {
        let source_index_ordinal = *index_ordinals
            .first()
            .expect("manifest reference group is non-empty");
        let references = index_ordinals
            .iter()
            .map(|ordinal| {
                index_objects[*ordinal]
                    .committed()
                    .expect("manifest reference ordinal names a committed index")
            })
            .collect::<Vec<_>>();
        let manifest = match read_observed_strict_remote_manifest_for_references_v1(op, &references)
            .await?
        {
            StrictObservedRemoteManifestReadV1::Complete(manifest) => manifest,
            StrictObservedRemoteManifestReadV1::Incomplete {
                reason,
                observed_object,
            } => {
                if let Some(object) = observed_object {
                    if let Err(incomplete) = budget.observe_object(listing_pass, &object, contract)
                    {
                        return Ok(Err(incomplete));
                    }
                }
                return Ok(Err(StrictBoundRemoteObservationIncompleteV1::Manifest {
                    pass: listing_pass,
                    reason,
                }));
            }
        };
        if let Err(incomplete) = budget.observe_object(listing_pass, manifest.object(), contract) {
            return Ok(Err(incomplete));
        }
        manifests.push(BoundRemoteManifestObservationV1 {
            source_index_ordinal,
            manifest,
        });
    }

    let claims = claims
        .into_iter()
        .map(
            |(folded_path, RetainedRemoteNamespaceClaimValueV1 { exact_path, role })| {
                RetainedRemoteNamespaceClaimV1 {
                    folded_path,
                    exact_path,
                    role,
                }
            },
        )
        .collect();
    Ok(Ok(FullyDrainedBoundRemotePassV1 {
        index_objects,
        reservations,
        manifests,
        claims,
    }))
}

async fn read_bound_remote_observation_with_between_passes_v1<F, Fut>(
    op: &Operator,
    remote_prefix: &str,
    between_passes: F,
) -> anyhow::Result<StrictBoundRemoteObservationReadV1>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let remote_prefix = match validate_canonical_namespace_remote_prefix(remote_prefix) {
        Ok(prefix) => prefix,
        Err(_) => {
            return Ok(StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::InvalidRemotePrefix,
            ));
        }
    };
    let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let first = match read_fully_drained_bound_remote_pass_v1(
        op,
        remote_prefix,
        RemoteNamespaceListingPassV1::First,
        contract,
    )
    .await?
    {
        Ok(pass) => pass,
        Err(incomplete) => {
            return Ok(StrictBoundRemoteObservationReadV1::Incomplete(incomplete));
        }
    };
    between_passes().await;
    let second = match read_fully_drained_bound_remote_pass_v1(
        op,
        remote_prefix,
        RemoteNamespaceListingPassV1::Second,
        contract,
    )
    .await?
    {
        Ok(pass) => pass,
        Err(incomplete) => {
            return Ok(StrictBoundRemoteObservationReadV1::Incomplete(incomplete));
        }
    };

    let index_objects = first.index_objects != second.index_objects;
    let reservations = first.reservations != second.reservations;
    let manifests = first.manifests != second.manifests;
    let claims = first.claims != second.claims;
    if index_objects || reservations || manifests || claims {
        return Ok(StrictBoundRemoteObservationReadV1::Incomplete(
            StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                index_objects,
                reservations,
                manifests,
                claims,
            },
        ));
    }

    Ok(StrictBoundRemoteObservationReadV1::Matched(
        MatchingTwoPassBoundRemoteEvidenceV1 {
            remote_prefix: remote_prefix.to_owned(),
            evidence: second,
        },
    ))
}

pub(crate) async fn read_bound_remote_observation_two_pass_v1(
    op: &Operator,
    remote_prefix: &str,
) -> anyhow::Result<StrictBoundRemoteObservationReadV1> {
    read_bound_remote_observation_with_between_passes_v1(op, remote_prefix, || async {}).await
}

async fn read_remote_namespace_keys_with_between_passes_v1<F, Fut>(
    op: &Operator,
    remote_prefix: &str,
    between_passes: F,
) -> StrictRemoteNamespaceKeyReadV1
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let remote_prefix = match validate_canonical_namespace_remote_prefix(remote_prefix) {
        Ok(prefix) => prefix,
        Err(_) => {
            return StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::InvalidRemotePrefix,
            );
        }
    };
    let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    let first = match read_remote_namespace_key_pass_v1(
        op,
        remote_prefix,
        RemoteNamespaceListingPassV1::First,
        contract,
    )
    .await
    {
        Ok(pass) => pass,
        Err(incomplete) => return StrictRemoteNamespaceKeyReadV1::Incomplete(incomplete),
    };
    between_passes().await;
    let second = match read_remote_namespace_key_pass_v1(
        op,
        remote_prefix,
        RemoteNamespaceListingPassV1::Second,
        contract,
    )
    .await
    {
        Ok(pass) => pass,
        Err(incomplete) => return StrictRemoteNamespaceKeyReadV1::Incomplete(incomplete),
    };
    // Synthetic directory rows are traversal scaffolding, not physical
    // namespace objects. Only the classified FILE keys participate in the
    // two-pass comparison.
    let index_disagreed = first.index_objects != second.index_objects;
    let reservations_disagreed = first.reservation_objects != second.reservation_objects;
    if index_disagreed || reservations_disagreed {
        return StrictRemoteNamespaceKeyReadV1::Incomplete(
            StrictRemoteNamespaceKeyIncompleteV1::PassesDisagreed {
                index: index_disagreed,
                reservations: reservations_disagreed,
            },
        );
    }

    StrictRemoteNamespaceKeyReadV1::Matched(MatchingTwoPassListedRemoteKeysV1 {
        remote_prefix: remote_prefix.to_owned(),
        index_objects: second.index_objects.into_iter().collect(),
        reservation_objects: second.reservation_objects.into_iter().collect(),
    })
}

pub(crate) async fn list_remote_namespace_keys_two_pass_v1(
    op: &Operator,
    remote_prefix: &str,
) -> StrictRemoteNamespaceKeyReadV1 {
    read_remote_namespace_keys_with_between_passes_v1(op, remote_prefix, || async {}).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_entry::{manifest_object_id, namespace_reservation_object_id};
    use opendal::raw::{oio, Access, AccessorInfo, OpList, OpRead, OpStat, RpList, RpRead, RpStat};
    use opendal::services::Memory;
    use opendal::{Buffer, Capability, Error, ErrorKind, Metadata, OperatorBuilder};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    enum ScriptedListRow {
        Entry(String, EntryMode),
        Error,
    }

    #[derive(Debug)]
    struct ScriptedListCall {
        expected_path: String,
        rows: VecDeque<ScriptedListRow>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ObservedListCall {
        path: String,
        limit: Option<usize>,
        recursive: bool,
        versions: bool,
        deleted: bool,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum ObservationEvent {
        ListOpen(String),
        ListRow(String),
        ListEof(String),
        Stat(String),
        Read(String),
        BetweenPasses,
    }

    #[derive(Clone, Debug)]
    struct ScriptedBoundObject {
        bytes: Vec<u8>,
        etag: Option<String>,
        version: Option<String>,
    }

    #[derive(Debug)]
    struct ScriptedLister {
        path: String,
        rows: VecDeque<ScriptedListRow>,
        events: Arc<Mutex<Vec<ObservationEvent>>>,
        finished: bool,
    }

    impl oio::List for ScriptedLister {
        async fn next(&mut self) -> opendal::Result<Option<oio::Entry>> {
            match self.rows.pop_front() {
                None => {
                    if !self.finished {
                        self.events
                            .lock()
                            .unwrap()
                            .push(ObservationEvent::ListEof(self.path.clone()));
                        self.finished = true;
                    }
                    Ok(None)
                }
                Some(ScriptedListRow::Entry(path, mode)) => {
                    self.events
                        .lock()
                        .unwrap()
                        .push(ObservationEvent::ListRow(path.clone()));
                    Ok(Some(oio::Entry::new(&path, Metadata::new(mode))))
                }
                Some(ScriptedListRow::Error) => Err(Error::new(
                    ErrorKind::Unexpected,
                    "scripted list stream failed",
                )),
            }
        }
    }

    #[derive(Clone, Debug)]
    struct ScriptedListBackend {
        info: Arc<AccessorInfo>,
        calls: Arc<Mutex<VecDeque<ScriptedListCall>>>,
        observed: Arc<Mutex<Vec<ObservedListCall>>>,
        objects: Arc<Mutex<BTreeMap<String, ScriptedBoundObject>>>,
        events: Arc<Mutex<Vec<ObservationEvent>>>,
    }

    impl Access for ScriptedListBackend {
        type Reader = Buffer;
        type Writer = ();
        type Lister = ScriptedLister;
        type Deleter = ();

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn list(&self, path: &str, args: OpList) -> opendal::Result<(RpList, Self::Lister)> {
            self.events
                .lock()
                .unwrap()
                .push(ObservationEvent::ListOpen(path.to_owned()));
            self.observed.lock().unwrap().push(ObservedListCall {
                path: path.to_owned(),
                limit: args.limit(),
                recursive: args.recursive(),
                versions: args.versions(),
                deleted: args.deleted(),
            });
            let call = self
                .calls
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| Error::new(ErrorKind::Unexpected, "unexpected list call"))?;
            if call.expected_path != path {
                return Err(Error::new(
                    ErrorKind::Unexpected,
                    "scripted list path did not match",
                ));
            }
            Ok((
                RpList::default(),
                ScriptedLister {
                    path: path.to_owned(),
                    rows: call.rows,
                    events: self.events.clone(),
                    finished: false,
                },
            ))
        }

        async fn stat(&self, path: &str, _: OpStat) -> opendal::Result<RpStat> {
            self.events
                .lock()
                .unwrap()
                .push(ObservationEvent::Stat(path.to_owned()));
            let object = self
                .objects
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "scripted object is missing"))?;
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
            self.events
                .lock()
                .unwrap()
                .push(ObservationEvent::Read(path.to_owned()));
            let object = self
                .objects
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "scripted object is missing"))?;
            if args
                .version()
                .is_some_and(|expected| object.version.as_deref() != Some(expected))
            {
                return Err(Error::new(
                    ErrorKind::ConditionNotMatch,
                    "scripted object version changed",
                ));
            }
            if args
                .if_match()
                .is_some_and(|expected| object.etag.as_deref() != Some(expected))
            {
                return Err(Error::new(
                    ErrorKind::ConditionNotMatch,
                    "scripted object ETag changed",
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
            Ok((
                RpRead::new().with_size(Some(u64::try_from(selected.len()).unwrap())),
                Buffer::from(selected),
            ))
        }
    }

    fn memory_operator() -> Operator {
        Operator::new(Memory::default()).unwrap().finish()
    }

    fn scripted_list_call(expected_path: &str, rows: Vec<ScriptedListRow>) -> ScriptedListCall {
        ScriptedListCall {
            expected_path: expected_path.to_owned(),
            rows: rows.into(),
        }
    }

    fn scripted_list_operator(calls: Vec<ScriptedListCall>) -> (Operator, ScriptedListBackend) {
        let info = AccessorInfo::default();
        info.set_scheme("registered-remote-list-test")
            .set_root("/")
            .set_name("registered-remote-list-test")
            .set_native_capability(Capability {
                list: true,
                list_with_limit: true,
                list_with_recursive: true,
                stat: true,
                read: true,
                read_with_if_match: true,
                read_with_version: true,
                ..Default::default()
            });
        let backend = ScriptedListBackend {
            info: Arc::new(info),
            calls: Arc::new(Mutex::new(calls.into())),
            observed: Arc::new(Mutex::new(Vec::new())),
            objects: Arc::new(Mutex::new(BTreeMap::new())),
            events: Arc::new(Mutex::new(Vec::new())),
        };
        (OperatorBuilder::new(backend.clone()).finish(), backend)
    }

    fn bound_object(bytes: Vec<u8>, etag: &str) -> ScriptedBoundObject {
        ScriptedBoundObject {
            bytes,
            etag: Some(etag.to_owned()),
            version: None,
        }
    }

    fn versioned_bound_object(bytes: Vec<u8>, version: &str, etag: &str) -> ScriptedBoundObject {
        ScriptedBoundObject {
            bytes,
            etag: Some(etag.to_owned()),
            version: Some(version.to_owned()),
        }
    }

    fn install_bound_object(
        backend: &ScriptedListBackend,
        key: impl Into<String>,
        object: ScriptedBoundObject,
    ) {
        backend.objects.lock().unwrap().insert(key.into(), object);
    }

    fn regular_manifest_json(rel_path: &str) -> Vec<u8> {
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

    fn committed_index_json(manifest_hash: &str) -> Vec<u8> {
        format!(
            r#"{{"version":2,"state":"committed","current":{{"manifest_hash":"{manifest_hash}","size":4,"chunks":1}},"pending":null}}"#
        )
        .into_bytes()
    }

    fn deleted_index_json(safety_copy_key: Option<&str>) -> Vec<u8> {
        match safety_copy_key {
            Some(safety_copy_key) => format!(
                r#"{{"version":4,"state":"deleted","current":null,"pending":null,"deletion_evidence":{{"safety_copy_key":"{safety_copy_key}","safety_copy_blake3":"{digest}"}}}}"#,
                digest = "a".repeat(64),
            )
            .into_bytes(),
            None => {
                br#"{"version":4,"state":"deleted","current":null,"pending":null}"#.to_vec()
            }
        }
    }

    fn canonical_reservation_json(exact_path: &str, folded_path: &str, role: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"exact_path":"{exact_path}","folded_path":"{folded_path}","role":"{role}"}}"#
        )
        .into_bytes()
    }

    fn two_bound_pass_list_calls(
        index_keys: &[String],
        reservation_keys: &[String],
    ) -> Vec<ScriptedListCall> {
        bound_pass_list_calls(index_keys, reservation_keys, index_keys, reservation_keys)
    }

    fn bound_pass_list_calls(
        first_index_keys: &[String],
        first_reservation_keys: &[String],
        second_index_keys: &[String],
        second_reservation_keys: &[String],
    ) -> Vec<ScriptedListCall> {
        let corpus_rows = |keys: &[String]| {
            keys.iter()
                .map(|key| ScriptedListRow::Entry(key.clone(), EntryMode::FILE))
                .collect()
        };
        vec![
            scripted_list_call("roots/index/", corpus_rows(first_index_keys)),
            scripted_list_call(
                "roots/.tcfs-namespace/v1/",
                corpus_rows(first_reservation_keys),
            ),
            scripted_list_call("roots/index/", corpus_rows(second_index_keys)),
            scripted_list_call(
                "roots/.tcfs-namespace/v1/",
                corpus_rows(second_reservation_keys),
            ),
        ]
    }

    fn observe_one_row(
        corpus: RemoteNamespaceCorpusV1,
        requested_prefix: &str,
        key: &str,
        mode: EntryMode,
    ) -> Result<RemoteNamespaceKeyPassV1, StrictRemoteNamespaceKeyIncompleteV1> {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1::default();
        observe_listing_row_v1(
            &mut pass,
            &mut budget,
            RemoteNamespaceListingPassV1::First,
            corpus,
            requested_prefix,
            key,
            mode,
            false,
            None,
            contract,
        )?;
        Ok(pass)
    }

    #[tokio::test]
    async fn empty_prefix_and_empty_keysets_are_matching_listed_outputs() {
        let op = memory_operator();

        let observed = match list_remote_namespace_keys_two_pass_v1(&op, "").await {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("expected matching empty listed outputs, got {other:?}"),
        };

        assert_eq!(observed.remote_prefix(), "");
        assert!(observed.index_objects().is_empty());
        assert!(observed.reservation_objects().is_empty());
    }

    #[tokio::test]
    async fn empty_prefix_lists_only_the_root_namespace_prefixes() {
        let op = memory_operator();
        let reservation_id = "b".repeat(64);
        op.write("index/file", b"untrusted-index-body".to_vec())
            .await
            .unwrap();
        op.write(
            &format!(".tcfs-namespace/v1/{reservation_id}"),
            b"untrusted-reservation-body".to_vec(),
        )
        .await
        .unwrap();

        let observed = match list_remote_namespace_keys_two_pass_v1(&op, "").await {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("expected matching root listed outputs, got {other:?}"),
        };

        assert_eq!(observed.index_objects().len(), 1);
        assert_eq!(observed.index_objects()[0].object_key(), "index/file");
        assert_eq!(
            observed.reservation_objects()[0].object_key(),
            format!(".tcfs-namespace/v1/{reservation_id}")
        );
    }

    #[tokio::test]
    async fn matching_memory_keysets_are_listed_twice_without_a_digest() {
        let op = memory_operator();
        let reservation_id = "a".repeat(64);
        op.write("roots/index/file", b"index".to_vec())
            .await
            .unwrap();
        op.write("roots/index/dir/.tcfs_dir", b"marker".to_vec())
            .await
            .unwrap();
        op.write(
            &format!("roots/.tcfs-namespace/v1/{reservation_id}"),
            b"reservation".to_vec(),
        )
        .await
        .unwrap();

        let observed = match list_remote_namespace_keys_two_pass_v1(&op, "roots").await {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("expected matching two-pass keys, got {other:?}"),
        };
        assert_eq!(observed.remote_prefix(), "roots");
        assert_eq!(observed.index_objects().len(), 2);
        assert_eq!(observed.index_objects()[0].logical_path(), "dir");
        assert_eq!(
            observed.index_objects()[0].index_rel_path(),
            "dir/.tcfs_dir"
        );
        assert_eq!(
            observed.index_objects()[0].class(),
            ListedRemoteIndexKeyClassV1::DirectoryMarkerCandidate
        );
        assert_eq!(observed.index_objects()[1].logical_path(), "file");
        assert_eq!(observed.reservation_objects().len(), 1);
        assert_eq!(
            observed.reservation_objects()[0].object_id(),
            reservation_id
        );
    }

    #[tokio::test]
    async fn keyset_mutation_between_passes_is_typed_incomplete() {
        let op = memory_operator();
        op.write("roots/index/file", b"index".to_vec())
            .await
            .unwrap();
        let mutation_op = op.clone();
        let result =
            read_remote_namespace_keys_with_between_passes_v1(&op, "roots", move || async move {
                mutation_op
                    .write("roots/index/new", b"index".to_vec())
                    .await
                    .unwrap();
            })
            .await;
        assert_eq!(
            result,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::PassesDisagreed {
                    index: true,
                    reservations: false,
                }
            )
        );
    }

    #[tokio::test]
    async fn reservation_disagreement_is_reported_separately() {
        let op = memory_operator();
        let mutation_op = op.clone();
        let result =
            read_remote_namespace_keys_with_between_passes_v1(&op, "roots", move || async move {
                mutation_op
                    .write(
                        &format!("roots/.tcfs-namespace/v1/{}", "c".repeat(64)),
                        b"reservation".to_vec(),
                    )
                    .await
                    .unwrap();
            })
            .await;
        assert_eq!(
            result,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::PassesDisagreed {
                    index: false,
                    reservations: true,
                }
            )
        );
    }

    #[tokio::test]
    async fn both_corpora_can_disagree_without_collapsing_the_evidence() {
        let op = memory_operator();
        let mutation_op = op.clone();
        let result =
            read_remote_namespace_keys_with_between_passes_v1(&op, "roots", move || async move {
                mutation_op
                    .write("roots/index/new", b"index".to_vec())
                    .await
                    .unwrap();
                mutation_op
                    .write(
                        &format!("roots/.tcfs-namespace/v1/{}", "e".repeat(64)),
                        b"reservation".to_vec(),
                    )
                    .await
                    .unwrap();
            })
            .await;
        assert_eq!(
            result,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::PassesDisagreed {
                    index: true,
                    reservations: true,
                }
            )
        );
    }

    #[tokio::test]
    async fn same_key_body_rewrite_is_outside_this_key_only_stage() {
        let op = memory_operator();
        op.write("roots/index/file", b"first-body".to_vec())
            .await
            .unwrap();
        let mutation_op = op.clone();
        let result =
            read_remote_namespace_keys_with_between_passes_v1(&op, "roots", move || async move {
                mutation_op
                    .write("roots/index/file", b"second-body".to_vec())
                    .await
                    .unwrap();
            })
            .await;
        let observed = match result {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("body-only rewrite must remain outside key listing: {other:?}"),
        };
        assert_eq!(observed.index_objects().len(), 1);
        assert_eq!(observed.index_objects()[0].object_key(), "roots/index/file");
    }

    #[tokio::test]
    async fn synthetic_directory_row_difference_is_not_object_key_disagreement() {
        let op = memory_operator();
        let mutation_op = op.clone();
        let result =
            read_remote_namespace_keys_with_between_passes_v1(&op, "roots", move || async move {
                mutation_op
                    .create_dir("roots/index/scaffolding/")
                    .await
                    .unwrap();
            })
            .await;
        let observed = match result {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("synthetic directory rows are not object keys: {other:?}"),
        };
        assert!(observed.index_objects().is_empty());
        assert!(observed.reservation_objects().is_empty());
    }

    #[tokio::test]
    async fn scripted_reordered_rows_match_in_fixed_four_call_order() {
        let (op, backend) = scripted_list_operator(vec![
            scripted_list_call(
                "roots/index/",
                vec![
                    ScriptedListRow::Entry("roots/index/b".to_owned(), EntryMode::FILE),
                    ScriptedListRow::Entry("roots/index/a".to_owned(), EntryMode::FILE),
                ],
            ),
            scripted_list_call("roots/.tcfs-namespace/v1/", vec![]),
            scripted_list_call(
                "roots/index/",
                vec![
                    ScriptedListRow::Entry("roots/index/a".to_owned(), EntryMode::FILE),
                    ScriptedListRow::Entry("roots/index/b".to_owned(), EntryMode::FILE),
                ],
            ),
            scripted_list_call("roots/.tcfs-namespace/v1/", vec![]),
        ]);

        let observed = match list_remote_namespace_keys_two_pass_v1(&op, "roots").await {
            StrictRemoteNamespaceKeyReadV1::Matched(observed) => observed,
            other => panic!("reordered raw rows should match by physical key: {other:?}"),
        };
        assert_eq!(
            observed
                .index_objects()
                .iter()
                .map(ListedRemoteIndexKeyV1::logical_path)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(backend.calls.lock().unwrap().is_empty());

        let request_limit = usize::try_from(
            RegisteredRootPlanContractV1::strict_v1()
                .remote_contract()
                .listing_page_request_limit(),
        )
        .unwrap();
        let expected = [
            "roots/index/",
            "roots/.tcfs-namespace/v1/",
            "roots/index/",
            "roots/.tcfs-namespace/v1/",
        ]
        .into_iter()
        .map(|path| ObservedListCall {
            path: path.to_owned(),
            limit: Some(request_limit),
            recursive: true,
            versions: false,
            deleted: false,
        })
        .collect::<Vec<_>>();
        assert_eq!(*backend.observed.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn scripted_duplicate_directory_row_stops_during_first_index_pass() {
        let duplicate = "roots/index/dir/".to_owned();
        let (op, backend) = scripted_list_operator(vec![scripted_list_call(
            "roots/index/",
            vec![
                ScriptedListRow::Entry(duplicate.clone(), EntryMode::DIR),
                ScriptedListRow::Entry(duplicate, EntryMode::DIR),
            ],
        )]);

        assert_eq!(
            list_remote_namespace_keys_two_pass_v1(&op, "roots").await,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
                    pass: RemoteNamespaceListingPassV1::First,
                    corpus: RemoteNamespaceCorpusV1::Index,
                }
            )
        );
        assert_eq!(backend.observed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn scripted_midstream_failure_carries_second_reservation_context() {
        let reservation_key = format!("roots/.tcfs-namespace/v1/{}", "f".repeat(64));
        let (op, backend) = scripted_list_operator(vec![
            scripted_list_call("roots/index/", vec![]),
            scripted_list_call("roots/.tcfs-namespace/v1/", vec![]),
            scripted_list_call("roots/index/", vec![]),
            scripted_list_call(
                "roots/.tcfs-namespace/v1/",
                vec![
                    ScriptedListRow::Entry(reservation_key, EntryMode::FILE),
                    ScriptedListRow::Error,
                ],
            ),
        ]);

        assert_eq!(
            list_remote_namespace_keys_two_pass_v1(&op, "roots").await,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::ListingFailed {
                    pass: RemoteNamespaceListingPassV1::Second,
                    corpus: RemoteNamespaceCorpusV1::Reservation,
                }
            )
        );
        assert_eq!(backend.observed.lock().unwrap().len(), 4);
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_remote_prefix_is_rejected_before_listing() {
        let result = list_remote_namespace_keys_two_pass_v1(&memory_operator(), "roots/").await;
        assert_eq!(
            result,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::InvalidRemotePrefix
            )
        );
    }

    #[tokio::test]
    async fn oversized_derived_prefix_fails_before_backend_listing() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let remote_prefix = "r".repeat(usize::try_from(contract.max_storage_key_bytes()).unwrap());
        let (op, backend) = scripted_list_operator(vec![]);

        assert_eq!(
            list_remote_namespace_keys_two_pass_v1(&op, &remote_prefix).await,
            StrictRemoteNamespaceKeyReadV1::Incomplete(
                StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                    pass: RemoteNamespaceListingPassV1::First,
                    corpus: RemoteNamespaceCorpusV1::Index,
                    resource: RemoteNamespaceKeyResourceV1::StorageKeyBytes,
                }
            )
        );
        assert!(backend.observed.lock().unwrap().is_empty());
    }

    #[test]
    fn row_validation_counts_before_rejecting_and_detects_duplicates() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1::default();
        let prefix = "roots/index/";
        observe_listing_row_v1(
            &mut pass,
            &mut budget,
            RemoteNamespaceListingPassV1::First,
            RemoteNamespaceCorpusV1::Index,
            prefix,
            "roots/index/file",
            EntryMode::FILE,
            false,
            None,
            contract,
        )
        .unwrap();
        assert_eq!(budget.listing_rows, 1);
        assert_eq!(budget.index_objects, 1);

        assert_eq!(
            observe_listing_row_v1(
                &mut pass,
                &mut budget,
                RemoteNamespaceListingPassV1::First,
                RemoteNamespaceCorpusV1::Index,
                prefix,
                "roots/index/file",
                EntryMode::FILE,
                false,
                None,
                contract,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
                pass: RemoteNamespaceListingPassV1::First,
                corpus: RemoteNamespaceCorpusV1::Index
            })
        );
        assert_eq!(budget.listing_rows, 2);
        assert_eq!(budget.index_objects, 2);

        let rows_before_invalid = budget.listing_rows;
        let listing_bytes_before_invalid = budget.listing_key_bytes;
        let objects_before_invalid = budget.index_objects;
        let retained_bytes_before_invalid = budget.index_key_bytes;
        assert!(matches!(
            observe_listing_row_v1(
                &mut pass,
                &mut budget,
                RemoteNamespaceListingPassV1::First,
                RemoteNamespaceCorpusV1::Index,
                prefix,
                "outside/file",
                EntryMode::FILE,
                false,
                None,
                contract,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                reason: InvalidRemoteNamespaceKeyReasonV1::OutsideRequestedPrefix,
                ..
            })
        ));
        assert_eq!(budget.listing_rows, rows_before_invalid + 1);
        assert_eq!(
            budget.listing_key_bytes,
            listing_bytes_before_invalid + "outside/file".len() as u64
        );
        assert_eq!(budget.index_objects, objects_before_invalid + 1);
        assert_eq!(
            budget.index_key_bytes,
            retained_bytes_before_invalid + "outside/file".len() as u64
        );
    }

    #[test]
    fn requested_prefix_directories_are_accepted_but_reservations_stay_flat() {
        assert!(observe_one_row(
            RemoteNamespaceCorpusV1::Index,
            "roots/index/",
            "roots/index/",
            EntryMode::DIR,
        )
        .is_ok());
        assert!(observe_one_row(
            RemoteNamespaceCorpusV1::Reservation,
            "roots/.tcfs-namespace/v1/",
            "roots/.tcfs-namespace/v1/",
            EntryMode::DIR,
        )
        .is_ok());
        assert_eq!(
            observe_one_row(
                RemoteNamespaceCorpusV1::Reservation,
                "roots/.tcfs-namespace/v1/",
                "roots/.tcfs-namespace/v1/nested/",
                EntryMode::DIR,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                pass: RemoteNamespaceListingPassV1::First,
                corpus: RemoteNamespaceCorpusV1::Reservation,
                reason: InvalidRemoteNamespaceKeyReasonV1::UnexpectedReservationDirectory,
            })
        );
    }

    #[test]
    fn duplicate_synthetic_directory_rows_are_rejected_after_accounting() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1::default();
        for expected in [
            Ok(()),
            Err(StrictRemoteNamespaceKeyIncompleteV1::DuplicateObjectKey {
                pass: RemoteNamespaceListingPassV1::First,
                corpus: RemoteNamespaceCorpusV1::Index,
            }),
        ] {
            assert_eq!(
                observe_listing_row_v1(
                    &mut pass,
                    &mut budget,
                    RemoteNamespaceListingPassV1::First,
                    RemoteNamespaceCorpusV1::Index,
                    "roots/index/",
                    "roots/index/dir/",
                    EntryMode::DIR,
                    false,
                    None,
                    contract,
                ),
                expected
            );
        }
        assert_eq!(budget.listing_rows, 2);
        assert_eq!(budget.index_objects, 0);
    }

    #[test]
    fn directory_marker_keys_remain_candidates_and_aliases_fail_closed() {
        let valid = observe_one_row(
            RemoteNamespaceCorpusV1::Index,
            "roots/index/",
            "roots/index/dir/.tcfs_dir",
            EntryMode::FILE,
        )
        .unwrap();
        let candidate = valid.index_objects.iter().next().unwrap();
        assert_eq!(candidate.logical_path(), "dir");
        assert_eq!(
            candidate.class(),
            ListedRemoteIndexKeyClassV1::DirectoryMarkerCandidate
        );

        for key in [
            "roots/index/.tcfs_dir",
            "roots/index/dir/.TCFS_DIR",
            "roots/index/dir/.tcfs_dir/file",
        ] {
            assert_eq!(
                observe_one_row(
                    RemoteNamespaceCorpusV1::Index,
                    "roots/index/",
                    key,
                    EntryMode::FILE,
                ),
                Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                    pass: RemoteNamespaceListingPassV1::First,
                    corpus: RemoteNamespaceCorpusV1::Index,
                    reason: InvalidRemoteNamespaceKeyReasonV1::InvalidIndexPath,
                }),
                "unexpected marker-key result for {key:?}"
            );
        }
    }

    #[test]
    fn reservation_object_ids_require_one_lowercase_hex_component() {
        let valid_id = "d".repeat(64);
        let valid = observe_one_row(
            RemoteNamespaceCorpusV1::Reservation,
            "roots/.tcfs-namespace/v1/",
            &format!("roots/.tcfs-namespace/v1/{valid_id}"),
            EntryMode::FILE,
        )
        .unwrap();
        assert_eq!(
            valid.reservation_objects.iter().next().unwrap().object_id(),
            valid_id
        );

        for invalid_id in [
            "d".repeat(63),
            "d".repeat(65),
            "D".repeat(64),
            format!("{}g", "d".repeat(63)),
            format!("{}/child", "d".repeat(64)),
        ] {
            let key = format!("roots/.tcfs-namespace/v1/{invalid_id}");
            assert_eq!(
                observe_one_row(
                    RemoteNamespaceCorpusV1::Reservation,
                    "roots/.tcfs-namespace/v1/",
                    &key,
                    EntryMode::FILE,
                ),
                Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                    pass: RemoteNamespaceListingPassV1::First,
                    corpus: RemoteNamespaceCorpusV1::Reservation,
                    reason: InvalidRemoteNamespaceKeyReasonV1::InvalidReservationObjectId,
                }),
                "unexpected reservation-key result for {key:?}"
            );
        }
    }

    #[test]
    fn directory_rows_and_reservation_key_grammar_fail_closed() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1::default();
        assert!(observe_listing_row_v1(
            &mut pass,
            &mut budget,
            RemoteNamespaceListingPassV1::First,
            RemoteNamespaceCorpusV1::Index,
            "roots/index/",
            "roots/index/dir/",
            EntryMode::DIR,
            false,
            None,
            contract,
        )
        .is_ok());
        assert!(matches!(
            observe_listing_row_v1(
                &mut pass,
                &mut budget,
                RemoteNamespaceListingPassV1::First,
                RemoteNamespaceCorpusV1::Reservation,
                "roots/.tcfs-namespace/v1/",
                "roots/.tcfs-namespace/v1/not-flat/id",
                EntryMode::FILE,
                false,
                None,
                contract,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                reason: InvalidRemoteNamespaceKeyReasonV1::InvalidReservationObjectId,
                ..
            })
        ));
    }

    #[test]
    fn file_metadata_rejections_consume_raw_and_corpus_budgets() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let key = "roots/index/file";
        for (is_deleted, is_current, reason) in [
            (
                true,
                None,
                InvalidRemoteNamespaceKeyReasonV1::DeletedListingEntry,
            ),
            (
                false,
                Some(false),
                InvalidRemoteNamespaceKeyReasonV1::NonCurrentListingEntry,
            ),
        ] {
            let mut pass = RemoteNamespaceKeyPassV1::default();
            let mut budget = RemoteListingBudgetV1::default();
            assert_eq!(
                observe_listing_row_v1(
                    &mut pass,
                    &mut budget,
                    RemoteNamespaceListingPassV1::First,
                    RemoteNamespaceCorpusV1::Index,
                    "roots/index/",
                    key,
                    EntryMode::FILE,
                    is_deleted,
                    is_current,
                    contract,
                ),
                Err(StrictRemoteNamespaceKeyIncompleteV1::InvalidEntry {
                    pass: RemoteNamespaceListingPassV1::First,
                    corpus: RemoteNamespaceCorpusV1::Index,
                    reason,
                })
            );
            assert_eq!(budget.listing_rows, 1);
            assert_eq!(budget.listing_key_bytes, key.len() as u64);
            assert_eq!(budget.index_objects, 1);
            assert_eq!(budget.index_key_bytes, key.len() as u64);
        }
    }

    #[test]
    fn raw_listing_limits_are_checked_before_entry_validation() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1 {
            listing_rows: contract.max_listing_rows_per_pass(),
            ..RemoteListingBudgetV1::default()
        };
        assert_eq!(
            observe_listing_row_v1(
                &mut pass,
                &mut budget,
                RemoteNamespaceListingPassV1::First,
                RemoteNamespaceCorpusV1::Reservation,
                "roots/.tcfs-namespace/v1/",
                "malformed",
                EntryMode::Unknown,
                false,
                None,
                contract,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass: RemoteNamespaceListingPassV1::First,
                corpus: RemoteNamespaceCorpusV1::Reservation,
                resource: RemoteNamespaceKeyResourceV1::ListingRows,
            })
        );
    }

    #[test]
    fn retained_corpus_key_limits_are_typed_after_raw_accounting() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let mut pass = RemoteNamespaceKeyPassV1::default();
        let mut budget = RemoteListingBudgetV1 {
            index_key_bytes: contract.max_retained_index_key_bytes_per_pass(),
            ..RemoteListingBudgetV1::default()
        };
        let key = "roots/index/file";
        assert_eq!(
            observe_listing_row_v1(
                &mut pass,
                &mut budget,
                RemoteNamespaceListingPassV1::Second,
                RemoteNamespaceCorpusV1::Index,
                "roots/index/",
                key,
                EntryMode::FILE,
                false,
                None,
                contract,
            ),
            Err(StrictRemoteNamespaceKeyIncompleteV1::ResourceLimit {
                pass: RemoteNamespaceListingPassV1::Second,
                corpus: RemoteNamespaceCorpusV1::Index,
                resource: RemoteNamespaceKeyResourceV1::IndexKeyBytes,
            })
        );
        assert_eq!(budget.listing_rows, 1);
        assert_eq!(budget.listing_key_bytes, key.len() as u64);
        assert_eq!(budget.index_objects, 1);
        assert!(budget.index_key_bytes > contract.max_retained_index_key_bytes_per_pass());
    }

    #[test]
    fn bound_body_and_binding_aggregate_limits_accept_exact_and_reject_plus_one() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let pass = RemoteNamespaceListingPassV1::First;
        let mut exact = BoundRemotePassBudgetV1::default();
        assert_eq!(
            exact.observe_object_accounting(
                pass,
                contract.max_bound_object_bytes_per_pass(),
                contract.max_retained_binding_bytes_per_pass(),
                contract,
            ),
            Ok(())
        );
        assert_eq!(
            exact.bound_object_bytes,
            contract.max_bound_object_bytes_per_pass()
        );
        assert_eq!(
            exact.retained_binding_bytes,
            contract.max_retained_binding_bytes_per_pass()
        );

        let mut body_over = BoundRemotePassBudgetV1 {
            bound_object_bytes: contract.max_bound_object_bytes_per_pass(),
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            body_over.observe_object_accounting(pass, 1, 0, contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::BoundObjectBytes,
            })
        );

        let mut binding_over = BoundRemotePassBudgetV1 {
            retained_binding_bytes: contract.max_retained_binding_bytes_per_pass(),
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            binding_over.observe_object_accounting(pass, 0, 1, contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::RetainedBindingBytes,
            })
        );
    }

    #[tokio::test]
    async fn bound_object_budget_uses_actual_version_and_etag_token_lengths() {
        let bytes = deleted_index_json(None);
        let version = "version-token";
        let etag = "etag-token";
        let index_key = "roots/index/file";
        let (op, backend) = scripted_list_operator(vec![]);
        install_bound_object(
            &backend,
            index_key,
            versioned_bound_object(bytes.clone(), version, etag),
        );
        let deleted = match read_exact_observed_raw_index_entry_v1(&op, "roots", "file")
            .await
            .unwrap()
        {
            ExactObservedRawIndexEntryReadV1::Deleted(deleted) => deleted,
            other => panic!("expected a version-bound deleted index, got {other:?}"),
        };
        assert_eq!(
            deleted.object().binding(),
            &RegisteredRootRemoteObjectBindingV1::Version {
                version: version.to_owned(),
                etag: Some(etag.to_owned()),
            }
        );

        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let pass = RemoteNamespaceListingPassV1::First;
        let body_bytes = u64::try_from(bytes.len()).unwrap();
        let binding_bytes = u64::try_from(version.len() + etag.len()).unwrap();
        let mut exact = BoundRemotePassBudgetV1 {
            bound_object_bytes: contract.max_bound_object_bytes_per_pass() - body_bytes,
            retained_binding_bytes: contract.max_retained_binding_bytes_per_pass() - binding_bytes,
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            exact.observe_object(pass, deleted.object(), contract),
            Ok(())
        );
        assert_eq!(
            exact.bound_object_bytes,
            contract.max_bound_object_bytes_per_pass()
        );
        assert_eq!(
            exact.retained_binding_bytes,
            contract.max_retained_binding_bytes_per_pass()
        );

        let mut body_over = BoundRemotePassBudgetV1 {
            bound_object_bytes: contract.max_bound_object_bytes_per_pass() - body_bytes + 1,
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            body_over.observe_object(pass, deleted.object(), contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::BoundObjectBytes,
            })
        );

        let mut binding_over = BoundRemotePassBudgetV1 {
            retained_binding_bytes: contract.max_retained_binding_bytes_per_pass() - binding_bytes
                + 1,
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            binding_over.observe_object(pass, deleted.object(), contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::RetainedBindingBytes,
            })
        );
    }

    #[tokio::test]
    async fn observed_invalid_body_readers_retain_bound_accounting_evidence() {
        let (op, backend) = scripted_list_operator(vec![]);

        let invalid_index_key = "roots/index/invalid-index";
        let invalid_index_bytes = b"not-an-index".to_vec();
        install_bound_object(
            &backend,
            invalid_index_key,
            bound_object(invalid_index_bytes.clone(), "invalid-index"),
        );
        match read_exact_observed_raw_index_entry_v1(&op, "roots", "invalid-index")
            .await
            .unwrap()
        {
            ExactObservedRawIndexEntryReadV1::Incomplete {
                reason: StrictRemoteIndexIncompleteV1::InvalidIndexRecord,
                observed_object: Some(object),
            } => assert_eq!(object.raw_bytes_len(), invalid_index_bytes.len() as u64),
            other => panic!("expected bound invalid-index evidence, got {other:?}"),
        }
        install_bound_object(
            &backend,
            "roots/index/unbound-index",
            ScriptedBoundObject {
                bytes: deleted_index_json(None),
                etag: None,
                version: None,
            },
        );
        assert_eq!(
            read_exact_observed_raw_index_entry_v1(&op, "roots", "unbound-index")
                .await
                .unwrap(),
            ExactObservedRawIndexEntryReadV1::Incomplete {
                reason: StrictRemoteIndexIncompleteV1::UnboundObject,
                observed_object: None,
            }
        );
        assert_eq!(
            read_exact_observed_raw_index_entry_v1(&op, "roots", "missing-index")
                .await
                .unwrap(),
            ExactObservedRawIndexEntryReadV1::Missing
        );

        let invalid_marker_bytes = b"not-a-marker".to_vec();
        install_bound_object(
            &backend,
            "roots/index/invalid-marker/.tcfs_dir",
            bound_object(invalid_marker_bytes.clone(), "invalid-marker"),
        );
        match read_exact_observed_raw_directory_marker_v1(&op, "roots", "invalid-marker")
            .await
            .unwrap()
        {
            ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                reason: StrictRemoteDirectoryMarkerIncompleteV1::InvalidMarkerRecord,
                observed_object: Some(object),
            } => assert_eq!(object.raw_bytes_len(), invalid_marker_bytes.len() as u64),
            other => panic!("expected bound invalid-marker evidence, got {other:?}"),
        }
        install_bound_object(
            &backend,
            "roots/index/unbound-marker/.tcfs_dir",
            ScriptedBoundObject {
                bytes: crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                etag: None,
                version: None,
            },
        );
        assert_eq!(
            read_exact_observed_raw_directory_marker_v1(&op, "roots", "unbound-marker")
                .await
                .unwrap(),
            ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                reason: StrictRemoteDirectoryMarkerIncompleteV1::UnboundObject,
                observed_object: None,
            }
        );
        assert_eq!(
            read_exact_observed_raw_directory_marker_v1(&op, "roots", "missing-marker")
                .await
                .unwrap(),
            ExactObservedRawDirectoryMarkerReadV1::Missing
        );

        let invalid_reservation_id = namespace_reservation_object_id("invalid-reservation");
        let invalid_reservation_bytes = b"not-a-reservation".to_vec();
        install_bound_object(
            &backend,
            format!("roots/.tcfs-namespace/v1/{invalid_reservation_id}"),
            bound_object(invalid_reservation_bytes.clone(), "invalid-reservation"),
        );
        match read_exact_observed_namespace_reservation_v1(&op, "roots", &invalid_reservation_id)
            .await
            .unwrap()
        {
            ExactObservedNamespaceReservationReadV1::Incomplete {
                reason: StrictNamespaceReservationIncompleteV1::InvalidReservation,
                observed_object: Some(object),
            } => assert_eq!(
                object.raw_bytes_len(),
                invalid_reservation_bytes.len() as u64
            ),
            other => panic!("expected bound invalid-reservation evidence, got {other:?}"),
        }
        let unbound_reservation_id = namespace_reservation_object_id("unbound-reservation");
        install_bound_object(
            &backend,
            format!("roots/.tcfs-namespace/v1/{unbound_reservation_id}"),
            ScriptedBoundObject {
                bytes: canonical_reservation_json(
                    "unbound-reservation",
                    "unbound-reservation",
                    "file",
                ),
                etag: None,
                version: None,
            },
        );
        assert_eq!(
            read_exact_observed_namespace_reservation_v1(&op, "roots", &unbound_reservation_id)
                .await
                .unwrap(),
            ExactObservedNamespaceReservationReadV1::Incomplete {
                reason: StrictNamespaceReservationIncompleteV1::UnboundObject,
                observed_object: None,
            }
        );
        let missing_reservation_id = namespace_reservation_object_id("missing-reservation");
        assert_eq!(
            read_exact_observed_namespace_reservation_v1(&op, "roots", &missing_reservation_id)
                .await
                .unwrap(),
            ExactObservedNamespaceReservationReadV1::Missing
        );
    }

    #[tokio::test]
    async fn observed_manifest_reader_retains_only_identity_bound_invalid_bytes() {
        let (op, backend) = scripted_list_operator(vec![]);
        let invalid_manifest_id = "a".repeat(64);
        let unbound_manifest_id = "b".repeat(64);
        let missing_manifest_id = "c".repeat(64);
        for (rel_path, manifest_id) in [
            ("invalid-manifest", &invalid_manifest_id),
            ("unbound-manifest", &unbound_manifest_id),
            ("missing-manifest", &missing_manifest_id),
        ] {
            install_bound_object(
                &backend,
                format!("roots/index/{rel_path}"),
                bound_object(committed_index_json(manifest_id), rel_path),
            );
        }
        let invalid_index =
            match read_exact_observed_raw_index_entry_v1(&op, "roots", "invalid-manifest")
                .await
                .unwrap()
            {
                ExactObservedRawIndexEntryReadV1::Committed(index) => index,
                other => panic!("expected invalid-manifest index reference, got {other:?}"),
            };
        let unbound_index =
            match read_exact_observed_raw_index_entry_v1(&op, "roots", "unbound-manifest")
                .await
                .unwrap()
            {
                ExactObservedRawIndexEntryReadV1::Committed(index) => index,
                other => panic!("expected unbound-manifest index reference, got {other:?}"),
            };
        let missing_index =
            match read_exact_observed_raw_index_entry_v1(&op, "roots", "missing-manifest")
                .await
                .unwrap()
            {
                ExactObservedRawIndexEntryReadV1::Committed(index) => index,
                other => panic!("expected missing-manifest index reference, got {other:?}"),
            };

        let invalid_manifest_bytes = b"not-the-addressed-manifest".to_vec();
        install_bound_object(
            &backend,
            format!("roots/manifests/{invalid_manifest_id}"),
            bound_object(invalid_manifest_bytes.clone(), "invalid-manifest"),
        );
        match read_observed_strict_remote_manifest_for_references_v1(&op, &[&invalid_index])
            .await
            .unwrap()
        {
            StrictObservedRemoteManifestReadV1::Incomplete {
                reason: StrictRemoteManifestIncompleteV1::AddressMismatch,
                observed_object: Some(object),
            } => assert_eq!(object.raw_bytes_len(), invalid_manifest_bytes.len() as u64),
            other => panic!("expected bound invalid-manifest evidence, got {other:?}"),
        }

        install_bound_object(
            &backend,
            format!("roots/manifests/{unbound_manifest_id}"),
            ScriptedBoundObject {
                bytes: regular_manifest_json("unbound-manifest"),
                etag: None,
                version: None,
            },
        );
        assert_eq!(
            read_observed_strict_remote_manifest_for_references_v1(&op, &[&unbound_index])
                .await
                .unwrap(),
            StrictObservedRemoteManifestReadV1::Incomplete {
                reason: StrictRemoteManifestIncompleteV1::UnboundObject,
                observed_object: None,
            }
        );
        assert_eq!(
            read_observed_strict_remote_manifest_for_references_v1(&op, &[&missing_index])
                .await
                .unwrap(),
            StrictObservedRemoteManifestReadV1::Incomplete {
                reason: StrictRemoteManifestIncompleteV1::MissingObject,
                observed_object: None,
            }
        );
    }

    #[test]
    fn claim_aggregate_limits_charge_generated_before_dedupe_and_bound_retention() {
        let contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let pass = RemoteNamespaceListingPassV1::Second;
        let claim = PortableNamespaceReservationV1::from_json_bytes(&canonical_reservation_json(
            "path", "path", "file",
        ))
        .unwrap();
        let claim_bytes = u64::try_from(claim.exact_path().len() + claim.folded_path().len())
            .expect("test claim length fits u64");

        let mut exact = BoundRemotePassBudgetV1 {
            generated_claims: contract.max_generated_claim_observations_per_pass() - 1,
            generated_claim_bytes: contract.max_generated_claim_bytes_per_pass() - claim_bytes,
            retained_claims: contract.max_retained_unique_claims_per_pass() - 1,
            retained_claim_bytes: contract.max_retained_unique_claim_bytes_per_pass() - claim_bytes,
            ..BoundRemotePassBudgetV1::default()
        };
        let mut exact_claims = BTreeMap::new();
        assert_eq!(
            exact.observe_claim(pass, &claim, &mut exact_claims, contract),
            Ok(())
        );
        assert_eq!(
            exact.generated_claims,
            contract.max_generated_claim_observations_per_pass()
        );
        assert_eq!(
            exact.generated_claim_bytes,
            contract.max_generated_claim_bytes_per_pass()
        );
        assert_eq!(
            exact.retained_claims,
            contract.max_retained_unique_claims_per_pass()
        );
        assert_eq!(
            exact.retained_claim_bytes,
            contract.max_retained_unique_claim_bytes_per_pass()
        );

        let retained_before = exact.retained_claims;
        assert_eq!(
            exact.observe_claim(pass, &claim, &mut exact_claims, contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::GeneratedClaims,
            })
        );
        assert_eq!(exact.retained_claims, retained_before);

        let mut generated_bytes_over = BoundRemotePassBudgetV1 {
            generated_claim_bytes: contract.max_generated_claim_bytes_per_pass(),
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            generated_bytes_over.observe_claim(pass, &claim, &mut BTreeMap::new(), contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::GeneratedClaimBytes,
            })
        );

        let mut retained_count_over = BoundRemotePassBudgetV1 {
            retained_claims: contract.max_retained_unique_claims_per_pass(),
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            retained_count_over.observe_claim(pass, &claim, &mut BTreeMap::new(), contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::RetainedClaims,
            })
        );

        let mut retained_bytes_over = BoundRemotePassBudgetV1 {
            retained_claim_bytes: contract.max_retained_unique_claim_bytes_per_pass(),
            ..BoundRemotePassBudgetV1::default()
        };
        assert_eq!(
            retained_bytes_over.observe_claim(pass, &claim, &mut BTreeMap::new(), contract),
            Err(StrictBoundRemoteObservationIncompleteV1::ResourceLimit {
                pass,
                resource: BoundRemotePassResourceV1::RetainedClaimBytes,
            })
        );

        let mut duplicate_budget = BoundRemotePassBudgetV1::default();
        let mut duplicate_claims = BTreeMap::new();
        duplicate_budget
            .observe_claim(pass, &claim, &mut duplicate_claims, contract)
            .unwrap();
        duplicate_budget
            .observe_claim(pass, &claim, &mut duplicate_claims, contract)
            .unwrap();
        assert_eq!(duplicate_budget.generated_claims, 2);
        assert_eq!(duplicate_budget.generated_claim_bytes, claim_bytes * 2);
        assert_eq!(duplicate_budget.retained_claims, 1);
        assert_eq!(duplicate_budget.retained_claim_bytes, claim_bytes);
        assert_eq!(duplicate_claims.len(), 1);
    }

    #[tokio::test]
    async fn full_passes_finish_every_body_before_starting_the_second_listing() {
        let manifest_bytes = regular_manifest_json("file");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let index_key = "roots/index/file".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );
        install_bound_object(
            &backend,
            &manifest_key,
            bound_object(manifest_bytes, "manifest-v1"),
        );
        let events = backend.events.clone();

        let result = read_bound_remote_observation_with_between_passes_v1(
            &op,
            "roots",
            move || async move {
                events.lock().unwrap().push(ObservationEvent::BetweenPasses);
            },
        )
        .await
        .unwrap();
        let evidence = match result {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected matching bound evidence, got {other:?}"),
        };
        assert_eq!(evidence.remote_prefix(), "roots");
        assert_eq!(evidence.index_object_count(), 1);
        assert_eq!(evidence.reservation_count(), 0);
        assert_eq!(evidence.manifest_count(), 1);
        assert_eq!(evidence.claim_count(), 1);

        assert_eq!(
            *backend.events.lock().unwrap(),
            vec![
                ObservationEvent::ListOpen("roots/index/".to_owned()),
                ObservationEvent::ListRow(index_key.clone()),
                ObservationEvent::ListEof("roots/index/".to_owned()),
                ObservationEvent::ListOpen("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::ListEof("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::Stat(index_key.clone()),
                ObservationEvent::Read(index_key.clone()),
                ObservationEvent::Stat(manifest_key.clone()),
                ObservationEvent::Read(manifest_key.clone()),
                ObservationEvent::BetweenPasses,
                ObservationEvent::ListOpen("roots/index/".to_owned()),
                ObservationEvent::ListRow(index_key.clone()),
                ObservationEvent::ListEof("roots/index/".to_owned()),
                ObservationEvent::ListOpen("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::ListEof("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::Stat(index_key.clone()),
                ObservationEvent::Read(index_key),
                ObservationEvent::Stat(manifest_key.clone()),
                ObservationEvent::Read(manifest_key.clone()),
            ]
        );
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(manifest_key.clone()))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Read(manifest_key.clone()))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn second_pass_uses_a_fresh_index_listing_without_retry() {
        let first_key = "roots/index/a".to_owned();
        let second_key = "roots/index/b".to_owned();
        let (op, backend) = scripted_list_operator(bound_pass_list_calls(
            std::slice::from_ref(&first_key),
            &[],
            std::slice::from_ref(&second_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &first_key,
            bound_object(deleted_index_json(None), "index-a"),
        );
        install_bound_object(
            &backend,
            &second_key,
            bound_object(deleted_index_json(None), "index-b"),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: true,
                    reservations: false,
                    manifests: false,
                    claims: true,
                }
            )
        );
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Read(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn second_pass_uses_a_fresh_reservation_listing_without_retry() {
        let first_id = namespace_reservation_object_id("a");
        let second_id = namespace_reservation_object_id("b");
        let first_key = format!("roots/.tcfs-namespace/v1/{first_id}");
        let second_key = format!("roots/.tcfs-namespace/v1/{second_id}");
        let (op, backend) = scripted_list_operator(bound_pass_list_calls(
            &[],
            std::slice::from_ref(&first_key),
            &[],
            std::slice::from_ref(&second_key),
        ));
        install_bound_object(
            &backend,
            &first_key,
            bound_object(canonical_reservation_json("a", "a", "file"), "res-a"),
        );
        install_bound_object(
            &backend,
            &second_key,
            bound_object(canonical_reservation_json("b", "b", "file"), "res-b"),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: false,
                    reservations: true,
                    manifests: false,
                    claims: true,
                }
            )
        );
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Read(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn manifest_binding_change_between_passes_is_observed_without_retry() {
        let manifest_bytes = regular_manifest_json("file");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let index_key = "roots/index/file".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );
        install_bound_object(
            &backend,
            &manifest_key,
            bound_object(manifest_bytes.clone(), "manifest-v1"),
        );
        let mutation_backend = backend.clone();
        let mutation_key = manifest_key.clone();

        let result = read_bound_remote_observation_with_between_passes_v1(
            &op,
            "roots",
            move || async move {
                install_bound_object(
                    &mutation_backend,
                    mutation_key,
                    bound_object(manifest_bytes, "manifest-v2"),
                );
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: false,
                    reservations: false,
                    manifests: true,
                    claims: false,
                }
            )
        );
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(manifest_key.clone()))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Read(manifest_key.clone()))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn same_binding_semantically_equal_index_rewrite_changes_body_evidence() {
        let manifest_bytes = regular_manifest_json("file");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let index_key = "roots/index/file".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let initial_index = committed_index_json(&manifest_id);
        let rewritten_index = String::from_utf8(initial_index.clone())
            .unwrap()
            .replace(",\"state\"", ",\n  \"state\"")
            .into_bytes();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(initial_index, "index-v1"),
        );
        install_bound_object(
            &backend,
            &manifest_key,
            bound_object(manifest_bytes, "manifest-v1"),
        );
        let mutation_backend = backend.clone();
        let mutation_key = index_key.clone();

        let result = read_bound_remote_observation_with_between_passes_v1(
            &op,
            "roots",
            move || async move {
                install_bound_object(
                    &mutation_backend,
                    mutation_key,
                    bound_object(rewritten_index, "index-v1"),
                );
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: true,
                    reservations: false,
                    manifests: false,
                    claims: false,
                }
            )
        );
    }

    #[tokio::test]
    async fn live_marker_to_deleted_marker_transition_disagrees() {
        let marker_key = "roots/index/dir/.tcfs_dir".to_owned();
        let deleted_marker =
            br#"{"version":4,"state":"deleted","current":null,"pending":null}"#.to_vec();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&marker_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &marker_key,
            bound_object(
                crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                "marker-v1",
            ),
        );
        let mutation_backend = backend.clone();
        let mutation_key = marker_key.clone();

        let result = read_bound_remote_observation_with_between_passes_v1(
            &op,
            "roots",
            move || async move {
                install_bound_object(
                    &mutation_backend,
                    mutation_key,
                    bound_object(deleted_marker, "marker-v2"),
                );
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: true,
                    reservations: false,
                    manifests: false,
                    claims: false,
                }
            )
        );
    }

    #[tokio::test]
    async fn canonical_reservation_alias_mutation_disagrees() {
        let folded_path = "doc.txt";
        let reservation_id = namespace_reservation_object_id(folded_path);
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[],
            std::slice::from_ref(&reservation_key),
        ));
        install_bound_object(
            &backend,
            &reservation_key,
            bound_object(
                canonical_reservation_json("Doc.txt", folded_path, "file"),
                "reservation-v1",
            ),
        );
        let mutation_backend = backend.clone();
        let mutation_key = reservation_key.clone();

        let result = read_bound_remote_observation_with_between_passes_v1(
            &op,
            "roots",
            move || async move {
                install_bound_object(
                    &mutation_backend,
                    mutation_key,
                    bound_object(
                        canonical_reservation_json("DOC.txt", folded_path, "file"),
                        "reservation-v2",
                    ),
                );
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::PassesDisagreed {
                    index_objects: false,
                    reservations: true,
                    manifests: false,
                    claims: true,
                }
            )
        );
    }

    #[tokio::test]
    async fn ordinary_file_and_same_path_marker_fail_the_role_claim() {
        let file_key = "roots/index/a".to_owned();
        let marker_key = "roots/index/a/.tcfs_dir".to_owned();
        let manifest_id = "a".repeat(64);
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[file_key.clone(), marker_key.clone()],
            &[],
        ));
        install_bound_object(
            &backend,
            &file_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );
        install_bound_object(
            &backend,
            &marker_key,
            bound_object(
                crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                "marker-v1",
            ),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Claim {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: RemoteNamespaceClaimConflictV1::FileDirectoryRole,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn standalone_descendant_reservation_preserves_ancestor_role_history() {
        let file_key = "roots/index/a".to_owned();
        let folded_path = "a/b";
        let reservation_id = namespace_reservation_object_id(folded_path);
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&file_key),
            std::slice::from_ref(&reservation_key),
        ));
        install_bound_object(
            &backend,
            &file_key,
            bound_object(committed_index_json(&"a".repeat(64)), "index-v1"),
        );
        install_bound_object(
            &backend,
            &reservation_key,
            bound_object(
                canonical_reservation_json(folded_path, folded_path, "file"),
                "reservation-v1",
            ),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Claim {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: RemoteNamespaceClaimConflictV1::FileDirectoryRole,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn live_marker_on_fixed_ingress_path_is_typed_incomplete() {
        let marker_key = "roots/index/home/.SSH/.tcfs_dir".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&marker_key),
            &[],
        ));
        install_bound_object(
            &backend,
            marker_key,
            bound_object(
                crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                "marker-v1",
            ),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::LiveMarkerExcluded {
                    pass: RemoteNamespaceListingPassV1::First,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn deleted_fixed_ingress_marker_remains_historical_claim_evidence() {
        let marker_key = "roots/index/home/.SSH/.tcfs_dir".to_owned();
        let deleted_marker =
            br#"{"version":4,"state":"deleted","current":null,"pending":null}"#.to_vec();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&marker_key),
            &[],
        ));
        install_bound_object(
            &backend,
            marker_key,
            bound_object(deleted_marker, "marker-v1"),
        );

        let evidence = match read_bound_remote_observation_two_pass_v1(&op, "roots")
            .await
            .unwrap()
        {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected historical marker evidence, got {other:?}"),
        };
        assert_eq!(evidence.index_object_count(), 1);
        assert_eq!(evidence.manifest_count(), 0);
        assert_eq!(evidence.claim_count(), 2);
    }

    #[tokio::test]
    async fn reservation_only_remote_history_is_complete_bound_evidence() {
        let folded_path = "archive/file";
        let reservation_id = namespace_reservation_object_id(folded_path);
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[],
            std::slice::from_ref(&reservation_key),
        ));
        install_bound_object(
            &backend,
            &reservation_key,
            bound_object(
                canonical_reservation_json(folded_path, folded_path, "file"),
                "reservation-v1",
            ),
        );

        let evidence = match read_bound_remote_observation_two_pass_v1(&op, "roots")
            .await
            .unwrap()
        {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected reservation-only historical evidence, got {other:?}"),
        };
        assert_eq!(evidence.index_object_count(), 0);
        assert_eq!(evidence.reservation_count(), 1);
        assert_eq!(evidence.manifest_count(), 0);
        assert_eq!(evidence.claim_count(), 2);
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(reservation_key.clone()))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Read(reservation_key.clone()))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn fixed_ingress_reservation_remains_historical_claim_evidence() {
        let exact_path = "home/.SSH/key";
        let folded_path = "home/.ssh/key";
        let reservation_id = namespace_reservation_object_id(folded_path);
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[],
            std::slice::from_ref(&reservation_key),
        ));
        install_bound_object(
            &backend,
            reservation_key,
            bound_object(
                canonical_reservation_json(exact_path, folded_path, "file"),
                "reservation-v1",
            ),
        );

        let evidence = match read_bound_remote_observation_two_pass_v1(&op, "roots")
            .await
            .unwrap()
        {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected fixed-ingress reservation history, got {other:?}"),
        };
        assert_eq!(evidence.index_object_count(), 0);
        assert_eq!(evidence.reservation_count(), 1);
        assert_eq!(evidence.manifest_count(), 0);
        assert_eq!(evidence.claim_count(), 3);
    }

    #[tokio::test]
    async fn same_pass_folded_spelling_aliases_fail_closed() {
        let upper_key = "roots/index/DOC.txt".to_owned();
        let lower_key = "roots/index/Doc.txt".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[upper_key.clone(), lower_key.clone()],
            &[],
        ));
        install_bound_object(
            &backend,
            upper_key,
            bound_object(deleted_index_json(None), "index-upper"),
        );
        install_bound_object(
            &backend,
            lower_key,
            bound_object(deleted_index_json(None), "index-lower"),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Claim {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: RemoteNamespaceClaimConflictV1::FoldedSpellingAlias,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Read(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn deleted_index_evidence_is_retained_without_fetching_the_safety_copy() {
        let index_key = "roots/index/dir/file".to_owned();
        let safety_copy_key = "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/dir/file";
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(
                deleted_index_json(Some(safety_copy_key)),
                "deleted-index-v1",
            ),
        );

        let evidence = match read_bound_remote_observation_two_pass_v1(&op, "roots")
            .await
            .unwrap()
        {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected evidence-bearing tombstone, got {other:?}"),
        };
        assert_eq!(evidence.index_object_count(), 1);
        assert_eq!(evidence.reservation_count(), 0);
        assert_eq!(evidence.manifest_count(), 0);
        assert_eq!(evidence.claim_count(), 2);
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(index_key.clone()))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Read(index_key.clone()))
                .count(),
            2
        );
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                ObservationEvent::Stat(key) | ObservationEvent::Read(key)
                    if key == safety_copy_key
            )
        }));
    }

    #[tokio::test]
    async fn directory_ancestor_and_child_file_claims_are_compatible() {
        let marker_key = "roots/index/a/.tcfs_dir".to_owned();
        let file_key = "roots/index/a/b".to_owned();
        let manifest_bytes = regular_manifest_json("a/b");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[marker_key.clone(), file_key.clone()],
            &[],
        ));
        install_bound_object(
            &backend,
            marker_key,
            bound_object(
                crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                "marker-v1",
            ),
        );
        install_bound_object(
            &backend,
            file_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );
        install_bound_object(
            &backend,
            manifest_key,
            bound_object(manifest_bytes, "manifest-v1"),
        );

        let evidence = match read_bound_remote_observation_two_pass_v1(&op, "roots")
            .await
            .unwrap()
        {
            StrictBoundRemoteObservationReadV1::Matched(evidence) => evidence,
            other => panic!("expected compatible directory ancestry, got {other:?}"),
        };
        assert_eq!(evidence.index_object_count(), 2);
        assert_eq!(evidence.manifest_count(), 1);
        assert_eq!(evidence.claim_count(), 2);
    }

    #[tokio::test]
    async fn shared_manifest_is_fetched_once_then_validated_against_every_route() {
        let manifest_bytes = regular_manifest_json("a");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let a_key = "roots/index/a".to_owned();
        let b_key = "roots/index/b".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[a_key.clone(), b_key.clone()],
            &[],
        ));
        install_bound_object(
            &backend,
            &a_key,
            bound_object(committed_index_json(&manifest_id), "index-a"),
        );
        install_bound_object(
            &backend,
            &b_key,
            bound_object(committed_index_json(&manifest_id), "index-b"),
        );
        install_bound_object(
            &backend,
            &manifest_key,
            bound_object(manifest_bytes, "manifest-v1"),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Manifest {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictRemoteManifestIncompleteV1::InvalidManifest,
                }
            )
        );
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(manifest_key.clone()))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Read(manifest_key.clone()))
                .count(),
            1
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missing_directory_marker_is_typed_and_never_retried() {
        let marker_key = "roots/index/dir/.tcfs_dir".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&marker_key),
            &[],
        ));

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                    pass: RemoteNamespaceListingPassV1::First,
                    kind: BoundRemoteObjectKindV1::DirectoryMarker,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        assert_eq!(
            *backend.events.lock().unwrap(),
            vec![
                ObservationEvent::ListOpen("roots/index/".to_owned()),
                ObservationEvent::ListRow(marker_key.clone()),
                ObservationEvent::ListEof("roots/index/".to_owned()),
                ObservationEvent::ListOpen("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::ListEof("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::Stat(marker_key),
            ]
        );
    }

    #[tokio::test]
    async fn unbound_directory_marker_is_typed_without_reading_bytes() {
        let marker_key = "roots/index/dir/.tcfs_dir".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&marker_key),
            &[],
        ));
        backend.objects.lock().unwrap().insert(
            marker_key.clone(),
            ScriptedBoundObject {
                bytes: crate::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
                etag: None,
                version: None,
            },
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Marker {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictRemoteDirectoryMarkerIncompleteV1::UnboundObject,
                }
            )
        );
        let events = backend.events.lock().unwrap();
        assert_eq!(events.last(), Some(&ObservationEvent::Stat(marker_key)));
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObservationEvent::Read(_))));
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missing_namespace_reservation_is_typed_and_never_retried() {
        let reservation_id = namespace_reservation_object_id("file");
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[],
            std::slice::from_ref(&reservation_key),
        ));

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                    pass: RemoteNamespaceListingPassV1::First,
                    kind: BoundRemoteObjectKindV1::NamespaceReservation,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(reservation_key.clone()))
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObservationEvent::Read(_))));
    }

    #[tokio::test]
    async fn unbound_namespace_reservation_is_typed_without_reading_bytes() {
        let folded_path = "file";
        let reservation_id = namespace_reservation_object_id(folded_path);
        let reservation_key = format!("roots/.tcfs-namespace/v1/{reservation_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            &[],
            std::slice::from_ref(&reservation_key),
        ));
        backend.objects.lock().unwrap().insert(
            reservation_key.clone(),
            ScriptedBoundObject {
                bytes: canonical_reservation_json(folded_path, folded_path, "file"),
                etag: None,
                version: None,
            },
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Reservation {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictNamespaceReservationIncompleteV1::UnboundObject,
                }
            )
        );
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events.last(),
            Some(&ObservationEvent::Stat(reservation_key))
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObservationEvent::Read(_))));
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missing_manifest_is_typed_after_one_bound_index_read() {
        let manifest_id = "a".repeat(64);
        let index_key = "roots/index/file".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Manifest {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictRemoteManifestIncompleteV1::MissingObject,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        assert_eq!(
            *backend.events.lock().unwrap(),
            vec![
                ObservationEvent::ListOpen("roots/index/".to_owned()),
                ObservationEvent::ListRow(index_key.clone()),
                ObservationEvent::ListEof("roots/index/".to_owned()),
                ObservationEvent::ListOpen("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::ListEof("roots/.tcfs-namespace/v1/".to_owned()),
                ObservationEvent::Stat(index_key.clone()),
                ObservationEvent::Read(index_key),
                ObservationEvent::Stat(manifest_key),
            ]
        );
    }

    #[tokio::test]
    async fn unbound_manifest_is_typed_without_reading_manifest_bytes() {
        let manifest_bytes = regular_manifest_json("file");
        let manifest_id = manifest_object_id(&manifest_bytes);
        let index_key = "roots/index/file".to_owned();
        let manifest_key = format!("roots/manifests/{manifest_id}");
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(committed_index_json(&manifest_id), "index-v1"),
        );
        backend.objects.lock().unwrap().insert(
            manifest_key.clone(),
            ScriptedBoundObject {
                bytes: manifest_bytes,
                etag: None,
                version: None,
            },
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Manifest {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictRemoteManifestIncompleteV1::UnboundObject,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == ObservationEvent::Stat(manifest_key.clone()))
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|event| *event == ObservationEvent::Read(manifest_key.clone())));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Read(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn listed_missing_object_is_typed_and_never_retried() {
        let index_key = "roots/index/file".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                    pass: RemoteNamespaceListingPassV1::First,
                    kind: BoundRemoteObjectKindV1::OrdinaryIndex,
                }
            )
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        assert_eq!(
            backend.events.lock().unwrap().last(),
            Some(&ObservationEvent::Stat(index_key))
        );
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObservationEvent::Read(_))));
    }

    #[tokio::test]
    async fn second_pass_disappearance_is_typed_without_a_third_attempt() {
        let index_key = "roots/index/file".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        install_bound_object(
            &backend,
            &index_key,
            bound_object(deleted_index_json(None), "index-v1"),
        );
        let mutation_backend = backend.clone();
        let mutation_key = index_key.clone();

        assert_eq!(
            read_bound_remote_observation_with_between_passes_v1(
                &op,
                "roots",
                move || async move {
                    mutation_backend
                        .objects
                        .lock()
                        .unwrap()
                        .remove(&mutation_key);
                }
            )
            .await
            .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                    pass: RemoteNamespaceListingPassV1::Second,
                    kind: BoundRemoteObjectKindV1::OrdinaryIndex,
                }
            )
        );
        assert!(backend.calls.lock().unwrap().is_empty());
        let events = backend.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Read(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn unbound_listed_object_is_typed_without_reading_bytes() {
        let index_key = "roots/index/file".to_owned();
        let (op, backend) = scripted_list_operator(two_bound_pass_list_calls(
            std::slice::from_ref(&index_key),
            &[],
        ));
        backend.objects.lock().unwrap().insert(
            index_key.clone(),
            ScriptedBoundObject {
                bytes: committed_index_json(&"a".repeat(64)),
                etag: None,
                version: None,
            },
        );

        assert_eq!(
            read_bound_remote_observation_two_pass_v1(&op, "roots")
                .await
                .unwrap(),
            StrictBoundRemoteObservationReadV1::Incomplete(
                StrictBoundRemoteObservationIncompleteV1::Index {
                    pass: RemoteNamespaceListingPassV1::First,
                    reason: StrictRemoteIndexIncompleteV1::UnboundObject,
                }
            )
        );
        let events = backend.events.lock().unwrap();
        assert_eq!(events.last(), Some(&ObservationEvent::Stat(index_key)));
        assert!(!events
            .iter()
            .any(|event| matches!(event, ObservationEvent::Read(_))));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::Stat(_)))
                .count(),
            1
        );
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }
}
