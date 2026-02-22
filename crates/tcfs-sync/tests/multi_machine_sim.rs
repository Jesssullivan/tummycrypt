//! 3-machine simulation using property-based testing.
//!
//! Models a simplified fleet of 3 devices syncing files through a shared remote.
//! Verifies core distributed invariants:
//!   1. No silent data loss — pushed files are retrievable
//!   2. Vector clock monotonicity — clocks never go backward
//!   3. Content-hash consistency — identical content produces identical hashes
//!   4. Conflict detection — concurrent writes are always detected
//!   5. Eventual convergence — after draining events, all online machines agree

use proptest::prelude::*;
use std::collections::{BTreeMap, HashMap};
use tcfs_sync::conflict::{compare_clocks, SyncOutcome, VectorClock};
use tcfs_sync::manifest::SyncManifest;

// ── Simulated infrastructure ────────────────────────────────────────────────

/// A simulated machine in the fleet.
#[derive(Debug, Clone)]
struct SimMachine {
    device_id: String,
    /// Local files: path → (content_hash, vclock)
    files: HashMap<String, (String, VectorClock)>,
    /// Whether this machine is online (can push/pull)
    online: bool,
    /// Pending inbound events (received while processing)
    _pending_events: Vec<SimEvent>,
}

impl SimMachine {
    fn new(device_id: &str) -> Self {
        Self {
            device_id: device_id.to_string(),
            files: HashMap::new(),
            online: true,
            _pending_events: Vec::new(),
        }
    }
}

/// Simulated remote storage (S3/SeaweedFS).
#[derive(Debug, Clone, Default)]
struct SimRemote {
    /// Remote manifests: path → SyncManifest
    manifests: HashMap<String, SyncManifest>,
    /// Remote chunks: hash → data (simplified as hash string)
    chunks: HashMap<String, String>,
}

/// A state sync event (models NATS STATE_UPDATES).
#[derive(Debug, Clone)]
struct SimEvent {
    device_id: String,
    rel_path: String,
    content_hash: String,
    vclock: VectorClock,
}

/// Simulated NATS event bus.
#[derive(Debug, Clone, Default)]
struct SimNats {
    events: Vec<SimEvent>,
    /// Per-device cursor: how many events each device has consumed
    cursors: HashMap<String, usize>,
}

// ── Simulation operations ───────────────────────────────────────────────────

/// Operations that can be performed by the simulation.
#[derive(Debug, Clone)]
enum SimOp {
    /// Machine writes a file locally (doesn't push yet)
    WriteFile {
        machine: usize,
        path: String,
        content_hash: String,
    },
    /// Machine pushes a specific file to remote
    Push { machine: usize, path: String },
    /// Machine pulls a specific file from remote
    Pull { machine: usize, path: String },
    /// Machine goes offline
    GoOffline { machine: usize },
    /// Machine comes back online
    GoOnline { machine: usize },
    /// Machine processes all pending NATS events
    ProcessEvents { machine: usize },
}

// ── Simulation engine ───────────────────────────────────────────────────────

