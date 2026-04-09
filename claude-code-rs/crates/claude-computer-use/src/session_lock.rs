//! File-based session lock to prevent concurrent Computer Use sessions.
//!
//! Only one Computer Use session should control the desktop at a time.
//! The lock is automatically released when the `SessionLock` is dropped.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, warn};

/// File-based session lock for Computer Use.
///
/// Prevents multiple instances from simultaneously controlling the desktop.
/// Creates a lock file containing the PID; drops the file on release.
pub struct SessionLock {
    path: PathBuf,
}

impl SessionLock {
    /// Default lock file path.
    fn default_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("claude-code-rs")
            .join("computer-use.lock")
    }

    /// Attempt to acquire the session lock.
    ///
    /// Returns `Ok(lock)` if acquired, `Err` if another session holds the lock.
    pub fn acquire() -> anyhow::Result<Self> {
        Self::acquire_at(Self::default_path())
    }

    /// Acquire a lock at a specific path (useful for testing).
    fn acquire_at(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Check for stale lock
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    if is_process_alive(pid) {
                        anyhow::bail!(
                            "Computer Use session already active (PID {pid}). \
                             Only one session can control the desktop at a time."
                        );
                    }
                    debug!(pid, "Removing stale Computer Use lock");
                }
            }
        }

        // Write our PID
        let mut file = fs::File::create(&path)?;
        write!(file, "{}", std::process::id())?;
        debug!(path = %path.display(), "Acquired Computer Use session lock");

        Ok(Self { path })
    }

    /// Check if the lock is currently held (by us).
    pub fn is_held(&self) -> bool {
        self.path.exists()
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            warn!(error = %e, "Failed to release Computer Use session lock");
        } else {
            debug!("Released Computer Use session lock");
        }
    }
}

/// Check if a process with the given PID is still running.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    #[cfg(unix)]
    {
        let path = format!("/proc/{pid}");
        std::path::Path::new(&path).exists()
    }

    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("test.lock");
        let lock = SessionLock::acquire_at(lock_path.clone()).expect("should acquire");
        assert!(lock.is_held());
        drop(lock);
        assert!(!lock_path.exists(), "lock file should be removed after drop");
    }

    #[test]
    fn stale_lock_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("test.lock");

        // Write a lock file with a non-existent PID
        fs::write(&lock_path, "99999999").unwrap();

        // Should recover from stale lock
        let lock = SessionLock::acquire_at(lock_path).expect("should recover stale lock");
        assert!(lock.is_held());
        drop(lock);
    }

    #[test]
    fn process_alive_self() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn process_alive_nonexistent() {
        assert!(!is_process_alive(99_999_999));
    }
}
