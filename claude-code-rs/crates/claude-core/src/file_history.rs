//! File history — snapshot-based backup for file edits, supporting undo/rewind.
//!
//! Aligned with TS `utils/fileHistory.ts`:
//! - SHA-256 based backup naming (`{hash16}@v{version}`)
//! - Per-message snapshots with tracked file backups
//! - Rewind: restore all files to a previous snapshot state
//! - Diff stats: count insertions/deletions between snapshots
//! - MAX_SNAPSHOTS = 100 eviction policy

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use tracing::{debug, warn};

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of snapshots to keep per session.
pub const MAX_SNAPSHOTS: usize = 100;

// ── Types ────────────────────────────────────────────────────────────────────

/// A backup of a single file at a specific version.
#[derive(Debug, Clone)]
pub struct FileHistoryBackup {
    /// Backup file name (`{hash}@v{version}`), or None if file didn't exist.
    pub backup_file_name: Option<String>,
    /// Monotonically increasing version number.
    pub version: u32,
    /// When the backup was created.
    pub backup_time: SystemTime,
}

/// A snapshot of all tracked file backups at a particular message turn.
#[derive(Debug, Clone)]
pub struct FileHistorySnapshot {
    /// The message ID this snapshot is associated with.
    pub message_id: String,
    /// Map of tracking path → backup info for each tracked file.
    pub tracked_file_backups: HashMap<String, FileHistoryBackup>,
    /// When this snapshot was created.
    pub timestamp: SystemTime,
}

/// Diff statistics for a potential rewind operation.
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    /// Files that would change.
    pub files_changed: Vec<String>,
    /// Total lines added.
    pub insertions: usize,
    /// Total lines removed.
    pub deletions: usize,
}

/// Full file history state for a session.
#[derive(Debug)]
pub struct FileHistoryState {
    /// Session ID (used for backup directory isolation).
    session_id: String,
    /// Working directory for relative path resolution.
    cwd: PathBuf,
    /// Base directory for backups (`~/.claude/file-history/`).
    base_dir: PathBuf,
    /// Ordered list of snapshots (newest last).
    pub snapshots: Vec<FileHistorySnapshot>,
    /// Set of relative paths currently being tracked.
    pub tracked_files: HashSet<String>,
    /// Monotonically increasing counter (activity signal).
    pub snapshot_sequence: u64,
    /// Whether file history is enabled.
    pub enabled: bool,
}

impl FileHistoryState {
    /// Create a new file history state.
    pub fn new(session_id: String, cwd: PathBuf) -> Self {
        let base_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("file-history");

        Self {
            session_id,
            cwd,
            base_dir,
            snapshots: Vec::new(),
            tracked_files: HashSet::new(),
            snapshot_sequence: 0,
            enabled: true,
        }
    }

    /// Get the backup directory for this session.
    fn backup_dir(&self) -> PathBuf {
        self.base_dir.join(&self.session_id)
    }

    /// Resolve a backup file name to its full path.
    fn resolve_backup_path(&self, backup_file_name: &str) -> PathBuf {
        self.backup_dir().join(backup_file_name)
    }

    /// Shorten an absolute path to a relative path (for storage keys).
    fn shorten_path(&self, path: &str) -> String {
        let abs = Path::new(path);
        if abs.is_relative() {
            return path.to_string();
        }
        match abs.strip_prefix(&self.cwd) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => path.to_string(),
        }
    }

    /// Expand a relative tracking path to an absolute path.
    fn expand_path(&self, tracking_path: &str) -> PathBuf {
        let p = Path::new(tracking_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }

    /// Get the most recent snapshot (if any).
    fn most_recent_snapshot(&self) -> Option<&FileHistorySnapshot> {
        self.snapshots.last()
    }

    /// Get the latest backup for a tracked file from the most recent snapshot.
    fn latest_backup(&self, tracking_path: &str) -> Option<&FileHistoryBackup> {
        self.most_recent_snapshot()
            .and_then(|s| s.tracked_file_backups.get(tracking_path))
    }
}

