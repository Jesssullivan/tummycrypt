//! Strict, read-only inputs for registered-root reconciliation planning.
//!
//! This module is intentionally separate from the compatibility readers and
//! mutable reconciliation engine. V1 planning never recovers remote
//! `preparing` records, reads a state backup, creates a state lock, or performs
//! a storage write.

use anyhow::{Context, Result};
use opendal::Operator;
use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use tcfs_core::config::{RegisteredRootPlanContractV1, RootStateContractV1};
use unicode_normalization::UnicodeNormalization;

use crate::blacklist::Blacklist;
use crate::conflict::{ConflictInfo, VectorClock};
use crate::index_entry::portable_casefold_path;
use crate::index_entry::{
    manifest_object_id, namespace_reservation_object_id, namespace_reservation_prefix,
    read_expected_raw_object_snapshot_v1, read_raw_object_snapshot_v1,
    validate_canonical_namespace_remote_prefix, validate_namespace_logical_path,
    validate_relative_storage_key, validate_storage_key_component,
    validate_trash_safety_copy_route, DeletionEvidence, ExpectedRawObjectBindingV1,
    IndexEntryState, PortableNamespaceReservationV1, PortableNamespaceRole, RawObjectReadV1,
    RawObjectSnapshotV1, RemoteEntryKind, DIRECTORY_MARKER_BYTES,
};
use crate::registered_local_snapshot::PendingStrictLocalSnapshotV1;
use crate::state::{
    read_primary_state_bytes_read_only_v1, FileSyncStatus, ReadOnlyPrimaryStateBytesV1,
};

const REGISTERED_ROOT_IDENTITY_MAX_BYTES_V1: usize = 512;
const REGISTERED_ROOT_RECIPIENT_MAX_BYTES_V1: usize = 1024;
const REGISTERED_ROOT_ALGORITHM_MAX_BYTES_V1: usize = 128;
const REGISTERED_ROOT_WRAPPED_KEY_MAX_BYTES_V1: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UniqueBTreeMap<K, V>(BTreeMap<K, V>);

impl<K, V> UniqueBTreeMap<K, V> {
    fn into_inner(self) -> BTreeMap<K, V> {
        self.0
    }
}

impl<'de, K, V> Deserialize<'de> for UniqueBTreeMap<K, V>
where
    K: Deserialize<'de> + Ord + fmt::Debug,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueMapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for UniqueMapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord + fmt::Debug,
            V: Deserialize<'de>,
        {
            type Value = UniqueBTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map with unique keys")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry()? {
                    if values.insert(key, value).is_some() {
                        return Err(A::Error::custom("duplicate key in registered-root V1 map"));
                    }
                }
                Ok(UniqueBTreeMap(values))
            }
        }

        deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequiredNullable<T>(Option<T>);

impl<'de, T> Deserialize<'de> for RequiredNullable<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictVectorClockWireV1 {
    clocks: UniqueBTreeMap<String, u64>,
}

impl StrictVectorClockWireV1 {
    fn try_into_vector_clock(self) -> Result<VectorClock> {
        self.try_into_vector_clock_bounded(u64::MAX)
    }

    fn try_into_vector_clock_bounded(self, max_entries: u64) -> Result<VectorClock> {
        let clocks = self.clocks.into_inner();
        anyhow::ensure!(
            u64::try_from(clocks.len())
                .context("strict vector-clock entry count does not fit u64")?
                <= max_entries,
            "strict vector clock exceeds its entry bound"
        );
        for (device_id, counter) in &clocks {
            validate_strict_identity_v1(device_id, "strict vector-clock device id")?;
            anyhow::ensure!(*counter > 0, "strict vector-clock counters must be nonzero");
        }
        Ok(VectorClock { clocks })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictConflictWireV1 {
    rel_path: String,
    local_vclock: StrictVectorClockWireV1,
    remote_vclock: StrictVectorClockWireV1,
    local_blake3: String,
    remote_blake3: String,
    local_device: String,
    remote_device: String,
    detected_at: u64,
    times_recorded: u64,
    remote_manifest_key: RequiredNullable<String>,
}

impl StrictConflictWireV1 {
    fn try_into_conflict(self) -> Result<ConflictInfo> {
        validate_strict_identity_v1(&self.local_device, "strict conflict local device id")?;
        validate_strict_identity_v1(&self.remote_device, "strict conflict remote device id")?;
        Ok(ConflictInfo {
            rel_path: self.rel_path,
            local_vclock: self.local_vclock.try_into_vector_clock()?,
            remote_vclock: self.remote_vclock.try_into_vector_clock()?,
            local_blake3: self.local_blake3,
            remote_blake3: self.remote_blake3,
            local_device: self.local_device,
            remote_device: self.remote_device,
            detected_at: self.detected_at,
            times_recorded: self.times_recorded,
            remote_manifest_key: self.remote_manifest_key.0,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSyncStateWireV1 {
    blake3: String,
    size: u64,
    mtime: u64,
    chunk_count: u64,
    remote_path: String,
    last_synced: u64,
    vclock: StrictVectorClockWireV1,
    device_id: String,
    #[serde(default)]
    conflict: Option<StrictConflictWireV1>,
    status: FileSyncStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictStateCacheWireV1 {
    last_nats_seq: u64,
    device_id: String,
    entries: UniqueBTreeMap<String, StrictSyncStateWireV1>,
}

/// Strict semantic state for one cache key.
#[derive(Debug, Clone)]
pub struct StrictSyncStateV1 {
    blake3: String,
    size: u64,
    mtime: u64,
    chunk_count: u64,
    remote_path: String,
    last_synced: u64,
    vclock: VectorClock,
    device_id: String,
    conflict: Option<ConflictInfo>,
    status: FileSyncStatus,
}

/// One state entry bound to the selected root and remote namespace.
#[derive(Debug, Clone)]
pub struct BoundStrictSyncStateV1 {
    rel_path: String,
    index_key: String,
    baseline_manifest_key: String,
    state: StrictSyncStateV1,
}

impl BoundStrictSyncStateV1 {
    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    pub fn index_key(&self) -> &str {
        &self.index_key
    }

    pub fn baseline_manifest_key(&self) -> &str {
        &self.baseline_manifest_key
    }

    pub const fn state(&self) -> &StrictSyncStateV1 {
        &self.state
    }
}

impl StrictSyncStateV1 {
    pub fn blake3(&self) -> &str {
        &self.blake3
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn mtime(&self) -> u64 {
        self.mtime
    }

    pub const fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    pub fn remote_path(&self) -> &str {
        &self.remote_path
    }

    pub const fn last_synced(&self) -> u64 {
        self.last_synced
    }

    pub const fn vclock(&self) -> &VectorClock {
        &self.vclock
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub const fn conflict(&self) -> Option<&ConflictInfo> {
        self.conflict.as_ref()
    }

    pub const fn status(&self) -> FileSyncStatus {
        self.status
    }
}

impl StrictSyncStateWireV1 {
    fn into_state(self) -> Result<StrictSyncStateV1> {
        validate_lower_hex_64(&self.blake3, "strict state content digest")?;
        validate_registered_remote_storage_key_bounds_v1(
            &self.remote_path,
            "strict state remote path",
        )?;
        validate_strict_identity_v1(&self.device_id, "strict state device id")?;
        let vclock = self.vclock.try_into_vector_clock()?;
        anyhow::ensure!(
            (self.status == FileSyncStatus::Conflict) == self.conflict.is_some(),
            "strict state conflict status and payload must be present together"
        );
        if let Some(conflict) = self.conflict.as_ref() {
            validate_registered_remote_logical_path_bounds_v1(&conflict.rel_path)?;
            validate_lower_hex_64(&conflict.local_blake3, "strict conflict local digest")?;
            validate_lower_hex_64(&conflict.remote_blake3, "strict conflict remote digest")?;
            validate_strict_identity_v1(&conflict.local_device, "strict conflict local device id")?;
            validate_strict_identity_v1(
                &conflict.remote_device,
                "strict conflict remote device id",
            )?;
            if let Some(key) = conflict.remote_manifest_key.0.as_deref() {
                validate_registered_remote_storage_key_bounds_v1(
                    key,
                    "strict conflict remote manifest key",
                )?;
            }
        }
        Ok(StrictSyncStateV1 {
            blake3: self.blake3,
            size: self.size,
            mtime: self.mtime,
            chunk_count: self.chunk_count,
            remote_path: self.remote_path,
            last_synced: self.last_synced,
            vclock,
            device_id: self.device_id,
            conflict: self
                .conflict
                .map(StrictConflictWireV1::try_into_conflict)
                .transpose()?,
            status: self.status,
        })
    }
}

/// Exact representation digest for one accepted state primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrictPrimaryStateBytesDigestV1([u8; 32]);

impl StrictPrimaryStateBytesDigestV1 {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Strict, quiet, primary-only registered-root state.
pub struct StrictPrimaryStateSnapshotV1 {
    raw_bytes_digest: StrictPrimaryStateBytesDigestV1,
    selected_state_path: PathBuf,
    canonical_local_root: PathBuf,
    remote_prefix: String,
    namespace_claims: BTreeMap<String, StrictStateNamespaceClaimV1>,
    entries: BTreeMap<String, BoundStrictSyncStateV1>,
    last_nats_seq: u64,
    device_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StrictStateNamespaceClaimV1 {
    exact_path: String,
    role: PortableNamespaceRole,
}

impl StrictStateNamespaceClaimV1 {
    pub(crate) fn exact_path(&self) -> &str {
        &self.exact_path
    }

    pub(crate) const fn role(&self) -> PortableNamespaceRole {
        self.role
    }
}

impl fmt::Debug for StrictPrimaryStateSnapshotV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictPrimaryStateSnapshotV1")
            .field("raw_bytes_digest", &self.raw_bytes_digest)
            .field("selected_state_path", &self.selected_state_path)
            .field("canonical_local_root", &self.canonical_local_root)
            .field("remote_prefix", &self.remote_prefix)
            .field("namespace_claim_count", &self.namespace_claims.len())
            .field("entry_count", &self.entries.len())
            .field("last_nats_seq", &self.last_nats_seq)
            .field("device_id", &self.device_id)
            .finish()
    }
}

impl StrictPrimaryStateSnapshotV1 {
    pub const fn raw_bytes_digest(&self) -> StrictPrimaryStateBytesDigestV1 {
        self.raw_bytes_digest
    }

    pub(crate) fn selected_state_path(&self) -> &Path {
        &self.selected_state_path
    }

    pub(crate) fn canonical_local_root(&self) -> &Path {
        &self.canonical_local_root
    }

    pub(crate) fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    pub(crate) fn namespace_claims(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &StrictStateNamespaceClaimV1)> {
        self.namespace_claims
            .iter()
            .map(|(folded_path, claim)| (folded_path.as_str(), claim))
    }

    pub const fn entries(&self) -> &BTreeMap<String, BoundStrictSyncStateV1> {
        &self.entries
    }

    pub const fn last_nats_seq(&self) -> u64 {
        self.last_nats_seq
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictPrimaryStateIncompleteV1 {
    PrimaryMissing,
    WriterActive,
    PrimaryChangedDuringRead,
    InvalidPrimary,
    PersistedEntriesBusy {
        active_keys: Vec<String>,
        locked_keys: Vec<String>,
    },
    InvalidRootBinding,
    NamespaceResourceLimit {
        resource: StrictStateNamespaceResourceV1,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictStateNamespaceResourceV1 {
    GeneratedClaims,
    GeneratedClaimBytes,
    RetainedClaims,
    RetainedClaimBytes,
}

#[derive(Debug, thiserror::Error)]
#[error("strict state namespace claim resource limit exceeded: {0:?}")]
struct StrictStateNamespaceResourceLimitErrorV1(StrictStateNamespaceResourceV1);

impl StrictStateNamespaceResourceLimitErrorV1 {
    const fn resource(&self) -> StrictStateNamespaceResourceV1 {
        self.0
    }
}

fn strict_state_binding_incomplete_v1(error: &anyhow::Error) -> StrictPrimaryStateIncompleteV1 {
    error
        .downcast_ref::<StrictStateNamespaceResourceLimitErrorV1>()
        .map_or(
            StrictPrimaryStateIncompleteV1::InvalidRootBinding,
            |limit| StrictPrimaryStateIncompleteV1::NamespaceResourceLimit {
                resource: limit.resource(),
            },
        )
}

#[derive(Debug)]
pub enum StrictPrimaryStateReadV1 {
    Complete(StrictPrimaryStateSnapshotV1),
    Incomplete(StrictPrimaryStateIncompleteV1),
}

/// Read, strictly parse, and route-bind one selected V1 state primary.
///
/// `Complete` means every persisted entry has an exact canonical path under
/// `canonical_local_root` and an immutable manifest key under
/// `canonical_remote_prefix`; compatibility suffix matching is never used.
pub fn read_and_bind_strict_primary_state_v1(
    state_path: &Path,
    canonical_local_root: &Path,
    canonical_remote_prefix: &str,
) -> Result<StrictPrimaryStateReadV1> {
    read_and_bind_strict_primary_state_inner_v1(
        state_path,
        canonical_local_root,
        canonical_remote_prefix,
        StrictStateRootBindingModeV1::PathnameProbed,
    )
}

/// Bind state lexically to the exact spelling owned by a still-live root
/// capability.
///
/// The pending capability authenticates and retains the selected root
/// spelling. Descendant topology and file/directory role proof remain a
/// mandatory cross-input composition step. Probing cache-key parents by
/// pathname here would reintroduce a second, racy authority path and would
/// reject valid remote-only paths whose parents do not exist locally yet.
#[allow(dead_code)] // Becomes live when the strict remote-universe constructor lands.
pub(crate) fn read_and_bind_strict_primary_state_for_pending_root_v1(
    state_path: &Path,
    pending_local: &PendingStrictLocalSnapshotV1,
    canonical_remote_prefix: &str,
) -> Result<StrictPrimaryStateReadV1> {
    read_and_bind_strict_primary_state_inner_v1(
        state_path,
        pending_local.canonical_local_root(),
        canonical_remote_prefix,
        StrictStateRootBindingModeV1::HeldDescriptor,
    )
}

#[allow(dead_code)] // HeldDescriptor is exercised now and composed in the next source-only slice.
#[derive(Clone, Copy)]
enum StrictStateRootBindingModeV1 {
    PathnameProbed,
    HeldDescriptor,
}

fn read_and_bind_strict_primary_state_inner_v1(
    state_path: &Path,
    canonical_local_root: &Path,
    canonical_remote_prefix: &str,
    binding_mode: StrictStateRootBindingModeV1,
) -> Result<StrictPrimaryStateReadV1> {
    let raw_snapshot = match read_primary_state_bytes_read_only_v1(state_path)? {
        ReadOnlyPrimaryStateBytesV1::Missing => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::PrimaryMissing,
            ));
        }
        ReadOnlyPrimaryStateBytesV1::WriterActive => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::WriterActive,
            ));
        }
        ReadOnlyPrimaryStateBytesV1::Unstable => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::PrimaryChangedDuringRead,
            ));
        }
        ReadOnlyPrimaryStateBytesV1::Snapshot(snapshot) => snapshot,
    };

    let wire: StrictStateCacheWireV1 = match serde_json::from_slice(raw_snapshot.raw_bytes()) {
        Ok(wire) => wire,
        Err(_) => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::InvalidPrimary,
            ));
        }
    };
    if validate_strict_identity_v1(&wire.device_id, "strict state-cache device id").is_err() {
        return Ok(StrictPrimaryStateReadV1::Incomplete(
            StrictPrimaryStateIncompleteV1::InvalidPrimary,
        ));
    }

    let mut active_keys = Vec::new();
    let mut locked_keys = Vec::new();
    let mut raw_entries = BTreeMap::new();
    for (cache_key, state) in wire.entries.into_inner() {
        let state = match state.into_state() {
            Ok(state) => state,
            Err(_) => {
                return Ok(StrictPrimaryStateReadV1::Incomplete(
                    StrictPrimaryStateIncompleteV1::InvalidPrimary,
                ));
            }
        };
        match state.status() {
            FileSyncStatus::Active => active_keys.push(cache_key.clone()),
            FileSyncStatus::Locked => locked_keys.push(cache_key.clone()),
            FileSyncStatus::NotSynced | FileSyncStatus::Synced | FileSyncStatus::Conflict => {}
        }
        raw_entries.insert(cache_key, state);
    }
    if !active_keys.is_empty() || !locked_keys.is_empty() {
        return Ok(StrictPrimaryStateReadV1::Incomplete(
            StrictPrimaryStateIncompleteV1::PersistedEntriesBusy {
                active_keys,
                locked_keys,
            },
        ));
    }
    let bound = match bind_strict_state_entries_v1(
        raw_entries,
        canonical_local_root,
        canonical_remote_prefix,
        binding_mode,
    ) {
        Ok(entries) => entries,
        Err(error) => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                strict_state_binding_incomplete_v1(&error),
            ));
        }
    };

    let raw_digest = StrictPrimaryStateBytesDigestV1(*raw_snapshot.raw_bytes_digest().as_bytes());
    let (_raw_bytes, _) = raw_snapshot.into_parts();
    Ok(StrictPrimaryStateReadV1::Complete(
        StrictPrimaryStateSnapshotV1 {
            raw_bytes_digest: raw_digest,
            selected_state_path: state_path.to_owned(),
            canonical_local_root: canonical_local_root.to_owned(),
            remote_prefix: bound.remote_prefix,
            namespace_claims: bound.namespace_claims,
            entries: bound.entries,
            last_nats_seq: wire.last_nats_seq,
            device_id: wire.device_id,
        },
    ))
}