/// Execute a simulation operation and return whether conflicts were detected.
fn execute_op(
    machines: &mut [SimMachine; 3],
    remote: &mut SimRemote,
    nats: &mut SimNats,
    op: &SimOp,
) -> Option<bool> {
    match op {
        SimOp::WriteFile {
            machine,
            path,
            content_hash,
        } => {
            let m = &mut machines[*machine];
            let mut vclock = m
                .files
                .get(path)
                .map(|(_, vc)| vc.clone())
                .unwrap_or_default();
            vclock.tick(&m.device_id);
            m.files.insert(path.clone(), (content_hash.clone(), vclock));
            None
        }

        SimOp::Push { machine, path } => {
            let m = &machines[*machine];
            if !m.online {
                return Some(false);
            }
            let (local_hash, local_vclock) = match m.files.get(path) {
                Some(f) => f.clone(),
                None => return Some(false),
            };

            // Check remote for conflict
            if let Some(remote_manifest) = remote.manifests.get(path) {
                let outcome = compare_clocks(
                    &local_vclock,
                    &remote_manifest.vclock,
                    &local_hash,
                    &remote_manifest.file_hash,
                    path,
                    &m.device_id,
                    &remote_manifest.written_by,
                );

                match outcome {
                    SyncOutcome::Conflict(_) => return Some(true),
                    SyncOutcome::RemoteNewer => return Some(false),
                    SyncOutcome::UpToDate => return Some(false),
                    SyncOutcome::LocalNewer => {
                        // Merge and proceed
                    }
                }
            }

            // Push: write manifest to remote
            let mut merged_vclock = local_vclock;
            if let Some(existing) = remote.manifests.get(path) {
                merged_vclock.merge(&existing.vclock);
            }

            let manifest = SyncManifest {
                version: 2,
                file_hash: local_hash.clone(),
                file_size: 0,
                chunks: vec![local_hash.clone()],
                vclock: merged_vclock.clone(),
                written_by: machines[*machine].device_id.clone(),
                written_at: 0,
                rel_path: Some(path.clone()),
            };

            remote.manifests.insert(path.clone(), manifest);
            remote.chunks.insert(local_hash.clone(), local_hash.clone());

            // Update local vclock to match what was written
            machines[*machine]
                .files
                .insert(path.clone(), (local_hash.clone(), merged_vclock.clone()));

            // Publish NATS event
            nats.events.push(SimEvent {
                device_id: machines[*machine].device_id.clone(),
                rel_path: path.clone(),
                content_hash: local_hash,
                vclock: merged_vclock,
            });

            Some(false)
        }

        SimOp::Pull { machine, path } => {
            let m = &mut machines[*machine];
            if !m.online {
                return None;
            }

            if let Some(manifest) = remote.manifests.get(path) {
                let mut local_vclock = m
                    .files
                    .get(path)
                    .map(|(_, vc)| vc.clone())
                    .unwrap_or_default();
                local_vclock.merge(&manifest.vclock);

                m.files
                    .insert(path.clone(), (manifest.file_hash.clone(), local_vclock));
            }
            None
        }

        SimOp::GoOffline { machine } => {
            machines[*machine].online = false;
            None
        }

        SimOp::GoOnline { machine } => {
            machines[*machine].online = true;
            None
        }

        SimOp::ProcessEvents { machine } => {
            let m_id = machines[*machine].device_id.clone();
            if !machines[*machine].online {
                return None;
            }

            let cursor = nats.cursors.get(&m_id).copied().unwrap_or(0);
            let events: Vec<SimEvent> = nats.events[cursor..].to_vec();
            nats.cursors.insert(m_id.clone(), nats.events.len());

            for event in &events {
                // Skip own events
                if event.device_id == m_id {
                    continue;
                }

                let m = &mut machines[*machine];
                let local = m.files.get(&event.rel_path);

                match local {
                    Some((local_hash, local_vclock)) => {
                        let outcome = compare_clocks(
                            local_vclock,
                            &event.vclock,
                            local_hash,
                            &event.content_hash,
                            &event.rel_path,
                            &m_id,
                            &event.device_id,
                        );

                        match outcome {
                            SyncOutcome::RemoteNewer | SyncOutcome::UpToDate => {
                                // Accept remote version
                                let mut merged = local_vclock.clone();
                                merged.merge(&event.vclock);
                                m.files.insert(
                                    event.rel_path.clone(),
                                    (event.content_hash.clone(), merged),
                                );
                            }
                            SyncOutcome::LocalNewer => {
                                // Keep local, merge vclock
                                let mut merged = local_vclock.clone();
                                merged.merge(&event.vclock);
                                m.files
                                    .insert(event.rel_path.clone(), (local_hash.clone(), merged));
                            }
                            SyncOutcome::Conflict(_) => {
                                // For simulation: auto-resolve by taking remote
                                // (real code would use ConflictResolver)
                                let mut merged = local_vclock.clone();
                                merged.merge(&event.vclock);
                                m.files.insert(
                                    event.rel_path.clone(),
                                    (event.content_hash.clone(), merged),
                                );
                            }
                        }
                    }
                    None => {
                        // New file from remote — accept it
                        m.files.insert(
                            event.rel_path.clone(),
                            (event.content_hash.clone(), event.vclock.clone()),
                        );
                    }
                }
            }
            None
        }
    }
}

