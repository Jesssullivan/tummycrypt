//! Stat-gated freshness memo for the reconcile classifier.
//!
//! # Why this exists
//!
//! `reconcile()` classifies every path present on **both** sides through
//! `compare_both_exist_with_context`, which unconditionally pays
//!
//!   * one full-file BLAKE3 (`tcfs_chunks::hash_file`), and
//!   * one S3 GET of the remote manifest (`op.read(manifests/<hash>)`),
//!
//! inside a strictly sequential per-path loop. At the measured live constant of
//! ~29 ms per both-exist path that is ~10.1 h for a single 1.26 M-file estate
//! pass against a 300 s cadence — 122x over budget, 244x with the mandatory
//! `--expect-plan` double pass. The classifier, not registry validation and not
//! the status cache, is the estate blocker.
//!
//! # Why the obvious fast path was (correctly) refused before
//!
//! The engine already has a `(size, mtime-seconds)` quick check (`needs_sync`),
//! and reconcile deliberately does **not** use it. `reconcile.rs` says why, at
//! the push execute site: a same-second, same-size 41-byte `.git` ref rewrite
//! (`git commit` moving a branch head) is invisible to `(size, mtime-secs)` and
//! would be silently skipped. `SyncState.mtime` is a `u64` of **seconds**, so
//! the state cache does not even retain the precision a sound gate needs.
//!
//! This module supplies the precision the state cache lacks, without touching
//! `SyncState` — which is serialized onto the wire and must not start carrying
//! device-local inode numbers.
//!
//! # What makes the gate sound
//!
//! `compare_both_exist_with_context` is a **pure function** of exactly:
//!
//!   1. the local file bytes,
//!   2. the tracked local vector clock (`SyncState.vclock`),
//!   3. the tracked baseline hash (`SyncState.blake3`),
//!   4. the remote manifest **body**,
//!   5. this device's id, and
//!   6. the path (plus the remote prefix it is resolved against).
//!
//! (4) is content-addressed: the remote index entry names its manifest by
//! `manifest_hash`, and `engine::validate_indexed_manifest_binding` refuses any
//! manifest body that does not hash to that name. An unchanged `manifest_hash`
//! therefore **is** an unchanged manifest body — unchanged remote vclock,
//! unchanged remote `file_hash`, unchanged `written_by`. No GET is required to
//! know that, and that is where the per-path S3 round trip goes.
//!
//! (2), (3), (5) and (6) are compared through [`InputFingerprint`], a domain-
//! separated BLAKE3 over all of them at once.
//!
//! That leaves (1) as the only input we approximate. [`StatIdentity`] is the
//! approximation: `(dev, ino, size, mtime_ns, ctime_ns)`. A record is installed
//! **only** for a pair whose full evaluation converged on `UpToDate`, so a hit
//! replays a decision — it never invents one.
//!
//! ## ctime is the load-bearing field
//!
//! `mtime` alone is forgeable and is routinely forged by benign tools: `cp -p`,
//! `rsync --times`, `tar -x`, and restore utilities all rewrite content and then
//! put mtime back. `ctime` (the inode *change* time) is kernel-maintained and
//! has no portable API that sets it: `utimensat`/`futimens` set atime/mtime and
//! **bump** ctime as a side effect. A same-size in-place rewrite with a forged
//! mtime therefore still moves ctime — that is the property test in this module,
//! asserted both on synthetic identities and on a real file via `utimensat`.
//!
//! ### darwin caveat (R-C / J1)
//!
//! macOS is the one platform with an API that can move ctime directly:
//! `setattrlist(2)` with `ATTR_CMN_CHGTIME`. The live R-C/J1 probe found ctime
//! *resists* there in practice — the write that follows re-bumps it — but the
//! honest statement is narrower: **this gate is a performance memo against
//! benign staleness, not an integrity boundary.** Anyone able to call
//! `setattrlist` on the file already has write access to it and could simply
//! leave the file alone instead. Two independent brakes bound the exposure:
//!
//!   * `ino` moves under the write-temp-then-rename pattern that essentially
//!     every metadata-restoring tool uses, and
//!   * every record carries a **jittered TTL** (default 24 h, phase derived from
//!     the path so a 1.26 M-record corpus does not expire in one cycle), so any
//!     forged record is re-proved by full hash within a day.
//!
//! `--paranoid` disables the memo entirely and restores unconditional
//! re-hashing; that is the integrity answer.
//!
//! # Plan-hash neutrality
//!
//! `ReconcilePlan::sha256` hashes actions only, and an `UpToDate` action
//! contributes exactly `path` and `kind="up-to-date"`. A memoized `UpToDate` is
//! byte-identical to a computed one, so the fast path **cannot** move the plan
//! SHA-256 and the `--expect-plan` dry-run/execute protocol is unaffected. The
//! persisted sidecar additionally lets the execute pass reuse the dry-run pass's
//! work, which is where the 2x `--expect-plan` cost goes.
//!
//! # Invalidation is structural
//!
//! Nothing needs to explicitly evict after an execute: every mutation moves at
//! least one compared input.
//!
//!   * a **pull** rewrites the local file (stat moves) and the tracked baseline;
//!   * a **push** publishes a new manifest, so `manifest_hash` moves;
//!   * a **local delete** removes the path from the both-exist arm entirely;
//!   * a **state-cache rollback** (recovery from backup) moves `blake3` or
//!     `vclock`, which is still a mismatch and still a miss.
//!
//! A stale record can therefore only ever cause a *miss* — a wasted full
//! evaluation — never a wrong skip.
//!
//! # Memory
//!
//! The estate is 1.26 M files, so a record that stores its inputs verbatim
//! (two hex hashes, a prefix, a device id, a `BTreeMap` vector clock) costs
//! roughly 670 B and the whole memo would be ~845 MB on a laptop. It is not
//! stored that way. A record never needs to *reproduce* its inputs, only to
//! answer "are they all still identical", so everything except the stat is
//! folded into a 16-byte fingerprint and the key is a 16-byte fingerprint of the
//! canonical path:
//!
//! | field | bytes |
//! |---|---|
//! | key ([`PathFingerprint`]) | 16 |
//! | [`StatIdentity`] | 56 |
//! | [`InputFingerprint`] | 16 |
//! | `verified_at` | 8 |
//!
//! ~96 B of payload, ~135 B/record including hash-table overhead: **~170 MB at
//! the full 1.26 M**, and [`FreshnessCache::with_max_entries`] caps it above
//! that. A key collision is harmless by construction: the path is folded into
//! the input fingerprint, so a collided lookup fails the fingerprint check and
//! degrades to a miss.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::{debug, warn};

