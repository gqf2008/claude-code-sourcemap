//! File conflict tracker for multi-agent swarms.
//!
//! Tracks which agents are editing which files. When two agents try to edit
//! the same file, a conflict is detected. This enables the coordinator to
//! either warn, serialize the edits, or ask one agent to wait.
//!
//! Uses `std::sync::RwLock<HashMap>` for thread-safe concurrent access.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::Serialize;

/// Information about a file currently being edited.
#[derive(Debug, Clone, Serialize)]
pub struct FileLock {
    /// Agent that holds the lock.
    pub agent_id: String,
    /// Absolute path to the file.
    pub file_path: String,
    /// Tool that acquired the lock (e.g. "FileEdit", "FileWrite").
    pub tool_name: String,
    /// When the lock was acquired.
    #[serde(skip)]
    pub acquired_at: Instant,
}

/// Result of attempting to acquire a file lock.
#[derive(Debug)]
pub enum LockResult {
    /// Lock acquired successfully.
    Acquired,
    /// Conflict — another agent already holds the lock.
    Conflict {
        holder: String,
        file_path: String,
        held_since_ms: u64,
    },
}

/// Thread-safe file conflict tracker.
#[derive(Clone)]
pub struct FileConflictTracker {
    /// Map: normalized file path → FileLock.
    locks: Arc<RwLock<HashMap<String, FileLock>>>,
}

impl FileConflictTracker {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Try to acquire a lock on a file for a given agent.
    ///
    /// - If the file is not locked, acquires it and returns `Acquired`.
    /// - If the same agent already holds it, refreshes the lock and returns `Acquired`.
    /// - If a different agent holds it, returns `Conflict`.
    pub fn try_lock(
        &self,
        file_path: &str,
        agent_id: &str,
        tool_name: &str,
    ) -> LockResult {
        let key = normalize_path(file_path);
        let mut locks = self.locks.write().unwrap();

        if let Some(existing) = locks.get(&key) {
            if existing.agent_id == agent_id {
                // Same agent — refresh the lock
                locks.insert(
                    key,
                    FileLock {
                        agent_id: agent_id.to_string(),
                        file_path: file_path.to_string(),
                        tool_name: tool_name.to_string(),
                        acquired_at: Instant::now(),
                    },
                );
                return LockResult::Acquired;
            }
            return LockResult::Conflict {
                holder: existing.agent_id.clone(),
                file_path: file_path.to_string(),
                held_since_ms: existing.acquired_at.elapsed().as_millis() as u64,
            };
        }

        // No existing lock — acquire
        locks.insert(
            key,
            FileLock {
                agent_id: agent_id.to_string(),
                file_path: file_path.to_string(),
                tool_name: tool_name.to_string(),
                acquired_at: Instant::now(),
            },
        );
        LockResult::Acquired
    }

    /// Release a lock on a file. Only the holder can release.
    pub fn release(&self, file_path: &str, agent_id: &str) -> bool {
        let key = normalize_path(file_path);
        let mut locks = self.locks.write().unwrap();
        if let Some(existing) = locks.get(&key) {
            if existing.agent_id == agent_id {
                locks.remove(&key);
                return true;
            }
        }
        false
    }

    /// Release all locks held by a specific agent (cleanup on agent exit).
    pub fn release_all(&self, agent_id: &str) -> usize {
        let mut locks = self.locks.write().unwrap();
        let keys_to_remove: Vec<String> = locks
            .iter()
            .filter(|(_, v)| v.agent_id == agent_id)
            .map(|(k, _)| k.clone())
            .collect();

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            locks.remove(&key);
        }
        count
    }

    /// Get all current locks.
    pub fn active_locks(&self) -> Vec<FileLock> {
        let locks = self.locks.read().unwrap();
        locks.values().cloned().collect()
    }

    /// Get locks held by a specific agent.
    pub fn locks_by_agent(&self, agent_id: &str) -> Vec<FileLock> {
        let locks = self.locks.read().unwrap();
        locks
            .values()
            .filter(|v| v.agent_id == agent_id)
            .cloned()
            .collect()
    }

    /// Get a conflict summary: files locked by multiple agents (shouldn't happen
    /// with try_lock, but useful for diagnostics).
    pub fn conflict_summary(&self) -> HashMap<String, Vec<String>> {
        let locks = self.locks.read().unwrap();
        let mut by_file: HashMap<String, Vec<String>> = HashMap::new();
        for v in locks.values() {
            by_file
                .entry(v.file_path.clone())
                .or_default()
                .push(v.agent_id.clone());
        }
        // Only return files with 2+ agents (shouldn't happen normally)
        by_file.retain(|_, agents| agents.len() > 1);
        by_file
    }

    /// Number of currently held locks.
    pub fn lock_count(&self) -> usize {
        let locks = self.locks.read().unwrap();
        locks.len()
    }
}