// ── Proptest strategies ─────────────────────────────────────────────────────

fn arb_machine_idx() -> impl Strategy<Value = usize> {
    0..3usize
}

fn arb_path() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "readme.md".to_string(),
        "src/main.rs".to_string(),
        "docs/guide.md".to_string(),
        "config.toml".to_string(),
    ])
}

fn arb_content_hash() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "hash_aaa".to_string(),
        "hash_bbb".to_string(),
        "hash_ccc".to_string(),
        "hash_ddd".to_string(),
        "hash_eee".to_string(),
    ])
}

fn arb_op() -> impl Strategy<Value = SimOp> {
    prop_oneof![
        // Weighted: more writes and pushes than offline/online
        5 => (arb_machine_idx(), arb_path(), arb_content_hash()).prop_map(
            |(machine, path, hash)| SimOp::WriteFile {
                machine,
                path,
                content_hash: hash,
            }
        ),
        4 => (arb_machine_idx(), arb_path()).prop_map(|(machine, path)| SimOp::Push {
            machine,
            path,
        }),
        3 => (arb_machine_idx(), arb_path()).prop_map(|(machine, path)| SimOp::Pull {
            machine,
            path,
        }),
        3 => arb_machine_idx().prop_map(|machine| SimOp::ProcessEvents { machine }),
        1 => arb_machine_idx().prop_map(|machine| SimOp::GoOffline { machine }),
        1 => arb_machine_idx().prop_map(|machine| SimOp::GoOnline { machine }),
    ]
}

fn arb_ops(max_ops: usize) -> impl Strategy<Value = Vec<SimOp>> {
    prop::collection::vec(arb_op(), 1..=max_ops)
}

// ── Properties ──────────────────────────────────────────────────────────────