use crate::conflict::VectorClock;

/// On-disk schema tag. A sidecar written by a different schema is discarded
/// rather than migrated: this is a pure cache, so throwing it away costs one
/// slow cycle and nothing else.
const FRESHNESS_SCHEMA: &str = "tcfs-freshness-v1";

/// Default maximum age of a memoized verdict before it is re-proved by full
/// hash plus manifest read. At a 300 s cadence this re-verifies each path about
/// once per 288 cycles — roughly 0.35 % of the unoptimized pass.
pub const DEFAULT_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Default cap on memoized paths, chosen so a full-estate memo stays under
/// ~170 MB resident. Past the cap the memo stops installing new records; it
/// never evicts a record it might still replay.
pub const DEFAULT_MAX_ENTRIES: usize = 1_000_000;

/// Wall-clock seconds, used only for the memo's re-verification TTL. Never a
/// correctness input: a clock that moves can only cause misses.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn unhex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

macro_rules! hex_fingerprint {
    ($name:ident, $domain:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 16]);

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&hex16(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                unhex16(&text)
                    .map($name)
                    .ok_or_else(|| D::Error::custom("expected 32 hex characters"))
            }
        }

        impl $name {
            /// Domain-separated 128-bit truncation of a BLAKE3 digest.
            fn from_hasher(hasher: blake3::Hasher) -> Self {
                let digest = hasher.finalize();
                let mut out = [0u8; 16];
                out.copy_from_slice(&digest.as_bytes()[..16]);
                Self(out)
            }

            fn hasher() -> blake3::Hasher {
                let mut hasher = blake3::Hasher::new();
                field(&mut hasher, "domain", $domain.as_bytes());
                hasher
            }
        }
    };
}

