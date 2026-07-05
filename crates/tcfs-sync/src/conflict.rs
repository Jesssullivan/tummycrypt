//! Vector clock based conflict detection and resolution for multi-machine sync.
//!
//! Each device maintains a vector clock that tracks the logical ordering of
//! operations across machines. When two devices modify the same file concurrently,
//! the vector clocks allow us to detect the conflict rather than silently overwriting.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

// ── Vector Clock ──────────────────────────────────────────────────────────────

/// A vector clock tracking logical timestamps per device.
///
/// Provides a partial ordering on events: if clock A dominates clock B
/// (all entries in A >= B, at least one strictly greater), then A happened
/// after B. If neither dominates, the events are concurrent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorClock {
    pub clocks: BTreeMap<String, u64>,
}

impl VectorClock {
    /// Create a new empty vector clock.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the clock for the given device.
    pub fn tick(&mut self, device_id: &str) {
        let entry = self.clocks.entry(device_id.to_string()).or_insert(0);
        *entry += 1;
    }

    /// Get the clock value for a device (0 if not present).
    pub fn get(&self, device_id: &str) -> u64 {
        self.clocks.get(device_id).copied().unwrap_or(0)
    }

    /// Merge another vector clock into this one (pointwise max).
    pub fn merge(&mut self, other: &VectorClock) {
        for (device, &ts) in &other.clocks {
            let entry = self.clocks.entry(device.clone()).or_insert(0);
            *entry = (*entry).max(ts);
        }
    }

    /// Compare two vector clocks, returning their partial ordering.
    ///
    /// Returns `Some(Ordering)` if one dominates the other, `None` if concurrent.
    pub fn partial_cmp_vc(&self, other: &VectorClock) -> Option<Ordering> {
        let all_keys: BTreeMap<&str, ()> = self
            .clocks
            .keys()
            .chain(other.clocks.keys())
            .map(|k| (k.as_str(), ()))
            .collect();

        let mut has_greater = false;
        let mut has_less = false;

        for key in all_keys.keys() {
            let a = self.get(key);
            let b = other.get(key);
            match a.cmp(&b) {
                Ordering::Greater => has_greater = true,
                Ordering::Less => has_less = true,
                Ordering::Equal => {}
            }
            if has_greater && has_less {
                return None; // concurrent
            }
        }

        match (has_greater, has_less) {
            (true, false) => Some(Ordering::Greater),
            (false, true) => Some(Ordering::Less),
            (false, false) => Some(Ordering::Equal),
            (true, true) => None, // unreachable due to early return above
        }
    }

    /// Check if two vector clocks are concurrent (neither dominates).
    pub fn is_concurrent(&self, other: &VectorClock) -> bool {
        self.partial_cmp_vc(other).is_none()
    }
}

// ── Sync Outcome ──────────────────────────────────────────────────────────────

/// Result of comparing a local file's state against a remote version.
///
/// This is a short-lived decision value: `compare_clocks` returns it and the
/// caller immediately matches it into a `ReconcileAction`. It is never held in
/// bulk collections, so the size gap between the data-carrying `Conflict`
/// variant and the unit variants is immaterial — boxing `ConflictInfo` here
/// would add an allocation per classification for no benefit and ripple through
/// ~20 match sites across crates. keep-both PR-2 grew `ConflictInfo` by one
/// `Option<String>` (`remote_manifest_key`), nudging it just past clippy's
/// 200-byte `large_enum_variant` threshold; allow it for this transient enum.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SyncOutcome {
    /// Local version is newer — safe to push.
    LocalNewer,
    /// Remote version is newer — should pull before modifying.
    RemoteNewer,
    /// Both versions are identical.
    UpToDate,
    /// Concurrent modifications detected — human/agent decision needed.
    Conflict(ConflictInfo),
}

