//! E2E test: file watcher and sync scheduler integration
//!
//! Tests the FileWatcher (debounce, ignore rules) and SyncScheduler
//! (priority queue, retry logic) components.

use std::sync::Arc;
use std::time::Duration;

use tcfs_sync::scheduler::{Priority, SchedulerConfig, SyncOp, SyncScheduler, SyncTask};
use tcfs_sync::watcher::{FileWatcher, WatcherConfig};
use tempfile::TempDir;
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn short_debounce() -> Duration {
    Duration::from_millis(100)
}

async fn sleep_past_debounce(debounce: Duration) {
    tokio::time::sleep(debounce + Duration::from_millis(150)).await;
}

/// Canonicalize a path for comparison (resolves /tmp → /private/tmp on macOS).
fn canon(p: &std::path::Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

#[tokio::test]
async fn watcher_detects_file_creation() {
    let dir = TempDir::new().unwrap();
    let debounce = short_debounce();
    let (tx, mut rx) = mpsc::channel(64);

    let config = WatcherConfig {
        debounce,
        ignore_names: vec![],
    };
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    let file_path = dir.path().join("hello.txt");
    tokio::fs::write(&file_path, b"hello").await.unwrap();

    sleep_past_debounce(debounce).await;

    let event = timeout(TEST_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("channel closed unexpectedly");

    // macOS FSEvents may report Created or Modified for new files
    assert!(
        matches!(
            event.kind,
            tcfs_sync::watcher::WatchEventKind::Created
                | tcfs_sync::watcher::WatchEventKind::Modified
        ),
        "expected Created or Modified for new file, got {:?}",
        event.kind
    );
    assert_eq!(canon(&event.path), canon(&file_path));
}

#[tokio::test]
async fn watcher_detects_file_modification() {
    let dir = TempDir::new().unwrap();
    let debounce = short_debounce();
    let (tx, mut rx) = mpsc::channel(64);

    // Create file before starting watcher
    let file_path = dir.path().join("data.txt");
    tokio::fs::write(&file_path, b"initial").await.unwrap();

    let config = WatcherConfig {
        debounce,
        ignore_names: vec![],
    };
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    // Overwrite the file
    tokio::fs::write(&file_path, b"updated content")
        .await
        .unwrap();

    sleep_past_debounce(debounce).await;

    let event = timeout(TEST_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("channel closed unexpectedly");

    assert!(
        matches!(
            event.kind,
            tcfs_sync::watcher::WatchEventKind::Modified
                | tcfs_sync::watcher::WatchEventKind::Created
        ),
        "expected Modified for overwritten file, got {:?}",
        event.kind
    );
    assert_eq!(canon(&event.path), canon(&file_path));
}

#[tokio::test]
async fn watcher_detects_file_deletion() {
    let dir = TempDir::new().unwrap();
    let debounce = short_debounce();
    let (tx, mut rx) = mpsc::channel(64);

    let file_path = dir.path().join("doomed.txt");
    tokio::fs::write(&file_path, b"bye").await.unwrap();

    let config = WatcherConfig {
        debounce,
        ignore_names: vec![],
    };
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    // Drain initial events from watcher noticing the file
    sleep_past_debounce(debounce).await;
    while rx.try_recv().is_ok() {}

    let canonical = canon(&file_path);

    // Delete the file
    tokio::fs::remove_file(&file_path).await.unwrap();

    sleep_past_debounce(debounce).await;

    let event = timeout(TEST_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("channel closed unexpectedly");

    // macOS FSEvents may report Deleted or Modified for removed files
    assert!(
        matches!(
            event.kind,
            tcfs_sync::watcher::WatchEventKind::Deleted
                | tcfs_sync::watcher::WatchEventKind::Modified
        ),
        "expected Deleted or Modified for removed file, got {:?}",
        event.kind
    );
    assert_eq!(canon(&event.path), canonical);
}

#[tokio::test]
async fn watcher_ignores_git_directory() {
    let dir = TempDir::new().unwrap();
    let debounce = short_debounce();
    let (tx, mut rx) = mpsc::channel(64);

    let config = WatcherConfig::default();
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    let git_dir = dir.path().join(".git");
    tokio::fs::create_dir_all(&git_dir).await.unwrap();
    tokio::fs::write(git_dir.join("HEAD"), b"ref: refs/heads/main")
        .await
        .unwrap();

    tokio::time::sleep(debounce + Duration::from_millis(300)).await;

    assert!(
        rx.try_recv().is_err(),
        "expected no events for .git directory files"
    );
}

#[tokio::test]
async fn watcher_ignores_tc_stub_files() {
    let dir = TempDir::new().unwrap();
    let debounce = short_debounce();
    let (tx, mut rx) = mpsc::channel(64);

    let config = WatcherConfig {
        debounce,
        ignore_names: vec![],
    };
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    tokio::fs::write(dir.path().join("secret.tc"), b"stub")
        .await
        .unwrap();

    tokio::time::sleep(debounce + Duration::from_millis(300)).await;

    assert!(
        rx.try_recv().is_err(),
        "expected no events for .tc stub files"
    );
}

#[tokio::test]
async fn watcher_debounce_coalesces_rapid_writes() {
    let dir = TempDir::new().unwrap();
    let debounce = Duration::from_millis(200);
    let (tx, mut rx) = mpsc::channel(64);

    let config = WatcherConfig {
        debounce,
        ignore_names: vec![],
    };
    let _watcher = FileWatcher::start(dir.path(), config, tx).unwrap();

    let file_path = dir.path().join("rapid.txt");

    for i in 0..5 {
        tokio::fs::write(&file_path, format!("write {i}"))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    sleep_past_debounce(debounce).await;

    let mut events = vec![];
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    assert_eq!(
        events.len(),
        1,
        "expected 1 coalesced event, got {}: {:?}",
        events.len(),
        events.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

/// Test scheduler priority ordering using the sender channel.
///
/// We submit tasks via the sender, then spawn `run()` with a short-lived
/// handler that records processed order, and use a done signal to stop early.
#[tokio::test]
async fn scheduler_processes_high_priority_first() {
    let config = SchedulerConfig {
        max_concurrent: 1,
        max_retries: 0,
        base_backoff: Duration::from_millis(10),
    };

    let scheduler = SyncScheduler::new(config);
    let sender = scheduler.sender();

    // Pre-enqueue directly into the priority queue
    scheduler
        .enqueue(SyncTask::new(
            "low.txt".into(),
            SyncOp::Push,
            Priority::Low,
        ))
        .await;
    scheduler
        .enqueue(SyncTask::new(
            "high.txt".into(),
            SyncOp::Push,
            Priority::High,
        ))
        .await;
    scheduler
        .enqueue(SyncTask::new(
            "normal.txt".into(),
            SyncOp::Push,
            Priority::Normal,
        ))
        .await;

    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let order_clone = order.clone();
    let done_tx = Arc::new(tokio::sync::Notify::new());
    let done_rx = done_tx.clone();

    let handler = move |task: SyncTask| {
        let order = order_clone.clone();
        let done = done_tx.clone();
        Box::pin(async move {
            let mut v = order.lock().await;
            v.push(task.path.to_string_lossy().to_string());
            if v.len() == 3 {
                done.notify_one();
            }
            Ok(())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    };

    // Spawn the scheduler run in background
    let sched_handle = tokio::spawn(async move {
        scheduler.run(handler).await;
    });

    // Wait for all 3 tasks to be processed
    timeout(TEST_TIMEOUT, done_rx.notified())
        .await
        .expect("scheduler didn't process all tasks in time");

    // Drop the sender to let run() exit
    drop(sender);
    // Give it a moment to shut down
    let _ = timeout(Duration::from_millis(500), sched_handle).await;

    let processed = order.lock().await;
    assert_eq!(processed.len(), 3, "all 3 tasks should be processed");
    assert_eq!(
        processed[0], "high.txt",
        "high priority task should be processed first, got order: {:?}",
        *processed
    );
}

/// Test scheduler retries a failed task before dropping it.
#[tokio::test]
async fn scheduler_retries_failed_task() {
    let config = SchedulerConfig {
        max_concurrent: 1,
        max_retries: 2,
        base_backoff: Duration::from_millis(10),
    };

    let scheduler = SyncScheduler::new(config);
    let sender = scheduler.sender();

    scheduler
        .enqueue(SyncTask::new(
            "retry-me.txt".into(),
            SyncOp::Push,
            Priority::Normal,
        ))
        .await;

    let attempt_count: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let attempt_clone = attempt_count.clone();
    let done_tx = Arc::new(tokio::sync::Notify::new());
    let done_rx = done_tx.clone();

    let handler = move |_task: SyncTask| {
        let attempts = attempt_clone.clone();
        let done = done_tx.clone();
        Box::pin(async move {
            let mut count = attempts.lock().await;
            *count += 1;
            if *count == 1 {
                anyhow::bail!("transient failure");
            }
            // Succeed on second attempt, signal done
            done.notify_one();
            Ok(())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    };

    let sched_handle = tokio::spawn(async move {
        scheduler.run(handler).await;
    });

    // Wait for the retry to succeed
    timeout(TEST_TIMEOUT, done_rx.notified())
        .await
        .expect("retry didn't succeed in time");

    drop(sender);
    let _ = timeout(Duration::from_millis(500), sched_handle).await;

    let attempts = *attempt_count.lock().await;
    assert_eq!(
        attempts, 2,
        "task should have been processed twice (1 original + 1 retry), got {attempts}"
    );
}