impl Default for FileConflictTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize a file path for consistent map keys.
fn normalize_path(path: &str) -> String {
    // Replace backslashes with forward slashes for consistency
    let normalized = path.replace('\\', "/");
    // Remove trailing slash
    normalized.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release() {
        let tracker = FileConflictTracker::new();
        let result = tracker.try_lock("/src/main.rs", "agent-1", "FileEdit");
        assert!(matches!(result, LockResult::Acquired));
        assert_eq!(tracker.lock_count(), 1);

        assert!(tracker.release("/src/main.rs", "agent-1"));
        assert_eq!(tracker.lock_count(), 0);
    }

    #[test]
    fn conflict_detection() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/src/main.rs", "agent-1", "FileEdit");

        let result = tracker.try_lock("/src/main.rs", "agent-2", "FileWrite");
        match result {
            LockResult::Conflict {
                holder,
                file_path,
                ..
            } => {
                assert_eq!(holder, "agent-1");
                assert_eq!(file_path, "/src/main.rs");
            }
            _ => panic!("Expected conflict"),
        }
    }

    #[test]
    fn same_agent_refreshes_lock() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/src/main.rs", "agent-1", "FileEdit");
        let result = tracker.try_lock("/src/main.rs", "agent-1", "FileWrite");
        assert!(matches!(result, LockResult::Acquired));
        assert_eq!(tracker.lock_count(), 1);
    }

    #[test]
    fn release_wrong_agent_fails() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/src/main.rs", "agent-1", "FileEdit");
        assert!(!tracker.release("/src/main.rs", "agent-2"));
        assert_eq!(tracker.lock_count(), 1);
    }

    #[test]
    fn release_all_by_agent() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/a.rs", "agent-1", "FileEdit");
        tracker.try_lock("/b.rs", "agent-1", "FileEdit");
        tracker.try_lock("/c.rs", "agent-2", "FileEdit");

        let released = tracker.release_all("agent-1");
        assert_eq!(released, 2);
        assert_eq!(tracker.lock_count(), 1);
    }

    #[test]
    fn locks_by_agent_filter() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/a.rs", "agent-1", "FileEdit");
        tracker.try_lock("/b.rs", "agent-2", "FileEdit");
        tracker.try_lock("/c.rs", "agent-1", "FileWrite");

        let locks = tracker.locks_by_agent("agent-1");
        assert_eq!(locks.len(), 2);
    }

    #[test]
    fn normalize_path_consistency() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("src\\main.rs", "agent-1", "FileEdit");

        // Forward slash should conflict with backslash
        let result = tracker.try_lock("src/main.rs", "agent-2", "FileWrite");
        assert!(matches!(result, LockResult::Conflict { .. }));
    }

    #[test]
    fn active_locks_returns_all() {
        let tracker = FileConflictTracker::new();
        tracker.try_lock("/a.rs", "agent-1", "FileEdit");
        tracker.try_lock("/b.rs", "agent-2", "FileEdit");

        let locks = tracker.active_locks();
        assert_eq!(locks.len(), 2);
    }

    #[test]
    fn empty_tracker() {
        let tracker = FileConflictTracker::new();
        assert_eq!(tracker.lock_count(), 0);
        assert!(tracker.active_locks().is_empty());
        assert!(tracker.conflict_summary().is_empty());
    }
}