/// Detailed information about a sync conflict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// Relative path of the conflicting file
    pub rel_path: String,
    /// Local vector clock
    pub local_vclock: VectorClock,
    /// Remote vector clock
    pub remote_vclock: VectorClock,
    /// Local BLAKE3 hash
    pub local_blake3: String,
    /// Remote BLAKE3 hash
    pub remote_blake3: String,
    /// Local device ID
    pub local_device: String,
    /// Remote device ID
    pub remote_device: String,
    /// Unix timestamp when conflict was detected
    pub detected_at: u64,
    /// Number of reconcile cycles that have re-recorded this conflict.
    ///
    /// Bumped at the reconcile record site instead of being overwritten each
    /// cycle, so `tcfs conflicts` can surface a forever-Conflict repo (the
    /// record-only arm that never converges). `serde(default)` keeps older
    /// state caches — written before this field existed — deserializing to 0.
    #[serde(default)]
    pub times_recorded: u64,
    /// Storage key (S3/prefix path) of the remote side's manifest for this
    /// path, captured when the conflict is classified.
    ///
    /// keep-both PR-2 data-model graft: the remote ref blob's manifest key is
    /// only in scope at classification time (`reconcile::outcome_to_action`,
    /// where the remote manifest path is already computed) — NOT at the later
    /// record site, which holds only the local state entry. A future PR-3
    /// resolve verb needs this key to fetch the remote ref SHA directly instead
    /// of depending on the incidental, unversioned `SyncState.remote_path`.
    /// `None` for conflicts recorded before this field existed (old state
    /// caches) and for any path where the key was not captured. `serde(default)`
    /// keeps those older caches deserializing.
    #[serde(default)]
    pub remote_manifest_key: Option<String>,
}

// ── Resolution ────────────────────────────────────────────────────────────────

/// How to resolve a sync conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Keep the local version, overwrite remote.
    KeepLocal,
    /// Keep the remote version, overwrite local.
    KeepRemote,
    /// Keep both: rename the loser as `filename.conflict-{device_id}.ext`.
    KeepBoth,
    /// Defer: mark as unresolved, skip for now.
    Defer,
}

/// Trait for conflict resolution strategies.
pub trait ConflictResolver: Send + Sync {
    fn resolve(&self, conflict: &ConflictInfo) -> Option<Resolution>;
}

/// Automatic resolver: deterministic tie-break using lexicographic device name.
///
/// When two devices concurrently modify a file, the device with the
/// lexicographically smaller name "wins" (keeps its version as the primary).
pub struct AutoResolver;

impl ConflictResolver for AutoResolver {
    fn resolve(&self, conflict: &ConflictInfo) -> Option<Resolution> {
        if conflict.local_device <= conflict.remote_device {
            Some(Resolution::KeepLocal)
        } else {
            Some(Resolution::KeepRemote)
        }
    }
}

/// Compare a local and remote vector clock to produce a SyncOutcome.
pub fn compare_clocks(
    local: &VectorClock,
    remote: &VectorClock,
    local_blake3: &str,
    remote_blake3: &str,
    rel_path: &str,
    local_device: &str,
    remote_device: &str,
) -> SyncOutcome {
    // Content-identical means up-to-date regardless of clocks
    if local_blake3 == remote_blake3 {
        return SyncOutcome::UpToDate;
    }

    match local.partial_cmp_vc(remote) {
        Some(Ordering::Greater) => SyncOutcome::LocalNewer,
        Some(Ordering::Less) => SyncOutcome::RemoteNewer,
        // Equal clock with differing content, or concurrent (incomparable)
        // clocks — both are conflicts with identical ConflictInfo.
        Some(Ordering::Equal) | None => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            SyncOutcome::Conflict(ConflictInfo {
                rel_path: rel_path.to_string(),
                local_vclock: local.clone(),
                remote_vclock: remote.clone(),
                local_blake3: local_blake3.to_string(),
                remote_blake3: remote_blake3.to_string(),
                local_device: local_device.to_string(),
                remote_device: remote_device.to_string(),
                detected_at: now,
                times_recorded: 0,
                // Not in scope here: `compare_clocks` sees only hashes/clocks,
                // not the remote manifest storage key. Populated later at
                // `reconcile::outcome_to_action`, where the remote manifest
                // path is already computed. (keep-both PR-2)
                remote_manifest_key: None,
            })
        }
    }
}

#[cfg(test)]
mod proptest_suite {
    use super::*;
    use proptest::prelude::*;

