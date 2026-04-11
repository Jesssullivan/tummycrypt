//! NATS JetStream integration for tcfs sync tasks.
//!
//! Defines the `SyncTask` message format and provides:
//! - `NatsClient` — connect, ensure streams exist, publish tasks
//! - `task_stream()` — pull consumer for worker pods
//! - `state_consumer()` — per-device durable consumer for STATE_UPDATES
//!
//! Streams:
//!   SYNC_TASKS         — push/pull/unsync work items (HPA-scaled workers consume)
//!   HYDRATION_EVENTS   — FUSE hydration events (future Phase 3 daemon-side use)
//!   STATE_UPDATES      — sync state change notifications (hierarchical subjects)
//!
//! Requires feature `nats` (async-nats optional dep).

#[cfg(feature = "nats")]
pub use inner::*;

#[cfg(feature = "nats")]
mod inner {
    use anyhow::Result;
    use async_nats::jetstream::{self, consumer::pull, stream};
    use futures::StreamExt;
    use serde::{Deserialize, Serialize};
    use std::time::Duration;
    use tracing::{debug, error, info, warn};

    use crate::conflict::VectorClock;

    // ── Stream / consumer names ───────────────────────────────────────────────

    pub const STREAM_SYNC_TASKS: &str = "SYNC_TASKS";
    pub const STREAM_HYDRATION: &str = "HYDRATION_EVENTS";
    pub const STREAM_STATE: &str = "STATE_UPDATES";
    pub const CONSUMER_SYNC_WORKERS: &str = "sync-workers";

    // ── StateEvent ────────────────────────────────────────────────────────────

    /// A state change event published to STATE_UPDATES stream.
    ///
    /// Subject hierarchy: `STATE.{device_id}.{event_type}`
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum StateEvent {
        /// A file was successfully synced (pushed) to remote storage.
        FileSynced {
            device_id: String,
            rel_path: String,
            blake3: String,
            size: u64,
            vclock: VectorClock,
            manifest_path: String,
            timestamp: u64,
        },
        /// A file was deleted from remote storage.
        FileDeleted {
            device_id: String,
            rel_path: String,
            vclock: VectorClock,
            timestamp: u64,
        },
        /// A file was renamed in remote storage.
        FileRenamed {
            device_id: String,
            old_path: String,
            new_path: String,
            vclock: VectorClock,
            timestamp: u64,
        },
        /// A device has come online and is ready to sync.
        DeviceOnline {
            device_id: String,
            last_seq: u64,
            timestamp: u64,
        },
        /// A device is going offline (graceful shutdown).
        DeviceOffline {
            device_id: String,
            last_seq: u64,
            timestamp: u64,
        },
        /// A conflict was resolved.
        ConflictResolved {
            device_id: String,
            rel_path: String,
            resolution: String,
            merged_vclock: VectorClock,
            timestamp: u64,
        },
    }

    impl StateEvent {
        pub fn device_id(&self) -> &str {
            match self {
                StateEvent::FileSynced { device_id, .. } => device_id,
                StateEvent::FileDeleted { device_id, .. } => device_id,
                StateEvent::FileRenamed { device_id, .. } => device_id,
                StateEvent::DeviceOnline { device_id, .. } => device_id,
                StateEvent::DeviceOffline { device_id, .. } => device_id,
                StateEvent::ConflictResolved { device_id, .. } => device_id,
            }
        }

        pub fn event_type(&self) -> &'static str {
            match self {
                StateEvent::FileSynced { .. } => "file_synced",
                StateEvent::FileDeleted { .. } => "file_deleted",
                StateEvent::FileRenamed { .. } => "file_renamed",
                StateEvent::DeviceOnline { .. } => "device_online",
                StateEvent::DeviceOffline { .. } => "device_offline",
                StateEvent::ConflictResolved { .. } => "conflict_resolved",
            }
        }

        /// Build the NATS subject for this event.
        pub fn subject(&self) -> String {
            format!("STATE.{}.{}", self.device_id(), self.event_type())
        }

        pub fn to_bytes(&self) -> Result<bytes::Bytes> {
            let json = serde_json::to_vec(self)
                .map_err(|e| anyhow::anyhow!("serializing StateEvent: {e}"))?;
            Ok(bytes::Bytes::from(json))
        }

        pub fn from_bytes(data: &[u8]) -> Result<Self> {
            serde_json::from_slice(data)
                .map_err(|e| anyhow::anyhow!("deserializing StateEvent: {e}"))
        }

