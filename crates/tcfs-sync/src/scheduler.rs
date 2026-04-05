//! Sync scheduler: priority queue for sync tasks with concurrency control.
//!
//! The scheduler receives events from the file watcher and queues them for
//! processing by the sync engine. Tasks are prioritized:
//!   - High: user-initiated operations (Finder drag, CLI push)
//!   - Normal: watcher-detected changes (file save, create, delete)
//!   - Low: background full-scan reconciliation
//!
//! Failed tasks are retried with exponential backoff.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

/// Priority levels for sync tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// User-initiated sync (Finder drag, CLI push/pull).
    High,
    /// Watcher-detected change (file save, create, delete).
    Normal,
    /// Background reconciliation scan.
    Low,
}

impl Priority {
    fn ordinal(&self) -> u8 {
        match self {
            Priority::High => 2,
            Priority::Normal => 1,
            Priority::Low => 0,
        }
    }
}

/// Operation type for a sync task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOp {
    Push,
    Pull,
    Delete,
}

/// A unit of work for the sync scheduler.
#[derive(Debug, Clone)]
pub struct SyncTask {
    pub path: PathBuf,
    pub op: SyncOp,
    pub priority: Priority,
    pub created_at: Instant,
    /// Number of times this task has been retried.
    pub retries: u32,
}

impl SyncTask {
    pub fn new(path: PathBuf, op: SyncOp, priority: Priority) -> Self {
        Self {
            path,
            op,
            priority,
            created_at: Instant::now(),
            retries: 0,
        }
    }
}

// BinaryHeap is a max-heap, so we order by (priority desc, created_at asc).
impl PartialEq for SyncTask {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.path == other.path
    }
}
impl Eq for SyncTask {}

impl PartialOrd for SyncTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SyncTask {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, then older tasks first
        self.priority
            .ordinal()
            .cmp(&other.priority.ordinal())
            .then_with(|| other.created_at.cmp(&self.created_at))
    }
}

/// Configuration for the sync scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum concurrent sync operations (default: 4).
    pub max_concurrent: usize,
    /// Maximum retries before dropping a task (default: 3).
    pub max_retries: u32,
    /// Base backoff duration for retries (default: 1s, doubles each retry).
    pub base_backoff: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            max_retries: 3,
            base_backoff: Duration::from_secs(1),
        }
    }
}

/// Sync scheduler with priority queue and concurrency control.
///
/// Call `enqueue()` to add tasks, and `run()` to start the processing loop.
/// The scheduler dequeues tasks by priority and dispatches them to the provided
/// handler function.
pub struct SyncScheduler {
    queue: Mutex<BinaryHeap<SyncTask>>,
    config: SchedulerConfig,
    /// Channel for submitting tasks from outside the run loop.
    submit_tx: mpsc::Sender<SyncTask>,
    submit_rx: Mutex<mpsc::Receiver<SyncTask>>,
    /// Number of currently active (in-flight) tasks.
    active_count: std::sync::Arc<AtomicUsize>,
    /// Total tasks completed successfully.
    completed_count: std::sync::Arc<AtomicUsize>,
    /// Total tasks that failed after max retries.
    failed_count: std::sync::Arc<AtomicUsize>,
}

impl SyncScheduler {
    /// Create a new scheduler with the given configuration.
    pub fn new(config: SchedulerConfig) -> Self {
        let (submit_tx, submit_rx) = mpsc::channel(256);
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            config,
            submit_tx,
            submit_rx: Mutex::new(submit_rx),
            active_count: std::sync::Arc::new(AtomicUsize::new(0)),
            completed_count: std::sync::Arc::new(AtomicUsize::new(0)),
            failed_count: std::sync::Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Get a sender handle for submitting tasks.
    pub fn sender(&self) -> mpsc::Sender<SyncTask> {
        self.submit_tx.clone()
    }

    /// Enqueue a task for processing.
    pub async fn enqueue(&self, task: SyncTask) {
        let mut queue = self.queue.lock().await;
        debug!(path = %task.path.display(), op = ?task.op, priority = ?task.priority, "task enqueued");
        queue.push(task);
    }

    /// Run the scheduler loop, dispatching tasks to the handler.
    ///
    /// The handler receives a `SyncTask` and returns `Ok(())` on success or
    /// `Err` on failure (which triggers retry with backoff).
    ///
    /// This method runs until the submit channel is closed and the queue is drained.
    pub async fn run<F>(&self, handler: F)
    where
        F: Fn(
                SyncTask,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let semaphore =
            std::sync::Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrent));
        let mut rx = self.submit_rx.lock().await;

        info!(
            max_concurrent = self.config.max_concurrent,
            max_retries = self.config.max_retries,
            "scheduler started"
        );

