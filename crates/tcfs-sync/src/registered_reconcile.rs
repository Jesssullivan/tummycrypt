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
use std::path::Path;
use tcfs_core::config::RegisteredRootPlanContractV1;
use unicode_normalization::UnicodeNormalization;

use crate::blacklist::Blacklist;
use crate::conflict::{ConflictInfo, VectorClock};
use crate::index_entry::portable_casefold_path;
use crate::index_entry::{
    manifest_object_id, read_raw_object_snapshot_v1, validate_canonical_namespace_remote_prefix,
    validate_namespace_logical_path, validate_relative_storage_key, validate_storage_key_component,
    DeletionEvidence, IndexEntryState, PortableNamespaceRole, RawObjectReadV1, RawObjectSnapshotV1,
    RemoteEntryKind,
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
#[derive(Clone)]
pub struct StrictPrimaryStateSnapshotV1 {
    raw_bytes_digest: StrictPrimaryStateBytesDigestV1,
    entries: BTreeMap<String, BoundStrictSyncStateV1>,
    last_nats_seq: u64,
    device_id: String,
}

impl fmt::Debug for StrictPrimaryStateSnapshotV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictPrimaryStateSnapshotV1")
            .field("raw_bytes_digest", &self.raw_bytes_digest)
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
}

#[derive(Debug, Clone)]
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
    let entries = match bind_strict_state_entries_v1(
        raw_entries,
        canonical_local_root,
        canonical_remote_prefix,
        binding_mode,
    ) {
        Ok(entries) => entries,
        Err(_) => {
            return Ok(StrictPrimaryStateReadV1::Incomplete(
                StrictPrimaryStateIncompleteV1::InvalidRootBinding,
            ));
        }
    };

    let raw_digest = StrictPrimaryStateBytesDigestV1(*raw_snapshot.raw_bytes_digest().as_bytes());
    let (_raw_bytes, _) = raw_snapshot.into_parts();
    Ok(StrictPrimaryStateReadV1::Complete(
        StrictPrimaryStateSnapshotV1 {
            raw_bytes_digest: raw_digest,
            entries,
            last_nats_seq: wire.last_nats_seq,
            device_id: wire.device_id,
        },
    ))
}

