//! Scheduler lease lock for `.claude/scheduled_tasks.lock`.
//!
//! When multiple sessions run in the same project directory, only one should
//! drive the cron scheduler. The first session to acquire this lock becomes
//! the scheduler; others stay passive and periodically probe.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const LOCK_FILE_REL: &str = ".claude/scheduled_tasks.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchedulerLock {
    session_id: String,
    pid: u32,
    acquired_at: i64,
}

fn get_lock_path(dir: &Path) -> PathBuf {
    dir.join(LOCK_FILE_REL)
}

fn read_lock_sync(dir: &Path) -> Option<SchedulerLock> {
    let raw = std::fs::read_to_string(get_lock_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn is_process_running(pid: u32) -> bool {
    // Use a portable approach: try to find the process via std::process::Command
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Try to create the lock file exclusively. Returns true on success.
async fn try_create_exclusive(lock: &SchedulerLock, dir: &Path) -> std::io::Result<bool> {
    let path = get_lock_path(dir);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let body = serde_json::to_string(lock)?;

    // Use OpenOptions with create_new for atomic exclusive create
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
    {
        Ok(_file) => {
            // File created successfully, now write the content
            tokio::fs::write(&path, &body).await?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Try to acquire the scheduler lock for the current session.
/// Returns true on success, false if another live session holds it.
pub async fn try_acquire_scheduler_lock(
    dir: &Path,
    session_id: &str,
) -> std::io::Result<bool> {
    let pid = std::process::id();
    let lock = SchedulerLock {
        session_id: session_id.to_string(),
        pid,
        acquired_at: chrono::Utc::now().timestamp_millis(),
    };

    if try_create_exclusive(&lock, dir).await? {
        tracing::debug!(pid, "acquired scheduler lock");
        return Ok(true);
    }

    let existing = read_lock_sync(dir);

    // Already ours (idempotent)
    if let Some(ref ex) = existing {
        if ex.session_id == session_id {
            if ex.pid != pid {
                // Update PID (e.g. after --resume)
                let body = serde_json::to_string(&lock)?;
                tokio::fs::write(get_lock_path(dir), body).await?;
            }
            return Ok(true);
        }
    }

    // Another live session holds it
    if let Some(ref ex) = existing {
        if is_process_running(ex.pid) {
            tracing::debug!(
                held_by = %ex.session_id,
                held_pid = ex.pid,
                "scheduler lock held by another session"
            );
            return Ok(false);
        }
    }

    // Stale lock — unlink and retry once
    if let Some(ref ex) = existing {
        tracing::debug!(stale_pid = ex.pid, "recovering stale scheduler lock");
    }
    let _ = tokio::fs::remove_file(get_lock_path(dir)).await;

    if try_create_exclusive(&lock, dir).await? {
        tracing::debug!(pid, "acquired scheduler lock (stale recovery)");
        Ok(true)
    } else {
        Ok(false) // Another session won the race
    }
}

/// Release the scheduler lock if the current session owns it.
pub async fn release_scheduler_lock(dir: &Path, session_id: &str) -> std::io::Result<()> {
    let existing = read_lock_sync(dir);
    if let Some(ex) = existing {
        if ex.session_id != session_id {
            return Ok(());
        }
    } else {
        return Ok(());
    }
    match tokio::fs::remove_file(get_lock_path(dir)).await {
        Ok(()) => {
            tracing::debug!("released scheduler lock");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_acquire_and_release() {
        let dir = TempDir::new().unwrap();
        let acquired = try_acquire_scheduler_lock(dir.path(), "session1").await.unwrap();
        assert!(acquired);

        // Acquiring again with same session is idempotent
        let acquired2 = try_acquire_scheduler_lock(dir.path(), "session1").await.unwrap();
        assert!(acquired2);

        release_scheduler_lock(dir.path(), "session1").await.unwrap();

        // Now another session can acquire
        let acquired3 = try_acquire_scheduler_lock(dir.path(), "session2").await.unwrap();
        assert!(acquired3);
    }

    #[tokio::test]
    async fn test_different_session_blocked() {
        let dir = TempDir::new().unwrap();
        let acquired = try_acquire_scheduler_lock(dir.path(), "session1").await.unwrap();
        assert!(acquired);

        // Different session blocked (current PID is running)
        let acquired2 = try_acquire_scheduler_lock(dir.path(), "session2").await.unwrap();
        assert!(!acquired2);
    }

    #[tokio::test]
    async fn test_stale_lock_recovery() {
        let dir = TempDir::new().unwrap();
        // Write a lock with a dead PID
        let stale = SchedulerLock {
            session_id: "dead_session".to_string(),
            pid: 999999999, // unlikely to be running
            acquired_at: 0,
        };
        let path = get_lock_path(dir.path());
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, serde_json::to_string(&stale).unwrap())
            .await
            .unwrap();

        // Should recover stale lock
        let acquired = try_acquire_scheduler_lock(dir.path(), "new_session").await.unwrap();
        assert!(acquired);
    }

    #[test]
    fn test_current_process_is_running() {
        assert!(is_process_running(std::process::id()));
    }

    #[test]
    fn test_dead_pid_not_running() {
        // PID 999999999 is almost certainly not running
        assert!(!is_process_running(999999999));
    }
}