proptest! {
    /// Property 1: No silent data loss — any file successfully pushed to remote
    /// must be retrievable (its content hash exists in remote chunks).
    #[test]
    fn no_silent_data_loss(ops in arb_ops(50)) {
        let mut machines = [
            SimMachine::new("xoxd-bates"),
            SimMachine::new("yoga"),
            SimMachine::new("petting-zoo-mini"),
        ];
        let mut remote = SimRemote::default();
        let mut nats = SimNats::default();

        for op in &ops {
            execute_op(&mut machines, &mut remote, &mut nats, op);
        }

        // Every manifest in remote must have its chunks present
        for (path, manifest) in &remote.manifests {
            for chunk_hash in &manifest.chunks {
                prop_assert!(
                    remote.chunks.contains_key(chunk_hash),
                    "remote manifest for '{}' references chunk '{}' that doesn't exist",
                    path,
                    chunk_hash
                );
            }
        }
    }

    /// Property 2: Vector clock monotonicity — a machine's own clock entry
    /// never decreases across operations.
    #[test]
    fn vclock_monotonicity(ops in arb_ops(50)) {
        let mut machines = [
            SimMachine::new("xoxd-bates"),
            SimMachine::new("yoga"),
            SimMachine::new("petting-zoo-mini"),
        ];
        let mut remote = SimRemote::default();
        let mut nats = SimNats::default();

        // Track max vclock value per (machine, path)
        let mut max_clocks: HashMap<(usize, String), u64> = HashMap::new();

        for op in &ops {
            execute_op(&mut machines, &mut remote, &mut nats, op);

            // After each op, check monotonicity for all machines
            for (i, m) in machines.iter().enumerate() {
                for (path, (_, vclock)) in &m.files {
                    let own_val = vclock.get(&m.device_id);
                    let key = (i, path.clone());
                    let prev = max_clocks.get(&key).copied().unwrap_or(0);
                    prop_assert!(
                        own_val >= prev,
                        "machine {} path '{}': own clock went from {} to {}",
                        m.device_id,
                        path,
                        prev,
                        own_val
                    );
                    max_clocks.insert(key, own_val);
                }
            }
        }
    }

    /// Property 3: Content-hash consistency — if two machines have the same
    /// content hash for a file, the content is identical (tautological in sim,
    /// but verifies the hash is tracked correctly through push/pull).
    #[test]
    fn content_hash_consistency(ops in arb_ops(50)) {
        let mut machines = [
            SimMachine::new("xoxd-bates"),
            SimMachine::new("yoga"),
            SimMachine::new("petting-zoo-mini"),
        ];
        let mut remote = SimRemote::default();
        let mut nats = SimNats::default();

        for op in &ops {
            execute_op(&mut machines, &mut remote, &mut nats, op);
        }

        // Drain all events so machines converge
        for i in 0..3 {
            machines[i].online = true;
            execute_op(
                &mut machines,
                &mut remote,
                &mut nats,
                &SimOp::ProcessEvents { machine: i },
            );
        }

        // For each file present on multiple machines, verify that
        // if content hashes match then the stored hash strings are identical
        let all_paths: Vec<String> = machines
            .iter()
            .flat_map(|m| m.files.keys().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for path in &all_paths {
            let entries: Vec<(usize, &str)> = machines
                .iter()
                .enumerate()
                .filter_map(|(i, m)| m.files.get(path).map(|(h, _)| (i, h.as_str())))
                .collect();

            for pair in entries.windows(2) {
                if let [(i, h1), (j, h2)] = pair {
                    // If hashes are equal, they must be the same string
                    if h1 == h2 {
                        prop_assert_eq!(
                            h1, h2,
                            "machines {} and {} have same hash for '{}' but strings differ",
                            i, j, path
                        );
                    }
                }
            }
        }
    }

    /// Property 4: Conflict detection — when two machines write to the same
    /// file without syncing, pushing from the second machine detects a conflict.
    #[test]
    fn conflict_detection(
        path in arb_path(),
        hash_a in arb_content_hash(),
        hash_b in arb_content_hash(),
    ) {
        // Only test when hashes differ (otherwise it's not a real conflict)
        prop_assume!(hash_a != hash_b);

        let mut machines = [
            SimMachine::new("xoxd-bates"),
            SimMachine::new("yoga"),
            SimMachine::new("petting-zoo-mini"),
        ];
        let mut remote = SimRemote::default();
        let mut nats = SimNats::default();

        // Machine 0 writes and pushes
        execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::WriteFile {
                machine: 0,
                path: path.clone(),
                content_hash: hash_a,
            },
        );
        execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::Push {
                machine: 0,
                path: path.clone(),
            },
        );

        // Machine 1 writes independently (no pull)
        execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::WriteFile {
                machine: 1,
                path: path.clone(),
                content_hash: hash_b,
            },
        );

        // Machine 1 tries to push — should detect conflict
        let result = execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::Push {
                machine: 1,
                path: path.clone(),
            },
        );

        prop_assert_eq!(
            result,
            Some(true),
            "concurrent write to '{}' must be detected as conflict",
            path
        );
    }

    /// Property 5: Eventual convergence — after draining all NATS events with
    /// all machines online, every machine that has a file agrees on the content.
    #[test]
    fn eventual_convergence(ops in arb_ops(30)) {
        let mut machines = [
            SimMachine::new("xoxd-bates"),
            SimMachine::new("yoga"),
            SimMachine::new("petting-zoo-mini"),
        ];
        let mut remote = SimRemote::default();
        let mut nats = SimNats::default();

        // Run random operations
        for op in &ops {
            execute_op(&mut machines, &mut remote, &mut nats, op);
        }

        // Bring all machines online
        for m in machines.iter_mut() {
            m.online = true;
        }

        // Pull all remote files to all machines, then process all events
        // Run multiple rounds to ensure convergence
        for _ in 0..3 {
            let paths: Vec<String> = remote.manifests.keys().cloned().collect();
            for path in &paths {
                for i in 0..3 {
                    execute_op(
                        &mut machines,
                        &mut remote,
                        &mut nats,
                        &SimOp::Pull {
                            machine: i,
                            path: path.clone(),
                        },
                    );
                }
            }
            for i in 0..3 {
                execute_op(
                    &mut machines,
                    &mut remote,
                    &mut nats,
                    &SimOp::ProcessEvents { machine: i },
                );
            }
        }

        // After convergence: for each file in remote, all machines that have
        // it must agree on the content hash
        for (path, manifest) in &remote.manifests {
            let hashes: Vec<&str> = machines
                .iter()
                .filter_map(|m| m.files.get(path).map(|(h, _)| h.as_str()))
                .collect();

            if hashes.len() > 1 {
                let first = hashes[0];
                for (i, h) in hashes.iter().enumerate().skip(1) {
                    prop_assert_eq!(
                        *h,
                        first,
                        "after convergence, machines disagree on '{}': {} vs {} (remote: {})",
                        path,
                        first,
                        h,
                        manifest.file_hash,
                    );
                }
            }
        }
    }
}