struct BoundStrictStateEntriesV1 {
    remote_prefix: String,
    namespace_claims: BTreeMap<String, StrictStateNamespaceClaimV1>,
    entries: BTreeMap<String, BoundStrictSyncStateV1>,
}

fn bind_strict_state_entries_v1(
    raw_entries: BTreeMap<String, StrictSyncStateV1>,
    canonical_local_root: &Path,
    canonical_remote_prefix: &str,
    binding_mode: StrictStateRootBindingModeV1,
) -> Result<BoundStrictStateEntriesV1> {
    anyhow::ensure!(
        canonical_local_root.is_absolute(),
        "strict state root binding requires an absolute local root"
    );
    if matches!(binding_mode, StrictStateRootBindingModeV1::PathnameProbed) {
        let observed_root = std::fs::canonicalize(canonical_local_root)
            .context("canonicalizing strict state local root")?;
        anyhow::ensure!(
            observed_root == canonical_local_root,
            "strict state local root input is not its canonical spelling"
        );
        anyhow::ensure!(
            std::fs::symlink_metadata(canonical_local_root)
                .context("inspecting strict state local root")?
                .is_dir(),
            "strict state local root is not a directory"
        );
    }
    canonical_local_root
        .to_str()
        .context("strict state local root is not valid UTF-8")?;

    let prefix = validate_canonical_namespace_remote_prefix(canonical_remote_prefix)?;
    anyhow::ensure!(
        !prefix.is_empty(),
        "strict state root binding requires a non-empty remote prefix"
    );
    validate_registered_remote_storage_key_bounds_v1(prefix, "strict state remote prefix")?;

    let mut namespace_claims = BTreeMap::new();
    let mut namespace_budget = StrictStateNamespaceBudgetV1::default();
    // The retained state-side map is required for ordered cross-source
    // composition. Its state-owned, fingerprinted ceilings prevent one valid
    // primary from amplifying into an unbounded claim set.
    let namespace_contract = RegisteredRootPlanContractV1::strict_v1().state_contract();
    let mut entries = BTreeMap::new();
    for (cache_key, state) in raw_entries {
        let rel_path = bind_strict_cache_key_v1(&cache_key, canonical_local_root, binding_mode)?;
        if Blacklist::default()
            .check_fixed_ingress_path_components(Path::new(&rel_path))
            .is_some()
        {
            anyhow::bail!("strict state cache path is excluded by the fixed profile");
        }
        let baseline_manifest_key =
            validate_exact_manifest_key_v1(prefix, state.remote_path())?.to_owned();
        if let Some(conflict) = state.conflict() {
            anyhow::ensure!(
                conflict.rel_path == rel_path,
                "strict conflict path does not match its bound state path"
            );
            let conflict_manifest_key = conflict
                .remote_manifest_key
                .as_deref()
                .context("strict conflict requires an exact remote manifest pin")?;
            validate_exact_manifest_key_v1(prefix, conflict_manifest_key)?;
        }

        reserve_state_namespace_claims_v1(
            &rel_path,
            &mut namespace_claims,
            &mut namespace_budget,
            namespace_contract,
        )?;
        let index_key = format!("{prefix}/index/{rel_path}");
        validate_registered_remote_storage_key_bounds_v1(&index_key, "strict state index key")?;
        let entry = BoundStrictSyncStateV1 {
            rel_path: rel_path.clone(),
            index_key,
            baseline_manifest_key,
            state,
        };
        anyhow::ensure!(
            entries.insert(rel_path, entry).is_none(),
            "strict state cache contains duplicate bound paths"
        );
    }
    Ok(BoundStrictStateEntriesV1 {
        remote_prefix: prefix.to_owned(),
        namespace_claims,
        entries,
    })
}

fn bind_strict_cache_key_v1(
    cache_key: &str,
    canonical_local_root: &Path,
    binding_mode: StrictStateRootBindingModeV1,
) -> Result<String> {
    anyhow::ensure!(
        !cache_key.is_empty()
            && !cache_key.ends_with('/')
            && !cache_key.contains("//")
            && !cache_key.contains('\\')
            && !cache_key.contains('\u{fffd}')
            && !cache_key.chars().any(char::is_control),
        "strict state cache key has a noncanonical spelling"
    );
    let cache_path = Path::new(cache_key);
    anyhow::ensure!(
        cache_path.is_absolute() && cache_path != canonical_local_root,
        "strict state cache key is not a file path below its selected root"
    );
    let rel_path = cache_path
        .strip_prefix(canonical_local_root)
        .context("strict state cache key is outside its selected root")?
        .to_str()
        .context("strict state relative path is not valid UTF-8")?
        .to_owned();
    validate_registered_remote_logical_path_bounds_v1(&rel_path)?;
    anyhow::ensure!(
        canonical_local_root.join(&rel_path) == cache_path,
        "strict state cache key does not round-trip through its selected root"
    );
    if matches!(binding_mode, StrictStateRootBindingModeV1::PathnameProbed) {
        let parent = cache_path
            .parent()
            .context("strict state cache key has no parent")?;
        anyhow::ensure!(
            std::fs::canonicalize(parent)
                .context("canonicalizing strict state cache-key parent")?
                == parent,
            "strict state cache-key parent is missing or uses an alternate path"
        );
    }
    Ok(rel_path)
}

fn validate_exact_manifest_key_v1<'a>(remote_prefix: &str, key: &'a str) -> Result<&'a str> {
    validate_registered_remote_storage_key_bounds_v1(key, "strict state manifest key")?;
    let manifest_prefix = format!("{remote_prefix}/manifests/");
    let object_id = key
        .strip_prefix(&manifest_prefix)
        .context("strict state remote path is outside its manifest namespace")?;
    validate_lower_hex_64(object_id, "strict state manifest object id")?;
    anyhow::ensure!(
        key == format!("{manifest_prefix}{object_id}"),
        "strict state manifest key is not canonical"
    );
    Ok(key)
}

#[derive(Default)]
struct StrictStateNamespaceBudgetV1 {
    generated_claims: u64,
    generated_claim_bytes: u64,
    retained_claims: u64,
    retained_claim_bytes: u64,
}

fn checked_state_namespace_increment_v1(
    value: &mut u64,
    increment: u64,
    maximum: u64,
    resource: StrictStateNamespaceResourceV1,
) -> Result<()> {
    *value = value
        .checked_add(increment)
        .filter(|next| *next <= maximum)
        .ok_or(StrictStateNamespaceResourceLimitErrorV1(resource))?;
    Ok(())
}

impl StrictStateNamespaceBudgetV1 {
    fn observe_generated_claim(
        &mut self,
        claim_bytes: u64,
        contract: RootStateContractV1,
    ) -> Result<()> {
        checked_state_namespace_increment_v1(
            &mut self.generated_claims,
            1,
            contract.max_generated_claim_observations(),
            StrictStateNamespaceResourceV1::GeneratedClaims,
        )?;
        checked_state_namespace_increment_v1(
            &mut self.generated_claim_bytes,
            claim_bytes,
            contract.max_generated_claim_bytes(),
            StrictStateNamespaceResourceV1::GeneratedClaimBytes,
        )
    }

    fn observe_retained_claim(
        &mut self,
        claim_bytes: u64,
        contract: RootStateContractV1,
    ) -> Result<()> {
        checked_state_namespace_increment_v1(
            &mut self.retained_claims,
            1,
            contract.max_retained_unique_claims(),
            StrictStateNamespaceResourceV1::RetainedClaims,
        )?;
        checked_state_namespace_increment_v1(
            &mut self.retained_claim_bytes,
            claim_bytes,
            contract.max_retained_unique_claim_bytes(),
            StrictStateNamespaceResourceV1::RetainedClaimBytes,
        )
    }
}