// ── Backup file naming ───────────────────────────────────────────────────────

/// Compute the deterministic backup file name for a file path and version.
///
/// Format: `{sha256_hex_prefix_16}@v{version}`
pub fn get_backup_file_name(file_path: &str, version: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_path.as_bytes());
    let hash = hasher.finalize();
    let hex = hex_encode(&hash[..8]); // 16 hex chars from 8 bytes
    format!("{}@v{}", hex, version)
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ── Core operations ──────────────────────────────────────────────────────────

/// Track a file edit — create a v1 backup BEFORE the file is modified.
///
/// This is called before FileWrite/FileEdit tools modify a file.
/// If the file is already tracked in the current snapshot, this is a no-op.
pub fn track_edit(state: &mut FileHistoryState, file_path: &str) -> anyhow::Result<()> {
    if !state.enabled {
        return Ok(());
    }

    let tracking_path = state.shorten_path(file_path);

    // Check if already tracked in current snapshot
    if let Some(snapshot) = state.most_recent_snapshot() {
        if snapshot.tracked_file_backups.contains_key(&tracking_path) {
            debug!("File already tracked: {}", tracking_path);
            return Ok(());
        }
    }

    // Create backup
    let abs_path = state.expand_path(&tracking_path);
    let backup = create_backup(state, &abs_path, &tracking_path, 1)?;

    // Add to tracked files
    state.tracked_files.insert(tracking_path.clone());

    // Update or create the most recent snapshot
    if state.snapshots.is_empty() {
        // Create initial snapshot
        let mut backups = HashMap::new();
        backups.insert(tracking_path, backup);
        state.snapshots.push(FileHistorySnapshot {
            message_id: String::new(), // Will be set by make_snapshot
            tracked_file_backups: backups,
            timestamp: SystemTime::now(),
        });
    } else {
        let last = state.snapshots.last_mut().unwrap();
        last.tracked_file_backups.insert(tracking_path, backup);
    }

    Ok(())
}

/// Create a snapshot at the end of a turn (message boundary).
///
/// For each tracked file, checks if it changed since the last snapshot
/// and creates a new versioned backup if so.
pub fn make_snapshot(state: &mut FileHistoryState, message_id: &str) -> anyhow::Result<()> {
    if !state.enabled {
        return Ok(());
    }

    let tracked: Vec<String> = state.tracked_files.iter().cloned().collect();
    if tracked.is_empty() {
        return Ok(());
    }

    let mut backups = HashMap::new();

    for tracking_path in &tracked {
        let abs_path = state.expand_path(tracking_path);

        // Check if file exists
        if !abs_path.exists() {
            // File was deleted — record null backup
            let latest = state.latest_backup(tracking_path);
            let version = latest.map(|b| b.version + 1).unwrap_or(1);
            backups.insert(tracking_path.clone(), FileHistoryBackup {
                backup_file_name: None,
                version,
                backup_time: SystemTime::now(),
            });
            continue;
        }

        // Check if file changed since last backup
        let latest = state.latest_backup(tracking_path);
        match latest {
            Some(prev_backup) => {
                if let Some(ref prev_name) = prev_backup.backup_file_name {
                    let prev_path = state.resolve_backup_path(prev_name);
                    if !check_file_changed(&abs_path, &prev_path) {
                        // File unchanged — reuse previous backup
                        backups.insert(tracking_path.clone(), prev_backup.clone());
                        continue;
                    }
                }
                // File changed — create new version
                let next_version = prev_backup.version + 1;
                match create_backup(state, &abs_path, tracking_path, next_version) {
                    Ok(backup) => { backups.insert(tracking_path.clone(), backup); }
                    Err(e) => {
                        warn!("Failed to backup {}: {}", tracking_path, e);
                        backups.insert(tracking_path.clone(), prev_backup.clone());
                    }
                }
            }
            None => {
                // First backup for this file
                match create_backup(state, &abs_path, tracking_path, 1) {
                    Ok(backup) => { backups.insert(tracking_path.clone(), backup); }
                    Err(e) => {
                        warn!("Failed to backup {}: {}", tracking_path, e);
                    }
                }
            }
        }
    }

    // Create new snapshot
    let new_snapshot = FileHistorySnapshot {
        message_id: message_id.to_string(),
        tracked_file_backups: backups,
        timestamp: SystemTime::now(),
    };

    state.snapshots.push(new_snapshot);

    // Evict old snapshots if over limit
    if state.snapshots.len() > MAX_SNAPSHOTS {
        let drain_count = state.snapshots.len() - MAX_SNAPSHOTS;
        state.snapshots.drain(..drain_count);
    }

    state.snapshot_sequence += 1;
    debug!(
        "Created snapshot for message {} (seq={}, {} files)",
        message_id,
        state.snapshot_sequence,
        tracked.len()
    );

    Ok(())
}