        /// Helper to get the current unix timestamp.
        pub fn now() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }
    }

    // ── SyncTask message format ───────────────────────────────────────────────

    /// A unit of work published to the SYNC_TASKS stream.
    ///
    /// Workers deserialize this from NATS message payloads.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum SyncTask {
        /// Upload a local file/directory tree to remote storage.
        Push {
            task_id: String,
            local_path: String,
            remote_prefix: String,
        },
        /// Download a remote manifest to a local path.
        Pull {
            task_id: String,
            manifest_path: String,
            remote_prefix: String,
            local_path: String,
        },
        /// Convert a hydrated file back to a .tc stub.
        Unsync { task_id: String, local_path: String },
    }

    impl SyncTask {
        pub fn task_id(&self) -> &str {
            match self {
                SyncTask::Push { task_id, .. } => task_id,
                SyncTask::Pull { task_id, .. } => task_id,
                SyncTask::Unsync { task_id, .. } => task_id,
            }
        }

        pub fn type_name(&self) -> &'static str {
            match self {
                SyncTask::Push { .. } => "push",
                SyncTask::Pull { .. } => "pull",
                SyncTask::Unsync { .. } => "unsync",
            }
        }

        pub fn to_bytes(&self) -> Result<bytes::Bytes> {
            let json = serde_json::to_vec(self)
                .map_err(|e| anyhow::anyhow!("serializing SyncTask: {e}"))?;
            Ok(bytes::Bytes::from(json))
        }

        pub fn from_bytes(data: &[u8]) -> Result<Self> {
            serde_json::from_slice(data).map_err(|e| anyhow::anyhow!("deserializing SyncTask: {e}"))
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample_vclock() -> VectorClock {
            let mut vc = VectorClock::new();
            vc.tick("neo");
            vc.tick("neo");
            vc.tick("honey");
            vc
        }

        // ── StateEvent serialization ─────────────────────────────────────

        #[test]
        fn state_event_file_synced_roundtrip() {
            let event = StateEvent::FileSynced {
                device_id: "neo".into(),
                rel_path: "docs/readme.md".into(),
                blake3: "abc123def456".into(),
                size: 2048,
                vclock: sample_vclock(),
                manifest_path: "data/manifests/abc123def456".into(),
                timestamp: 1700000000,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();

            assert_eq!(decoded.device_id(), "neo");
            assert_eq!(decoded.event_type(), "file_synced");
            if let StateEvent::FileSynced { rel_path, size, .. } = decoded {
                assert_eq!(rel_path, "docs/readme.md");
                assert_eq!(size, 2048);
            } else {
                panic!("wrong variant");
            }
        }

        #[test]
        fn state_event_file_deleted_roundtrip() {
            let event = StateEvent::FileDeleted {
                device_id: "honey".into(),
                rel_path: "old.txt".into(),
                vclock: sample_vclock(),
                timestamp: 1700000001,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.device_id(), "honey");
            assert_eq!(decoded.event_type(), "file_deleted");
        }

        #[test]
        fn state_event_file_renamed_roundtrip() {
            let event = StateEvent::FileRenamed {
                device_id: "neo".into(),
                old_path: "a.txt".into(),
                new_path: "b.txt".into(),
                vclock: VectorClock::new(),
                timestamp: 0,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            if let StateEvent::FileRenamed {
                old_path, new_path, ..
            } = decoded
            {
                assert_eq!(old_path, "a.txt");
                assert_eq!(new_path, "b.txt");
            } else {
                panic!("wrong variant");
            }
        }

        #[test]
        fn state_event_device_online_roundtrip() {
            let event = StateEvent::DeviceOnline {
                device_id: "neo".into(),
                last_seq: 42,
                timestamp: 1700000000,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.event_type(), "device_online");
            if let StateEvent::DeviceOnline { last_seq, .. } = decoded {
                assert_eq!(last_seq, 42);
            } else {
                panic!("wrong variant");
            }
        }

        #[test]
        fn state_event_device_offline_roundtrip() {
            let event = StateEvent::DeviceOffline {
                device_id: "honey".into(),
                last_seq: 99,
                timestamp: 1700000000,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.event_type(), "device_offline");
        }

        #[test]
        fn state_event_conflict_resolved_roundtrip() {
            let event = StateEvent::ConflictResolved {
                device_id: "neo".into(),
                rel_path: "conflict.txt".into(),
                resolution: "keep_local".into(),
                merged_vclock: sample_vclock(),
                timestamp: 1700000000,
            };
            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.event_type(), "conflict_resolved");
            if let StateEvent::ConflictResolved { resolution, .. } = decoded {
                assert_eq!(resolution, "keep_local");
            } else {
                panic!("wrong variant");
            }
        }

        // ── StateEvent subject generation ────────────────────────────────

        #[test]
        fn state_event_subject_format() {
            let event = StateEvent::FileSynced {
                device_id: "neo".into(),
                rel_path: "f.txt".into(),
                blake3: "x".into(),
                size: 0,
                vclock: VectorClock::new(),
                manifest_path: "m".into(),
                timestamp: 0,
            };
            assert_eq!(event.subject(), "STATE.neo.file_synced");

            let event2 = StateEvent::DeviceOnline {
                device_id: "honey".into(),
                last_seq: 0,
                timestamp: 0,
            };
            assert_eq!(event2.subject(), "STATE.honey.device_online");
        }

        // ── SyncTask serialization ───────────────────────────────────────

        #[test]
        fn sync_task_push_roundtrip() {
            let task = SyncTask::Push {
                task_id: "task-001".into(),
                local_path: "/home/jess/tcfs".into(),
                remote_prefix: "data".into(),
            };
            let bytes = task.to_bytes().unwrap();
            let decoded = SyncTask::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.task_id(), "task-001");
            assert_eq!(decoded.type_name(), "push");
        }

        #[test]
        fn sync_task_pull_roundtrip() {
            let task = SyncTask::Pull {
                task_id: "task-002".into(),
                manifest_path: "data/manifests/abc123".into(),
                remote_prefix: "data".into(),
                local_path: "/tmp/out.txt".into(),
            };
            let bytes = task.to_bytes().unwrap();
            let decoded = SyncTask::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.task_id(), "task-002");
            assert_eq!(decoded.type_name(), "pull");
        }

        #[test]
        fn sync_task_unsync_roundtrip() {
            let task = SyncTask::Unsync {
                task_id: "task-003".into(),
                local_path: "/home/jess/tcfs/big.bin".into(),
            };
            let bytes = task.to_bytes().unwrap();
            let decoded = SyncTask::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.task_id(), "task-003");
            assert_eq!(decoded.type_name(), "unsync");
        }

        #[test]
        fn state_event_invalid_json_errors() {
            let result = StateEvent::from_bytes(b"not json at all");
            assert!(result.is_err());
        }

        #[test]
        fn sync_task_invalid_json_errors() {
            let result = SyncTask::from_bytes(b"{\"type\":\"unknown\"}");
            assert!(result.is_err());
        }

        // ── VectorClock survives JSON roundtrip ──────────────────────────

        #[test]
        fn vclock_preserved_through_state_event() {
            let mut vc = VectorClock::new();
            vc.tick("neo");
            vc.tick("neo");
            vc.tick("honey");

            let event = StateEvent::FileSynced {
                device_id: "neo".into(),
                rel_path: "f.txt".into(),
                blake3: "x".into(),
                size: 0,
                vclock: vc.clone(),
                manifest_path: "m".into(),
                timestamp: 0,
            };

            let bytes = event.to_bytes().unwrap();
            let decoded = StateEvent::from_bytes(&bytes).unwrap();
            if let StateEvent::FileSynced { vclock, .. } = decoded {
                assert_eq!(vclock.get("neo"), 2);
                assert_eq!(vclock.get("honey"), 1);
            } else {
                panic!("wrong variant");
            }
        }
    }

    // ── NatsClient ────────────────────────────────────────────────────────────

    /// Thin wrapper around an async-nats JetStream context.
    #[derive(Clone)]
    pub struct NatsClient {
        js: jetstream::Context,
    }

    /// Resolve the effective NATS URL based on TLS requirements.
    ///
    /// - `require_tls=true` + `nats://` → upgraded to `tls://`
    /// - `require_tls=true` + `tls://` → unchanged
    /// - `require_tls=false` + `nats://` → unchanged (plaintext warning logged)
    /// - `require_tls=false` + `tls://` → unchanged
    pub fn resolve_nats_url(url: &str, require_tls: bool) -> String {
        if require_tls && url.starts_with("nats://") {
            let upgraded = url.replacen("nats://", "tls://", 1);
            warn!(
                original = url,
                upgraded = %upgraded,
                "NATS: upgrading to TLS (nats_tls=true)"
            );
            upgraded
        } else {
            if !require_tls && !url.starts_with("tls://") {
                warn!(
                    url,
                    "NATS: connecting without TLS — credentials transmitted in plaintext"
                );
            }
            url.to_string()
        }
    }

    impl NatsClient {
        /// Connect to NATS and return a client with JetStream enabled.
        ///
        /// If `require_tls` is true and the URL uses `nats://`, it is upgraded to `tls://`.
        pub async fn connect(url: &str, require_tls: bool, token: Option<&str>) -> Result<Self> {
            let effective_url = resolve_nats_url(url, require_tls);

            let client = if let Some(tok) = token {
                info!("NATS: connecting with token auth");
                async_nats::ConnectOptions::with_token(tok.to_string())
                    .connect(&effective_url)
                    .await
                    .map_err(|e| anyhow::anyhow!("connecting to NATS at {effective_url}: {e}"))?
            } else {
                async_nats::connect(&effective_url)
                    .await
                    .map_err(|e| anyhow::anyhow!("connecting to NATS at {effective_url}: {e}"))?
            };

            info!("NATS: connected to {effective_url}");
            let js = jetstream::new(client);
            Ok(NatsClient { js })
        }

        /// Ensure all required JetStream streams exist.
        ///
        /// Uses `create_or_update_stream` (not `get_or_create_stream`) so that
        /// config changes — especially subject filter updates — are applied to
        /// existing streams. Without this, a stream created by an older binary
        /// with different subjects would never get its filter updated.
        pub async fn ensure_streams(&self) -> Result<()> {
            self.js
                .create_or_update_stream(stream::Config {
                    name: STREAM_SYNC_TASKS.to_string(),
                    subjects: vec![STREAM_SYNC_TASKS.to_string()],
                    max_messages: 1_000_000,
                    max_age: Duration::from_secs(7 * 24 * 3600),
                    retention: stream::RetentionPolicy::WorkQueue,
                    num_replicas: 1,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring SYNC_TASKS stream: {e}"))?;

            self.js
                .create_or_update_stream(stream::Config {
                    name: STREAM_HYDRATION.to_string(),
                    subjects: vec![STREAM_HYDRATION.to_string()],
                    max_messages: 100_000,
                    max_age: Duration::from_secs(3600),
                    num_replicas: 1,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring HYDRATION_EVENTS stream: {e}"))?;

            // CRITICAL: Jepsen testing (Dec 2025) found NATS JetStream 2.12.1 can lose
            // acknowledged writes under power failure due to lazy fsync. The NATS server
            // MUST be configured with `sync_always: true` in its jetstream block.
            // See: docs/ops/fleet-deployment.md for required server configuration.
            // Client-side, we set num_replicas=1 explicitly and verify stream health below.

            // STATE_UPDATES: fan-out (Limits retention), hierarchical subjects, 7-day TTL
            self.js
                .create_or_update_stream(stream::Config {
                    name: STREAM_STATE.to_string(),
                    subjects: vec!["STATE.>".to_string()],
                    max_messages: 500_000,
                    max_age: Duration::from_secs(7 * 24 * 3600),
                    retention: stream::RetentionPolicy::Limits,
                    storage: stream::StorageType::File,
                    num_replicas: 1,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring STATE_UPDATES stream: {e}"))?;

            // Verify STATE_UPDATES stream config by reading it back
            match self.js.get_stream(STREAM_STATE).await {
                Ok(mut stream) => {
                    let info = stream.info().await;
                    if let Ok(info) = info {
                        if info.config.num_replicas < 3 {
                            warn!(
                                replicas = info.config.num_replicas,
                                "NATS: STATE_UPDATES has < 3 replicas — \
                                 ensure server has sync_always: true to prevent data loss \
                                 (ref: Jepsen NATS JetStream Dec 2025)"
                            );
                        }
                        info!(
                            subjects = ?info.config.subjects,
                            replicas = info.config.num_replicas,
                            storage = ?info.config.storage,
                            messages = info.state.messages,
                            "NATS: STATE_UPDATES stream verified"
                        );
                    }
                }
                Err(e) => warn!("NATS: could not read STATE_UPDATES config: {e}"),
            }
            info!("NATS: streams verified (SYNC_TASKS, HYDRATION_EVENTS, STATE_UPDATES)");
            Ok(())
        }

        /// Publish a sync task to SYNC_TASKS.
        ///
        /// Double-awaits: first sends the publish, second waits for server ack.
        pub async fn publish_task(&self, task: &SyncTask) -> Result<()> {
            let payload = task.to_bytes()?;
            self.js
                .publish(STREAM_SYNC_TASKS, payload)
                .await
                .map_err(|e| anyhow::anyhow!("publishing to SYNC_TASKS: {e}"))?
                .await
                .map_err(|e| anyhow::anyhow!("awaiting NATS publish ack: {e}"))?;
            debug!(
                task_id = task.task_id(),
                task_type = task.type_name(),
                "task queued"
            );
            Ok(())
        }

        /// Publish a state event to STATE_UPDATES.
        pub async fn publish_state_event(&self, event: &StateEvent) -> Result<()> {
            let subject = event.subject();
            let payload = event.to_bytes()?;
            self.js
                .publish(subject, payload)
                .await
                .map_err(|e| anyhow::anyhow!("publishing state event: {e}"))?
                .await
                .map_err(|e| anyhow::anyhow!("awaiting state event ack: {e}"))?;
            debug!(
                device = event.device_id(),
                event_type = event.event_type(),
                "state event published"
            );
            Ok(())
        }

        /// Open a streaming pull consumer for sync workers.
        ///
        /// Returns a `Box`ed async stream of `TaskMessage`s.
        /// The consumer is durable ("sync-workers") and uses CreateOrUpdate semantics.
        pub async fn task_stream(
            &self,
        ) -> Result<impl futures::Stream<Item = Result<TaskMessage>>> {
            // create_consumer_on_stream uses CreateOrUpdate — idempotent
            let consumer: jetstream::consumer::Consumer<pull::Config> = self
                .js
                .create_consumer_on_stream(
                    pull::Config {
                        durable_name: Some(CONSUMER_SYNC_WORKERS.to_string()),
                        ack_wait: Duration::from_secs(60),
                        max_deliver: 3,
                        ..Default::default()
                    },
                    STREAM_SYNC_TASKS,
                )
                .await
                .map_err(|e| anyhow::anyhow!("creating sync-workers consumer: {e}"))?;

            let messages = consumer
                .messages()
                .await
                .map_err(|e| anyhow::anyhow!("opening pull consumer message stream: {e}"))?;

            let stream = messages.map(|msg_result| {
                let msg = msg_result.map_err(|e| anyhow::anyhow!("receiving NATS message: {e}"))?;
                let task = SyncTask::from_bytes(&msg.payload)?;
                Ok(TaskMessage { task, msg })
            });

            Ok(stream)
        }

        /// Create a per-device durable consumer for STATE_UPDATES.
        ///
        /// Consumer name: `state-{device_id}` (durable, survives disconnects).
        /// Subscribes to all `STATE.>` events, including own device events.
        pub async fn state_consumer(
            &self,
            device_id: &str,
        ) -> Result<impl futures::Stream<Item = Result<StateEventMessage>>> {
            let consumer_name = format!("state-{device_id}");

            let consumer: jetstream::consumer::Consumer<pull::Config> = self
                .js
                .create_consumer_on_stream(
                    pull::Config {
                        durable_name: Some(consumer_name.clone()),
                        ack_wait: Duration::from_secs(30),
                        max_deliver: 5,
                        ..Default::default()
                    },
                    STREAM_STATE,
                )
                .await
                .map_err(|e| anyhow::anyhow!("creating state consumer '{consumer_name}': {e}"))?;

            let messages = consumer
                .messages()
                .await
                .map_err(|e| anyhow::anyhow!("opening state consumer message stream: {e}"))?;

            let stream = messages.map(|msg_result| {
                let msg = msg_result.map_err(|e| anyhow::anyhow!("receiving state msg: {e}"))?;
                let event = StateEvent::from_bytes(&msg.payload)?;
                Ok(StateEventMessage { event, msg })
            });

            Ok(stream)
        }

        /// Create an ephemeral (non-durable) pull consumer for STATE_UPDATES.
        ///
        /// Each call produces an independent consumer that receives messages
        /// from the stream's current position. Unlike `state_consumer`, these do
        /// NOT share a cursor with the daemon's durable consumer, so Watch RPC
        /// callers get every event without competing with state_sync_loop.
        pub async fn state_consumer_ephemeral(
            &self,
        ) -> Result<impl futures::Stream<Item = Result<StateEventMessage>>> {
            let consumer: jetstream::consumer::Consumer<pull::Config> = self
                .js
                .create_consumer_on_stream(
                    pull::Config {
                        // No durable_name → ephemeral consumer, auto-deleted on disconnect
                        deliver_policy: jetstream::consumer::DeliverPolicy::New,
                        ack_wait: Duration::from_secs(30),
                        ..Default::default()
                    },
                    STREAM_STATE,
                )
                .await
                .map_err(|e| anyhow::anyhow!("creating ephemeral state consumer: {e}"))?;

            let messages = consumer
                .messages()
                .await
                .map_err(|e| anyhow::anyhow!("opening ephemeral state consumer: {e}"))?;

            let stream = messages.map(|msg_result| {
                let msg = msg_result.map_err(|e| anyhow::anyhow!("receiving state msg: {e}"))?;
                let event = StateEvent::from_bytes(&msg.payload)?;
                Ok(StateEventMessage { event, msg })
            });

            Ok(stream)
        }
    }

    // ── TaskMessage ───────────────────────────────────────────────────────────

    /// A deserialized task + the underlying NATS message (for ack/nak).
    pub struct TaskMessage {
        pub task: SyncTask,
        pub(crate) msg: jetstream::Message,
    }

    impl TaskMessage {
        /// Acknowledge successful processing — removes from queue.
        pub async fn ack(self) -> Result<()> {
            self.msg
                .ack()
                .await
                .map_err(|e| anyhow::anyhow!("acking NATS message: {e}"))
        }

        /// Negative-acknowledge — message will be redelivered after ack_wait.
        pub async fn nak(self) -> Result<()> {
            self.msg
                .ack_with(jetstream::AckKind::Nak(None))
                .await
                .map_err(|e| anyhow::anyhow!("naking NATS message: {e}"))
        }

        /// Extend the ack deadline (call periodically for long-running tasks).
        pub async fn in_progress(&self) -> Result<()> {
            self.msg
                .ack_with(jetstream::AckKind::Progress)
                .await
                .map_err(|e| anyhow::anyhow!("sending in-progress ack: {e}"))
        }
    }

    // ── StateEventMessage ─────────────────────────────────────────────────────

    /// A deserialized state event + the underlying NATS message (for ack).
    pub struct StateEventMessage {
        pub event: StateEvent,
        pub(crate) msg: jetstream::Message,
    }

    impl StateEventMessage {
        /// Acknowledge processing of this state event.
        pub async fn ack(self) -> Result<()> {
            self.msg
                .ack()
                .await
                .map_err(|e| anyhow::anyhow!("acking state event: {e}"))
        }
    }

    // ── process_with_retry helper ─────────────────────────────────────────────

    /// Process a task: run `f`, ack on success, nak on error.
    ///
    /// After `max_deliver` naks NATS stops redelivering the message.
    pub async fn process_with_retry<F, Fut>(msg: TaskMessage, f: F)
    where
        F: FnOnce(SyncTask) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let task_id = msg.task.task_id().to_string();
        let task_type = msg.task.type_name();
        let task = msg.task.clone();

        match f(task).await {
            Ok(()) => {
                debug!(task_id, task_type, "task succeeded");
                if let Err(e) = msg.ack().await {
                    warn!(task_id, "ack failed: {e}");
                }
            }
            Err(e) => {
                error!(task_id, task_type, error = %e, "task failed — naking for retry");
                if let Err(nak_err) = msg.nak().await {
                    warn!(task_id, "nak failed: {nak_err}");
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tls_url_upgrade_nats_to_tls() {
            let url = resolve_nats_url("nats://nats.example.com:4222", true);
            assert_eq!(url, "tls://nats.example.com:4222");
        }

        #[test]
        fn tls_url_already_tls_unchanged() {
            let url = resolve_nats_url("tls://nats.example.com:4222", true);
            assert_eq!(url, "tls://nats.example.com:4222");
        }

        #[test]
        fn plaintext_url_preserved_when_tls_not_required() {
            let url = resolve_nats_url("nats://localhost:4222", false);
            assert_eq!(url, "nats://localhost:4222");
        }

        #[test]
        fn tls_url_preserved_when_tls_not_required() {
            let url = resolve_nats_url("tls://nats.example.com:4222", false);
            assert_eq!(url, "tls://nats.example.com:4222");
        }
    }
}