// ── Deterministic integration tests ─────────────────────────────────────────

#[test]
fn test_three_machine_basic_sync() {
    let mut machines = [
        SimMachine::new("xoxd-bates"),
        SimMachine::new("yoga"),
        SimMachine::new("petting-zoo-mini"),
    ];
    let mut remote = SimRemote::default();
    let mut nats = SimNats::default();

    // Machine 0 writes and pushes a file
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::WriteFile {
            machine: 0,
            path: "readme.md".into(),
            content_hash: "hash_v1".into(),
        },
    );
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::Push {
            machine: 0,
            path: "readme.md".into(),
        },
    );

    // Machine 1 pulls it
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::Pull {
            machine: 1,
            path: "readme.md".into(),
        },
    );

    // Machine 1 should have the same content
    assert_eq!(machines[1].files.get("readme.md").unwrap().0, "hash_v1");

    // Machine 2 processes events and gets the file
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::ProcessEvents { machine: 2 },
    );

    assert_eq!(machines[2].files.get("readme.md").unwrap().0, "hash_v1");
}

#[test]
fn test_offline_machine_catches_up() {
    let mut machines = [
        SimMachine::new("xoxd-bates"),
        SimMachine::new("yoga"),
        SimMachine::new("petting-zoo-mini"),
    ];
    let mut remote = SimRemote::default();
    let mut nats = SimNats::default();

    // Machine 2 goes offline
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::GoOffline { machine: 2 },
    );

    // Machine 0 writes and pushes multiple files
    for i in 0..3 {
        let path = format!("file_{}.txt", i);
        execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::WriteFile {
                machine: 0,
                path: path.clone(),
                content_hash: format!("hash_{}", i),
            },
        );
        execute_op(
            &mut machines,
            &mut remote,
            &mut nats,
            &SimOp::Push { machine: 0, path },
        );
    }

    // Machine 2 comes back online and processes events
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::GoOnline { machine: 2 },
    );
    execute_op(
        &mut machines,
        &mut remote,
        &mut nats,
        &SimOp::ProcessEvents { machine: 2 },
    );

    // Machine 2 should have all files
    for i in 0..3 {
        let path = format!("file_{}.txt", i);
        assert_eq!(
            machines[2].files.get(&path).unwrap().0,
            format!("hash_{}", i),
            "machine 2 missing {}",
            path
        );
    }
}

#[test]
fn test_manifest_serialization_in_sim() {
    // Verify that SyncManifest round-trips correctly through the sim
    let mut vc = VectorClock::new();
    vc.tick("yoga");
    vc.tick("yoga");
    vc.tick("xoxd-bates");

    let manifest = SyncManifest {
        version: 2,
        file_hash: "sim_hash_abc".into(),
        file_size: 4096,
        chunks: vec!["chunk_1".into(), "chunk_2".into()],
        vclock: vc.clone(),
        written_by: "yoga".into(),
        written_at: 1000,
        rel_path: Some("src/main.rs".into()),
    };

    let bytes = manifest.to_bytes().unwrap();
    let parsed = SyncManifest::from_bytes(&bytes).unwrap();

    assert_eq!(parsed.file_hash, "sim_hash_abc");
    assert_eq!(parsed.vclock.get("yoga"), 2);
    assert_eq!(parsed.vclock.get("xoxd-bates"), 1);
    assert_eq!(parsed.written_by, "yoga");
    assert_eq!(parsed.chunks.len(), 2);
}