/// Rewind all tracked files to the state at a given message ID.
///
/// Returns the list of files that were changed during rewind.
pub fn rewind(state: &mut FileHistoryState, message_id: &str) -> anyhow::Result<Vec<String>> {
    if !state.enabled {
        return Ok(Vec::new());
    }

    // Find target snapshot
    let target_idx = state.snapshots.iter()
        .position(|s| s.message_id == message_id)
        .ok_or_else(|| anyhow::anyhow!("Snapshot not found for message: {}", message_id))?;

    let target_snapshot = state.snapshots[target_idx].clone();
    let mut changed_files = Vec::new();

    for tracking_path in &state.tracked_files.clone() {
        let abs_path = state.expand_path(tracking_path);

        // Get target backup (what the file should look like)
        let target_backup = target_snapshot.tracked_file_backups.get(tracking_path.as_str());

        // If no backup at target, try to find the v1 backup
        let fallback_backup = if target_backup.is_none() {
            get_first_version_backup(state, tracking_path)
        } else {
            None
        };
        let effective_backup = target_backup.or(fallback_backup.as_ref());
        let backup_name = effective_backup.and_then(|b| b.backup_file_name.as_deref());

        match backup_name {
            None => {
                // File shouldn't exist at target — delete it
                if abs_path.exists() {
                    if let Err(e) = std::fs::remove_file(&abs_path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            warn!("Failed to delete {} during rewind: {}", tracking_path, e);
                        }
                    } else {
                        changed_files.push(tracking_path.clone());
                    }
                }
            }
            Some(name) => {
                let backup_path = state.resolve_backup_path(name);
                if check_file_changed(&abs_path, &backup_path) {
                    match restore_backup(&abs_path, &backup_path) {
                        Ok(()) => changed_files.push(tracking_path.clone()),
                        Err(e) => warn!("Failed to restore {}: {}", tracking_path, e),
                    }
                }
            }
        }
    }

    debug!(
        "Rewind to message {}: {} files changed",
        message_id,
        changed_files.len()
    );

    Ok(changed_files)
}

/// Get diff statistics for a potential rewind to a given message ID.
///
/// Does NOT modify any files — this is a preview operation.
pub fn get_diff_stats(state: &FileHistoryState, message_id: &str) -> anyhow::Result<DiffStats> {
    let target_idx = state.snapshots.iter()
        .position(|s| s.message_id == message_id)
        .ok_or_else(|| anyhow::anyhow!("Snapshot not found for message: {}", message_id))?;

    let target_snapshot = &state.snapshots[target_idx];
    let mut stats = DiffStats::default();

    for tracking_path in &state.tracked_files {
        let abs_path = state.expand_path(tracking_path);

        let target_backup = target_snapshot.tracked_file_backups.get(tracking_path.as_str());
        let backup_name = target_backup.and_then(|b| b.backup_file_name.as_deref());

        let current_content = read_file_or_empty(&abs_path);
        let backup_content = match backup_name {
            Some(name) => read_file_or_empty(&state.resolve_backup_path(name)),
            None => String::new(), // File didn't exist
        };

        if current_content != backup_content {
            stats.files_changed.push(tracking_path.clone());
            let diff = TextDiff::from_lines(&backup_content, &current_content);
            for change in diff.iter_all_changes() {
                match change.tag() {
                    ChangeTag::Insert => stats.deletions += 1, // Reverting: insert = deletion
                    ChangeTag::Delete => stats.insertions += 1, // Reverting: delete = insertion
                    ChangeTag::Equal => {}
                }
            }
        }
    }

    Ok(stats)
}

