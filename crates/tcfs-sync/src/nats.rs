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

    // ── NatsClient ────────────────────────────────────────────────────────────

    /// Thin wrapper around an async-nats JetStream context.
    pub struct NatsClient {
        js: jetstream::Context,
    }

    impl NatsClient {
        /// Connect to NATS and return a client with JetStream enabled.
        pub async fn connect(url: &str) -> Result<Self> {
            let client = async_nats::connect(url)
                .await
                .map_err(|e| anyhow::anyhow!("connecting to NATS at {url}: {e}"))?;
            info!("NATS: connected to {url}");
            let js = jetstream::new(client);
            Ok(NatsClient { js })
        }

        /// Ensure all required JetStream streams exist (idempotent via CreateOrUpdate).
        pub async fn ensure_streams(&self) -> Result<()> {
            self.js
                .get_or_create_stream(stream::Config {
                    name: STREAM_SYNC_TASKS.to_string(),
                    subjects: vec![STREAM_SYNC_TASKS.to_string()],
                    max_messages: 1_000_000,
                    max_age: Duration::from_secs(7 * 24 * 3600),
                    retention: stream::RetentionPolicy::WorkQueue,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring SYNC_TASKS stream: {e}"))?;

            self.js
                .get_or_create_stream(stream::Config {
                    name: STREAM_HYDRATION.to_string(),
                    subjects: vec![STREAM_HYDRATION.to_string()],
                    max_messages: 100_000,
                    max_age: Duration::from_secs(3600),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring HYDRATION_EVENTS stream: {e}"))?;

            // STATE_UPDATES: fan-out (Limits retention), hierarchical subjects, 7-day TTL
            self.js
                .get_or_create_stream(stream::Config {
                    name: STREAM_STATE.to_string(),
                    subjects: vec!["STATE.>".to_string()],
                    max_messages: 500_000,
                    max_age: Duration::from_secs(7 * 24 * 3600),
                    retention: stream::RetentionPolicy::Limits,
                    storage: stream::StorageType::File,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("ensuring STATE_UPDATES stream: {e}"))?;

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
                .map_err(|e| {
                    anyhow::anyhow!("creating state consumer '{consumer_name}': {e}")
                })?;

            let messages = consumer.messages().await.map_err(|e| {
                anyhow::anyhow!("opening state consumer message stream: {e}")
            })?;

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
}