/// Length-prefixed field mixing, so no two distinct input tuples can produce the
/// same byte stream by concatenation.
fn field(hasher: &mut blake3::Hasher, tag: &str, value: &[u8]) {
    hasher.update(&(tag.len() as u32).to_be_bytes());
    hasher.update(tag.as_bytes());
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn text_field(hasher: &mut blake3::Hasher, tag: &str, value: &str) {
    field(hasher, tag, value.as_bytes());
}

fn number_field(hasher: &mut blake3::Hasher, tag: &str, value: u64) {
    field(hasher, tag, &value.to_be_bytes());
}

hex_fingerprint!(
    PathFingerprint,
    "dev.tinyland.tcfs.freshness.path.v1",
    "Cache key: a 128-bit fingerprint of a canonical absolute local path.\n\nA collision is harmless — the path is also folded into\n[`InputFingerprint`], so a collided lookup fails the input check and\ndegrades to an ordinary miss."
);

hex_fingerprint!(
    InputFingerprint,
    "dev.tinyland.tcfs.freshness.inputs.v1",
    "Every classifier input except the local file bytes, folded into 128 bits.\n\nStored instead of the inputs themselves so the whole-estate memo fits in\nmemory; a record never needs to reproduce its inputs, only to detect that\nthey moved."
);

impl PathFingerprint {
    /// Fingerprint a canonical state-cache key (see `state::canonical_path_key`).
    pub fn of(canonical_key: &str) -> Self {
        let mut hasher = Self::hasher();
        text_field(&mut hasher, "path", canonical_key);
        Self::from_hasher(hasher)
    }
}

/// Everything the gate compares, other than the local stat identity.
///
/// Built fresh from the live cycle on lookup, and from the proved evaluation on
/// install. The two must agree exactly.
pub struct FreshnessInputs<'a> {
    /// Canonical absolute local path — pins the record to one file, and makes a
    /// [`PathFingerprint`] collision a miss rather than a wrong hit.
    pub canonical_key: &'a str,
    /// Relative path as the classifier saw it.
    pub rel_path: &'a str,
    /// Remote prefix this verdict was proved against, so two roots overlapping
    /// on one absolute path cannot serve each other's verdicts.
    pub remote_prefix: &'a str,
    /// `SyncState.blake3` — classifier input (3). An `UpToDate` verdict can only
    /// be proved when the local bytes hash to exactly this, so this doubles as
    /// the content identity the memo vouches for.
    pub tracked_blake3: &'a str,
    /// `SyncState.vclock` — classifier input (2).
    pub tracked_vclock: &'a VectorClock,
    /// The remote index entry's content-addressed manifest name — input (4).
    pub remote_manifest_hash: &'a str,
    /// The remote index entry's declared size.
    pub remote_size: u64,
    /// This device's id — classifier input (5).
    pub device_id: &'a str,
}

impl InputFingerprint {
    pub fn of(inputs: &FreshnessInputs<'_>) -> Self {
        let mut hasher = Self::hasher();
        text_field(&mut hasher, "path", inputs.canonical_key);
        text_field(&mut hasher, "rel-path", inputs.rel_path);
        text_field(&mut hasher, "remote-prefix", inputs.remote_prefix);
        text_field(&mut hasher, "tracked-blake3", inputs.tracked_blake3);
        text_field(&mut hasher, "remote-manifest", inputs.remote_manifest_hash);
        number_field(&mut hasher, "remote-size", inputs.remote_size);
        text_field(&mut hasher, "device", inputs.device_id);
        number_field(
            &mut hasher,
            "vclock.len",
            inputs.tracked_vclock.clocks.len() as u64,
        );
        for (device, tick) in &inputs.tracked_vclock.clocks {
            text_field(&mut hasher, "vclock.device", device);
            number_field(&mut hasher, "vclock.tick", *tick);
        }
        Self::from_hasher(hasher)
    }
}

/// The local-side identity a freshness verdict is bound to.
///
/// Nanosecond mtime **and** ctime are both retained on purpose — see the module
/// docs. `size` is included even though it is implied by the content, because it
/// is the cheapest possible early-out and the one a future refactor is most
/// likely to weaken by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
}

impl StatIdentity {
    /// Whole-nanosecond ctime, for ordering assertions and diagnostics.
    pub fn ctime_ns(&self) -> i128 {
        i128::from(self.ctime_sec) * 1_000_000_000 + i128::from(self.ctime_nsec)
    }

    /// Whole-nanosecond mtime, for ordering assertions and diagnostics.
    pub fn mtime_ns(&self) -> i128 {
        i128::from(self.mtime_sec) * 1_000_000_000 + i128::from(self.mtime_nsec)
    }
}

/// Capture the stat identity of a **regular file**.
///
/// `None` for anything that is not a regular file — symlinks and directories are
/// classified by other arms and must never take this path — and on any platform
/// without POSIX inode metadata. Both are fail-closed: no identity, no fast path.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn stat_identity(metadata: &std::fs::Metadata) -> Option<StatIdentity> {
    use std::os::unix::fs::MetadataExt;

    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    Some(StatIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        size: metadata.len(),
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn stat_identity(_metadata: &std::fs::Metadata) -> Option<StatIdentity> {
    None
}

/// One memoized `UpToDate` verdict. Plain old data, 80 bytes, no heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessRecord {
    /// Local identity at the moment the verdict was proved.
    pub stat: StatIdentity,
    /// Every other classifier input at the moment the verdict was proved.
    pub inputs: InputFingerprint,
    /// Unix seconds at proof time, for the jittered TTL.
    pub verified_at: u64,
}

