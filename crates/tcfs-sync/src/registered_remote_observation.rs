//! Listed-key acquisition primitives for strict registered-root planning.
//!
//! This module deliberately stops before reading object bodies. Matching
//! keysets from two non-atomic backend listings are not a namespace snapshot,
//! do not satisfy `CompleteOrNoDigestV1`, and cannot mint any digest or plan
//! input. This artifact is diagnostic and must be discarded. A complete
//! acquisition must freshly rerun pass A list+bind and pass B list+bind for
//! every index, marker, reservation, and referenced manifest object.

use futures::TryStreamExt;
use opendal::{EntryMode, Operator};
use std::collections::BTreeSet;
use std::future::Future;
use tcfs_core::config::{RegisteredRootPlanContractV1, RootRemoteContractV1};

use crate::index_entry::{
    namespace_index_prefix, namespace_logical_entry_from_index_path, namespace_reservation_prefix,
    validate_canonical_namespace_remote_prefix, PortableNamespaceRole,
};
use crate::registered_reconcile::validate_registered_remote_logical_path_bounds_v1;

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
    use opendal::raw::{oio, Access, AccessorInfo, OpList, RpList};
    use opendal::services::Memory;
    use opendal::{Capability, Error, ErrorKind, Metadata, OperatorBuilder};
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

    #[derive(Debug)]
    struct ScriptedLister {
        rows: VecDeque<ScriptedListRow>,
    }

    impl oio::List for ScriptedLister {
        async fn next(&mut self) -> opendal::Result<Option<oio::Entry>> {
            match self.rows.pop_front() {
                None => Ok(None),
                Some(ScriptedListRow::Entry(path, mode)) => {
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
    }

    impl Access for ScriptedListBackend {
        type Reader = ();
        type Writer = ();
        type Lister = ScriptedLister;
        type Deleter = ();

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn list(&self, path: &str, args: OpList) -> opendal::Result<(RpList, Self::Lister)> {
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
            Ok((RpList::default(), ScriptedLister { rows: call.rows }))
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
                ..Default::default()
            });
        let backend = ScriptedListBackend {
            info: Arc::new(info),
            calls: Arc::new(Mutex::new(calls.into())),
            observed: Arc::new(Mutex::new(Vec::new())),
        };
        (OperatorBuilder::new(backend.clone()).finish(), backend)
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
}
