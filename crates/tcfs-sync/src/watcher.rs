//! File system watcher with debounce for detecting local changes.
//!
//! Uses the `notify` crate for cross-platform FS events (FSEvents on macOS,
//! inotify on Linux). Events are debounced per-path to coalesce rapid writes
//! into a single event.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Event emitted by the file watcher after debounce.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub kind: WatchEventKind,
}

/// Kind of file system event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEventKind {
    Created,
    Modified,
    Deleted,
}

/// Configuration for the file watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// How long to wait after the last event before emitting (default: 500ms).
    pub debounce: Duration,
    /// File/directory names to ignore.
    pub ignore_names: Vec<String>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(500),
            ignore_names: vec![
                ".git".into(),
                ".DS_Store".into(),
                "target".into(),
                "node_modules".into(),
            ],
        }
    }
}

/// Cross-platform file watcher with debounce.
///
/// Watches a directory recursively and emits debounced events via a tokio channel.
/// Rapid writes to the same path are coalesced — only the final event is emitted
/// after the debounce window expires.
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    /// Handle to the debounce task (kept alive for cancellation on drop).
    _debounce_handle: tokio::task::JoinHandle<()>,
}

impl FileWatcher {
    /// Create and start a file watcher on the given directory.
    ///
    /// Returns the watcher handle and a receiver for debounced events.
    pub fn start(
        watch_dir: &Path,
        config: WatcherConfig,
        tx: mpsc::Sender<WatchEvent>,
    ) -> anyhow::Result<Self> {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let _ = raw_tx.send(res);
            })?;

        watcher.watch(watch_dir, RecursiveMode::Recursive)?;

        info!(dir = %watch_dir.display(), "file watcher started");

        let debounce_handle = tokio::task::spawn_blocking({
            let config = config.clone();
            move || debounce_loop(raw_rx, tx, config)
        });

        Ok(Self {
            _watcher: watcher,
            _debounce_handle: debounce_handle,
        })
    }
}

/// Background loop that receives raw notify events and emits debounced WatchEvents.
fn debounce_loop(
    raw_rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    tx: mpsc::Sender<WatchEvent>,
    config: WatcherConfig,
) {
    // Map from path → (last event kind, last event time)
    let mut pending: HashMap<PathBuf, (WatchEventKind, Instant)> = HashMap::new();

    loop {
        // Drain pending events that have exceeded the debounce window
        let now = Instant::now();
        let mut to_emit = Vec::new();
        pending.retain(|path, (kind, last_seen)| {
            if now.duration_since(*last_seen) >= config.debounce {
                to_emit.push(WatchEvent {
                    path: path.clone(),
                    kind: *kind,
                });
                false
            } else {
                true
            }
        });

        for event in to_emit {
            debug!(path = %event.path.display(), kind = ?event.kind, "emitting debounced event");
            if tx.blocking_send(event).is_err() {
                return; // receiver dropped
            }
        }

        // Wait for next raw event with a short timeout to check debounce expiry
        let timeout = if pending.is_empty() {
            Duration::from_secs(60) // idle — long poll
        } else {
            // Check again after the shortest remaining debounce
            let min_remaining = pending
                .values()
                .map(|(_, t)| config.debounce.saturating_sub(now.duration_since(*t)))
                .min()
                .unwrap_or(config.debounce);
            min_remaining.max(Duration::from_millis(10))
        };

        match raw_rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                let kind = match event.kind {
                    notify::EventKind::Create(_) => WatchEventKind::Created,
                    notify::EventKind::Modify(_) => WatchEventKind::Modified,
                    notify::EventKind::Remove(_) => WatchEventKind::Deleted,
                    _ => continue, // skip access, other, any
                };

                for path in event.paths {
                    if should_ignore(&path, &config.ignore_names) {
                        continue;
                    }
                    // Skip .tc stub files (FUSE hydration artifacts)
                    if path.extension().map(|e| e == "tc").unwrap_or(false) {
                        continue;
                    }
                    pending.insert(path, (kind, Instant::now()));
                }
            }
            Ok(Err(e)) => {
                warn!("watcher error: {e}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Normal — just loop back to check debounce expiry
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                info!("watcher channel disconnected, stopping");
                return;
            }
        }
    }
}

/// Check if a path should be ignored based on any component matching ignore list.
fn should_ignore(path: &Path, ignore_names: &[String]) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if ignore_names.iter().any(|ig| ig == name_str.as_ref()) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_ignore() {
        let ignore = vec![".git".into(), "node_modules".into()];

        assert!(should_ignore(
            Path::new("/home/user/project/.git/HEAD"),
            &ignore
        ));
        assert!(should_ignore(
            Path::new("/home/user/project/node_modules/pkg/index.js"),
            &ignore
        ));
        assert!(!should_ignore(
            Path::new("/home/user/project/src/main.rs"),
            &ignore
        ));
    }

    #[test]
    fn test_should_ignore_tc_extension() {
        let path = Path::new("/home/user/tcfs/file.tc");
        assert_eq!(path.extension().map(|e| e == "tc"), Some(true));
    }
}