/// The complete, closed condition under which a memoized `UpToDate` verdict may
/// be replayed.
///
/// Deliberately a free function over plain data: it is the safety-critical
/// predicate and it is unit-testable with no filesystem, no `Operator`, and no
/// tokio runtime.
pub fn memo_still_holds(
    record: &FreshnessRecord,
    stat: StatIdentity,
    inputs: InputFingerprint,
    now: u64,
) -> bool {
    // The local bytes, approximated by the full stat identity. `ctime` is what
    // makes this hold against a forged mtime.
    if record.stat != stat {
        return false;
    }
    // Cheapest independent early-out, kept explicit so a refactor of
    // `StatIdentity`'s `PartialEq` cannot silently drop it.
    if record.stat.size != stat.size {
        return false;
    }
    // Every other classifier input.
    if record.inputs != inputs {
        return false;
    }
    // A record from the future is a clock-step artifact; refuse it rather than
    // let it outlive its TTL by the size of the step.
    if record.verified_at > now {
        return false;
    }
    true
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<PathFingerprint, FreshnessRecord>,
    /// Keys this cycle actually classified, so `retain_touched` can drop records
    /// for paths that have left the corpus without the caller paying a second
    /// canonicalization pass over the whole corpus just to build a live set.
    touched: HashSet<PathFingerprint>,
    dirty: bool,
    hits: u64,
    misses: u64,
    /// Installs refused because the entry cap was reached.
    capped: u64,
}

/// Hit/miss counters for one process lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FreshnessStats {
    pub hits: u64,
    pub misses: u64,
    pub capped: u64,
    pub entries: usize,
}

/// A pure cache of converged `UpToDate` verdicts.
///
/// Interior mutability is deliberate: `reconcile()` holds `&ReconcileConfig`
/// throughout planning and must not be forced to `&mut`. Nothing here is
/// authority for anything — every method may lose its data at any moment and the
/// only consequence is a slow cycle.
#[derive(Debug)]
pub struct FreshnessCache {
    /// Sidecar JSON path. `None` keeps the cache purely in-memory, which is all
    /// a long-lived daemon needs.
    path: Option<PathBuf>,
    /// Records older than this (plus a per-path jitter) are re-proved.
    max_age_secs: u64,
    /// Hard bound on resident records.
    max_entries: usize,
    inner: RwLock<Inner>,
}

