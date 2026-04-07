//! Write queue — batched, async JSONL flush for session transcripts.
//!
//! Aligned with TS `sessionStorage.ts` `enqueueWrite()` / `drainWriteQueue()`:
//! - Entries are enqueued immediately (non-blocking)
//! - A background timer flushes every 100ms
//! - Graceful shutdown forces a final flush
//!
//! Thread-safe: can be shared across tokio tasks via `Arc<WriteQueue>`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, warn};

/// Default flush interval (100ms), matching TS `FLUSH_INTERVAL_MS`.
const FLUSH_INTERVAL_MS: u64 = 100;

/// Maximum bytes per flush batch (100MB safety cap).
const MAX_CHUNK_BYTES: usize = 100 * 1024 * 1024;

/// A queued write entry: serialized JSONL line + target file.
#[derive(Debug, Clone)]
struct QueuedEntry {
    path: PathBuf,
    line: String,
}

/// Batched write queue for JSONL transcript files.
///
/// Usage:
/// ```ignore
/// let queue = WriteQueue::start();
/// queue.enqueue("/path/to/session.jsonl", "{...}\n");
/// // ... entries are flushed every 100ms ...
/// queue.flush().await; // force flush on shutdown
/// ```
pub struct WriteQueue {
    tx: mpsc::UnboundedSender<QueuedEntry>,
    /// Signal to force an immediate flush.
    flush_notify: Arc<Notify>,
    /// Shared state for shutdown coordination.
    state: Arc<Mutex<WriteQueueState>>,
}

struct WriteQueueState {
    /// Number of entries currently buffered (approximate).
    buffered_count: usize,
    /// Total bytes written since creation.
    total_bytes_written: u64,
    /// Total entries written.
    total_entries_written: u64,
    /// Whether the queue has been shut down.
    shutdown: bool,
}

/// Deduplication guard — tracks UUIDs to prevent duplicate transcript entries.
///
/// Aligned with TS `sessionStorage.ts` `messageSet` tracking.
#[derive(Debug, Default)]
pub struct DeduplicationGuard {
    seen: std::collections::HashSet<String>,
}

impl DeduplicationGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this UUID has been seen before. Returns `true` if new (not a dup).
    pub fn check_and_insert(&mut self, uuid: &str) -> bool {
        self.seen.insert(uuid.to_string())
    }

    /// Check without inserting.
    pub fn contains(&self, uuid: &str) -> bool {
        self.seen.contains(uuid)
    }

    /// Number of tracked UUIDs.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether no UUIDs are tracked.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Bulk-load UUIDs (for restoring from a transcript).
    pub fn load_from_entries(&mut self, uuids: impl IntoIterator<Item = String>) {
        for uuid in uuids {
            self.seen.insert(uuid);
        }
    }
}

impl WriteQueue {
    /// Start the write queue with a background flush timer.
    ///
    /// Spawns a tokio task that drains the queue every `FLUSH_INTERVAL_MS`.
    pub fn start() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let flush_notify = Arc::new(Notify::new());
        let state = Arc::new(Mutex::new(WriteQueueState {
            buffered_count: 0,
            total_bytes_written: 0,
            total_entries_written: 0,
            shutdown: false,
        }));

        let queue = Arc::new(Self {
            tx,
            flush_notify: Arc::clone(&flush_notify),
            state: Arc::clone(&state),
        });

        // Spawn background drain task
        let flush_notify_clone = Arc::clone(&flush_notify);
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            drain_loop(rx, flush_notify_clone, state_clone).await;
        });

        queue
    }

    /// Enqueue a JSONL line for writing (non-blocking).
    ///
    /// The line should already include a trailing newline.
    pub fn enqueue(&self, path: impl Into<PathBuf>, line: impl Into<String>) {
        let entry = QueuedEntry {
            path: path.into(),
            line: line.into(),
        };
        if self.tx.send(entry).is_err() {
            warn!("WriteQueue: channel closed, entry dropped");
        }
    }

    /// Force an immediate flush of all buffered entries.
    ///
    /// Call this on graceful shutdown to ensure no data is lost.
    pub async fn flush(&self) {
        self.flush_notify.notify_one();
        // Give the drain loop a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// Shut down the write queue, flushing remaining entries.
    pub async fn shutdown(&self) {
        {
            let mut state = self.state.lock().await;
            state.shutdown = true;
        }
        self.flush_notify.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    /// Get queue statistics.
    pub async fn stats(&self) -> (u64, u64) {
        let state = self.state.lock().await;
        (state.total_entries_written, state.total_bytes_written)
    }
}

/// Background drain loop — flushes queued entries every FLUSH_INTERVAL_MS.
async fn drain_loop(
    mut rx: mpsc::UnboundedReceiver<QueuedEntry>,
    flush_notify: Arc<Notify>,
    state: Arc<Mutex<WriteQueueState>>,
) {
    let interval = std::time::Duration::from_millis(FLUSH_INTERVAL_MS);
    let mut buffer: Vec<QueuedEntry> = Vec::new();

    loop {
        // Wait for either: interval timeout, flush signal, or new entry
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = flush_notify.notified() => {}
            entry = rx.recv() => {
                match entry {
                    Some(e) => buffer.push(e),
                    None => {
                        // Channel closed — flush remaining and exit
                        flush_buffer(&mut buffer, &state).await;
                        return;
                    }
                }
            }
        }

        // Drain all available entries from the channel
        while let Ok(entry) = rx.try_recv() {
            buffer.push(entry);
        }

        // Flush the buffer
        if !buffer.is_empty() {
            flush_buffer(&mut buffer, &state).await;
        }

        // Check shutdown
        let shutdown = {
            let s = state.lock().await;
            s.shutdown
        };
        if shutdown {
            // Final drain
            while let Ok(entry) = rx.try_recv() {
                buffer.push(entry);
            }
            flush_buffer(&mut buffer, &state).await;
            return;
        }
    }
}