    fn arb_device_ids() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("[a-z]{1,8}", 1..5)
    }

    fn arb_vclock() -> impl Strategy<Value = VectorClock> {
        arb_device_ids().prop_flat_map(|ids| {
            let len = ids.len();
            prop::collection::vec(0u64..10, len).prop_map(move |vals| {
                let mut vc = VectorClock::new();
                for (id, val) in ids.iter().zip(vals.iter()) {
                    for _ in 0..*val {
                        vc.tick(id);
                    }
                }
                vc
            })
        })
    }

    proptest! {
        #[test]
        fn tick_monotonic(device in "[a-z]{1,8}", n in 1u64..100) {
            let mut vc = VectorClock::new();
            for _ in 0..n {
                let before = vc.get(&device);
                vc.tick(&device);
                prop_assert!(vc.get(&device) == before + 1);
            }
        }

        #[test]
        fn merge_commutative(a in arb_vclock(), b in arb_vclock()) {
            let mut ab = a.clone();
            ab.merge(&b);
            let mut ba = b.clone();
            ba.merge(&a);
            prop_assert_eq!(ab, ba);
        }

        #[test]
        fn merge_idempotent(a in arb_vclock()) {
            let mut merged = a.clone();
            merged.merge(&a);
            prop_assert_eq!(merged, a);
        }

        #[test]
        fn merge_associative(a in arb_vclock(), b in arb_vclock(), c in arb_vclock()) {
            let mut ab_c = a.clone();
            ab_c.merge(&b);
            ab_c.merge(&c);

            let mut a_bc = a.clone();
            let mut bc = b.clone();
            bc.merge(&c);
            a_bc.merge(&bc);

            prop_assert_eq!(ab_c, a_bc);
        }

        #[test]
        fn merge_dominates(a in arb_vclock(), b in arb_vclock()) {
            let mut merged = a.clone();
            merged.merge(&b);
            // merged >= a and merged >= b
            let cmp_a = merged.partial_cmp_vc(&a);
            let cmp_b = merged.partial_cmp_vc(&b);
            prop_assert!(cmp_a == Some(Ordering::Greater) || cmp_a == Some(Ordering::Equal));
            prop_assert!(cmp_b == Some(Ordering::Greater) || cmp_b == Some(Ordering::Equal));
        }

        #[test]
        fn ordering_antisymmetric(a in arb_vclock(), b in arb_vclock()) {
            match (a.partial_cmp_vc(&b), b.partial_cmp_vc(&a)) {
                (Some(Ordering::Greater), Some(Ordering::Less)) => {}
                (Some(Ordering::Less), Some(Ordering::Greater)) => {}
                (Some(Ordering::Equal), Some(Ordering::Equal)) => {}
                (None, None) => {} // concurrent
                (x, y) => prop_assert!(false, "antisymmetry violated: {:?} vs {:?}", x, y),
            }
        }

        #[test]
        fn concurrency_symmetric(a in arb_vclock(), b in arb_vclock()) {
            prop_assert_eq!(a.is_concurrent(&b), b.is_concurrent(&a));
        }

        #[test]
        fn tick_advances(device in "[a-z]{1,8}") {
            let mut a = VectorClock::new();
            let b = a.clone();
            a.tick(&device);
            // After tick, a > b
            prop_assert_eq!(a.partial_cmp_vc(&b), Some(Ordering::Greater));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_increments() {
        let mut vc = VectorClock::new();
        vc.tick("a");
        assert_eq!(vc.get("a"), 1);
        vc.tick("a");
        assert_eq!(vc.get("a"), 2);
    }

    #[test]
    fn test_get_absent() {
        let vc = VectorClock::new();
        assert_eq!(vc.get("nonexistent"), 0);
    }

    #[test]
    fn test_merge_basic() {
        let mut a = VectorClock::new();
        a.tick("x");
        a.tick("x");

        let mut b = VectorClock::new();
        b.tick("y");

        a.merge(&b);
        assert_eq!(a.get("x"), 2);
        assert_eq!(a.get("y"), 1);
    }

    #[test]
    fn test_ordering_equal() {
        let a = VectorClock::new();
        let b = VectorClock::new();
        assert_eq!(a.partial_cmp_vc(&b), Some(Ordering::Equal));
    }

    #[test]
    fn test_ordering_greater() {
        let mut a = VectorClock::new();
        a.tick("x");
        let b = VectorClock::new();
        assert_eq!(a.partial_cmp_vc(&b), Some(Ordering::Greater));
    }

    #[test]
    fn test_ordering_less() {
        let a = VectorClock::new();
        let mut b = VectorClock::new();
        b.tick("x");
        assert_eq!(a.partial_cmp_vc(&b), Some(Ordering::Less));
    }

    #[test]
    fn test_ordering_concurrent() {
        let mut a = VectorClock::new();
        a.tick("x");
        let mut b = VectorClock::new();
        b.tick("y");
        assert!(a.is_concurrent(&b));
    }

    #[test]
    fn test_auto_resolver() {
        let resolver = AutoResolver;
        let info = ConflictInfo {
            rel_path: "test.txt".into(),
            local_vclock: VectorClock::new(),
            remote_vclock: VectorClock::new(),
            local_blake3: "aaa".into(),
            remote_blake3: "bbb".into(),
            local_device: "alpha".into(),
            remote_device: "beta".into(),
            detected_at: 0,
            times_recorded: 0,
            remote_manifest_key: None,
        };
        // "alpha" < "beta" → keep local
        assert_eq!(resolver.resolve(&info), Some(Resolution::KeepLocal));

        let info2 = ConflictInfo {
            local_device: "zeta".into(),
            remote_device: "alpha".into(),
            ..info
        };
        // "zeta" > "alpha" → keep remote
        assert_eq!(resolver.resolve(&info2), Some(Resolution::KeepRemote));
    }

    #[test]
    fn test_compare_clocks_up_to_date() {
        let a = VectorClock::new();
        let b = VectorClock::new();
        match compare_clocks(&a, &b, "hash1", "hash1", "f.txt", "d1", "d2") {
            SyncOutcome::UpToDate => {}
            other => panic!("expected UpToDate, got {other:?}"),
        }
    }

    #[test]
    fn test_compare_clocks_conflict() {
        let mut a = VectorClock::new();
        a.tick("d1");
        let mut b = VectorClock::new();
        b.tick("d2");
        match compare_clocks(&a, &b, "hash_a", "hash_b", "f.txt", "d1", "d2") {
            SyncOutcome::Conflict(info) => {
                assert_eq!(info.rel_path, "f.txt");
                // A freshly recorded conflict starts at zero cycles.
                assert_eq!(info.times_recorded, 0);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn conflict_info_deserializes_without_times_recorded_field() {
        // A state cache written before `times_recorded` existed must still
        // deserialize (serde default → 0), and round-trip cleanly afterwards.
        let legacy = r#"{
            "rel_path": "repo/.git/refs/heads/main",
            "local_vclock": {"clocks": {}},
            "remote_vclock": {"clocks": {}},
            "local_blake3": "aaa",
            "remote_blake3": "bbb",
            "local_device": "neo",
            "remote_device": "honey",
            "detected_at": 1700000000
        }"#;
        let info: ConflictInfo =
            serde_json::from_str(legacy).expect("legacy ConflictInfo must deserialize");
        assert_eq!(info.times_recorded, 0, "missing field must default to 0");
        assert_eq!(info.detected_at, 1_700_000_000);

        // Round-trip: serialize then re-parse preserves the (defaulted) value.
        let bytes = serde_json::to_string(&info).unwrap();
        let reparsed: ConflictInfo = serde_json::from_str(&bytes).unwrap();
        assert_eq!(reparsed.times_recorded, 0);
        assert_eq!(reparsed.rel_path, "repo/.git/refs/heads/main");
    }

    #[test]
    fn conflict_info_serde_roundtrips_without_remote_manifest_key() {
        // keep-both PR-2 back-compat (test c): a state cache written before
        // `remote_manifest_key` existed has no such field. `#[serde(default)]`
        // must deserialize it to None so old caches load unchanged.
        let legacy = r#"{
            "rel_path": "repo/.git/refs/heads/main",
            "local_vclock": {"clocks": {}},
            "remote_vclock": {"clocks": {}},
            "local_blake3": "aaa",
            "remote_blake3": "bbb",
            "local_device": "neo",
            "remote_device": "honey",
            "detected_at": 1700000000,
            "times_recorded": 2
        }"#;
        let info: ConflictInfo =
            serde_json::from_str(legacy).expect("legacy ConflictInfo must deserialize");
        assert_eq!(
            info.remote_manifest_key, None,
            "missing remote_manifest_key must default to None"
        );
        // Unrelated fields still parse correctly.
        assert_eq!(info.times_recorded, 2);

        // A populated key round-trips losslessly.
        let mut with_key = info.clone();
        with_key.remote_manifest_key = Some("data/manifests/deadbeef".to_string());
        let bytes = serde_json::to_string(&with_key).unwrap();
        let reparsed: ConflictInfo = serde_json::from_str(&bytes).unwrap();
        assert_eq!(
            reparsed.remote_manifest_key.as_deref(),
            Some("data/manifests/deadbeef")
        );
    }
}
