//! Real-time file watcher for CLAUDE.md and settings.json.
//!
//! Uses OS-native file-system notifications (`notify` crate) with debouncing,
//! replacing the old polling-on-submit approach.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Events emitted by the [`ConfigWatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChangeEvent {
    /// A CLAUDE.md file was modified (contains its absolute path).
    ClaudeMd(PathBuf),
    /// The user settings.json was modified.
    Settings(PathBuf),
}

/// A handle returned by [`ConfigWatcher::start`] that receives change events.
pub struct ConfigWatchHandle {
    rx: mpsc::Receiver<ConfigChangeEvent>,
    /// Keep the debouncer alive; dropping it stops the watcher.
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

impl ConfigWatchHandle {
    /// Non-blocking receive — returns `None` if no event is pending.
    pub fn try_recv(&mut self) -> Option<ConfigChangeEvent> {
        self.rx.try_recv().ok()
    }

    /// Drain all pending events into a deduplicated set.
    pub fn drain(&mut self) -> Vec<ConfigChangeEvent> {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        while let Some(evt) = self.try_recv() {
            let key = format!("{evt:?}");
            if seen.insert(key) {
                events.push(evt);
            }
        }
        events
    }

    /// Async receive — blocks until an event arrives or the watcher is dropped.
    pub async fn recv(&mut self) -> Option<ConfigChangeEvent> {
        self.rx.recv().await
    }
}

/// Watches CLAUDE.md files and settings.json for changes using OS-native events.
pub struct ConfigWatcher;

impl ConfigWatcher {
    /// Discover all CLAUDE.md files from `project_dir` up to root.
    fn collect_watch_paths(project_dir: &Path) -> Vec<(PathBuf, WatchedFileKind)> {
        let mut paths = Vec::new();

        // Project-level CLAUDE.md
        let local_md = project_dir.join("CLAUDE.md");
        if local_md.exists() {
            paths.push((local_md, WatchedFileKind::ClaudeMd));
        }

        // Also watch for .claude/CLAUDE.md in project
        let local_dot_md = project_dir.join(".claude").join("CLAUDE.md");
        if local_dot_md.exists() {
            paths.push((local_dot_md, WatchedFileKind::ClaudeMd));
        }

        // Walk parents for CLAUDE.md
        if let Some(parent) = project_dir.parent() {
            let mut cur = parent;
            loop {
                let md = cur.join("CLAUDE.md");
                if md.exists() {
                    paths.push((md, WatchedFileKind::ClaudeMd));
                }
                match cur.parent() {
                    Some(p) if p != cur => cur = p,
                    _ => break,
                }
            }
        }

        // User-level settings.json
        if let Some(settings) = crate::config::settings_path() {
            if settings.exists() {
                paths.push((settings, WatchedFileKind::Settings));
            }
        }

        // Home-level CLAUDE.md (~/.claude/CLAUDE.md)
        if let Some(home) = dirs::home_dir() {
            let home_md = home.join(".claude").join("CLAUDE.md");
            if home_md.exists() {
                paths.push((home_md, WatchedFileKind::ClaudeMd));
            }
        }

        paths
    }