fn bind_strict_state_entries_v1(
    raw_entries: BTreeMap<String, StrictSyncStateV1>,
    canonical_local_root: &Path,
    canonical_remote_prefix: &str,
    binding_mode: StrictStateRootBindingModeV1,
) -> Result<BTreeMap<String, BoundStrictSyncStateV1>> {
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

    let mut namespace_claims: BTreeMap<String, (String, PortableNamespaceRole)> = BTreeMap::new();
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

        reserve_state_namespace_claims_v1(&rel_path, &mut namespace_claims)?;
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
    Ok(entries)
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

fn reserve_state_namespace_claims_v1(
    rel_path: &str,
    claims: &mut BTreeMap<String, (String, PortableNamespaceRole)>,
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
        if let Some((existing_path, existing_role)) = claims.get(&folded_path) {
            anyhow::ensure!(
                existing_path == &exact_path && *existing_role == role,
                "strict state namespace has a portable spelling or role collision"
            );
        } else {
            claims.insert(folded_path, (exact_path, role));
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

fn bind_remote_object_v1(snapshot: RawObjectSnapshotV1) -> BoundRemoteObjectSnapshotV1 {
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
    remote_prefix: String,
    rel_path: String,
    index_key: String,
}

impl RegisteredRootRemoteIndexRouteV1 {
    pub fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
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

fn validate_strict_index_path_semantics_v1(
    parsed: &ParsedStrictIndexRecordV1,
    rel_path: &str,
) -> Result<()> {
    let validate_entry = |entry: &StrictRemoteIndexEntryV1| -> Result<()> {
        if let Some(target) = entry.symlink_target() {
            crate::engine::validate_indexed_symlink_target(Path::new(rel_path), target)?;
        }
        Ok(())
    };
    match parsed {
        ParsedStrictIndexRecordV1::Deleted { .. } => {}
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
    let prefix = validate_canonical_namespace_remote_prefix(remote_prefix)?;
    validate_registered_remote_logical_path_bounds_v1(rel_path)?;
    let index_key = if prefix.is_empty() {
        format!("index/{rel_path}")
    } else {
        format!("{prefix}/index/{rel_path}")
    };
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_index_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, &index_key, max_bytes).await? else {
        return Ok(ExactRawIndexEntryReadV1::Missing);
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(ExactRawIndexEntryReadV1::Incomplete(
                StrictRemoteIndexIncompleteV1::UnboundObject,
            ));
        }
    };
    let parsed = match parse_strict_index_record_v1(raw_snapshot.raw_bytes()) {
        Ok(parsed) if validate_strict_index_path_semantics_v1(&parsed, rel_path).is_ok() => parsed,
        Err(_) => {
            return Ok(ExactRawIndexEntryReadV1::Incomplete(
                StrictRemoteIndexIncompleteV1::InvalidIndexRecord,
            ));
        }
        Ok(_) => {
            return Ok(ExactRawIndexEntryReadV1::Incomplete(
                StrictRemoteIndexIncompleteV1::InvalidIndexRecord,
            ));
        }
    };
    if matches!(&parsed, ParsedStrictIndexRecordV1::Preparing { .. }) {
        return Ok(ExactRawIndexEntryReadV1::Incomplete(
            StrictRemoteIndexIncompleteV1::PreparingObserved,
        ));
    }
    let object = bind_remote_object_v1(raw_snapshot);
    let route = RegisteredRootRemoteIndexRouteV1 {
        remote_prefix: prefix.to_owned(),
        rel_path: rel_path.to_owned(),
        index_key,
    };

    Ok(match parsed {
        ParsedStrictIndexRecordV1::Deleted { deletion_evidence } => {
            ExactRawIndexEntryReadV1::Deleted(RawDeletedIndexEntryV1 {
                object,
                deletion_evidence,
                route,
            })
        }
        ParsedStrictIndexRecordV1::Preparing { .. } => {
            unreachable!("preparing records return incomplete before binding object evidence")
        }
        ParsedStrictIndexRecordV1::Committed { current, format } => {
            ExactRawIndexEntryReadV1::Committed(RawCommittedIndexEntryV1 {
                object,
                current,
                format,
                route,
            })
        }
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

fn validate_registered_remote_storage_key_bounds_v1(key: &str, description: &str) -> Result<()> {
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

/// Read the exact manifest object named by one strict committed index entry.
///
/// The object key must equal the domain-addressed manifest bytes literally;
/// legacy embedded file-hash and symlink-target identities are not accepted.
pub async fn read_strict_remote_manifest_v1(
    op: &Operator,
    committed_index: &RawCommittedIndexEntryV1,
) -> Result<StrictRemoteManifestReadV1> {
    let index_entry = committed_index.current();
    let prefix = committed_index.remote_prefix();
    let rel_path = committed_index.rel_path();
    if Blacklist::default()
        .check_fixed_ingress_path_components(Path::new(rel_path))
        .is_some()
    {
        return Ok(StrictRemoteManifestReadV1::Incomplete(
            StrictRemoteManifestIncompleteV1::ExcludedPath,
        ));
    }
    let manifest_key = if prefix.is_empty() {
        format!("manifests/{}", index_entry.manifest_hash())
    } else {
        format!("{prefix}/manifests/{}", index_entry.manifest_hash())
    };
    let max_bytes = RegisteredRootPlanContractV1::strict_v1()
        .remote_contract()
        .max_manifest_object_bytes();
    let Some(raw_read) = read_raw_object_snapshot_v1(op, &manifest_key, max_bytes).await? else {
        return Ok(StrictRemoteManifestReadV1::Incomplete(
            StrictRemoteManifestIncompleteV1::MissingObject,
        ));
    };
    let raw_snapshot = match raw_read {
        RawObjectReadV1::Bound(snapshot) => snapshot,
        RawObjectReadV1::Unbound => {
            return Ok(StrictRemoteManifestReadV1::Incomplete(
                StrictRemoteManifestIncompleteV1::UnboundObject,
            ));
        }
    };
    if manifest_object_id(raw_snapshot.raw_bytes()) != index_entry.manifest_hash() {
        return Ok(StrictRemoteManifestReadV1::Incomplete(
            StrictRemoteManifestIncompleteV1::AddressMismatch,
        ));
    }

    let parsed = match index_entry.kind() {
        RemoteEntryKind::RegularFile => {
            let wire: StrictRegularManifestWireV1 =
                match serde_json::from_slice(raw_snapshot.raw_bytes()) {
                    Ok(wire) => wire,
                    Err(_) => {
                        return Ok(StrictRemoteManifestReadV1::Incomplete(
                            StrictRemoteManifestIncompleteV1::InvalidManifest,
                        ));
                    }
                };
            match strict_regular_manifest_v1(wire, rel_path, index_entry) {
                Ok(manifest) => ParsedStrictManifestV1::Regular(manifest),
                Err(_) => {
                    return Ok(StrictRemoteManifestReadV1::Incomplete(
                        StrictRemoteManifestIncompleteV1::InvalidManifest,
                    ));
                }
            }
        }
        RemoteEntryKind::Symlink => {
            let wire: StrictSymlinkManifestWireV1 =
                match serde_json::from_slice(raw_snapshot.raw_bytes()) {
                    Ok(wire) => wire,
                    Err(_) => {
                        return Ok(StrictRemoteManifestReadV1::Incomplete(
                            StrictRemoteManifestIncompleteV1::InvalidManifest,
                        ));
                    }
                };
            match strict_symlink_manifest_v1(wire, rel_path, index_entry) {
                Ok(manifest) => ParsedStrictManifestV1::Symlink(manifest),
                Err(_) => {
                    return Ok(StrictRemoteManifestReadV1::Incomplete(
                        StrictRemoteManifestIncompleteV1::InvalidManifest,
                    ));
                }
            }
        }
    };
    let object = bind_remote_object_v1(raw_snapshot);
    Ok(StrictRemoteManifestReadV1::Complete(Box::new(
        match parsed {
            ParsedStrictManifestV1::Regular(manifest) => {
                StrictRemoteManifestV1::Regular { object, manifest }
            }
            ParsedStrictManifestV1::Symlink(manifest) => {
                StrictRemoteManifestV1::Symlink { object, manifest }
            }
        },
    )))
}

enum ParsedStrictManifestV1 {
    Regular(StrictRegularManifestV1),
    Symlink(StrictSymlinkManifestV1),
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
        OperatorBuilder::new(BoundObjectTestBackend {
            objects: Arc::new(objects),
            info: Arc::new(info),
        })
        .finish()
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
                remote_prefix: "roots".to_owned(),
                rel_path: "file".to_owned(),
                index_key: "roots/index/file".to_owned(),
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
