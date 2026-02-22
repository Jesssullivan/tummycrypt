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
#[derive(Debug, Clone)]
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
        Some(Ordering::Equal) => {
            // Same clock but different content — treat as conflict
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
            })
        }
        None => {
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
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }
}