    /// Start watching config files. Returns a handle for receiving change events.
    ///
    /// The watcher runs in a background thread (managed by `notify`) and sends
    /// debounced events to the returned handle. Drop the handle to stop watching.
    pub fn start(project_dir: &Path) -> anyhow::Result<ConfigWatchHandle> {
        let watch_paths = Self::collect_watch_paths(project_dir);

        if watch_paths.is_empty() {
            debug!("ConfigWatcher: no config files found to watch");
        }

        // Build a lookup set for classification
        let path_map: Arc<Vec<(PathBuf, WatchedFileKind)>> = Arc::new(
            watch_paths
                .iter()
                .map(|(p, k)| {
                    (
                        p.canonicalize().unwrap_or_else(|_| p.clone()),
                        k.clone(),
                    )
                })
                .collect(),
        );

        let (tx, rx) = mpsc::channel::<ConfigChangeEvent>(32);

        let pm = Arc::clone(&path_map);
        let mut debouncer = new_debouncer(
            Duration::from_millis(500),
            move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
                match res {
                    Ok(events) => {
                        for evt in events {
                            if evt.kind != DebouncedEventKind::Any {
                                continue;
                            }
                            let canonical = evt
                                .path
                                .canonicalize()
                                .unwrap_or_else(|_| evt.path.clone());

                            for (watched, kind) in pm.iter() {
                                if canonical == *watched {
                                    let change = match kind {
                                        WatchedFileKind::ClaudeMd => {
                                            ConfigChangeEvent::ClaudeMd(evt.path.clone())
                                        }
                                        WatchedFileKind::Settings => {
                                            ConfigChangeEvent::Settings(evt.path.clone())
                                        }
                                    };
                                    debug!("ConfigWatcher: detected change: {change:?}");
                                    // Non-blocking send — drop event if buffer full
                                    let _ = tx.try_send(change);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("ConfigWatcher error: {e}");
                    }
                }
            },
        )?;

        // Watch parent directories (more reliable than watching individual files,
        // since editors often delete+recreate files on save)
        let mut watched_dirs = HashSet::new();
        for (path, _kind) in &watch_paths {
            if let Some(dir) = path.parent() {
                if watched_dirs.insert(dir.to_path_buf()) {
                    debug!("ConfigWatcher: watching directory {}", dir.display());
                    if let Err(e) = debouncer
                        .watcher()
                        .watch(dir, notify::RecursiveMode::NonRecursive)
                    {
                        warn!(
                            "ConfigWatcher: failed to watch {}: {e}",
                            dir.display()
                        );
                    }
                }
            }
        }

        debug!(
            "ConfigWatcher: started, watching {} paths in {} directories",
            watch_paths.len(),
            watched_dirs.len()
        );

        Ok(ConfigWatchHandle {
            rx,
            _debouncer: debouncer,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WatchedFileKind {
    ClaudeMd,
    Settings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn collect_paths_finds_claude_md() {
        let dir = TempDir::new().unwrap();
        let md = dir.path().join("CLAUDE.md");
        fs::write(&md, "# Test").unwrap();

        let paths = ConfigWatcher::collect_watch_paths(dir.path());
        assert!(paths.iter().any(|(p, k)| p == &md && *k == WatchedFileKind::ClaudeMd));
    }

    #[test]
    fn collect_paths_finds_dot_claude_md() {
        let dir = TempDir::new().unwrap();
        let dot_dir = dir.path().join(".claude");
        fs::create_dir_all(&dot_dir).unwrap();
        let md = dot_dir.join("CLAUDE.md");
        fs::write(&md, "# Test").unwrap();

        let paths = ConfigWatcher::collect_watch_paths(dir.path());
        assert!(paths.iter().any(|(p, k)| p == &md && *k == WatchedFileKind::ClaudeMd));
    }

    #[tokio::test]
    async fn watcher_detects_claude_md_change() {
        let dir = TempDir::new().unwrap();
        let md = dir.path().join("CLAUDE.md");
        fs::write(&md, "# Initial").unwrap();

        let mut handle = ConfigWatcher::start(dir.path()).unwrap();

        // Give the watcher time to initialize
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Modify the file
        fs::write(&md, "# Modified").unwrap();

        // Wait for debounced event (500ms debounce + some margin)
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let events = handle.drain();
        assert!(
            events.iter().any(|e| matches!(e, ConfigChangeEvent::ClaudeMd(_))),
            "Expected ClaudeMd event, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn watcher_starts_with_no_files() {
        let dir = TempDir::new().unwrap();
        let handle = ConfigWatcher::start(dir.path());
        assert!(handle.is_ok(), "Should succeed even with no config files");
    }

    #[test]
    fn drain_deduplicates() {
        let (tx, rx) = mpsc::channel(32);
        let path = PathBuf::from("/test/CLAUDE.md");
        // Send duplicate events
        tx.try_send(ConfigChangeEvent::ClaudeMd(path.clone())).unwrap();
        tx.try_send(ConfigChangeEvent::ClaudeMd(path)).unwrap();

        let debouncer = {
            // Create a minimal debouncer just for the test struct
            new_debouncer(Duration::from_secs(60), |_: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {}).unwrap()
        };

        let mut handle = ConfigWatchHandle {
            rx,
            _debouncer: debouncer,
        };

        let events = handle.drain();
        assert_eq!(events.len(), 1, "duplicates should be removed");
    }
}