/// Check whether any tracked files have changes since the last snapshot.
pub fn has_any_changes(state: &FileHistoryState) -> bool {
    let snapshot = match state.most_recent_snapshot() {
        Some(s) => s,
        None => return false,
    };

    for (tracking_path, backup) in &snapshot.tracked_file_backups {
        let abs_path = state.expand_path(tracking_path);
        match &backup.backup_file_name {
            Some(name) => {
                let backup_path = state.resolve_backup_path(name);
                if check_file_changed(&abs_path, &backup_path) {
                    return true;
                }
            }
            None => {
                if abs_path.exists() {
                    return true; // File exists but shouldn't
                }
            }
        }
    }
    false
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Create a backup of a file.
fn create_backup(
    state: &FileHistoryState,
    source_path: &Path,
    _tracking_path: &str,
    version: u32,
) -> anyhow::Result<FileHistoryBackup> {
    if !source_path.exists() {
        // File doesn't exist — record null backup
        return Ok(FileHistoryBackup {
            backup_file_name: None,
            version,
            backup_time: SystemTime::now(),
        });
    }

    let abs_str = source_path.to_string_lossy();
    let backup_name = get_backup_file_name(&abs_str, version);
    let backup_path = state.resolve_backup_path(&backup_name);

    // Ensure backup directory exists
    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Copy file to backup location
    std::fs::copy(source_path, &backup_path)?;

    // Preserve permissions on Unix
    #[cfg(unix)]
    {
        if let Ok(meta) = source_path.metadata() {
            let _ = std::fs::set_permissions(&backup_path, meta.permissions());
        }
    }

    debug!("Created backup: {} (v{})", backup_name, version);

    Ok(FileHistoryBackup {
        backup_file_name: Some(backup_name),
        version,
        backup_time: SystemTime::now(),
    })
}

/// Restore a file from its backup.
fn restore_backup(target_path: &Path, backup_path: &Path) -> anyhow::Result<()> {
    if !backup_path.exists() {
        anyhow::bail!("Backup file not found: {:?}", backup_path);
    }

    // Ensure target directory exists
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::copy(backup_path, target_path)?;

    // Restore permissions on Unix
    #[cfg(unix)]
    {
        if let Ok(meta) = backup_path.metadata() {
            let _ = std::fs::set_permissions(target_path, meta.permissions());
        }
    }

    Ok(())
}

/// Check if two files differ (stat-based fast path, then content comparison).
fn check_file_changed(original: &Path, backup: &Path) -> bool {
    let orig_meta = match original.metadata() {
        Ok(m) => m,
        Err(_) => return !backup.exists(),
    };
    let back_meta = match backup.metadata() {
        Ok(m) => m,
        Err(_) => return true, // Backup missing, file exists = changed
    };

    // Quick check: different sizes = definitely changed
    if orig_meta.len() != back_meta.len() {
        return true;
    }

    // Quick check: original older than backup = not changed (optimization)
    if let (Ok(orig_mtime), Ok(back_mtime)) = (orig_meta.modified(), back_meta.modified()) {
        if orig_mtime < back_mtime {
            return false;
        }
    }

    // Content comparison (expensive but definitive)
    let orig_content = std::fs::read(original).unwrap_or_default();
    let back_content = std::fs::read(backup).unwrap_or_default();
    orig_content != back_content
}

/// Find the first v1 backup for a file across all snapshots.
fn get_first_version_backup(state: &FileHistoryState, tracking_path: &str) -> Option<FileHistoryBackup> {
    for snapshot in &state.snapshots {
        if let Some(backup) = snapshot.tracked_file_backups.get(tracking_path) {
            if backup.version == 1 {
                return Some(backup.clone());
            }
        }
    }
    None
}

/// Read a file to string, returning empty string on failure.
fn read_file_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Check if file history is enabled (respects env var).
pub fn file_history_enabled() -> bool {
    std::env::var("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING")
        .map(|v| !matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(true)
}

/// Copy file history backups from a previous session (for resume).
pub fn copy_history_for_resume(
    base_dir: &Path,
    prev_session_id: &str,
    new_session_id: &str,
) -> anyhow::Result<usize> {
    let src_dir = base_dir.join(prev_session_id);
    let dst_dir = base_dir.join(new_session_id);

    if !src_dir.exists() {
        return Ok(0);
    }

    std::fs::create_dir_all(&dst_dir)?;

    let mut copied = 0;
    for entry in std::fs::read_dir(&src_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let dst_path = dst_dir.join(entry.file_name());
            // Try hard link first, fall back to copy
            #[cfg(unix)]
            {
                if std::fs::hard_link(entry.path(), &dst_path).is_err() {
                    std::fs::copy(entry.path(), &dst_path)?;
                }
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(entry.path(), &dst_path)?;
            }
            copied += 1;
        }
    }

    debug!("Copied {} backup files from session {} to {}", copied, prev_session_id, new_session_id);
    Ok(copied)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(tmp: &Path) -> FileHistoryState {
        let mut state = FileHistoryState::new("test-session".to_string(), tmp.to_path_buf());
        state.base_dir = tmp.join("file-history");
        state
    }

    #[test]
    fn backup_file_name_deterministic() {
        let name1 = get_backup_file_name("/home/user/file.rs", 1);
        let name2 = get_backup_file_name("/home/user/file.rs", 1);
        assert_eq!(name1, name2);
        assert!(name1.ends_with("@v1"));
        assert_eq!(name1.len(), 16 + "@v1".len());
    }

    #[test]
    fn backup_file_name_different_versions() {
        let v1 = get_backup_file_name("/file.rs", 1);
        let v2 = get_backup_file_name("/file.rs", 2);
        assert_ne!(v1, v2);
        assert!(v1.contains("@v1"));
        assert!(v2.contains("@v2"));
        // Hash prefix should be the same (same path)
        assert_eq!(&v1[..16], &v2[..16]);
    }

    #[test]
    fn backup_file_name_different_paths() {
        let a = get_backup_file_name("/a.rs", 1);
        let b = get_backup_file_name("/b.rs", 1);
        assert_ne!(&a[..16], &b[..16]); // Different hash prefixes
    }

    #[test]
    fn shorten_and_expand_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let state = FileHistoryState::new("s1".to_string(), cwd.clone());

        let file_path = cwd.join("src").join("main.rs");
        let shortened = state.shorten_path(&file_path.to_string_lossy());
        let expected_rel = PathBuf::from("src").join("main.rs");
        assert_eq!(shortened, expected_rel.to_string_lossy());

        let expanded = state.expand_path(&shortened);
        assert_eq!(expanded, cwd.join("src").join("main.rs"));
    }

    #[test]
    fn shorten_path_outside_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let state = FileHistoryState::new("s1".to_string(), cwd);

        let other = tmp.path().join("other").join("file.rs");
        let shortened = state.shorten_path(&other.to_string_lossy());
        assert_eq!(shortened, other.to_string_lossy());
    }

    #[test]
    fn track_edit_creates_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        // Create a file to track
        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();

        assert!(state.tracked_files.contains("test.rs"));
        assert_eq!(state.snapshots.len(), 1);
        let backup = state.snapshots[0].tracked_file_backups.get("test.rs").unwrap();
        assert!(backup.backup_file_name.is_some());
        assert_eq!(backup.version, 1);
    }

    #[test]
    fn track_edit_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("new_file.rs");
        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();

        let backup = state.snapshots[0].tracked_file_backups.get("new_file.rs").unwrap();
        assert!(backup.backup_file_name.is_none()); // File doesn't exist yet
    }

    #[test]
    fn track_edit_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "content").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();

        // Should still have only one backup
        assert_eq!(state.snapshots[0].tracked_file_backups.len(), 1);
    }

    #[test]
    fn make_snapshot_unchanged_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "content").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        let v1 = state.snapshots[0].tracked_file_backups.get("test.rs")
            .unwrap().version;

        make_snapshot(&mut state, "msg-1").unwrap();

        let last = state.snapshots.last().unwrap();
        let backup = last.tracked_file_backups.get("test.rs").unwrap();
        assert_eq!(backup.version, v1); // Unchanged, version stays the same
    }

    #[test]
    fn make_snapshot_changed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "v1 content").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();

        // Modify file
        std::fs::write(&file_path, "v2 content").unwrap();

        make_snapshot(&mut state, "msg-1").unwrap();

        let last = state.snapshots.last().unwrap();
        let backup = last.tracked_file_backups.get("test.rs").unwrap();
        assert_eq!(backup.version, 2); // New version created
    }

    #[test]
    fn rewind_restores_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "original").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        make_snapshot(&mut state, "msg-1").unwrap();

        // Modify file
        std::fs::write(&file_path, "modified").unwrap();
        make_snapshot(&mut state, "msg-2").unwrap();

        // Rewind to msg-1
        let changed = rewind(&mut state, "msg-1").unwrap();
        assert!(!changed.is_empty());

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "original");
    }

    #[test]
    fn rewind_deletes_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("new.rs");
        // File doesn't exist initially
        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        make_snapshot(&mut state, "msg-1").unwrap();

        // Create the file
        std::fs::write(&file_path, "created later").unwrap();
        assert!(file_path.exists());

        // Rewind to msg-1 (file shouldn't exist)
        rewind(&mut state, "msg-1").unwrap();
        assert!(!file_path.exists());
    }

    #[test]
    fn diff_stats_calculates_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        make_snapshot(&mut state, "msg-1").unwrap();

        // Modify file
        std::fs::write(&file_path, "line1\nchanged\nline3\nnew_line\n").unwrap();

        let stats = get_diff_stats(&state, "msg-1").unwrap();
        assert!(!stats.files_changed.is_empty());
        assert!(stats.insertions > 0 || stats.deletions > 0);
    }

    #[test]
    fn has_any_changes_false_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "content").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        make_snapshot(&mut state, "msg-1").unwrap();

        assert!(!has_any_changes(&state));
    }

    #[test]
    fn has_any_changes_true_when_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());

        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "content").unwrap();

        track_edit(&mut state, &file_path.to_string_lossy()).unwrap();
        make_snapshot(&mut state, "msg-1").unwrap();

        std::fs::write(&file_path, "modified").unwrap();
        assert!(has_any_changes(&state));
    }

    #[test]
    fn max_snapshots_eviction() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());
        state.tracked_files.insert("dummy".to_string());

        for i in 0..150 {
            state.snapshots.push(FileHistorySnapshot {
                message_id: format!("msg-{}", i),
                tracked_file_backups: HashMap::new(),
                timestamp: SystemTime::now(),
            });
        }

        make_snapshot(&mut state, "msg-final").unwrap();
        assert!(state.snapshots.len() <= MAX_SNAPSHOTS);
    }

    #[test]
    fn file_history_env_disabled() {
        std::env::set_var("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING", "1");
        assert!(!file_history_enabled());
        std::env::remove_var("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING");
    }

    #[test]
    fn copy_history_for_resume_no_source() {
        let tmp = tempfile::tempdir().unwrap();
        let result = copy_history_for_resume(tmp.path(), "old", "new").unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn copy_history_for_resume_copies_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("old-session");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("abc123@v1"), "backup data").unwrap();
        std::fs::write(src.join("abc123@v2"), "backup data v2").unwrap();

        let result = copy_history_for_resume(tmp.path(), "old-session", "new-session").unwrap();
        assert_eq!(result, 2);

        let dst = tmp.path().join("new-session");
        assert!(dst.join("abc123@v1").exists());
        assert!(dst.join("abc123@v2").exists());
    }
}