impl FreshnessCache {
    /// In-memory only — the right shape for the daemon's reconcile loop, which
    /// keeps the memo warm across cycles inside one process.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            max_entries: DEFAULT_MAX_ENTRIES,
            inner: RwLock::new(Inner::default()),
        }
    }

    /// Open (or start empty at) a JSON sidecar.
    ///
    /// Never fails. A missing, unreadable, truncated, or schema-mismatched
    /// sidecar yields an empty cache: this is a cache, and the fallback is
    /// exactly today's behavior.
    pub fn open(path: &Path) -> Self {
        let entries = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<OnDisk>(&bytes) {
                Ok(disk) if disk.schema == FRESHNESS_SCHEMA => disk.entries,
                Ok(disk) => {
                    debug!(
                        path = %path.display(),
                        found = %disk.schema,
                        expected = FRESHNESS_SCHEMA,
                        "freshness sidecar schema mismatch; starting empty"
                    );
                    HashMap::new()
                }
                Err(error) => {
                    debug!(
                        path = %path.display(),
                        error = %error,
                        "freshness sidecar is unreadable; starting empty"
                    );
                    HashMap::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                debug!(
                    path = %path.display(),
                    error = %error,
                    "freshness sidecar could not be read; starting empty"
                );
                HashMap::new()
            }
        };
        Self {
            path: Some(path.to_path_buf()),
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            max_entries: DEFAULT_MAX_ENTRIES,
            inner: RwLock::new(Inner {
                entries,
                ..Inner::default()
            }),
        }
    }

    /// Override the re-verification interval. `0` expires every record
    /// immediately, which is a second way to spell `--paranoid`.
    pub fn with_max_age_secs(mut self, max_age_secs: u64) -> Self {
        self.max_age_secs = max_age_secs;
        self
    }

    /// Override the resident-record cap.
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Sidecar path for a state-cache path: `state.json` -> `state.freshness.json`.
    ///
    /// Placing it beside the state cache means the existing `StateFileLock` /
    /// `lock_explicit_state_cache` serialization already covers it.
    pub fn sidecar_path_for_state(state_path: &Path) -> PathBuf {
        let mut file_name = state_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "state".to_string());
        file_name.push_str(".freshness.json");
        state_path.with_file_name(file_name)
    }

    /// Per-path expiry phase, so a corpus installed in one cycle does not expire
    /// in one cycle. Uniform over `[max_age/2, max_age)`.
    fn ttl_secs_for(&self, key: PathFingerprint) -> u64 {
        if self.max_age_secs == 0 {
            return 0;
        }
        let half = (self.max_age_secs / 2).max(1);
        let raw = u64::from_le_bytes([
            key.0[0], key.0[1], key.0[2], key.0[3], key.0[4], key.0[5], key.0[6], key.0[7],
        ]);
        half + (raw % half)
    }

    fn read_inner(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn write_inner(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Look up a record, applying the jittered TTL. Returns a copy so the lock
    /// is not held across the caller's comparison.
    pub fn lookup(&self, key: PathFingerprint, now: u64) -> Option<FreshnessRecord> {
        let ttl = self.ttl_secs_for(key);
        let inner = self.read_inner();
        let record = inner.entries.get(&key)?;
        if now.saturating_sub(record.verified_at) >= ttl {
            return None;
        }
        Some(*record)
    }

    /// Install a proved verdict, unless the cap is already reached.
    ///
    /// Refreshing a key that is already present is always allowed — it does not
    /// grow the map, and refusing it would strand a hot path at a stale TTL.
    pub fn record(&self, key: PathFingerprint, record: FreshnessRecord) {
        let mut inner = self.write_inner();
        if inner.entries.len() >= self.max_entries && !inner.entries.contains_key(&key) {
            inner.capped += 1;
            return;
        }
        inner.entries.insert(key, record);
        inner.dirty = true;
    }

    /// Drop a record. Not required for correctness (see the module docs on
    /// structural invalidation) — available for callers that want to be explicit.
    pub fn invalidate(&self, key: PathFingerprint) {
        let mut inner = self.write_inner();
        if inner.entries.remove(&key).is_some() {
            inner.dirty = true;
        }
    }

    /// Record a replayed verdict for `key`, and mark the key live for this
    /// cycle's GC.
    pub fn note_hit(&self, key: PathFingerprint) {
        let mut inner = self.write_inner();
        inner.hits += 1;
        inner.touched.insert(key);
    }

    /// Record a fallthrough to the full comparison for `key`, and mark the key
    /// live for this cycle's GC. A miss is still a live path.
    pub fn note_miss(&self, key: PathFingerprint) {
        let mut inner = self.write_inner();
        inner.misses += 1;
        inner.touched.insert(key);
    }

    pub fn stats(&self) -> FreshnessStats {
        let inner = self.read_inner();
        FreshnessStats {
            hits: inner.hits,
            misses: inner.misses,
            capped: inner.capped,
            entries: inner.entries.len(),
        }
    }

    /// Drop records for every key this cycle did not classify, then start a
    /// fresh cycle. Call once per reconcile pass, after planning.
    ///
    /// Keeps the memo bounded by the live corpus rather than by history, at the
    /// cost of nothing: the touch set is populated by the lookups the classifier
    /// was already doing.
    pub fn retain_touched(&self) {
        let mut inner = self.write_inner();
        let touched = std::mem::take(&mut inner.touched);
        let before = inner.entries.len();
        inner.entries.retain(|key, _| touched.contains(key));
        if inner.entries.len() != before {
            inner.dirty = true;
        }
    }

    /// Best-effort durable write. A failure is logged and swallowed: losing the
    /// sidecar costs one slow cycle, and failing the reconcile over a cache
    /// would be a far worse trade.
    pub fn flush_best_effort(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let snapshot = {
            let mut inner = self.write_inner();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            inner.entries.clone()
        };
        let disk = OnDisk {
            schema: FRESHNESS_SCHEMA.to_string(),
            entries: snapshot,
        };
        if let Err(error) = write_json_atomic(path, &disk) {
            warn!(
                path = %path.display(),
                error = %error,
                "could not persist the reconcile freshness sidecar; the next run re-proves every path"
            );
        }
    }
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    schema: String,
    entries: HashMap<PathFingerprint, FreshnessRecord>,
}

fn write_json_atomic(path: &Path, disk: &OnDisk) -> anyhow::Result<()> {
    use std::io::Write as _;

    let bytes = serde_json::to_vec(disk)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(size: u64, mtime: (i64, i64), ctime: (i64, i64)) -> StatIdentity {
        StatIdentity {
            dev: 1,
            ino: 42,
            size,
            mtime_sec: mtime.0,
            mtime_nsec: mtime.1,
            ctime_sec: ctime.0,
            ctime_nsec: ctime.1,
        }
    }

    fn baseline_inputs(clock: &VectorClock) -> FreshnessInputs<'_> {
        FreshnessInputs {
            canonical_key: "/estate/doc.txt",
            rel_path: "doc.txt",
            remote_prefix: "data",
            tracked_blake3: "aaaa",
            tracked_vclock: clock,
            remote_manifest_hash: "mmmm",
            remote_size: 41,
            device_id: "neo",
        }
    }

    fn record_for(stat: StatIdentity, inputs: &FreshnessInputs<'_>) -> FreshnessRecord {
        FreshnessRecord {
            stat,
            inputs: InputFingerprint::of(inputs),
            verified_at: 1_000,
        }
    }

    #[test]
    fn unchanged_pair_replays_the_memo() {
        let stat = identity(41, (100, 5), (100, 5));
        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(memo_still_holds(
            &record_for(stat, &inputs),
            stat,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    /// THE property. A same-size in-place rewrite with the mtime forged back to
    /// its old value is exactly the `(size, mtime-seconds)` blind spot reconcile
    /// documents at its push execute site — a 41-byte `git commit` ref rewrite.
    /// ctime is what catches it, so the gate must refuse the memo.
    #[test]
    fn forged_mtime_same_size_is_not_skipped_because_ctime_moved() {
        let before = identity(41, (100, 5), (100, 5));
        // Content rewritten, size identical, mtime restored byte-for-byte.
        let after = identity(41, (100, 5), (100, 9));
        assert_eq!(before.size, after.size);
        assert_eq!(before.mtime_ns(), after.mtime_ns());
        assert!(after.ctime_ns() > before.ctime_ns());

        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(!memo_still_holds(
            &record_for(before, &inputs),
            after,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    /// Same second, same size, same nanosecond mtime — the exact shape the
    /// engine's `needs_sync` quick check is blind to.
    #[test]
    fn same_second_same_size_rewrite_is_not_skipped() {
        let before = identity(41, (1_700_000_000, 0), (1_700_000_000, 0));
        let after = identity(41, (1_700_000_000, 0), (1_700_000_000, 1));
        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(!memo_still_holds(
            &record_for(before, &inputs),
            after,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    /// A rewrite-via-rename gets a new inode even if every timestamp is
    /// restored — the second brake named in the module docs.
    #[test]
    fn rename_replacement_is_not_skipped_even_with_every_timestamp_forged() {
        let before = identity(41, (100, 5), (100, 5));
        let mut after = before;
        after.ino = 43;
        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(!memo_still_holds(
            &record_for(before, &inputs),
            after,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    /// A file that moved to another filesystem with everything else identical.
    #[test]
    fn different_device_number_is_not_skipped() {
        let before = identity(41, (100, 5), (100, 5));
        let mut after = before;
        after.dev = 2;
        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(!memo_still_holds(
            &record_for(before, &inputs),
            after,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    /// Every non-stat classifier input must be covered by the fingerprint:
    /// changing any one of them alone must break the memo. This is the test that
    /// keeps the compact representation honest — a field dropped from
    /// `InputFingerprint::of` fails here immediately.
    #[test]
    fn every_classifier_input_is_covered_by_the_fingerprint() {
        let stat = identity(41, (100, 5), (100, 5));
        let clock = VectorClock::default();
        let baseline = baseline_inputs(&clock);
        let record = record_for(stat, &baseline);

        let mut ticked = VectorClock::default();
        ticked.tick("neo");

        let mutations: Vec<(&str, FreshnessInputs<'_>)> = vec![
            (
                "canonical_key",
                FreshnessInputs {
                    canonical_key: "/estate/other.txt",
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "rel_path",
                FreshnessInputs {
                    rel_path: "other.txt",
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "remote_prefix",
                FreshnessInputs {
                    remote_prefix: "other-root",
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "tracked_blake3",
                FreshnessInputs {
                    tracked_blake3: "bbbb",
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "tracked_vclock",
                FreshnessInputs {
                    tracked_vclock: &ticked,
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "remote_manifest_hash",
                FreshnessInputs {
                    remote_manifest_hash: "nnnn",
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "remote_size",
                FreshnessInputs {
                    remote_size: 42,
                    ..baseline_inputs(&clock)
                },
            ),
            (
                "device_id",
                FreshnessInputs {
                    device_id: "honey",
                    ..baseline_inputs(&clock)
                },
            ),
        ];

        for (name, mutated) in mutations {
            assert!(
                !memo_still_holds(&record, stat, InputFingerprint::of(&mutated), 1_001),
                "changing `{name}` must break the memo"
            );
        }
    }

    /// Length-prefixed field mixing: two different splits of the same
    /// concatenated bytes must not collide.
    #[test]
    fn fingerprint_fields_are_length_delimited() {
        let clock = VectorClock::default();
        let left = InputFingerprint::of(&FreshnessInputs {
            tracked_blake3: "ab",
            remote_manifest_hash: "cd",
            ..baseline_inputs(&clock)
        });
        let right = InputFingerprint::of(&FreshnessInputs {
            tracked_blake3: "a",
            remote_manifest_hash: "bcd",
            ..baseline_inputs(&clock)
        });
        assert_ne!(left, right);
    }

    #[test]
    fn record_from_the_future_is_not_skipped() {
        let stat = identity(41, (100, 5), (100, 5));
        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        let mut record = record_for(stat, &inputs);
        record.verified_at = 5_000;
        assert!(!memo_still_holds(
            &record,
            stat,
            InputFingerprint::of(&inputs),
            1_001
        ));
    }

    #[test]
    fn ttl_expiry_forces_a_full_re_proof() {
        let cache = FreshnessCache::in_memory().with_max_age_secs(1_000);
        let key = PathFingerprint::of("/estate/doc.txt");
        let clock = VectorClock::default();
        let stat = identity(41, (100, 5), (100, 5));
        cache.record(key, record_for(stat, &baseline_inputs(&clock)));
        // Inside the minimum half-window every record is still live.
        assert!(cache.lookup(key, 1_000 + 499).is_some());
        // Past the maximum window no record survives.
        assert!(cache.lookup(key, 1_000 + 1_000).is_none());
    }

    #[test]
    fn ttl_jitter_spreads_expiry_across_the_window() {
        let cache = FreshnessCache::in_memory().with_max_age_secs(1_000);
        let mut seen = HashSet::new();
        for index in 0..64 {
            seen.insert(cache.ttl_secs_for(PathFingerprint::of(&format!("/estate/path/{index}"))));
        }
        // A constant TTL would collapse 64 paths into one expiry cycle.
        assert!(seen.len() > 8, "expiry phases collapsed: {seen:?}");
        assert!(seen.iter().all(|ttl| (500u64..1_000u64).contains(ttl)));
    }

    #[test]
    fn zero_max_age_disables_the_memo_entirely() {
        let cache = FreshnessCache::in_memory().with_max_age_secs(0);
        let key = PathFingerprint::of("/estate/doc.txt");
        let clock = VectorClock::default();
        cache.record(
            key,
            record_for(identity(41, (100, 5), (100, 5)), &baseline_inputs(&clock)),
        );
        assert!(cache.lookup(key, 1_000).is_none());
    }

    #[test]
    fn retain_touched_drops_paths_that_left_the_corpus() {
        let cache = FreshnessCache::in_memory();
        let clock = VectorClock::default();
        let stat = identity(41, (100, 5), (100, 5));
        let live = PathFingerprint::of("/estate/a");
        let gone = PathFingerprint::of("/estate/b");
        cache.record(live, record_for(stat, &baseline_inputs(&clock)));
        cache.record(gone, record_for(stat, &baseline_inputs(&clock)));
        // Only `/estate/a` was classified this cycle; a miss counts as live.
        cache.note_miss(live);
        cache.retain_touched();
        assert_eq!(cache.stats().entries, 1);
        assert!(cache.lookup(live, 1_000).is_some());
        assert!(cache.lookup(gone, 1_000).is_none());
        // The touch set resets, so a cycle that classifies nothing empties it.
        cache.retain_touched();
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn entry_cap_stops_growth_but_still_refreshes_known_paths() {
        let cache = FreshnessCache::in_memory().with_max_entries(1);
        let clock = VectorClock::default();
        let stat = identity(41, (100, 5), (100, 5));
        let first = PathFingerprint::of("/estate/a");
        let second = PathFingerprint::of("/estate/b");

        cache.record(first, record_for(stat, &baseline_inputs(&clock)));
        cache.record(second, record_for(stat, &baseline_inputs(&clock)));
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().capped, 1);
        assert!(cache.lookup(second, 1_001).is_none());

        // Refreshing a resident key is not growth, so it is always allowed.
        let mut refreshed = record_for(stat, &baseline_inputs(&clock));
        refreshed.verified_at = 2_000;
        cache.record(first, refreshed);
        assert_eq!(cache.lookup(first, 2_001).unwrap().verified_at, 2_000);
        assert_eq!(cache.stats().capped, 1);
    }

    #[test]
    fn sidecar_round_trips_and_ignores_a_foreign_schema() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = FreshnessCache::sidecar_path_for_state(&dir.path().join("state.json"));
        assert_eq!(sidecar.file_name().unwrap(), "state.freshness.json");

        let clock = VectorClock::default();
        let key = PathFingerprint::of("/estate/doc.txt");
        {
            let cache = FreshnessCache::open(&sidecar);
            cache.record(
                key,
                record_for(identity(41, (100, 5), (100, 5)), &baseline_inputs(&clock)),
            );
            cache.flush_best_effort();
        }
        let reopened = FreshnessCache::open(&sidecar);
        assert!(reopened.lookup(key, 1_001).is_some());

        std::fs::write(&sidecar, br#"{"schema":"tcfs-freshness-v0","entries":{}}"#).unwrap();
        assert_eq!(FreshnessCache::open(&sidecar).stats().entries, 0);

        std::fs::write(&sidecar, b"not json at all").unwrap();
        assert_eq!(FreshnessCache::open(&sidecar).stats().entries, 0);
    }

    #[test]
    fn missing_sidecar_opens_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FreshnessCache::open(&dir.path().join("absent.freshness.json"));
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn fingerprint_hex_round_trips() {
        let key = PathFingerprint::of("/estate/doc.txt");
        let text = serde_json::to_string(&key).unwrap();
        assert_eq!(text.len(), 34, "32 hex characters plus quotes: {text}");
        assert_eq!(serde_json::from_str::<PathFingerprint>(&text).unwrap(), key);
        assert!(serde_json::from_str::<PathFingerprint>("\"zz\"").is_err());
    }

    /// End-to-end on a real file: rewrite the content in place at the same
    /// length, then forge mtime back with `utimensat`. The kernel bumps ctime for
    /// the forge itself, so the identity must differ and the memo must not hold.
    /// This is the live counterpart of
    /// `forged_mtime_same_size_is_not_skipped_because_ctime_moved`.
    ///
    /// A failure here is real information, not flake: it would mean the
    /// filesystem under the test cannot distinguish a rewrite from a forgery,
    /// and the memo is genuinely unsafe there. Both filesystems this ships on
    /// (APFS, ext4 with 256-byte inodes) carry nanosecond ctime.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_forged_mtime_rewrite_still_moves_ctime() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ref-head");
        // 41 bytes: the exact shape of a `.git` branch-head ref.
        std::fs::write(&path, vec![b'a'; 41]).unwrap();
        let before = stat_identity(&std::fs::symlink_metadata(&path).unwrap()).unwrap();

        std::fs::write(&path, vec![b'b'; 41]).unwrap();

        // Forge mtime back to its pre-rewrite value, atime untouched.
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT as _,
            },
            libc::timespec {
                tv_sec: before.mtime_sec as libc::time_t,
                tv_nsec: before.mtime_nsec as _,
            },
        ];
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a NUL-terminated path that outlives the call and
        // `times` is the two-element `timespec` array utimensat expects.
        let rc = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                c_path.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(
            rc,
            0,
            "utimensat failed: {}",
            std::io::Error::last_os_error()
        );

        let after = stat_identity(&std::fs::symlink_metadata(&path).unwrap()).unwrap();

        // The forgery succeeded on exactly the fields a naive gate would trust.
        assert_eq!(after.size, before.size, "size must be unchanged");
        assert_eq!(
            after.mtime_ns(),
            before.mtime_ns(),
            "mtime must be forged back"
        );
        assert_eq!(after.ino, before.ino, "an in-place rewrite keeps the inode");
        // And ctime is what refuses it.
        assert!(
            after.ctime_ns() > before.ctime_ns(),
            "ctime must advance across a rewrite + utimensat (before={}, after={})",
            before.ctime_ns(),
            after.ctime_ns()
        );

        let clock = VectorClock::default();
        let inputs = baseline_inputs(&clock);
        assert!(
            !memo_still_holds(
                &record_for(before, &inputs),
                after,
                InputFingerprint::of(&inputs),
                1_001
            ),
            "a same-size rewrite with a forged mtime must never be skipped"
        );
    }
}