        loop {
            // Drain submitted tasks into the priority queue
            {
                let mut queue = self.queue.lock().await;
                while let Ok(task) = rx.try_recv() {
                    queue.push(task);
                }
            }

            // Dequeue the highest-priority task
            let task = {
                let mut queue = self.queue.lock().await;
                queue.pop()
            };

            match task {
                Some(task) => {
                    let permit = semaphore.clone().acquire_owned().await;
                    if permit.is_err() {
                        break; // semaphore closed
                    }
                    let permit = permit.unwrap();

                    let max_retries = self.config.max_retries;
                    let base_backoff = self.config.base_backoff;
                    let submit_tx = self.submit_tx.clone();

                    debug!(
                        path = %task.path.display(),
                        op = ?task.op,
                        retries = task.retries,
                        "dispatching task"
                    );

                    let active = self.active_count.clone();
                    let completed = self.completed_count.clone();
                    let failed = self.failed_count.clone();

                    active.fetch_add(1, AtomicOrdering::Relaxed);

                    let fut = handler(task.clone());
                    tokio::spawn(async move {
                        let result = fut.await;
                        drop(permit); // release concurrency slot
                        active.fetch_sub(1, AtomicOrdering::Relaxed);

                        match result {
                            Ok(()) => {
                                completed.fetch_add(1, AtomicOrdering::Relaxed);
                            }
                            Err(e) => {
                                if task.retries < max_retries {
                                    // Exponential backoff with jitter (±25%)
                                    let base = base_backoff * 2u32.saturating_pow(task.retries);
                                    let jitter_range = base.as_millis() as u64 / 4;
                                    let jitter = if jitter_range > 0 {
                                        let seed = task.path.to_string_lossy().len() as u64
                                            ^ task.retries as u64;
                                        Duration::from_millis(seed % jitter_range)
                                    } else {
                                        Duration::ZERO
                                    };
                                    let backoff = base + jitter;
                                    warn!(
                                        path = %task.path.display(),
                                        op = ?task.op,
                                        retry = task.retries + 1,
                                        backoff_ms = backoff.as_millis(),
                                        error = %e,
                                        "task failed, scheduling retry"
                                    );
                                    tokio::time::sleep(backoff).await;
                                    let mut retry = task;
                                    retry.retries += 1;
                                    let _ = submit_tx.send(retry).await;
                                } else {
                                    failed.fetch_add(1, AtomicOrdering::Relaxed);
                                    warn!(
                                        path = %task.path.display(),
                                        op = ?task.op,
                                        error = %e,
                                        "task failed after max retries, dropping"
                                    );
                                }
                            }
                        }
                    });
                }
                None => {
                    // Queue empty — wait for new submissions
                    match rx.recv().await {
                        Some(task) => {
                            let mut queue = self.queue.lock().await;
                            queue.push(task);
                        }
                        None => {
                            info!("scheduler: submit channel closed, draining remaining tasks");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Current number of pending tasks in the queue.
    pub async fn pending(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// Number of currently active (in-flight) tasks.
    pub fn active(&self) -> usize {
        self.active_count.load(AtomicOrdering::Relaxed)
    }

    /// Total tasks completed successfully since scheduler start.
    pub fn completed(&self) -> usize {
        self.completed_count.load(AtomicOrdering::Relaxed)
    }

    /// Total tasks that failed after max retries.
    pub fn failed(&self) -> usize {
        self.failed_count.load(AtomicOrdering::Relaxed)
    }

    /// Scheduler configuration (for diagnostics reporting).
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_ordering_by_priority() {
        let high = SyncTask::new(PathBuf::from("a.txt"), SyncOp::Push, Priority::High);
        let normal = SyncTask::new(PathBuf::from("b.txt"), SyncOp::Push, Priority::Normal);
        let low = SyncTask::new(PathBuf::from("c.txt"), SyncOp::Push, Priority::Low);

        let mut heap = BinaryHeap::new();
        heap.push(low);
        heap.push(high);
        heap.push(normal);

        assert_eq!(heap.pop().unwrap().priority, Priority::High);
        assert_eq!(heap.pop().unwrap().priority, Priority::Normal);
        assert_eq!(heap.pop().unwrap().priority, Priority::Low);
    }

    #[test]
    fn test_task_ordering_same_priority_fifo() {
        // Same priority — older task should come first
        let older = SyncTask {
            path: PathBuf::from("old.txt"),
            op: SyncOp::Push,
            priority: Priority::Normal,
            created_at: Instant::now() - Duration::from_secs(10),
            retries: 0,
        };
        let newer = SyncTask::new(PathBuf::from("new.txt"), SyncOp::Push, Priority::Normal);

        let mut heap = BinaryHeap::new();
        heap.push(newer);
        heap.push(older);

        assert_eq!(heap.pop().unwrap().path, PathBuf::from("old.txt"));
        assert_eq!(heap.pop().unwrap().path, PathBuf::from("new.txt"));
    }

    #[tokio::test]
    async fn test_scheduler_enqueue_and_pending() {
        let scheduler = SyncScheduler::new(SchedulerConfig::default());
        assert_eq!(scheduler.pending().await, 0);

        scheduler
            .enqueue(SyncTask::new(
                PathBuf::from("test.txt"),
                SyncOp::Push,
                Priority::Normal,
            ))
            .await;

        assert_eq!(scheduler.pending().await, 1);
    }
}