/// Flush buffered entries to disk, grouped by file path.
async fn flush_buffer(
    buffer: &mut Vec<QueuedEntry>,
    state: &Arc<Mutex<WriteQueueState>>,
) {
    use std::collections::HashMap;
    use std::io::Write;

    if buffer.is_empty() {
        return;
    }

    // Group by file path
    let mut by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut total_bytes = 0usize;

    for entry in buffer.drain(..) {
        total_bytes += entry.line.len();
        by_file.entry(entry.path).or_default().push(entry.line);

        if total_bytes >= MAX_CHUNK_BYTES {
            break;
        }
    }

    // Write each file
    let entry_count = by_file.values().map(|v| v.len()).sum::<usize>();

    for (path, lines) in &by_file {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(mut file) => {
                let combined: String = lines.join("");
                if let Err(e) = file.write_all(combined.as_bytes()) {
                    warn!("WriteQueue: failed to write to {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                warn!("WriteQueue: failed to open {}: {}", path.display(), e);
            }
        }
    }

    // Update stats
    {
        let mut s = state.lock().await;
        s.total_entries_written += entry_count as u64;
        s.total_bytes_written += total_bytes as u64;
        s.buffered_count = 0;
    }

    debug!("WriteQueue: flushed {} entries, {} bytes", entry_count, total_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_guard_new_uuid() {
        let mut guard = DeduplicationGuard::new();
        assert!(guard.check_and_insert("uuid-1"));
        assert!(!guard.check_and_insert("uuid-1")); // duplicate
        assert!(guard.check_and_insert("uuid-2"));
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn dedup_guard_contains() {
        let mut guard = DeduplicationGuard::new();
        guard.check_and_insert("abc");
        assert!(guard.contains("abc"));
        assert!(!guard.contains("xyz"));
    }

    #[test]
    fn dedup_guard_bulk_load() {
        let mut guard = DeduplicationGuard::new();
        guard.load_from_entries(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(guard.len(), 3);
        assert!(!guard.check_and_insert("a")); // already loaded
        assert!(guard.check_and_insert("d")); // new
    }

    #[test]
    fn dedup_guard_empty() {
        let guard = DeduplicationGuard::new();
        assert!(guard.is_empty());
        assert_eq!(guard.len(), 0);
    }

    #[tokio::test]
    async fn write_queue_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let queue = WriteQueue::start();
        queue.enqueue(&path, "{\"type\":\"user\"}\n");
        queue.enqueue(&path, "{\"type\":\"assistant\"}\n");

        // Force flush
        queue.flush().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"user\""));
        assert!(content.contains("\"assistant\""));

        let (entries, bytes) = queue.stats().await;
        assert!(entries >= 2);
        assert!(bytes > 0);
    }

    #[tokio::test]
    async fn write_queue_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let path1 = dir.path().join("session1.jsonl");
        let path2 = dir.path().join("session2.jsonl");

        let queue = WriteQueue::start();
        queue.enqueue(&path1, "line1\n");
        queue.enqueue(&path2, "line2\n");

        queue.flush().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(std::fs::read_to_string(&path1).unwrap().contains("line1"));
        assert!(std::fs::read_to_string(&path2).unwrap().contains("line2"));
    }

    #[tokio::test]
    async fn write_queue_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shutdown.jsonl");

        let queue = WriteQueue::start();
        queue.enqueue(&path, "before-shutdown\n");
        queue.shutdown().await;

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("before-shutdown"));
    }
}