fn reserve_state_namespace_claims_v1(
    rel_path: &str,
    claims: &mut BTreeMap<String, StrictStateNamespaceClaimV1>,
    budget: &mut StrictStateNamespaceBudgetV1,
    contract: RootStateContractV1,
) -> Result<()> {
    let components: Vec<&str> = rel_path.split('/').collect();
    for end in 1..=components.len() {
        let exact_path = components[..end].join("/");
        let role = if end == components.len() {
            PortableNamespaceRole::File
        } else {
            PortableNamespaceRole::Directory
        };
        let folded_path = portable_casefold_path(&exact_path)?;
        let claim_bytes = u64::try_from(exact_path.len())
            .ok()
            .and_then(|exact_bytes| {
                u64::try_from(folded_path.len())
                    .ok()
                    .and_then(|folded_bytes| exact_bytes.checked_add(folded_bytes))
            })
            .ok_or(StrictStateNamespaceResourceLimitErrorV1(
                StrictStateNamespaceResourceV1::GeneratedClaimBytes,
            ))?;
        budget.observe_generated_claim(claim_bytes, contract)?;
        if let Some(existing) = claims.get(&folded_path) {
            anyhow::ensure!(
                existing.exact_path == exact_path && existing.role == role,
                "strict state namespace has a portable spelling or role collision"
            );
        } else {
            budget.observe_retained_claim(claim_bytes, contract)?;
            claims.insert(
                folded_path,
                StrictStateNamespaceClaimV1 { exact_path, role },
            );
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictRemoteIndexEntryWireV1 {
    manifest_hash: String,
    size: u64,
    chunks: u64,
    #[serde(default)]
    kind: Option<RemoteEntryKind>,
    #[serde(default)]
    symlink_target: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPendingIndexEntryWireV1 {
    manifest_hash: String,
    size: u64,
    chunks: u64,
    #[serde(default)]
    kind: Option<RemoteEntryKind>,
    #[serde(default)]
    symlink_target: Option<String>,
    staged_manifest_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictDeletionEvidenceWireV1 {
    safety_copy_key: String,
    safety_copy_blake3: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictVersionedIndexEntryWireV1 {
    version: u8,
    state: IndexEntryState,
    current: RequiredNullable<StrictRemoteIndexEntryWireV1>,
    pending: RequiredNullable<StrictPendingIndexEntryWireV1>,
    #[serde(default)]
    deletion_evidence: Option<StrictDeletionEvidenceWireV1>,
}

/// Strict committed manifest pointer from a V1 index record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictRemoteIndexEntryV1 {
    manifest_hash: String,
    size: u64,
    chunks: u64,
    kind: RemoteEntryKind,
    symlink_target: Option<String>,
}

impl StrictRemoteIndexEntryV1 {
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn chunks(&self) -> u64 {
        self.chunks
    }

    pub const fn kind(&self) -> RemoteEntryKind {
        self.kind
    }

    pub fn symlink_target(&self) -> Option<&str> {
        self.symlink_target.as_deref()
    }
}

impl StrictRemoteIndexEntryWireV1 {
    fn into_entry(self) -> Result<StrictRemoteIndexEntryV1> {
        strict_remote_entry_v1(
            self.manifest_hash,
            self.size,
            self.chunks,
            self.kind,
            self.symlink_target,
        )
    }
}

/// Strict pending publication pointer retained only for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictPendingIndexEntryV1 {
    entry: StrictRemoteIndexEntryV1,
    staged_manifest_key: String,
}

impl StrictPendingIndexEntryV1 {
    pub const fn entry(&self) -> &StrictRemoteIndexEntryV1 {
        &self.entry
    }

    pub fn staged_manifest_key(&self) -> &str {
        &self.staged_manifest_key
    }
}

impl StrictPendingIndexEntryWireV1 {
    fn into_entry(self) -> Result<StrictPendingIndexEntryV1> {
        validate_registered_remote_storage_key_bounds_v1(
            &self.staged_manifest_key,
            "strict pending staged manifest key",
        )?;
        Ok(StrictPendingIndexEntryV1 {
            entry: strict_remote_entry_v1(
                self.manifest_hash,
                self.size,
                self.chunks,
                self.kind,
                self.symlink_target,
            )?,
            staged_manifest_key: self.staged_manifest_key,
        })
    }
}

fn strict_remote_entry_v1(
    manifest_hash: String,
    size: u64,
    chunks: u64,
    kind: Option<RemoteEntryKind>,
    symlink_target: Option<String>,
) -> Result<StrictRemoteIndexEntryV1> {
    validate_lower_hex_64(&manifest_hash, "strict index manifest object id")?;
    let kind = kind.unwrap_or(RemoteEntryKind::RegularFile);
    match kind {
        RemoteEntryKind::RegularFile => {
            anyhow::ensure!(
                symlink_target.is_none(),
                "strict regular index entry forbids symlink target"
            );
        }
        RemoteEntryKind::Symlink => {
            let target = symlink_target
                .as_deref()
                .context("strict symlink index entry requires target")?;
            validate_registered_symlink_target_bound_v1(target)?;
            anyhow::ensure!(
                !target.is_empty() && !target.chars().any(char::is_control),
                "strict symlink index target must be non-empty and control-free"
            );
            anyhow::ensure!(
                chunks == 0
                    && size
                        == u64::try_from(target.len())
                            .context("strict symlink target length does not fit u64")?,
                "strict symlink index metadata does not match target"
            );
        }
    }
    Ok(StrictRemoteIndexEntryV1 {
        manifest_hash,
        size,
        chunks,
        kind,
        symlink_target,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredRootRemoteObjectBindingV1 {
    Version {
        version: String,
        etag: Option<String>,
    },
    Etag {
        etag: String,
    },
}

pub(crate) fn expected_raw_object_binding_v1(
    binding: &RegisteredRootRemoteObjectBindingV1,
) -> ExpectedRawObjectBindingV1<'_> {
    match binding {
        RegisteredRootRemoteObjectBindingV1::Version { version, etag } => {
            ExpectedRawObjectBindingV1::Version {
                version,
                etag: etag.as_deref(),
            }
        }
        RegisteredRootRemoteObjectBindingV1::Etag { etag } => {
            ExpectedRawObjectBindingV1::Etag { etag }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct BoundRemoteObjectSnapshotV1 {
    raw_bytes_len: u64,
    raw_blake3: [u8; 32],
    binding: RegisteredRootRemoteObjectBindingV1,
}

impl fmt::Debug for BoundRemoteObjectSnapshotV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundRemoteObjectSnapshotV1")
            .field("raw_bytes_len", &self.raw_bytes_len)
            .field("raw_blake3", &blake3::Hash::from_bytes(self.raw_blake3))
            .field("binding", &self.binding)
            .finish()
    }
}

impl BoundRemoteObjectSnapshotV1 {
    pub const fn raw_bytes_len(&self) -> u64 {
        self.raw_bytes_len
    }

    pub const fn raw_blake3(&self) -> &[u8; 32] {
        &self.raw_blake3
    }

    pub const fn binding(&self) -> &RegisteredRootRemoteObjectBindingV1 {
        &self.binding
    }
}

pub(crate) fn bind_remote_object_v1(snapshot: RawObjectSnapshotV1) -> BoundRemoteObjectSnapshotV1 {
    let binding = if let Some(version) = snapshot.binding().version() {
        RegisteredRootRemoteObjectBindingV1::Version {
            version: version.to_owned(),
            etag: snapshot.binding().etag().map(str::to_owned),
        }
    } else {
        RegisteredRootRemoteObjectBindingV1::Etag {
            etag: snapshot
                .binding()
                .etag()
                .expect("bound non-versioned object must carry an ETag")
                .to_owned(),
        }
    };
    let (raw_bytes, raw_blake3, _) = snapshot.into_parts();
    BoundRemoteObjectSnapshotV1 {
        raw_bytes_len: u64::try_from(raw_bytes.len())
            .expect("bounded remote object length must fit u64"),
        raw_blake3: *raw_blake3.as_bytes(),
        binding,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictIndexRecordFormatV1 {
    Version2,
    Version3,
    Version4Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRootRemoteIndexRouteV1 {
    index_key: String,
    remote_prefix_len: usize,
    index_rel_offset: usize,
}

impl RegisteredRootRemoteIndexRouteV1 {
    pub fn remote_prefix(&self) -> &str {
        self.index_key
            .get(..self.remote_prefix_len)
            .expect("bound index prefix offset is an internal invariant")
    }

    pub fn rel_path(&self) -> &str {
        self.index_key
            .get(self.index_rel_offset..)
            .expect("bound index relative-path offset is an internal invariant")
    }

    pub fn index_key(&self) -> &str {
        &self.index_key
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommittedIndexEntryV1 {
    object: BoundRemoteObjectSnapshotV1,
    current: StrictRemoteIndexEntryV1,
    format: StrictIndexRecordFormatV1,
    route: RegisteredRootRemoteIndexRouteV1,
}

impl RawCommittedIndexEntryV1 {
    pub const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        &self.object
    }

    pub const fn current(&self) -> &StrictRemoteIndexEntryV1 {
        &self.current
    }

    pub const fn format(&self) -> StrictIndexRecordFormatV1 {
        self.format
    }

    pub const fn route(&self) -> &RegisteredRootRemoteIndexRouteV1 {
        &self.route
    }

    pub fn remote_prefix(&self) -> &str {
        self.route.remote_prefix()
    }

    pub fn rel_path(&self) -> &str {
        self.route.rel_path()
    }

    pub fn index_key(&self) -> &str {
        self.route.index_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDeletedIndexEntryV1 {
    object: BoundRemoteObjectSnapshotV1,
    deletion_evidence: Option<DeletionEvidence>,
    route: RegisteredRootRemoteIndexRouteV1,
}

impl RawDeletedIndexEntryV1 {
    pub const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        &self.object
    }

    pub const fn deletion_evidence(&self) -> Option<&DeletionEvidence> {
        self.deletion_evidence.as_ref()
    }

    pub const fn route(&self) -> &RegisteredRootRemoteIndexRouteV1 {
        &self.route
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictRemoteIndexIncompleteV1 {
    UnboundObject,
    InvalidIndexRecord,
    PreparingObserved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactRawIndexEntryReadV1 {
    Missing,
    Incomplete(StrictRemoteIndexIncompleteV1),
    Deleted(RawDeletedIndexEntryV1),
    Committed(RawCommittedIndexEntryV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExactObservedRawIndexEntryReadV1 {
    Missing,
    Incomplete {
        reason: StrictRemoteIndexIncompleteV1,
        observed_object: Option<BoundRemoteObjectSnapshotV1>,
    },
    Deleted(RawDeletedIndexEntryV1),
    Committed(RawCommittedIndexEntryV1),
}

enum ParsedStrictIndexRecordV1 {
    Deleted {
        deletion_evidence: Option<DeletionEvidence>,
    },
    Preparing {
        current: Option<StrictRemoteIndexEntryV1>,
        pending: StrictPendingIndexEntryV1,
    },
    Committed {
        current: StrictRemoteIndexEntryV1,
        format: StrictIndexRecordFormatV1,
    },
}

fn parse_strict_index_record_v1(bytes: &[u8]) -> Result<ParsedStrictIndexRecordV1> {
    let wire: StrictVersionedIndexEntryWireV1 =
        serde_json::from_slice(bytes).context("parsing strict V1 index record")?;
    let current = wire
        .current
        .0
        .map(StrictRemoteIndexEntryWireV1::into_entry)
        .transpose()?;
    let pending = wire
        .pending
        .0
        .map(StrictPendingIndexEntryWireV1::into_entry)
        .transpose()?;
    let deletion_evidence = wire
        .deletion_evidence
        .map(|evidence| -> Result<DeletionEvidence> {
            validate_registered_remote_storage_key_bounds_v1(
                &evidence.safety_copy_key,
                "strict deletion safety-copy key",
            )?;
            validate_lower_hex_64(
                &evidence.safety_copy_blake3,
                "strict deletion safety-copy digest",
            )?;
            Ok(DeletionEvidence {
                safety_copy_key: evidence.safety_copy_key,
                safety_copy_blake3: evidence.safety_copy_blake3,
            })
        })
        .transpose()?;

    match (wire.version, wire.state) {
        (4, IndexEntryState::Deleted) => {
            anyhow::ensure!(
                current.is_none() && pending.is_none(),
                "strict deleted index record forbids current and pending entries"
            );
            Ok(ParsedStrictIndexRecordV1::Deleted { deletion_evidence })
        }
        (2 | 3, IndexEntryState::Committed) => {
            anyhow::ensure!(
                pending.is_none() && deletion_evidence.is_none(),
                "strict committed index record forbids pending/deletion evidence"
            );
            let current =
                current.context("strict committed index record requires current entry")?;
            validate_index_format_matches_entries_v1(wire.version, Some(&current), None)?;
            Ok(ParsedStrictIndexRecordV1::Committed {
                current,
                format: if wire.version == 2 {
                    StrictIndexRecordFormatV1::Version2
                } else {
                    StrictIndexRecordFormatV1::Version3
                },
            })
        }
        (2 | 3, IndexEntryState::Preparing) => {
            anyhow::ensure!(
                deletion_evidence.is_none(),
                "strict preparing index record forbids deletion evidence"
            );
            let pending =
                pending.context("strict preparing index record requires pending entry")?;
            validate_index_format_matches_entries_v1(
                wire.version,
                current.as_ref(),
                Some(pending.entry()),
            )?;
            Ok(ParsedStrictIndexRecordV1::Preparing { current, pending })
        }
        _ => anyhow::bail!("unsupported strict V1 index version/state combination"),
    }
}

fn validate_index_format_matches_entries_v1(
    version: u8,
    current: Option<&StrictRemoteIndexEntryV1>,
    pending: Option<&StrictRemoteIndexEntryV1>,
) -> Result<()> {
    let has_symlink = current
        .into_iter()
        .chain(pending)
        .any(|entry| entry.kind() == RemoteEntryKind::Symlink);
    anyhow::ensure!(
        (version == 3) == has_symlink,
        "strict index version does not match entry kinds"
    );
    Ok(())
}

fn validate_strict_index_route_semantics_v1(
    parsed: &ParsedStrictIndexRecordV1,
    remote_prefix: &str,
    rel_path: &str,
) -> Result<()> {
    let validate_entry = |entry: &StrictRemoteIndexEntryV1| -> Result<()> {
        if let Some(target) = entry.symlink_target() {
            crate::engine::validate_indexed_symlink_target(Path::new(rel_path), target)?;
        }
        Ok(())
    };
    match parsed {
        ParsedStrictIndexRecordV1::Deleted { deletion_evidence } => {
            if let Some(evidence) = deletion_evidence {
                validate_trash_safety_copy_route(
                    remote_prefix,
                    rel_path,
                    &evidence.safety_copy_key,
                )?;
            }
        }
        ParsedStrictIndexRecordV1::Preparing {
            current, pending, ..
        } => {
            if let Some(current) = current {
                validate_entry(current)?;
            }
            validate_entry(pending.entry())?;
        }
        ParsedStrictIndexRecordV1::Committed { current, .. } => validate_entry(current)?,
    }
    Ok(())
}

/// Read and classify one exact canonical path-index object without recovery.
pub async fn read_exact_raw_index_entry_v1(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
) -> Result<ExactRawIndexEntryReadV1> {
    Ok(
        match read_exact_observed_raw_index_entry_v1(op, remote_prefix, rel_path).await? {
            ExactObservedRawIndexEntryReadV1::Missing => ExactRawIndexEntryReadV1::Missing,
            ExactObservedRawIndexEntryReadV1::Incomplete { reason, .. } => {
                ExactRawIndexEntryReadV1::Incomplete(reason)
            }
            ExactObservedRawIndexEntryReadV1::Deleted(index) => {
                ExactRawIndexEntryReadV1::Deleted(index)
            }
            ExactObservedRawIndexEntryReadV1::Committed(index) => {
                ExactRawIndexEntryReadV1::Committed(index)
            }
        },
    )
}

pub(crate) async fn read_exact_observed_raw_index_entry_v1(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
) -> Result<ExactObservedRawIndexEntryReadV1> {
    let route = strict_remote_index_route_v1(remote_prefix, rel_path)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_index_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, route.index_key(), max_bytes).await?
    else {
        return Ok(ExactObservedRawIndexEntryReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedRawIndexEntryReadV1::Incomplete {
                reason: StrictRemoteIndexIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_raw_index_entry_v1(raw_snapshot, route))
}

#[allow(dead_code)] // Composed by the catalog-authoritative remote-universe pass.
pub(crate) async fn read_expected_observed_raw_index_entry_v1(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    expected_binding: &RegisteredRootRemoteObjectBindingV1,
) -> Result<ExactObservedRawIndexEntryReadV1> {
    let route = strict_remote_index_route_v1(remote_prefix, rel_path)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_index_object_bytes();
    let expected = expected_raw_object_binding_v1(expected_binding);
    let Some(raw_read) =
        read_expected_raw_object_snapshot_v1(op, route.index_key(), max_bytes, expected).await?
    else {
        return Ok(ExactObservedRawIndexEntryReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedRawIndexEntryReadV1::Incomplete {
                reason: StrictRemoteIndexIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_raw_index_entry_v1(raw_snapshot, route))
}

fn strict_remote_index_route_v1(
    remote_prefix: &str,
    rel_path: &str,
) -> Result<RegisteredRootRemoteIndexRouteV1> {
    let suffix_bytes = "index/"
        .len()
        .checked_add(rel_path.len())
        .context("strict index key length overflow")?;
    validate_registered_remote_derived_key_length_v1(
        remote_prefix,
        suffix_bytes,
        "strict index key",
    )?;
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_registered_remote_logical_path_bounds_v1(rel_path)?;
    let index_key = if prefix.is_empty() {
        format!("index/{rel_path}")
    } else {
        format!("{prefix}/index/{rel_path}")
    };
    let index_rel_offset = index_key
        .len()
        .checked_sub(rel_path.len())
        .expect("validated index key must end with its relative path");
    Ok(RegisteredRootRemoteIndexRouteV1 {
        index_key,
        remote_prefix_len: prefix.len(),
        index_rel_offset,
    })
}

fn classify_observed_raw_index_entry_v1(
    raw_snapshot: RawObjectSnapshotV1,
    route: RegisteredRootRemoteIndexRouteV1,
) -> ExactObservedRawIndexEntryReadV1 {
    let parsed = match parse_strict_index_record_v1(raw_snapshot.raw_bytes()) {
        Ok(parsed)
            if validate_strict_index_route_semantics_v1(
                &parsed,
                route.remote_prefix(),
                route.rel_path(),
            )
            .is_ok() =>
        {
            parsed
        }
        _ => {
            return ExactObservedRawIndexEntryReadV1::Incomplete {
                reason: StrictRemoteIndexIncompleteV1::InvalidIndexRecord,
                observed_object: Some(bind_remote_object_v1(raw_snapshot)),
            };
        }
    };
    if matches!(&parsed, ParsedStrictIndexRecordV1::Preparing { .. }) {
        return ExactObservedRawIndexEntryReadV1::Incomplete {
            reason: StrictRemoteIndexIncompleteV1::PreparingObserved,
            observed_object: Some(bind_remote_object_v1(raw_snapshot)),
        };
    }
    let object = bind_remote_object_v1(raw_snapshot);

    match parsed {
        ParsedStrictIndexRecordV1::Deleted { deletion_evidence } => {
            ExactObservedRawIndexEntryReadV1::Deleted(RawDeletedIndexEntryV1 {
                object,
                deletion_evidence,
                route,
            })
        }
        ParsedStrictIndexRecordV1::Preparing { .. } => {
            unreachable!("preparing records return incomplete before binding object evidence")
        }
        ParsedStrictIndexRecordV1::Committed { current, format } => {
            ExactObservedRawIndexEntryReadV1::Committed(RawCommittedIndexEntryV1 {
                object,
                current,
                format,
                route,
            })
        }
    }
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RegisteredRootRemoteDirectoryMarkerRouteV1 {
    marker_key: String,
    remote_prefix_len: usize,
    marker_rel_offset: usize,
    logical_rel_len: usize,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
impl RegisteredRootRemoteDirectoryMarkerRouteV1 {
    pub(crate) fn remote_prefix(&self) -> &str {
        self.marker_key
            .get(..self.remote_prefix_len)
            .expect("bound marker prefix offset is an internal invariant")
    }

    pub(crate) fn logical_dir(&self) -> &str {
        let end = self
            .marker_rel_offset
            .checked_add(self.logical_rel_len)
            .expect("bound marker logical-path length is an internal invariant");
        self.marker_key
            .get(self.marker_rel_offset..end)
            .expect("bound marker logical-path offsets are an internal invariant")
    }

    pub(crate) fn marker_rel_path(&self) -> &str {
        self.marker_key
            .get(self.marker_rel_offset..)
            .expect("bound marker relative-path offset is an internal invariant")
    }

    pub(crate) fn marker_key(&self) -> &str {
        &self.marker_key
    }
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawLiveDirectoryMarkerV1 {
    object: BoundRemoteObjectSnapshotV1,
    route: RegisteredRootRemoteDirectoryMarkerRouteV1,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
impl RawLiveDirectoryMarkerV1 {
    pub(crate) const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        &self.object
    }

    pub(crate) const fn route(&self) -> &RegisteredRootRemoteDirectoryMarkerRouteV1 {
        &self.route
    }
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawDeletedDirectoryMarkerV1 {
    object: BoundRemoteObjectSnapshotV1,
    deletion_evidence: Option<DeletionEvidence>,
    route: RegisteredRootRemoteDirectoryMarkerRouteV1,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
impl RawDeletedDirectoryMarkerV1 {
    pub(crate) const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        &self.object
    }

    pub(crate) const fn deletion_evidence(&self) -> Option<&DeletionEvidence> {
        self.deletion_evidence.as_ref()
    }

    pub(crate) const fn route(&self) -> &RegisteredRootRemoteDirectoryMarkerRouteV1 {
        &self.route
    }
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrictRemoteDirectoryMarkerIncompleteV1 {
    UnboundObject,
    InvalidMarkerRecord,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExactRawDirectoryMarkerReadV1 {
    Missing,
    Incomplete(StrictRemoteDirectoryMarkerIncompleteV1),
    Live(RawLiveDirectoryMarkerV1),
    Deleted(RawDeletedDirectoryMarkerV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExactObservedRawDirectoryMarkerReadV1 {
    Missing,
    Incomplete {
        reason: StrictRemoteDirectoryMarkerIncompleteV1,
        observed_object: Option<BoundRemoteObjectSnapshotV1>,
    },
    Live(RawLiveDirectoryMarkerV1),
    Deleted(RawDeletedDirectoryMarkerV1),
}

/// Read one listed directory-marker candidate by storage identity.
///
/// The only accepted live body is [`DIRECTORY_MARKER_BYTES`]. The only
/// accepted structured body is a strict v4 Deleted index record. Compatibility
/// marker readers are intentionally excluded from registered-root planning.
#[allow(dead_code)] // Composed by the next full list-and-bind pass.
pub(crate) async fn read_exact_raw_directory_marker_v1(
    op: &Operator,
    remote_prefix: &str,
    logical_dir: &str,
) -> Result<ExactRawDirectoryMarkerReadV1> {
    Ok(
        match read_exact_observed_raw_directory_marker_v1(op, remote_prefix, logical_dir).await? {
            ExactObservedRawDirectoryMarkerReadV1::Missing => {
                ExactRawDirectoryMarkerReadV1::Missing
            }
            ExactObservedRawDirectoryMarkerReadV1::Incomplete { reason, .. } => {
                ExactRawDirectoryMarkerReadV1::Incomplete(reason)
            }
            ExactObservedRawDirectoryMarkerReadV1::Live(marker) => {
                ExactRawDirectoryMarkerReadV1::Live(marker)
            }
            ExactObservedRawDirectoryMarkerReadV1::Deleted(marker) => {
                ExactRawDirectoryMarkerReadV1::Deleted(marker)
            }
        },
    )
}

pub(crate) async fn read_exact_observed_raw_directory_marker_v1(
    op: &Operator,
    remote_prefix: &str,
    logical_dir: &str,
) -> Result<ExactObservedRawDirectoryMarkerReadV1> {
    let route = strict_remote_directory_marker_route_v1(remote_prefix, logical_dir)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_index_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, route.marker_key(), max_bytes).await?
    else {
        return Ok(ExactObservedRawDirectoryMarkerReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                reason: StrictRemoteDirectoryMarkerIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_raw_directory_marker_v1(
        raw_snapshot,
        route,
    ))
}

#[allow(dead_code)] // Composed by the catalog-authoritative remote-universe pass.
pub(crate) async fn read_expected_observed_raw_directory_marker_v1(
    op: &Operator,
    remote_prefix: &str,
    logical_dir: &str,
    expected_binding: &RegisteredRootRemoteObjectBindingV1,
) -> Result<ExactObservedRawDirectoryMarkerReadV1> {
    let route = strict_remote_directory_marker_route_v1(remote_prefix, logical_dir)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_index_object_bytes();
    let expected = expected_raw_object_binding_v1(expected_binding);
    let Some(raw_read) =
        read_expected_raw_object_snapshot_v1(op, route.marker_key(), max_bytes, expected).await?
    else {
        return Ok(ExactObservedRawDirectoryMarkerReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                reason: StrictRemoteDirectoryMarkerIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_raw_directory_marker_v1(
        raw_snapshot,
        route,
    ))
}

fn strict_remote_directory_marker_route_v1(
    remote_prefix: &str,
    logical_dir: &str,
) -> Result<RegisteredRootRemoteDirectoryMarkerRouteV1> {
    let marker_rel_path_bytes = logical_dir
        .len()
        .checked_add("/.tcfs_dir".len())
        .context("strict directory-marker relative-path length overflow")?;
    let suffix_bytes = "index/"
        .len()
        .checked_add(marker_rel_path_bytes)
        .context("strict directory-marker key length overflow")?;
    validate_registered_remote_derived_key_length_v1(
        remote_prefix,
        suffix_bytes,
        "strict directory-marker key",
    )?;
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_registered_remote_logical_path_bounds_v1(logical_dir)?;
    let marker_rel_path = format!("{logical_dir}/.tcfs_dir");
    let index_key = if prefix.is_empty() {
        format!("index/{marker_rel_path}")
    } else {
        format!("{prefix}/index/{marker_rel_path}")
    };
    validate_registered_remote_storage_key_bounds_v1(&index_key, "strict directory-marker key")?;
    let marker_rel_offset = index_key
        .len()
        .checked_sub(marker_rel_path.len())
        .expect("validated marker key must end with its relative path");
    Ok(RegisteredRootRemoteDirectoryMarkerRouteV1 {
        marker_key: index_key,
        remote_prefix_len: prefix.len(),
        marker_rel_offset,
        logical_rel_len: logical_dir.len(),
    })
}

fn classify_observed_raw_directory_marker_v1(
    raw_snapshot: RawObjectSnapshotV1,
    route: RegisteredRootRemoteDirectoryMarkerRouteV1,
) -> ExactObservedRawDirectoryMarkerReadV1 {
    if raw_snapshot.raw_bytes() == DIRECTORY_MARKER_BYTES {
        return ExactObservedRawDirectoryMarkerReadV1::Live(RawLiveDirectoryMarkerV1 {
            object: bind_remote_object_v1(raw_snapshot),
            route,
        });
    }
    let deletion_evidence = match parse_strict_index_record_v1(raw_snapshot.raw_bytes()) {
        Ok(parsed @ ParsedStrictIndexRecordV1::Deleted { .. })
            if validate_strict_index_route_semantics_v1(
                &parsed,
                route.remote_prefix(),
                route.marker_rel_path(),
            )
            .is_ok() =>
        {
            match parsed {
                ParsedStrictIndexRecordV1::Deleted { deletion_evidence } => deletion_evidence,
                _ => unreachable!("guard only accepts a strict deleted marker"),
            }
        }
        _ => {
            return ExactObservedRawDirectoryMarkerReadV1::Incomplete {
                reason: StrictRemoteDirectoryMarkerIncompleteV1::InvalidMarkerRecord,
                observed_object: Some(bind_remote_object_v1(raw_snapshot)),
            };
        }
    };
    ExactObservedRawDirectoryMarkerReadV1::Deleted(RawDeletedDirectoryMarkerV1 {
        object: bind_remote_object_v1(raw_snapshot),
        deletion_evidence,
        route,
    })
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BoundNamespaceReservationV1 {
    object: BoundRemoteObjectSnapshotV1,
    object_key: String,
    object_id_offset: usize,
    reservation: PortableNamespaceReservationV1,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
impl BoundNamespaceReservationV1 {
    pub(crate) const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        &self.object
    }

    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }

    pub(crate) fn object_id(&self) -> &str {
        self.object_key
            .get(self.object_id_offset..)
            .expect("bound reservation-key offset is an internal invariant")
    }

    pub(crate) fn exact_path(&self) -> &str {
        self.reservation.exact_path()
    }

    pub(crate) fn folded_path(&self) -> &str {
        self.reservation.folded_path()
    }

    pub(crate) const fn role(&self) -> PortableNamespaceRole {
        self.reservation.role()
    }
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrictNamespaceReservationIncompleteV1 {
    UnboundObject,
    InvalidReservation,
    AddressMismatch,
}

#[allow(dead_code)] // Composed by the next full list-and-bind pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExactNamespaceReservationReadV1 {
    Missing,
    Incomplete(StrictNamespaceReservationIncompleteV1),
    Bound(BoundNamespaceReservationV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExactObservedNamespaceReservationReadV1 {
    Missing,
    Incomplete {
        reason: StrictNamespaceReservationIncompleteV1,
        observed_object: Option<BoundRemoteObjectSnapshotV1>,
    },
    Bound(BoundNamespaceReservationV1),
}

struct StrictNamespaceReservationRouteV1 {
    object_key: String,
    object_id_offset: usize,
}

impl StrictNamespaceReservationRouteV1 {
    fn object_key(&self) -> &str {
        &self.object_key
    }

    fn object_id(&self) -> &str {
        self.object_key
            .get(self.object_id_offset..)
            .expect("strict reservation-key offset is an internal invariant")
    }
}

/// Read one listed portable-namespace reservation by storage identity.
///
/// V1 planning accepts only the canonical serialized reservation body whose
/// folded path hashes to the listed 64-hex object ID.
#[allow(dead_code)] // Composed by the next full list-and-bind pass.
pub(crate) async fn read_exact_namespace_reservation_v1(
    op: &Operator,
    remote_prefix: &str,
    object_id: &str,
) -> Result<ExactNamespaceReservationReadV1> {
    Ok(
        match read_exact_observed_namespace_reservation_v1(op, remote_prefix, object_id).await? {
            ExactObservedNamespaceReservationReadV1::Missing => {
                ExactNamespaceReservationReadV1::Missing
            }
            ExactObservedNamespaceReservationReadV1::Incomplete { reason, .. } => {
                ExactNamespaceReservationReadV1::Incomplete(reason)
            }
            ExactObservedNamespaceReservationReadV1::Bound(reservation) => {
                ExactNamespaceReservationReadV1::Bound(reservation)
            }
        },
    )
}

pub(crate) async fn read_exact_observed_namespace_reservation_v1(
    op: &Operator,
    remote_prefix: &str,
    object_id: &str,
) -> Result<ExactObservedNamespaceReservationReadV1> {
    let route = strict_namespace_reservation_route_v1(remote_prefix, object_id)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_reservation_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, route.object_key(), max_bytes).await?
    else {
        return Ok(ExactObservedNamespaceReservationReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedNamespaceReservationReadV1::Incomplete {
                reason: StrictNamespaceReservationIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_namespace_reservation_v1(
        raw_snapshot,
        route,
    ))
}

#[allow(dead_code)] // Composed by the catalog-authoritative remote-universe pass.
pub(crate) async fn read_expected_observed_namespace_reservation_v1(
    op: &Operator,
    remote_prefix: &str,
    object_id: &str,
    expected_binding: &RegisteredRootRemoteObjectBindingV1,
) -> Result<ExactObservedNamespaceReservationReadV1> {
    let route = strict_namespace_reservation_route_v1(remote_prefix, object_id)?;
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_reservation_object_bytes();
    let expected = expected_raw_object_binding_v1(expected_binding);
    let Some(raw_read) =
        read_expected_raw_object_snapshot_v1(op, route.object_key(), max_bytes, expected).await?
    else {
        return Ok(ExactObservedNamespaceReservationReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactObservedNamespaceReservationReadV1::Incomplete {
                reason: StrictNamespaceReservationIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_namespace_reservation_v1(
        raw_snapshot,
        route,
    ))
}

fn strict_namespace_reservation_route_v1(
    remote_prefix: &str,
    object_id: &str,
) -> Result<StrictNamespaceReservationRouteV1> {
    let suffix_bytes = ".tcfs-namespace/v1/"
        .len()
        .checked_add(object_id.len())
        .context("strict namespace-reservation key length overflow")?;
    validate_registered_remote_derived_key_length_v1(
        remote_prefix,
        suffix_bytes,
        "strict namespace-reservation key",
    )?;
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_lower_hex_64(object_id, "strict namespace-reservation object id")?;
    let reservation_prefix = namespace_reservation_prefix(prefix);
    let object_key = format!("{reservation_prefix}{object_id}");
    validate_registered_remote_storage_key_bounds_v1(
        &object_key,
        "strict namespace-reservation key",
    )?;
    Ok(StrictNamespaceReservationRouteV1 {
        object_key,
        object_id_offset: reservation_prefix.len(),
    })
}

fn classify_observed_namespace_reservation_v1(
    raw_snapshot: RawObjectSnapshotV1,
    route: StrictNamespaceReservationRouteV1,
) -> ExactObservedNamespaceReservationReadV1 {
    let reservation =
        match PortableNamespaceReservationV1::from_json_bytes(raw_snapshot.raw_bytes()) {
            Ok(reservation)
                if validate_registered_remote_logical_path_bounds_v1(reservation.exact_path())
                    .is_ok()
                    && reservation
                        .to_json_bytes()
                        .is_ok_and(|canonical| canonical == raw_snapshot.raw_bytes()) =>
            {
                reservation
            }
            _ => {
                return ExactObservedNamespaceReservationReadV1::Incomplete {
                    reason: StrictNamespaceReservationIncompleteV1::InvalidReservation,
                    observed_object: Some(bind_remote_object_v1(raw_snapshot)),
                };
            }
        };
    if namespace_reservation_object_id(reservation.folded_path()) != route.object_id() {
        return ExactObservedNamespaceReservationReadV1::Incomplete {
            reason: StrictNamespaceReservationIncompleteV1::AddressMismatch,
            observed_object: Some(bind_remote_object_v1(raw_snapshot)),
        };
    }
    ExactObservedNamespaceReservationReadV1::Bound(BoundNamespaceReservationV1 {
        object: bind_remote_object_v1(raw_snapshot),
        object_key: route.object_key,
        object_id_offset: route.object_id_offset,
        reservation,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictWrappedFileKeyWireV1 {
    recipient_device_id: String,
    recipient: String,
    algorithm: String,
    wrapped_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictRegularManifestWireV1 {
    version: u32,
    file_hash: String,
    file_size: u64,
    chunks: Vec<String>,
    vclock: StrictVectorClockWireV1,
    written_by: String,
    written_at: u64,
    rel_path: String,
    mode: u32,
    mtime: (i64, u32),
    #[serde(default)]
    encrypted_file_key: Option<String>,
    #[serde(default)]
    wrapped_file_keys: Vec<StrictWrappedFileKeyWireV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSymlinkManifestWireV1 {
    version: u32,
    kind: RemoteEntryKind,
    symlink_target: String,
    vclock: StrictVectorClockWireV1,
    written_by: String,
    written_at: u64,
    rel_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictRegularManifestV1 {
    file_hash: String,
    file_size: u64,
    chunks: Vec<String>,
    vclock: VectorClock,
    written_by: String,
    written_at: u64,
    rel_path: String,
    mode: u32,
    mtime: (i64, u32),
}

impl StrictRegularManifestV1 {
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    pub const fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn chunks(&self) -> &[String] {
        &self.chunks
    }

    pub const fn vclock(&self) -> &VectorClock {
        &self.vclock
    }

    pub fn written_by(&self) -> &str {
        &self.written_by
    }

    pub const fn written_at(&self) -> u64 {
        self.written_at
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    pub const fn mode(&self) -> u32 {
        self.mode
    }

    pub const fn mtime(&self) -> (i64, u32) {
        self.mtime
    }
}

pub(crate) fn validate_registered_remote_logical_path_bounds_v1(rel_path: &str) -> Result<()> {
    validate_namespace_logical_path(rel_path)?;
    let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    anyhow::ensure!(
        u64::try_from(rel_path.len())
            .context("strict remote logical-path length does not fit u64")?
            <= remote_contract.max_logical_path_bytes(),
        "strict remote logical path exceeds the registered-root byte bound: {rel_path:?}"
    );
    anyhow::ensure!(
        u32::try_from(rel_path.split('/').count())
            .context("strict remote logical-path depth does not fit u32")?
            <= remote_contract.max_logical_path_depth(),
        "strict remote logical path exceeds the registered-root depth bound: {rel_path:?}"
    );
    anyhow::ensure!(
        rel_path.split('/').all(|component| {
            u64::try_from(component.len())
                .is_ok_and(|length| length <= remote_contract.max_logical_component_bytes())
        }),
        "strict remote logical path exceeds the portable component-byte bound: {rel_path:?}"
    );
    Ok(())
}

pub(crate) fn validate_registered_remote_storage_key_bounds_v1(
    key: &str,
    description: &str,
) -> Result<()> {
    validate_relative_storage_key(key, description)?;
    anyhow::ensure!(
        u64::try_from(key.len()).context("strict remote storage-key length does not fit u64")?
            <= RegisteredRootPlanContractV1::strict_v1()
                .remote_contract()
                .max_storage_key_bytes(),
        "{description} exceeds the registered-root storage-key byte bound"
    );
    Ok(())
}

fn validate_registered_remote_derived_key_length_v1(
    remote_prefix: &str,
    suffix_bytes: usize,
    description: &str,
) -> Result<()> {
    let length = remote_prefix
        .len()
        .checked_add(usize::from(!remote_prefix.is_empty()))
        .and_then(|length| length.checked_add(suffix_bytes))
        .context("strict remote derived storage-key length overflow")?;
    anyhow::ensure!(
        u64::try_from(length)
            .context("strict remote derived storage-key length does not fit u64")?
            <= RegisteredRootPlanContractV1::strict_v1()
                .remote_contract()
                .max_storage_key_bytes(),
        "{description} exceeds the registered-root storage-key byte bound"
    );
    Ok(())
}

fn validate_registered_symlink_target_bound_v1(target: &str) -> Result<()> {
    anyhow::ensure!(
        u64::try_from(target.len()).context("strict symlink target length does not fit u64")?
            <= RegisteredRootPlanContractV1::strict_v1()
                .local_snapshot_contract()
                .max_symlink_target_bytes(),
        "strict remote symlink target exceeds the local snapshot byte bound"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictSymlinkManifestV1 {
    symlink_target: String,
    vclock: VectorClock,
    written_by: String,
    written_at: u64,
    rel_path: String,
}

impl StrictSymlinkManifestV1 {
    pub fn symlink_target(&self) -> &str {
        &self.symlink_target
    }

    pub const fn vclock(&self) -> &VectorClock {
        &self.vclock
    }

    pub fn written_by(&self) -> &str {
        &self.written_by
    }

    pub const fn written_at(&self) -> u64 {
        self.written_at
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictRemoteManifestV1 {
    Regular {
        object: BoundRemoteObjectSnapshotV1,
        manifest: StrictRegularManifestV1,
    },
    Symlink {
        object: BoundRemoteObjectSnapshotV1,
        manifest: StrictSymlinkManifestV1,
    },
}

impl StrictRemoteManifestV1 {
    pub const fn object(&self) -> &BoundRemoteObjectSnapshotV1 {
        match self {
            Self::Regular { object, .. } | Self::Symlink { object, .. } => object,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictRemoteManifestIncompleteV1 {
    MissingObject,
    UnboundObject,
    AddressMismatch,
    InvalidManifest,
    ExcludedPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictRemoteManifestReadV1 {
    Complete(Box<StrictRemoteManifestV1>),
    Incomplete(StrictRemoteManifestIncompleteV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StrictObservedRemoteManifestReadV1 {
    Complete(Box<StrictRemoteManifestV1>),
    Incomplete {
        reason: StrictRemoteManifestIncompleteV1,
        observed_object: Option<BoundRemoteObjectSnapshotV1>,
    },
}

/// Read the exact manifest object named by one strict committed index entry.
///
/// The object key must equal the domain-addressed manifest bytes literally;
/// legacy embedded file-hash and symlink-target identities are not accepted.
pub async fn read_strict_remote_manifest_v1(
    op: &Operator,
    committed_index: &RawCommittedIndexEntryV1,
) -> Result<StrictRemoteManifestReadV1> {
    Ok(
        match read_observed_strict_remote_manifest_for_references_v1(op, &[committed_index]).await?
        {
            StrictObservedRemoteManifestReadV1::Complete(manifest) => {
                StrictRemoteManifestReadV1::Complete(manifest)
            }
            StrictObservedRemoteManifestReadV1::Incomplete { reason, .. } => {
                StrictRemoteManifestReadV1::Incomplete(reason)
            }
        },
    )
}

/// Bind one unique manifest object once and validate it against every index
/// route that named its content address.
///
/// Strict manifests embed their logical route, so a shared object ID across
/// distinct routes is expected to fail validation. The body is nevertheless
/// fetched only once and no reference is silently skipped.
pub(crate) async fn read_observed_strict_remote_manifest_for_references_v1(
    op: &Operator,
    committed_indexes: &[&RawCommittedIndexEntryV1],
) -> Result<StrictObservedRemoteManifestReadV1> {
    let Some(manifest_key) = strict_remote_manifest_key_for_references_v1(committed_indexes)?
    else {
        return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
            reason: StrictRemoteManifestIncompleteV1::ExcludedPath,
            observed_object: None,
        });
    };
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_manifest_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, &manifest_key, max_bytes).await? else {
        return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
            reason: StrictRemoteManifestIncompleteV1::MissingObject,
            observed_object: None,
        });
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
                reason: StrictRemoteManifestIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_strict_remote_manifest_for_references_v1(
        raw_snapshot,
        committed_indexes,
    ))
}

#[allow(dead_code)] // Composed by the catalog-authoritative remote-universe pass.
pub(crate) async fn read_expected_observed_strict_remote_manifest_for_references_v1(
    op: &Operator,
    committed_indexes: &[&RawCommittedIndexEntryV1],
    expected_binding: &RegisteredRootRemoteObjectBindingV1,
) -> Result<StrictObservedRemoteManifestReadV1> {
    let Some(manifest_key) = strict_remote_manifest_key_for_references_v1(committed_indexes)?
    else {
        return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
            reason: StrictRemoteManifestIncompleteV1::ExcludedPath,
            observed_object: None,
        });
    };
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_manifest_object_bytes();
    let expected = expected_raw_object_binding_v1(expected_binding);
    let Some(raw_read) =
        read_expected_raw_object_snapshot_v1(op, &manifest_key, max_bytes, expected).await?
    else {
        return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
            reason: StrictRemoteManifestIncompleteV1::MissingObject,
            observed_object: None,
        });
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(StrictObservedRemoteManifestReadV1::Incomplete {
                reason: StrictRemoteManifestIncompleteV1::UnboundObject,
                observed_object: None,
            });
        }
    };
    Ok(classify_observed_strict_remote_manifest_for_references_v1(
        raw_snapshot,
        committed_indexes,
    ))
}

fn strict_remote_manifest_key_for_references_v1(
    committed_indexes: &[&RawCommittedIndexEntryV1],
) -> Result<Option<String>> {
    let Some(committed_index) = committed_indexes.first().copied() else {
        anyhow::bail!("strict remote manifest reference group must be non-empty");
    };
    let index_entry = committed_index.current();
    let prefix = committed_index.remote_prefix();
    for reference in committed_indexes {
        if reference.remote_prefix() != prefix
            || reference.current().manifest_hash() != index_entry.manifest_hash()
        {
            anyhow::bail!("strict remote manifest reference group mixed roots or object addresses");
        }
        if Blacklist::default()
            .check_fixed_ingress_path_components(Path::new(reference.rel_path()))
            .is_some()
        {
            return Ok(None);
        }
    }
    let suffix_bytes = "manifests/"
        .len()
        .checked_add(index_entry.manifest_hash().len())
        .context("strict manifest key length overflow")?;
    validate_registered_remote_derived_key_length_v1(prefix, suffix_bytes, "strict manifest key")?;
    let manifest_key = if prefix.is_empty() {
        format!("manifests/{}", index_entry.manifest_hash())
    } else {
        format!("{prefix}/manifests/{}", index_entry.manifest_hash())
    };
    Ok(Some(manifest_key))
}

fn classify_observed_strict_remote_manifest_for_references_v1(
    raw_snapshot: RawObjectSnapshotV1,
    committed_indexes: &[&RawCommittedIndexEntryV1],
) -> StrictObservedRemoteManifestReadV1 {
    let committed_index = committed_indexes
        .first()
        .copied()
        .expect("validated strict remote manifest reference group must be non-empty");
    let index_entry = committed_index.current();
    let rel_path = committed_index.rel_path();
    if manifest_object_id(raw_snapshot.raw_bytes()) != index_entry.manifest_hash() {
        return observed_manifest_incomplete_after_bound_v1(
            StrictRemoteManifestIncompleteV1::AddressMismatch,
            raw_snapshot,
        );
    }
    if committed_indexes
        .iter()
        .any(|reference| reference.current().kind() != index_entry.kind())
    {
        return observed_manifest_incomplete_after_bound_v1(
            StrictRemoteManifestIncompleteV1::InvalidManifest,
            raw_snapshot,
        );
    }

    let parsed = match index_entry.kind() {
        RemoteEntryKind::RegularFile => {
            let wire: StrictRegularManifestWireV1 =
                match serde_json::from_slice(raw_snapshot.raw_bytes()) {
                    Ok(wire) => wire,
                    Err(_) => {
                        return observed_manifest_incomplete_after_bound_v1(
                            StrictRemoteManifestIncompleteV1::InvalidManifest,
                            raw_snapshot,
                        );
                    }
                };
            match strict_regular_manifest_v1(wire, rel_path, index_entry) {
                Ok(manifest) => ParsedStrictManifestV1::Regular(manifest),
                Err(_) => {
                    return observed_manifest_incomplete_after_bound_v1(
                        StrictRemoteManifestIncompleteV1::InvalidManifest,
                        raw_snapshot,
                    );
                }
            }
        }
        RemoteEntryKind::Symlink => {
            let wire: StrictSymlinkManifestWireV1 =
                match serde_json::from_slice(raw_snapshot.raw_bytes()) {
                    Ok(wire) => wire,
                    Err(_) => {
                        return observed_manifest_incomplete_after_bound_v1(
                            StrictRemoteManifestIncompleteV1::InvalidManifest,
                            raw_snapshot,
                        );
                    }
                };
            match strict_symlink_manifest_v1(wire, rel_path, index_entry) {
                Ok(manifest) => ParsedStrictManifestV1::Symlink(manifest),
                Err(_) => {
                    return observed_manifest_incomplete_after_bound_v1(
                        StrictRemoteManifestIncompleteV1::InvalidManifest,
                        raw_snapshot,
                    );
                }
            }
        }
    };
    if committed_indexes
        .iter()
        .any(|reference| validate_parsed_strict_manifest_reference_v1(&parsed, reference).is_err())
    {
        return observed_manifest_incomplete_after_bound_v1(
            StrictRemoteManifestIncompleteV1::InvalidManifest,
            raw_snapshot,
        );
    }
    let object = bind_remote_object_v1(raw_snapshot);
    StrictObservedRemoteManifestReadV1::Complete(Box::new(match parsed {
        ParsedStrictManifestV1::Regular(manifest) => {
            StrictRemoteManifestV1::Regular { object, manifest }
        }
        ParsedStrictManifestV1::Symlink(manifest) => {
            StrictRemoteManifestV1::Symlink { object, manifest }
        }
    }))
}

fn observed_manifest_incomplete_after_bound_v1(
    reason: StrictRemoteManifestIncompleteV1,
    raw_snapshot: RawObjectSnapshotV1,
) -> StrictObservedRemoteManifestReadV1 {
    StrictObservedRemoteManifestReadV1::Incomplete {
        reason,
        observed_object: Some(bind_remote_object_v1(raw_snapshot)),
    }
}

enum ParsedStrictManifestV1 {
    Regular(StrictRegularManifestV1),
    Symlink(StrictSymlinkManifestV1),
}

fn validate_parsed_strict_manifest_reference_v1(
    parsed: &ParsedStrictManifestV1,
    committed_index: &RawCommittedIndexEntryV1,
) -> Result<()> {
    let index_entry = committed_index.current();
    let rel_path = committed_index.rel_path();
    match (parsed, index_entry.kind()) {
        (ParsedStrictManifestV1::Regular(manifest), RemoteEntryKind::RegularFile) => {
            anyhow::ensure!(
                manifest.rel_path() == rel_path
                    && manifest.file_size() == index_entry.size()
                    && u64::try_from(manifest.chunks().len())
                        .context("strict regular manifest chunk count does not fit u64")?
                        == index_entry.chunks(),
                "strict regular manifest does not satisfy every index reference"
            );
        }
        (ParsedStrictManifestV1::Symlink(manifest), RemoteEntryKind::Symlink) => {
            anyhow::ensure!(
                manifest.rel_path() == rel_path
                    && index_entry.symlink_target() == Some(manifest.symlink_target()),
                "strict symlink manifest does not satisfy every index reference"
            );
            crate::engine::validate_indexed_symlink_target(
                Path::new(rel_path),
                manifest.symlink_target(),
            )?;
        }
        _ => anyhow::bail!("strict manifest kind does not match an index reference"),
    }
    Ok(())
}

fn strict_regular_manifest_v1(
    wire: StrictRegularManifestWireV1,
    rel_path: &str,
    index_entry: &StrictRemoteIndexEntryV1,
) -> Result<StrictRegularManifestV1> {
    const POSIX_FILE_TYPE_MASK: u32 = 0o170000;
    const POSIX_REGULAR_FILE: u32 = 0o100000;
    const PORTABLE_PERMISSION_MASK: u32 = 0o777;
    let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();

    anyhow::ensure!(
        matches!(wire.version, 2 | 3),
        "strict regular manifest requires version 2 or 3"
    );
    anyhow::ensure!(
        u64::try_from(wire.chunks.len())
            .context("strict regular manifest chunk count does not fit u64")?
            <= remote_contract.max_manifest_chunk_entries(),
        "strict regular manifest exceeds the chunk-entry bound"
    );
    anyhow::ensure!(
        u64::try_from(wire.wrapped_file_keys.len())
            .context("strict regular manifest wrapped-key count does not fit u64")?
            <= remote_contract.max_manifest_wrapped_key_entries(),
        "strict regular manifest exceeds the wrapped-key entry bound"
    );
    validate_lower_hex_64(&wire.file_hash, "strict regular manifest file digest")?;
    for chunk in &wire.chunks {
        validate_lower_hex_64(chunk, "strict regular manifest chunk digest")?;
    }
    validate_registered_remote_logical_path_bounds_v1(&wire.rel_path)?;
    anyhow::ensure!(
        wire.rel_path == rel_path,
        "strict regular manifest path does not match index path"
    );
    anyhow::ensure!(
        wire.file_size == index_entry.size()
            && u64::try_from(wire.chunks.len())
                .context("strict regular manifest chunk count does not fit u64")?
                == index_entry.chunks(),
        "strict regular manifest metadata does not match index entry"
    );
    // The current Unix writer serializes `PermissionsExt::mode()`, which
    // includes S_IFREG, while historical/non-Unix fixtures carry permission
    // bits alone. Accept those two exact shapes, reject every other file type
    // and special bit, and expose only the portable permission bits.
    let mode_file_type = wire.mode & POSIX_FILE_TYPE_MASK;
    let allowed_mode_bits = PORTABLE_PERMISSION_MASK
        | if mode_file_type == POSIX_REGULAR_FILE {
            POSIX_FILE_TYPE_MASK
        } else {
            0
        };
    anyhow::ensure!(
        matches!(mode_file_type, 0 | POSIX_REGULAR_FILE) && wire.mode & !allowed_mode_bits == 0,
        "strict regular manifest mode is not a supported regular-file permission encoding"
    );
    let portable_mode = wire.mode & PORTABLE_PERMISSION_MASK;
    anyhow::ensure!(
        wire.mtime.1 < 1_000_000_000,
        "strict regular manifest mtime nanos are out of range"
    );
    validate_strict_identity_v1(&wire.written_by, "strict regular manifest writer")?;
    if let Some(encrypted_file_key) = wire.encrypted_file_key.as_deref() {
        validate_bounded_control_free_v1(
            encrypted_file_key,
            REGISTERED_ROOT_WRAPPED_KEY_MAX_BYTES_V1,
            "strict regular manifest encrypted key",
        )?;
    }
    let mut recipient_device_ids = BTreeSet::new();
    for wrapped in &wire.wrapped_file_keys {
        validate_strict_identity_v1(
            &wrapped.recipient_device_id,
            "strict wrapped-key recipient device id",
        )?;
        validate_bounded_control_free_v1(
            &wrapped.recipient,
            REGISTERED_ROOT_RECIPIENT_MAX_BYTES_V1,
            "strict wrapped-key recipient",
        )?;
        validate_bounded_control_free_v1(
            &wrapped.algorithm,
            REGISTERED_ROOT_ALGORITHM_MAX_BYTES_V1,
            "strict wrapped-key algorithm",
        )?;
        validate_bounded_control_free_v1(
            &wrapped.wrapped_key,
            REGISTERED_ROOT_WRAPPED_KEY_MAX_BYTES_V1,
            "strict wrapped-key payload",
        )?;
        anyhow::ensure!(
            recipient_device_ids.insert(wrapped.recipient_device_id.as_str()),
            "strict regular manifest has duplicate wrapped-key recipient device ids"
        );
    }
    match wire.version {
        2 => {
            anyhow::ensure!(
                wire.wrapped_file_keys.is_empty() || wire.encrypted_file_key.is_some(),
                "strict v2 dual manifest requires its rollback master-wrapped key"
            );
        }
        3 => {
            anyhow::ensure!(
                !wire.wrapped_file_keys.is_empty() && wire.encrypted_file_key.is_none(),
                "strict v3 per-device manifest requires wraps and forbids a master-wrapped key"
            );
        }
        _ => unreachable!("manifest version was checked above"),
    }
    let vclock = wire
        .vclock
        .try_into_vector_clock_bounded(remote_contract.max_remote_vector_clock_entries())?;
    Ok(StrictRegularManifestV1 {
        file_hash: wire.file_hash,
        file_size: wire.file_size,
        chunks: wire.chunks,
        vclock,
        written_by: wire.written_by,
        written_at: wire.written_at,
        rel_path: wire.rel_path,
        mode: portable_mode,
        mtime: wire.mtime,
    })
}

fn strict_symlink_manifest_v1(
    wire: StrictSymlinkManifestWireV1,
    rel_path: &str,
    index_entry: &StrictRemoteIndexEntryV1,
) -> Result<StrictSymlinkManifestV1> {
    let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
    anyhow::ensure!(
        wire.version == 3 && wire.kind == RemoteEntryKind::Symlink,
        "strict symlink manifest requires version 3 symlink kind"
    );
    anyhow::ensure!(
        !wire.symlink_target.is_empty()
            && !wire.symlink_target.chars().any(char::is_control)
            && index_entry.symlink_target() == Some(wire.symlink_target.as_str()),
        "strict symlink manifest target does not match index entry"
    );
    validate_registered_symlink_target_bound_v1(&wire.symlink_target)?;
    crate::engine::validate_indexed_symlink_target(Path::new(rel_path), &wire.symlink_target)?;
    validate_registered_remote_logical_path_bounds_v1(&wire.rel_path)?;
    anyhow::ensure!(
        wire.rel_path == rel_path,
        "strict symlink manifest path does not match index path"
    );
    validate_strict_identity_v1(&wire.written_by, "strict symlink manifest writer")?;
    let vclock = wire
        .vclock
        .try_into_vector_clock_bounded(remote_contract.max_remote_vector_clock_entries())?;
    Ok(StrictSymlinkManifestV1 {
        symlink_target: wire.symlink_target,
        vclock,
        written_by: wire.written_by,
        written_at: wire.written_at,
        rel_path: wire.rel_path,
    })
}

fn validate_lower_hex_64(value: &str, description: &str) -> Result<()> {
    validate_storage_key_component(value, description)?;
    anyhow::ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{description} must be 64 lowercase hexadecimal characters"
    );
    Ok(())
}

fn validate_bounded_control_free_v1(
    value: &str,
    max_bytes: usize,
    description: &str,
) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control),
        "{description} must be non-empty, control-free, and at most {max_bytes} bytes"
    );
    Ok(())
}

fn validate_strict_identity_v1(value: &str, description: &str) -> Result<()> {
    validate_bounded_control_free_v1(value, REGISTERED_ROOT_IDENTITY_MAX_BYTES_V1, description)?;
    anyhow::ensure!(
        value.nfc().collect::<String>() == value,
        "{description} must use Unicode NFC spelling"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::raw::{Access, AccessorInfo, OpRead, OpStat, RpRead, RpStat};
    use opendal::services::Memory;
    use opendal::{Buffer, Capability, EntryMode, ErrorKind, Metadata, OperatorBuilder};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn directory_inventory(
        path: &Path,
    ) -> Vec<(String, bool, u64, std::time::SystemTime, Vec<u8>)> {
        let mut inventory = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
                let bytes = if metadata.is_file() {
                    std::fs::read(entry.path()).unwrap()
                } else {
                    Vec::new()
                };
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    metadata.is_dir(),
                    metadata.len(),
                    metadata.modified().unwrap(),
                    bytes,
                )
            })
            .collect::<Vec<_>>();
        inventory.sort_by(|left, right| left.0.cmp(&right.0));
        inventory
    }

    async fn object_inventory(op: &Operator) -> BTreeMap<String, Vec<u8>> {
        let mut inventory = BTreeMap::new();
        for entry in op.list_with("").recursive(true).await.unwrap() {
            if entry.path().ends_with('/') {
                continue;
            }
            inventory.insert(
                entry.path().to_owned(),
                op.read(entry.path()).await.unwrap().to_vec(),
            );
        }
        inventory
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[derive(Clone, Debug)]
    struct BoundObjectTestBackend {
        objects: Arc<BTreeMap<String, Vec<u8>>>,
        info: Arc<AccessorInfo>,
        stat_calls: Arc<AtomicUsize>,
    }

    impl Access for BoundObjectTestBackend {
        type Reader = Buffer;
        type Writer = ();
        type Lister = ();
        type Deleter = ();

        fn info(&self) -> Arc<AccessorInfo> {
            self.info.clone()
        }

        async fn stat(&self, path: &str, _: OpStat) -> opendal::Result<RpStat> {
            self.stat_calls.fetch_add(1, Ordering::SeqCst);
            let bytes = self.objects.get(path).ok_or_else(|| {
                opendal::Error::new(ErrorKind::NotFound, "bound test object is missing")
            })?;
            Ok(RpStat::new(
                Metadata::new(EntryMode::FILE)
                    .with_content_length(bytes.len() as u64)
                    .with_etag(blake3::hash(bytes).to_hex().to_string()),
            ))
        }

        async fn read(&self, path: &str, args: OpRead) -> opendal::Result<(RpRead, Buffer)> {
            let bytes = self.objects.get(path).ok_or_else(|| {
                opendal::Error::new(ErrorKind::NotFound, "bound test object is missing")
            })?;
            let etag = blake3::hash(bytes).to_hex().to_string();
            if args.if_match().is_some_and(|expected| expected != etag) {
                return Err(opendal::Error::new(
                    ErrorKind::ConditionNotMatch,
                    "bound test object ETag changed",
                ));
            }
            let range = args.range();
            let start = usize::try_from(range.offset()).unwrap_or(usize::MAX);
            let end = range
                .size()
                .and_then(|size| usize::try_from(size).ok())
                .map(|size| start.saturating_add(size))
                .unwrap_or(bytes.len())
                .min(bytes.len());
            let selected = if start <= end {
                bytes[start..end].to_vec()
            } else {
                Vec::new()
            };
            Ok((
                RpRead::new().with_size(Some(selected.len() as u64)),
                Buffer::from(selected),
            ))
        }
    }

    fn bound_object_test_operator(objects: BTreeMap<String, Vec<u8>>) -> Operator {
        bound_object_test_operator_with_stat_counter(objects).0
    }

    fn bound_object_test_operator_with_stat_counter(
        objects: BTreeMap<String, Vec<u8>>,
    ) -> (Operator, Arc<AtomicUsize>) {
        let info = AccessorInfo::default();
        info.set_scheme("registered-root-bound-test")
            .set_root("/")
            .set_name("registered-root-bound-test")
            .set_native_capability(Capability {
                stat: true,
                read: true,
                read_with_if_match: true,
                ..Default::default()
            });
        let stat_calls = Arc::new(AtomicUsize::new(0));
        let op = OperatorBuilder::new(BoundObjectTestBackend {
            objects: Arc::new(objects),
            info: Arc::new(info),
            stat_calls: stat_calls.clone(),
        })
        .finish();
        (op, stat_calls)
    }

    fn valid_state_json(local_root: &Path, status: &str) -> Vec<u8> {
        let cache_key = local_root.join("file").to_string_lossy().into_owned();
        format!(
            r#"{{
  "last_nats_seq": 7,
  "device_id": "sting",
  "entries": {{
    "{cache_key}": {{
      "blake3": "{digest}",
      "size": 4,
      "mtime": 5,
      "chunk_count": 1,
      "remote_path": "roots/manifests/{manifest_id}",
      "last_synced": 6,
      "vclock": {{ "clocks": {{ "sting": 1 }} }},
      "device_id": "sting",
      "status": "{status}"
    }}
  }}
}}"#,
            digest = "a".repeat(64),
            manifest_id = "b".repeat(64),
        )
        .into_bytes()
    }

    #[test]
    fn strict_primary_accepts_current_shape_and_rejects_legacy_unknown_and_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let local_root = dir.path().join("root");
        std::fs::create_dir(&local_root).unwrap();
        let local_root = std::fs::canonicalize(local_root).unwrap();
        write_private(&path, &valid_state_json(&local_root, "synced"));
        let before_inventory = directory_inventory(dir.path());
        let snapshot =
            match read_and_bind_strict_primary_state_v1(&path, &local_root, "roots").unwrap() {
                StrictPrimaryStateReadV1::Complete(snapshot) => snapshot,
                other => panic!("expected strict state snapshot, got {other:?}"),
            };
        assert_eq!(snapshot.last_nats_seq(), 7);
        assert_eq!(snapshot.device_id(), "sting");
        assert_eq!(snapshot.entries().len(), 1);
        let entry = &snapshot.entries()["file"];
        assert_eq!(entry.rel_path(), "file");
        assert_eq!(entry.index_key(), "roots/index/file");
        assert_eq!(
            entry.baseline_manifest_key(),
            format!("roots/manifests/{}", "b".repeat(64))
        );
        assert_ne!(
            entry.baseline_manifest_key().rsplit('/').next().unwrap(),
            entry.state().blake3()
        );
        assert_eq!(directory_inventory(dir.path()), before_inventory);

        write_private(&path, br#"{"/tmp/file":{"blake3":"legacy"}}"#);
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(StrictPrimaryStateIncompleteV1::InvalidPrimary)
        ));

        let mut unknown = String::from_utf8(valid_state_json(&local_root, "synced")).unwrap();
        unknown = unknown.replacen(
            "\"last_nats_seq\": 7,",
            "\"last_nats_seq\": 7, \"unexpected\": true,",
            1,
        );
        write_private(&path, unknown.as_bytes());
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(StrictPrimaryStateIncompleteV1::InvalidPrimary)
        ));

        write_private(&path, &valid_state_json(&local_root, "active"));
        let active_key = local_root.join("file").to_string_lossy().into_owned();
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::PersistedEntriesBusy {
                    active_keys,
                    locked_keys
                }
            ) if active_keys == vec![active_key] && locked_keys.is_empty()
        ));
    }

    #[test]
    fn empty_strict_primary_retains_exact_route_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let local_root = dir.path().join("root");
        std::fs::create_dir(&local_root).unwrap();
        let local_root = std::fs::canonicalize(local_root).unwrap();
        write_private(
            &state_path,
            br#"{"last_nats_seq":0,"device_id":"sting","entries":{}}"#,
        );
        let state_path = std::fs::canonicalize(state_path).unwrap();

        for prefix in ["roots-a", "roots-b"] {
            let snapshot =
                match read_and_bind_strict_primary_state_v1(&state_path, &local_root, prefix)
                    .unwrap()
                {
                    StrictPrimaryStateReadV1::Complete(snapshot) => snapshot,
                    other => panic!("expected empty strict state snapshot, got {other:?}"),
                };
            assert_eq!(snapshot.selected_state_path(), state_path);
            assert_eq!(snapshot.canonical_local_root(), local_root);
            assert_eq!(snapshot.remote_prefix(), prefix);
            assert_eq!(snapshot.entries().len(), 0);
            assert_eq!(snapshot.namespace_claims().len(), 0);
        }
    }

    #[test]
    fn strict_state_namespace_claim_budget_is_fingerprinted_and_charged_before_dedupe() {
        type ObserveClaimV1 =
            fn(&mut StrictStateNamespaceBudgetV1, u64, RootStateContractV1) -> Result<()>;

        fn assert_typed_limit_v1(error: &anyhow::Error, expected: StrictStateNamespaceResourceV1) {
            assert_eq!(
                error
                    .downcast_ref::<StrictStateNamespaceResourceLimitErrorV1>()
                    .unwrap()
                    .resource(),
                expected
            );
            assert_eq!(
                strict_state_binding_incomplete_v1(error),
                StrictPrimaryStateIncompleteV1::NamespaceResourceLimit { resource: expected }
            );
        }

        let contract = RegisteredRootPlanContractV1::strict_v1().state_contract();
        let mut claims = BTreeMap::new();
        let mut budget = StrictStateNamespaceBudgetV1::default();
        reserve_state_namespace_claims_v1("parent/first", &mut claims, &mut budget, contract)
            .unwrap();
        reserve_state_namespace_claims_v1("parent/second", &mut claims, &mut budget, contract)
            .unwrap();
        assert_eq!(budget.generated_claims, 4);
        assert_eq!(budget.retained_claims, 3);
        assert_eq!(claims.len(), 3);

        let rows: [(
            StrictStateNamespaceResourceV1,
            StrictStateNamespaceBudgetV1,
            ObserveClaimV1,
            u64,
        ); 4] = [
            (
                StrictStateNamespaceResourceV1::GeneratedClaims,
                StrictStateNamespaceBudgetV1 {
                    generated_claims: contract.max_generated_claim_observations() - 1,
                    ..StrictStateNamespaceBudgetV1::default()
                },
                StrictStateNamespaceBudgetV1::observe_generated_claim,
                0,
            ),
            (
                StrictStateNamespaceResourceV1::GeneratedClaimBytes,
                StrictStateNamespaceBudgetV1 {
                    generated_claim_bytes: contract.max_generated_claim_bytes() - 1,
                    ..StrictStateNamespaceBudgetV1::default()
                },
                StrictStateNamespaceBudgetV1::observe_generated_claim,
                1,
            ),
            (
                StrictStateNamespaceResourceV1::RetainedClaims,
                StrictStateNamespaceBudgetV1 {
                    retained_claims: contract.max_retained_unique_claims() - 1,
                    ..StrictStateNamespaceBudgetV1::default()
                },
                StrictStateNamespaceBudgetV1::observe_retained_claim,
                0,
            ),
            (
                StrictStateNamespaceResourceV1::RetainedClaimBytes,
                StrictStateNamespaceBudgetV1 {
                    retained_claim_bytes: contract.max_retained_unique_claim_bytes() - 1,
                    ..StrictStateNamespaceBudgetV1::default()
                },
                StrictStateNamespaceBudgetV1::observe_retained_claim,
                1,
            ),
        ];
        for (resource, mut exact, observe, increment) in rows {
            observe(&mut exact, increment, contract)
                .expect("the exact state claim resource ceiling is accepted");
            let error = observe(&mut exact, increment, contract)
                .expect_err("one claim beyond the state resource ceiling is rejected");
            assert_typed_limit_v1(&error, resource);
        }

        assert_eq!(
            strict_state_binding_incomplete_v1(&anyhow::anyhow!("invalid binding")),
            StrictPrimaryStateIncompleteV1::InvalidRootBinding
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pending_root_binds_missing_local_parent_without_pathname_reprobe() {
        use crate::registered_local_snapshot::{
            begin_strict_local_snapshot_v1, StrictLocalSnapshotFinishV1,
            StrictLocalSnapshotHoldReadV1,
        };
        use tcfs_core::config::RootProfileV1;

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let local_root = dir.path().join("root");
        std::fs::create_dir(&local_root).unwrap();
        let local_root = std::fs::canonicalize(local_root).unwrap();
        let old_key = local_root.join("file").to_string_lossy().into_owned();
        let new_key = local_root
            .join("remote-only/child")
            .to_string_lossy()
            .into_owned();
        let state = String::from_utf8(valid_state_json(&local_root, "synced"))
            .unwrap()
            .replacen(&old_key, &new_key, 1);
        write_private(&state_path, state.as_bytes());

        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::InvalidRootBinding
            )
        ));

        let pending = match begin_strict_local_snapshot_v1(
            &local_root,
            RootProfileV1::AgentStaticV1,
        )
        .unwrap()
        {
            StrictLocalSnapshotHoldReadV1::Pending(pending) => pending,
            StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
                panic!("expected pending local snapshot, got {incomplete:?}")
            }
        };
        let snapshot = match read_and_bind_strict_primary_state_for_pending_root_v1(
            &state_path,
            &pending,
            "roots",
        )
        .unwrap()
        {
            StrictPrimaryStateReadV1::Complete(snapshot) => snapshot,
            other => panic!("expected held-root state snapshot, got {other:?}"),
        };
        assert!(snapshot.entries().contains_key("remote-only/child"));
        assert!(matches!(
            pending.revalidate_inventory_c().unwrap(),
            StrictLocalSnapshotFinishV1::Complete(_)
        ));
    }

    #[test]
    fn strict_primary_rejects_duplicate_entry_and_clock_keys() {
        let entry = format!(
            r#"{{
  "blake3": "{digest}",
  "size": 1,
  "mtime": 2,
  "chunk_count": 1,
  "remote_path": "roots/file",
  "last_synced": 3,
  "vclock": {{ "clocks": {{ "sting": 1 }} }},
  "device_id": "sting",
  "status": "synced"
}}"#,
            digest = "d".repeat(64)
        );
        let duplicate_entries = format!(
            r#"{{
  "last_nats_seq": 0,
  "device_id": "sting",
  "entries": {{ "same": {entry}, "same": {entry} }}
}}"#
        );
        assert!(serde_json::from_str::<StrictStateCacheWireV1>(&duplicate_entries).is_err());

        let duplicate_clock = String::from_utf8(valid_state_json(Path::new("/tmp"), "synced"))
            .unwrap()
            .replacen("\"sting\": 1", "\"sting\": 1, \"sting\": 2", 1);
        assert!(serde_json::from_str::<StrictStateCacheWireV1>(&duplicate_clock).is_err());
    }

    #[test]
    fn strict_primary_rejects_semantic_clock_status_and_route_contradictions() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let local_root = dir.path().join("root");
        std::fs::create_dir(&local_root).unwrap();
        let local_root = std::fs::canonicalize(local_root).unwrap();

        let zero_clock = String::from_utf8(valid_state_json(&local_root, "synced"))
            .unwrap()
            .replacen("\"sting\": 1", "\"sting\": 0", 1);
        write_private(&state_path, zero_clock.as_bytes());
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(StrictPrimaryStateIncompleteV1::InvalidPrimary)
        ));

        write_private(&state_path, &valid_state_json(&local_root, "conflict"));
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(StrictPrimaryStateIncompleteV1::InvalidPrimary)
        ));

        let wrong_route = String::from_utf8(valid_state_json(&local_root, "synced"))
            .unwrap()
            .replacen(
                &format!("roots/manifests/{}", "b".repeat(64)),
                "roots/index/file",
                1,
            );
        write_private(&state_path, wrong_route.as_bytes());
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::InvalidRootBinding
            )
        ));
    }

    #[test]
    fn strict_primary_binds_conflict_pin_and_rejects_portable_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let local_root = dir.path().join("root");
        std::fs::create_dir(&local_root).unwrap();
        let local_root = std::fs::canonicalize(local_root).unwrap();

        let mut conflict: serde_json::Value =
            serde_json::from_slice(&valid_state_json(&local_root, "synced")).unwrap();
        let cache_key = local_root.join("file").to_string_lossy().into_owned();
        let state = &mut conflict["entries"][&cache_key];
        state["status"] = serde_json::json!("conflict");
        state["conflict"] = serde_json::json!({
            "rel_path": "file",
            "local_vclock": { "clocks": { "sting": 2 } },
            "remote_vclock": { "clocks": { "neo": 3 } },
            "local_blake3": "c".repeat(64),
            "remote_blake3": "d".repeat(64),
            "local_device": "sting",
            "remote_device": "neo",
            "detected_at": 10,
            "times_recorded": 2,
            "remote_manifest_key": format!("roots/manifests/{}", "e".repeat(64))
        });
        write_private(&state_path, &serde_json::to_vec(&conflict).unwrap());
        let snapshot =
            match read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap()
            {
                StrictPrimaryStateReadV1::Complete(snapshot) => snapshot,
                other => panic!("expected bound conflict state, got {other:?}"),
            };
        assert_ne!(
            snapshot.entries()["file"].baseline_manifest_key(),
            snapshot.entries()["file"]
                .state()
                .conflict()
                .unwrap()
                .remote_manifest_key
                .as_deref()
                .unwrap()
        );

        let mut aliases: serde_json::Value =
            serde_json::from_slice(&valid_state_json(&local_root, "synced")).unwrap();
        let original = aliases["entries"]
            .as_object_mut()
            .unwrap()
            .remove(&cache_key)
            .unwrap();
        aliases["entries"][local_root.join("Foo").to_string_lossy().as_ref()] = original.clone();
        aliases["entries"][local_root.join("foo").to_string_lossy().as_ref()] = original;
        write_private(&state_path, &serde_json::to_vec(&aliases).unwrap());
        assert!(matches!(
            read_and_bind_strict_primary_state_v1(&state_path, &local_root, "roots").unwrap(),
            StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::InvalidRootBinding
            )
        ));
    }

    #[test]
    fn strict_index_parser_classifies_versioned_records_and_rejects_unknowns() {
        let manifest_hash = "b".repeat(64);
        let committed = format!(
            r#"{{
  "version": 2,
  "state": "committed",
  "current": {{
    "manifest_hash": "{manifest_hash}",
    "size": 4,
    "chunks": 1
  }},
  "pending": null
}}"#
        );
        assert!(matches!(
            parse_strict_index_record_v1(committed.as_bytes()).unwrap(),
            ParsedStrictIndexRecordV1::Committed {
                format: StrictIndexRecordFormatV1::Version2,
                ..
            }
        ));

        let preparing = committed.replace("\"committed\"", "\"preparing\"").replace(
            "\"pending\": null",
            &format!(
                r#""pending": {{
    "manifest_hash": "{manifest_hash}",
    "size": 4,
    "chunks": 1,
    "staged_manifest_key": "roots/staging/object"
  }}"#
            ),
        );
        assert!(matches!(
            parse_strict_index_record_v1(preparing.as_bytes()).unwrap(),
            ParsedStrictIndexRecordV1::Preparing { .. }
        ));
        let overlong_key = "k".repeat(
            usize::try_from(
                RegisteredRootPlanContractV1::strict_v1()
                    .remote_contract()
                    .max_storage_key_bytes()
                    + 1,
            )
            .unwrap(),
        );
        assert!(parse_strict_index_record_v1(
            preparing
                .replace("roots/staging/object", &overlong_key)
                .as_bytes()
        )
        .is_err());
        let deleted_with_overlong_evidence = format!(
            r#"{{
  "version": 4,
  "state": "deleted",
  "current": null,
  "pending": null,
  "deletion_evidence": {{
    "safety_copy_key": "{overlong_key}",
    "safety_copy_blake3": "{manifest_hash}"
  }}
}}"#
        );
        assert!(parse_strict_index_record_v1(deleted_with_overlong_evidence.as_bytes()).is_err());

        let unknown = committed.replacen("\"version\": 2,", "\"version\": 2, \"extra\": 1,", 1);
        assert!(parse_strict_index_record_v1(unknown.as_bytes()).is_err());
        assert!(parse_strict_index_record_v1(b"manifest_hash=legacy\n").is_err());
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

    fn strict_deleted_index_json(safety_copy_key: Option<&str>) -> Vec<u8> {
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

    fn committed_index_for_test(index: StrictRemoteIndexEntryV1) -> RawCommittedIndexEntryV1 {
        RawCommittedIndexEntryV1 {
            object: BoundRemoteObjectSnapshotV1 {
                raw_bytes_len: 0,
                raw_blake3: *blake3::hash(b"test-index-proof").as_bytes(),
                binding: RegisteredRootRemoteObjectBindingV1::Version {
                    version: "test-version".to_owned(),
                    etag: Some("test-etag".to_owned()),
                },
            },
            current: index,
            format: StrictIndexRecordFormatV1::Version2,
            route: RegisteredRootRemoteIndexRouteV1 {
                index_key: "roots/index/file".to_owned(),
                remote_prefix_len: "roots".len(),
                index_rel_offset: "roots/index/".len(),
            },
        }
    }

    #[test]
    fn strict_manifest_requires_exact_current_metadata_shape() {
        let manifest_hash = "f".repeat(64);
        let index = StrictRemoteIndexEntryV1 {
            manifest_hash,
            size: 4,
            chunks: 1,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        };
        let bytes = regular_manifest_json("file");
        let wire: StrictRegularManifestWireV1 = serde_json::from_slice(&bytes).unwrap();
        let manifest = strict_regular_manifest_v1(wire, "file", &index).unwrap();
        assert_eq!(manifest.file_size(), 4);
        assert_eq!(manifest.mode(), 0o644);
        assert_eq!(manifest.mtime(), (8, 9));

        let unknown = String::from_utf8(bytes.clone()).unwrap().replacen(
            "\"version\": 2,",
            "\"version\": 2, \"extra\": true,",
            1,
        );
        assert!(serde_json::from_str::<StrictRegularManifestWireV1>(&unknown).is_err());
        let missing_mode = String::from_utf8(bytes)
            .unwrap()
            .replace("  \"mode\": 420,\n", "");
        assert!(serde_json::from_str::<StrictRegularManifestWireV1>(&missing_mode).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn strict_regular_manifest_accepts_and_normalizes_actual_unix_writer_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("writer-mode");
        std::fs::write(&file, b"test").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let writer_mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(writer_mode & 0o170000, 0o100000);

        let index = StrictRemoteIndexEntryV1 {
            manifest_hash: "f".repeat(64),
            size: 4,
            chunks: 1,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        };
        let mut writer_manifest: serde_json::Value =
            serde_json::from_slice(&regular_manifest_json("file")).unwrap();
        writer_manifest["mode"] = serde_json::json!(writer_mode);
        let manifest = strict_regular_manifest_v1(
            serde_json::from_value(writer_manifest).unwrap(),
            "file",
            &index,
        )
        .unwrap();
        assert_eq!(manifest.mode(), 0o640);

        for rejected in [0o040640_u32, 0o120640, 0o100640 | 0o4000] {
            let mut invalid: serde_json::Value =
                serde_json::from_slice(&regular_manifest_json("file")).unwrap();
            invalid["mode"] = serde_json::json!(rejected);
            assert!(
                strict_regular_manifest_v1(
                    serde_json::from_value(invalid).unwrap(),
                    "file",
                    &index,
                )
                .is_err(),
                "mode {rejected:o} must fail closed"
            );
        }
    }

    #[test]
    fn strict_regular_manifest_accepts_master_dual_and_per_device_shapes_only() {
        let index = StrictRemoteIndexEntryV1 {
            manifest_hash: "f".repeat(64),
            size: 4,
            chunks: 1,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        };
        let base: serde_json::Value =
            serde_json::from_slice(&regular_manifest_json("file")).unwrap();
        let wrapped = serde_json::json!([{
            "recipient_device_id": "sting",
            "recipient": "age1recipient",
            "algorithm": "age-x25519-v1",
            "wrapped_key": "wrapped"
        }]);

        let mut master = base.clone();
        master["encrypted_file_key"] = serde_json::json!("master");
        assert!(strict_regular_manifest_v1(
            serde_json::from_value(master).unwrap(),
            "file",
            &index
        )
        .is_ok());

        let mut dual = base.clone();
        dual["encrypted_file_key"] = serde_json::json!("master");
        dual["wrapped_file_keys"] = wrapped.clone();
        assert!(
            strict_regular_manifest_v1(serde_json::from_value(dual).unwrap(), "file", &index)
                .is_ok()
        );

        let mut invalid_dual = base.clone();
        invalid_dual["wrapped_file_keys"] = wrapped.clone();
        assert!(strict_regular_manifest_v1(
            serde_json::from_value(invalid_dual).unwrap(),
            "file",
            &index
        )
        .is_err());

        let mut per_device = base.clone();
        per_device["version"] = serde_json::json!(3);
        per_device["wrapped_file_keys"] = wrapped.clone();
        assert!(strict_regular_manifest_v1(
            serde_json::from_value(per_device.clone()).unwrap(),
            "file",
            &index
        )
        .is_ok());

        per_device["encrypted_file_key"] = serde_json::json!("master");
        assert!(strict_regular_manifest_v1(
            serde_json::from_value(per_device).unwrap(),
            "file",
            &index
        )
        .is_err());

        let mut duplicate = base;
        duplicate["encrypted_file_key"] = serde_json::json!("master");
        duplicate["wrapped_file_keys"] = serde_json::json!([
            {
                "recipient_device_id": "sting",
                "recipient": "age1a",
                "algorithm": "age-x25519-v1",
                "wrapped_key": "a"
            },
            {
                "recipient_device_id": "sting",
                "recipient": "age1b",
                "algorithm": "age-x25519-v1",
                "wrapped_key": "b"
            }
        ]);
        assert!(strict_regular_manifest_v1(
            serde_json::from_value(duplicate).unwrap(),
            "file",
            &index
        )
        .is_err());
    }

    #[test]
    fn strict_symlink_manifest_preserves_safe_target_and_rejects_escape() {
        let index = StrictRemoteIndexEntryV1 {
            manifest_hash: "f".repeat(64),
            size: 6,
            chunks: 0,
            kind: RemoteEntryKind::Symlink,
            symlink_target: Some("target".to_owned()),
        };
        let wire: StrictSymlinkManifestWireV1 = serde_json::from_value(serde_json::json!({
            "version": 3,
            "kind": "symlink",
            "symlink_target": "target",
            "vclock": { "clocks": { "sting": 1 } },
            "written_by": "sting",
            "written_at": 7,
            "rel_path": "dir/link"
        }))
        .unwrap();
        assert!(strict_symlink_manifest_v1(wire, "dir/link", &index).is_ok());

        let escaping_index = StrictRemoteIndexEntryV1 {
            symlink_target: Some("../../escape".to_owned()),
            size: 12,
            ..index
        };
        let escaping: StrictSymlinkManifestWireV1 = serde_json::from_value(serde_json::json!({
            "version": 3,
            "kind": "symlink",
            "symlink_target": "../../escape",
            "vclock": { "clocks": { "sting": 1 } },
            "written_by": "sting",
            "written_at": 7,
            "rel_path": "dir/link"
        }))
        .unwrap();
        assert!(strict_symlink_manifest_v1(escaping, "dir/link", &escaping_index).is_err());
    }

    #[test]
    fn strict_remote_routes_enforce_path_depth_and_storage_key_bounds() {
        let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let exact_path = [
            "a".repeat(255),
            "b".repeat(255),
            "c".repeat(255),
            "d".repeat(254),
            "e".to_owned(),
        ]
        .join("/");
        assert_eq!(
            u64::try_from(exact_path.len()).unwrap(),
            remote_contract.max_logical_path_bytes()
        );
        assert!(validate_registered_remote_logical_path_bounds_v1(&exact_path).is_ok());
        assert!(
            validate_registered_remote_logical_path_bounds_v1(&format!("{exact_path}a")).is_err()
        );
        let overlong_component =
            "a".repeat(usize::try_from(remote_contract.max_logical_component_bytes() + 1).unwrap());
        assert!(validate_registered_remote_logical_path_bounds_v1(&overlong_component).is_err());

        let over_depth = std::iter::repeat_n(
            "a",
            usize::try_from(remote_contract.max_logical_path_depth() + 1).unwrap(),
        )
        .collect::<Vec<_>>()
        .join("/");
        assert!(validate_registered_remote_logical_path_bounds_v1(&over_depth).is_err());

        let exact_key =
            "k".repeat(usize::try_from(remote_contract.max_storage_key_bytes()).unwrap());
        assert!(
            validate_registered_remote_storage_key_bounds_v1(&exact_key, "test storage key")
                .is_ok()
        );
        assert!(validate_registered_remote_storage_key_bounds_v1(
            &format!("{exact_key}k"),
            "test storage key"
        )
        .is_err());

        let overlong_target = "t".repeat(
            usize::try_from(
                RegisteredRootPlanContractV1::strict_v1()
                    .local_snapshot_contract()
                    .max_symlink_target_bytes()
                    + 1,
            )
            .unwrap(),
        );
        assert!(strict_remote_entry_v1(
            "f".repeat(64),
            u64::try_from(overlong_target.len()).unwrap(),
            0,
            Some(RemoteEntryKind::Symlink),
            Some(overlong_target),
        )
        .is_err());
    }

    #[test]
    fn strict_remote_manifest_enforces_decoded_collection_bounds() {
        let remote_contract = RegisteredRootPlanContractV1::strict_v1().remote_contract();
        let wrapped_file_keys = (0..=remote_contract.max_manifest_wrapped_key_entries())
            .map(|index| StrictWrappedFileKeyWireV1 {
                recipient_device_id: format!("device-{index}"),
                recipient: "age1test".to_owned(),
                algorithm: "age-x25519-v1".to_owned(),
                wrapped_key: "wrapped".to_owned(),
            })
            .collect();
        let wire = StrictRegularManifestWireV1 {
            version: 2,
            file_hash: "f".repeat(64),
            file_size: 0,
            chunks: Vec::new(),
            vclock: StrictVectorClockWireV1 {
                clocks: UniqueBTreeMap(BTreeMap::from([("sting".to_owned(), 1)])),
            },
            written_by: "sting".to_owned(),
            written_at: 1,
            rel_path: "file".to_owned(),
            mode: 0o644,
            mtime: (1, 0),
            encrypted_file_key: Some("master".to_owned()),
            wrapped_file_keys,
        };
        let index = StrictRemoteIndexEntryV1 {
            manifest_hash: "m".repeat(64),
            size: 0,
            chunks: 0,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        };
        let error = strict_regular_manifest_v1(wire, "file", &index).unwrap_err();
        assert!(
            format!("{error:#}").contains("wrapped-key entry bound"),
            "{error:#}"
        );

        let clocks = (0..=remote_contract.max_remote_vector_clock_entries())
            .map(|index| (format!("device-{index}"), 1))
            .collect();
        let error = StrictVectorClockWireV1 {
            clocks: UniqueBTreeMap(clocks),
        }
        .try_into_vector_clock_bounded(remote_contract.max_remote_vector_clock_entries())
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("vector clock exceeds"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn strict_index_read_marks_unbound_memory_incomplete_without_writes() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let manifest_hash = "c".repeat(64);
        let record = format!(
            r#"{{"version":2,"state":"committed","current":{{"manifest_hash":"{manifest_hash}","size":0,"chunks":0}},"pending":null}}"#
        );
        op.write("roots/index/file", record.clone()).await.unwrap();
        let before = object_inventory(&op).await;

        assert!(matches!(
            read_exact_raw_index_entry_v1(&op, "roots", "file")
                .await
                .unwrap(),
            ExactRawIndexEntryReadV1::Incomplete(StrictRemoteIndexIncompleteV1::UnboundObject)
        ));
        assert_eq!(object_inventory(&op).await, before);
    }

    #[tokio::test]
    async fn strict_bound_index_read_reports_preparing_without_object_evidence() {
        let manifest_hash = "c".repeat(64);
        let record = format!(
            r#"{{
  "version": 2,
  "state": "preparing",
  "current": null,
  "pending": {{
    "manifest_hash": "{manifest_hash}",
    "size": 4,
    "chunks": 1,
    "staged_manifest_key": "roots/staging/object"
  }}
}}"#
        )
        .into_bytes();
        let op =
            bound_object_test_operator(BTreeMap::from([("roots/index/file".to_owned(), record)]));

        assert_eq!(
            read_exact_raw_index_entry_v1(&op, "roots", "file")
                .await
                .unwrap(),
            ExactRawIndexEntryReadV1::Incomplete(StrictRemoteIndexIncompleteV1::PreparingObserved)
        );
    }

    #[tokio::test]
    async fn strict_bound_index_read_requires_route_canonical_deletion_evidence() {
        let generation = "123-00000000-0000-4000-8000-000000000000";
        let valid =
            strict_deleted_index_json(Some(&format!("roots/.tcfs-trash/{generation}/dir/file")));
        let op = bound_object_test_operator(BTreeMap::from([(
            "roots/index/dir/file".to_owned(),
            valid,
        )]));
        let deleted = match read_exact_raw_index_entry_v1(&op, "roots", "dir/file")
            .await
            .unwrap()
        {
            ExactRawIndexEntryReadV1::Deleted(deleted) => deleted,
            other => panic!("expected route-bound deleted index, got {other:?}"),
        };
        assert_eq!(
            deleted.deletion_evidence().unwrap().safety_copy_key,
            format!("roots/.tcfs-trash/{generation}/dir/file")
        );
        let evidence_free_op = bound_object_test_operator(BTreeMap::from([(
            "roots/index/dir/file".to_owned(),
            strict_deleted_index_json(None),
        )]));
        assert!(matches!(
            read_exact_raw_index_entry_v1(&evidence_free_op, "roots", "dir/file")
                .await
                .unwrap(),
            ExactRawIndexEntryReadV1::Deleted(ref deleted)
                if deleted.deletion_evidence().is_none()
        ));

        for safety_copy_key in [
            "other/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/dir/file",
            "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/other",
            "roots/.tcfs-trash//dir/file",
            "roots/.tcfs-trash/123/dir/file",
            "roots/.tcfs-trash/0123-00000000-0000-4000-8000-000000000000/dir/file",
            "roots/.tcfs-trash/18446744073709551616-00000000-0000-4000-8000-000000000000/dir/file",
            "roots/.tcfs-trash/123-00000000-0000-3000-8000-000000000000/dir/file",
            "roots/.tcfs-trash/123-00000000-0000-4000-7000-000000000000/dir/file",
            "roots/.tcfs-trash/123-00000000-0000-4000-8000-00000000000A/dir/file",
        ] {
            let op = bound_object_test_operator(BTreeMap::from([(
                "roots/index/dir/file".to_owned(),
                strict_deleted_index_json(Some(safety_copy_key)),
            )]));
            assert_eq!(
                read_exact_raw_index_entry_v1(&op, "roots", "dir/file")
                    .await
                    .unwrap(),
                ExactRawIndexEntryReadV1::Incomplete(
                    StrictRemoteIndexIncompleteV1::InvalidIndexRecord
                ),
                "accepted noncanonical deletion evidence {safety_copy_key:?}"
            );
        }
    }

    #[tokio::test]
    async fn strict_directory_marker_read_binds_live_and_deleted_bodies() {
        let marker_key = "roots/index/dir/.tcfs_dir";
        let live_op = bound_object_test_operator(BTreeMap::from([(
            marker_key.to_owned(),
            DIRECTORY_MARKER_BYTES.to_vec(),
        )]));
        let live = match read_exact_raw_directory_marker_v1(&live_op, "roots", "dir")
            .await
            .unwrap()
        {
            ExactRawDirectoryMarkerReadV1::Live(live) => live,
            other => panic!("expected bound live marker, got {other:?}"),
        };
        assert_eq!(live.route().remote_prefix(), "roots");
        assert_eq!(live.route().logical_dir(), "dir");
        assert_eq!(live.route().marker_rel_path(), "dir/.tcfs_dir");
        assert_eq!(live.route().marker_key(), marker_key);
        assert_eq!(
            live.object().raw_bytes_len(),
            u64::try_from(DIRECTORY_MARKER_BYTES.len()).unwrap()
        );
        assert_eq!(
            live.object().raw_blake3(),
            blake3::hash(DIRECTORY_MARKER_BYTES).as_bytes()
        );

        let deleted_op = bound_object_test_operator(BTreeMap::from([(
            marker_key.to_owned(),
            strict_deleted_index_json(Some(
                "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/dir/.tcfs_dir",
            )),
        )]));
        let deleted = match read_exact_raw_directory_marker_v1(&deleted_op, "roots", "dir")
            .await
            .unwrap()
        {
            ExactRawDirectoryMarkerReadV1::Deleted(deleted) => deleted,
            other => panic!("expected bound deleted marker, got {other:?}"),
        };
        assert_eq!(deleted.route().logical_dir(), "dir");
        assert_eq!(deleted.route().marker_rel_path(), "dir/.tcfs_dir");
        assert_eq!(deleted.route().marker_key(), marker_key);
        assert_eq!(
            deleted.object().raw_bytes_len(),
            u64::try_from(
                strict_deleted_index_json(Some(
                    "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/dir/.tcfs_dir"
                ))
                .len()
            )
            .unwrap()
        );
        assert_eq!(
            deleted.deletion_evidence().unwrap().safety_copy_key,
            "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/dir/.tcfs_dir"
        );
        let evidence_free_op = bound_object_test_operator(BTreeMap::from([(
            marker_key.to_owned(),
            strict_deleted_index_json(None),
        )]));
        assert!(matches!(
            read_exact_raw_directory_marker_v1(&evidence_free_op, "roots", "dir")
                .await
                .unwrap(),
            ExactRawDirectoryMarkerReadV1::Deleted(ref deleted)
                if deleted.deletion_evidence().is_none()
        ));
    }

    #[tokio::test]
    async fn strict_directory_marker_read_rejects_compatibility_and_wrong_route_bodies() {
        let marker_key = "roots/index/dir/.tcfs_dir";
        let committed = format!(
            r#"{{"version":2,"state":"committed","current":{{"manifest_hash":"{}","size":0,"chunks":0}},"pending":null}}"#,
            "b".repeat(64)
        )
        .into_bytes();
        let preparing = format!(
            r#"{{"version":2,"state":"preparing","current":null,"pending":{{"manifest_hash":"{}","size":0,"chunks":0,"staged_manifest_key":"roots/staging/object"}}}}"#,
            "b".repeat(64)
        )
        .into_bytes();
        for body in [
            b"type=directory\r\n".to_vec(),
            b"type=directory\n\n".to_vec(),
            committed,
            preparing,
            strict_deleted_index_json(Some(
                "roots/.tcfs-trash/123-00000000-0000-4000-8000-000000000000/other/.tcfs_dir",
            )),
        ] {
            let op = bound_object_test_operator(BTreeMap::from([(marker_key.to_owned(), body)]));
            assert_eq!(
                read_exact_raw_directory_marker_v1(&op, "roots", "dir")
                    .await
                    .unwrap(),
                ExactRawDirectoryMarkerReadV1::Incomplete(
                    StrictRemoteDirectoryMarkerIncompleteV1::InvalidMarkerRecord
                )
            );
        }

        let missing = bound_object_test_operator(BTreeMap::new());
        assert_eq!(
            read_exact_raw_directory_marker_v1(&missing, "roots", "dir")
                .await
                .unwrap(),
            ExactRawDirectoryMarkerReadV1::Missing
        );
    }

    #[tokio::test]
    async fn strict_namespace_reservation_read_requires_canonical_bound_body_and_address() {
        let canonical =
            br#"{"version":1,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file"}"#
                .to_vec();
        let object_id = namespace_reservation_object_id("doc.txt");
        let object_key = format!("roots/.tcfs-namespace/v1/{object_id}");
        let op =
            bound_object_test_operator(BTreeMap::from([(object_key.clone(), canonical.clone())]));
        let bound = match read_exact_namespace_reservation_v1(&op, "roots", &object_id)
            .await
            .unwrap()
        {
            ExactNamespaceReservationReadV1::Bound(bound) => bound,
            other => panic!("expected bound namespace reservation, got {other:?}"),
        };
        assert_eq!(bound.object_key(), object_key);
        assert_eq!(bound.object_id(), object_id);
        assert_eq!(bound.exact_path(), "doc.txt");
        assert_eq!(bound.folded_path(), "doc.txt");
        assert_eq!(bound.role(), PortableNamespaceRole::File);
        assert_eq!(
            bound.object().raw_bytes_len(),
            u64::try_from(canonical.len()).unwrap()
        );

        let noncanonical = br#"{
  "version": 1,
  "exact_path": "doc.txt",
  "folded_path": "doc.txt",
  "role": "file"
}"#
        .to_vec();
        let op = bound_object_test_operator(BTreeMap::from([(object_key.clone(), noncanonical)]));
        assert_eq!(
            read_exact_namespace_reservation_v1(&op, "roots", &object_id)
                .await
                .unwrap(),
            ExactNamespaceReservationReadV1::Incomplete(
                StrictNamespaceReservationIncompleteV1::InvalidReservation
            )
        );

        let wrong_id = namespace_reservation_object_id("other.txt");
        assert_ne!(wrong_id, object_id);
        let wrong_key = format!("roots/.tcfs-namespace/v1/{wrong_id}");
        let op = bound_object_test_operator(BTreeMap::from([(wrong_key, canonical)]));
        assert_eq!(
            read_exact_namespace_reservation_v1(&op, "roots", &wrong_id)
                .await
                .unwrap(),
            ExactNamespaceReservationReadV1::Incomplete(
                StrictNamespaceReservationIncompleteV1::AddressMismatch
            )
        );
    }

    #[tokio::test]
    async fn strict_namespace_reservation_read_rejects_invalid_semantics_and_bounds() {
        let object_id = namespace_reservation_object_id("doc.txt");
        let object_key = format!("roots/.tcfs-namespace/v1/{object_id}");
        let overlong = "a".repeat(
            usize::try_from(
                RegisteredRootPlanContractV1::strict_v1()
                    .remote_contract()
                    .max_logical_component_bytes()
                    + 1,
            )
            .unwrap(),
        );
        for body in [
            br#"{"version":1,"exact_path":"doc.txt","folded_path":"DOC.TXT","role":"file"}"#
                .to_vec(),
            br#"{"version":2,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file"}"#
                .to_vec(),
            br#"{"version":1,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file","extra":true}"#
                .to_vec(),
            format!(
                r#"{{"version":1,"exact_path":"{overlong}","folded_path":"{overlong}","role":"file"}}"#
            )
            .into_bytes(),
        ] {
            let op = bound_object_test_operator(BTreeMap::from([(
                object_key.clone(),
                body,
            )]));
            assert_eq!(
                read_exact_namespace_reservation_v1(&op, "roots", &object_id)
                    .await
                    .unwrap(),
                ExactNamespaceReservationReadV1::Incomplete(
                    StrictNamespaceReservationIncompleteV1::InvalidReservation
                )
            );
        }

        let missing = bound_object_test_operator(BTreeMap::new());
        assert_eq!(
            read_exact_namespace_reservation_v1(&missing, "roots", &object_id)
                .await
                .unwrap(),
            ExactNamespaceReservationReadV1::Missing
        );
    }

    #[tokio::test]
    async fn strict_marker_and_reservation_reads_refuse_unbound_objects() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        op.write("roots/index/dir/.tcfs_dir", DIRECTORY_MARKER_BYTES.to_vec())
            .await
            .unwrap();
        let reservation =
            br#"{"version":1,"exact_path":"doc.txt","folded_path":"doc.txt","role":"file"}"#;
        let object_id = namespace_reservation_object_id("doc.txt");
        op.write(
            &format!("roots/.tcfs-namespace/v1/{object_id}"),
            reservation.to_vec(),
        )
        .await
        .unwrap();
        let before = object_inventory(&op).await;

        assert_eq!(
            read_exact_raw_directory_marker_v1(&op, "roots", "dir")
                .await
                .unwrap(),
            ExactRawDirectoryMarkerReadV1::Incomplete(
                StrictRemoteDirectoryMarkerIncompleteV1::UnboundObject
            )
        );
        assert_eq!(
            read_exact_namespace_reservation_v1(&op, "roots", &object_id)
                .await
                .unwrap(),
            ExactNamespaceReservationReadV1::Incomplete(
                StrictNamespaceReservationIncompleteV1::UnboundObject
            )
        );
        assert_eq!(object_inventory(&op).await, before);
    }

    #[tokio::test]
    async fn strict_remote_body_readers_reject_oversized_prefix_before_backend_io() {
        let (op, stat_calls) = bound_object_test_operator_with_stat_counter(BTreeMap::new());
        let oversized_prefix = "p".repeat(
            usize::try_from(
                RegisteredRootPlanContractV1::strict_v1()
                    .remote_contract()
                    .max_storage_key_bytes()
                    + 1,
            )
            .unwrap(),
        );
        let object_id = namespace_reservation_object_id("doc.txt");

        assert!(
            read_exact_raw_index_entry_v1(&op, &oversized_prefix, "doc.txt")
                .await
                .is_err()
        );
        assert!(
            read_exact_raw_directory_marker_v1(&op, &oversized_prefix, "dir")
                .await
                .is_err()
        );
        assert!(
            read_exact_namespace_reservation_v1(&op, &oversized_prefix, &object_id)
                .await
                .is_err()
        );
        assert_eq!(stat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn strict_namespace_reservation_read_accepts_the_exact_path_ceiling() {
        let exact_path = format!(
            "{}/{}/{}/{}/e",
            "a".repeat(255),
            "b".repeat(255),
            "c".repeat(255),
            "d".repeat(254),
        );
        assert_eq!(
            u64::try_from(exact_path.len()).unwrap(),
            RegisteredRootPlanContractV1::strict_v1()
                .remote_contract()
                .max_logical_path_bytes()
        );
        let body = format!(
            r#"{{"version":1,"exact_path":"{exact_path}","folded_path":"{exact_path}","role":"directory"}}"#
        )
        .into_bytes();
        let object_id = namespace_reservation_object_id(&exact_path);
        let object_key = format!("roots/.tcfs-namespace/v1/{object_id}");
        let op = bound_object_test_operator(BTreeMap::from([(object_key, body)]));

        let bound = match read_exact_namespace_reservation_v1(&op, "roots", &object_id)
            .await
            .unwrap()
        {
            ExactNamespaceReservationReadV1::Bound(bound) => bound,
            other => panic!("expected ceiling-sized reservation, got {other:?}"),
        };
        assert_eq!(bound.exact_path(), exact_path);
        assert_eq!(bound.folded_path(), exact_path);
        assert_eq!(bound.role(), PortableNamespaceRole::Directory);
    }

    #[tokio::test]
    async fn strict_manifest_read_derives_the_committed_index_route() {
        let manifest_bytes = regular_manifest_json("dir/file");
        let manifest_hash = manifest_object_id(&manifest_bytes);
        let index_record = format!(
            r#"{{
  "version": 2,
  "state": "committed",
  "current": {{
    "manifest_hash": "{manifest_hash}",
    "size": 4,
    "chunks": 1
  }},
  "pending": null
}}"#
        )
        .into_bytes();
        let op = bound_object_test_operator(BTreeMap::from([
            ("roots/index/dir/file".to_owned(), index_record),
            (format!("roots/manifests/{manifest_hash}"), manifest_bytes),
        ]));

        let committed = match read_exact_raw_index_entry_v1(&op, "roots", "dir/file")
            .await
            .unwrap()
        {
            ExactRawIndexEntryReadV1::Committed(committed) => committed,
            other => panic!("expected route-bound committed index, got {other:?}"),
        };
        assert_eq!(committed.route().remote_prefix(), "roots");
        assert_eq!(committed.route().rel_path(), "dir/file");
        assert_eq!(committed.route().index_key(), "roots/index/dir/file");

        let manifest = read_strict_remote_manifest_v1(&op, &committed)
            .await
            .unwrap();
        assert!(matches!(
            manifest,
            StrictRemoteManifestReadV1::Complete(manifest)
                if matches!(
                    manifest.as_ref(),
                    StrictRemoteManifestV1::Regular { manifest, .. }
                        if manifest.rel_path() == "dir/file"
                )
        ));
    }

    #[tokio::test]
    async fn strict_manifest_refuses_unbound_completion_without_reading_bytes() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let bytes = regular_manifest_json("file");
        let object_id = manifest_object_id(&bytes);
        let object_key = format!("roots/manifests/{object_id}");
        op.write(&object_key, bytes.clone()).await.unwrap();
        let before_unbound = object_inventory(&op).await;
        let index = committed_index_for_test(StrictRemoteIndexEntryV1 {
            manifest_hash: object_id.clone(),
            size: 4,
            chunks: 1,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        });
        assert!(matches!(
            read_strict_remote_manifest_v1(&op, &index).await.unwrap(),
            StrictRemoteManifestReadV1::Incomplete(StrictRemoteManifestIncompleteV1::UnboundObject)
        ));
        assert_eq!(object_inventory(&op).await, before_unbound);

        let legacy_file_hash = "d".repeat(64);
        let legacy_key = format!("roots/manifests/{legacy_file_hash}");
        op.write(&legacy_key, bytes.clone()).await.unwrap();
        let before_legacy = object_inventory(&op).await;
        let legacy_index = committed_index_for_test(StrictRemoteIndexEntryV1 {
            manifest_hash: legacy_file_hash.clone(),
            size: 4,
            chunks: 1,
            kind: RemoteEntryKind::RegularFile,
            symlink_target: None,
        });
        assert!(matches!(
            read_strict_remote_manifest_v1(&op, &legacy_index)
                .await
                .unwrap(),
            StrictRemoteManifestReadV1::Incomplete(StrictRemoteManifestIncompleteV1::UnboundObject)
        ));
        assert_eq!(object_inventory(&op).await, before_legacy);
    }
}
