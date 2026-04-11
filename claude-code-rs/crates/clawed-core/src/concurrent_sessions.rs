//! Concurrent session management — PID-based session registration and tracking.
//!
//! Aligned with TS `utils/concurrentSessions.ts`:
//! - PID file registration in `~/.claude/sessions/{pid}.json`
//! - Session kind and status tracking
//! - Stale PID cleanup (skip on WSL)
//! - RAII cleanup via Drop

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::debug;

// ── Constants ────────────────────────────────────────────────────────────────

/// Directory permissions for sessions dir (owner-only).
#[cfg(unix)]
const SESSIONS_DIR_MODE: u32 = 0o700;

/// PID file name validation regex pattern.
const PID_FILE_PATTERN: &str = r"^\d+\.json$";

/// Compiled regex for PID file names (avoids recompilation on every call).
static PID_FILE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(PID_FILE_PATTERN).expect("PID_FILE_PATTERN is valid"));

// ── Types ────────────────────────────────────────────────────────────────────

/// The kind of session (how it was launched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    Interactive,
    Bg,
    Daemon,
    DaemonWorker,
}

impl SessionKind {
    /// Parse from environment variable value.
    pub fn from_env(val: &str) -> Option<Self> {
        match val {
            "bg" => Some(Self::Bg),
            "daemon" => Some(Self::Daemon),
            "daemon-worker" => Some(Self::DaemonWorker),
            "interactive" => Some(Self::Interactive),
            _ => None,
        }
    }
}

impl std::fmt::Display for SessionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interactive => write!(f, "interactive"),
            Self::Bg => write!(f, "bg"),
            Self::Daemon => write!(f, "daemon"),
            Self::DaemonWorker => write!(f, "daemon-worker"),
        }
    }
}

/// Activity status of a running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Busy,
    Idle,
    Waiting,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "busy"),
            Self::Idle => write!(f, "idle"),
            Self::Waiting => write!(f, "waiting"),
        }
    }
}

/// Content of a PID file (`~/.claude/sessions/{pid}.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidFileContent {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub started_at: i64,
    pub kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

// ── Session directory helpers ────────────────────────────────────────────────

/// Get the sessions directory path (`~/.claude/sessions/`).
pub fn sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("sessions"))
}

/// Ensure the sessions directory exists with correct permissions.
fn ensure_sessions_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(SESSIONS_DIR_MODE);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

/// Get PID file path for a given process ID.
fn pid_file_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{}.json", pid))
}

// ── Platform detection ───────────────────────────────────────────────────────

/// Detect if running under WSL.
pub fn is_wsl() -> bool {
    if cfg!(target_os = "linux") {
        // Check /proc/version for Microsoft
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            return version.to_lowercase().contains("microsoft");
        }
    }
    false
}

/// Check if a process is running by PID.
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

// ── Environment helpers ──────────────────────────────────────────────────────

/// Read session kind from environment variable.
pub fn env_session_kind() -> Option<SessionKind> {
    std::env::var("CLAUDE_CODE_SESSION_KIND")
        .ok()
        .and_then(|v| SessionKind::from_env(&v))
}

/// Check if this is a background session.
pub fn is_bg_session() -> bool {
    env_session_kind() == Some(SessionKind::Bg)
}

// ── Session registration ─────────────────────────────────────────────────────

/// RAII guard that deletes the PID file on drop.
pub struct SessionGuard {
    pid_file: PathBuf,
    active: Arc<AtomicBool>,
}

impl SessionGuard {
    /// Update activity status in the PID file.
    pub fn update_status(&self, status: SessionStatus, waiting_for: Option<&str>) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = update_pid_file(&self.pid_file, status, waiting_for) {
            debug!("Failed to update session status: {}", e);
        }
    }

    /// Update the session ID in the PID file (e.g., after `/resume`).
    pub fn update_session_id(&self, new_session_id: &str) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(data) = std::fs::read_to_string(&self.pid_file) {
            if let Ok(mut content) = serde_json::from_str::<PidFileContent>(&data) {
                content.session_id = new_session_id.to_string();
                content.updated_at = Some(Utc::now().timestamp_millis());
                if let Ok(json) = serde_json::to_string_pretty(&content) {
                    let _ = std::fs::write(&self.pid_file, json);
                }
            }
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.active.swap(false, Ordering::SeqCst) {
            if let Err(e) = std::fs::remove_file(&self.pid_file) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    debug!("Failed to cleanup PID file: {}", e);
                }
            }
        }
    }
}

/// Register the current session. Returns a guard that cleans up on drop.
///
/// Creates `~/.claude/sessions/{pid}.json` with session metadata.
pub fn register_session(
    session_id: &str,
    cwd: &str,
) -> Option<SessionGuard> {
    let dir = sessions_dir()?;
    if let Err(e) = ensure_sessions_dir(&dir) {
        debug!("Failed to create sessions dir: {}", e);
        return None;
    }

    let pid = std::process::id();
    let kind = env_session_kind().unwrap_or(SessionKind::Interactive);
    let pid_path = pid_file_path(&dir, pid);

    let content = PidFileContent {
        pid,
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        started_at: Utc::now().timestamp_millis(),
        kind,
        entrypoint: std::env::var("CLAUDE_CODE_ENTRYPOINT").ok(),
        name: std::env::var("CLAUDE_CODE_SESSION_NAME").ok(),
        log_path: std::env::var("CLAUDE_CODE_SESSION_LOG").ok(),
        agent: std::env::var("CLAUDE_CODE_AGENT").ok(),
        status: None,
        waiting_for: None,
        updated_at: None,
    };

    match serde_json::to_string_pretty(&content) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&pid_path, json) {
                debug!("Failed to write PID file: {}", e);
                return None;
            }
        }
        Err(e) => {
            debug!("Failed to serialize PID file: {}", e);
            return None;
        }
    }

    debug!("Registered session {} (pid={}, kind={})", session_id, pid, kind);

    Some(SessionGuard {
        pid_file: pid_path,
        active: Arc::new(AtomicBool::new(true)),
    })
}

/// Update the status fields in an existing PID file.
fn update_pid_file(
    pid_path: &Path,
    status: SessionStatus,
    waiting_for: Option<&str>,
) -> anyhow::Result<()> {
    let data = std::fs::read_to_string(pid_path)?;
    let mut content: PidFileContent = serde_json::from_str(&data)?;
    content.status = Some(status);
    content.waiting_for = waiting_for.map(String::from);
    content.updated_at = Some(Utc::now().timestamp_millis());
    let json = serde_json::to_string_pretty(&content)?;
    std::fs::write(pid_path, json)?;
    Ok(())
}

// ── Session counting ─────────────────────────────────────────────────────────

/// Count concurrent active sessions, cleaning up stale PID files.
///
/// Reads `~/.claude/sessions/*.json`, checks if PIDs are alive,
/// and removes stale files (except on WSL).
pub fn count_concurrent_sessions() -> usize {
    let dir = match sessions_dir() {
        Some(d) => d,
        None => return 0,
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("Cannot read sessions dir: {}", e);
            return 0;
        }
    };

    let pid_re = &*PID_FILE_REGEX;
    let current_pid = std::process::id();
    let wsl = is_wsl();
    let mut count = 0;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !pid_re.is_match(&name_str) {
            continue;
        }

        // Extract PID from filename
        let pid: u32 = match name_str.trim_end_matches(".json").parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if pid == current_pid {
            count += 1;
            continue;
        }

        if is_process_running(pid) {
            count += 1;
        } else if !wsl {
            // Clean up stale PID file (skip on WSL)
            let path = dir.join(&name);
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    debug!("Failed to remove stale PID file {}: {}", name_str, e);
                }
            } else {
                debug!("Removed stale PID file: {}", name_str);
            }
        }
    }

    count
}

/// List all active sessions with their metadata.
pub fn list_active_sessions() -> Vec<PidFileContent> {
    let dir = match sessions_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let pid_re = &*PID_FILE_REGEX;
    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !pid_re.is_match(&name_str) {
            continue;
        }

        let pid: u32 = match name_str.trim_end_matches(".json").parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if !is_process_running(pid) && pid != std::process::id() {
            continue;
        }

        let path = dir.join(&name);
        match std::fs::read_to_string(&path) {
            Ok(data) => {
                if let Ok(content) = serde_json::from_str::<PidFileContent>(&data) {
                    sessions.push(content);
                }
            }
            Err(_) => continue,
        }
    }

    sessions
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_kind_from_env() {
        assert_eq!(SessionKind::from_env("bg"), Some(SessionKind::Bg));
        assert_eq!(SessionKind::from_env("daemon"), Some(SessionKind::Daemon));
        assert_eq!(SessionKind::from_env("daemon-worker"), Some(SessionKind::DaemonWorker));
        assert_eq!(SessionKind::from_env("interactive"), Some(SessionKind::Interactive));
        assert_eq!(SessionKind::from_env("unknown"), None);
    }

    #[test]
    fn session_kind_display() {
        assert_eq!(SessionKind::Interactive.to_string(), "interactive");
        assert_eq!(SessionKind::Bg.to_string(), "bg");
        assert_eq!(SessionKind::Daemon.to_string(), "daemon");
        assert_eq!(SessionKind::DaemonWorker.to_string(), "daemon-worker");
    }

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Busy.to_string(), "busy");
        assert_eq!(SessionStatus::Idle.to_string(), "idle");
        assert_eq!(SessionStatus::Waiting.to_string(), "waiting");
    }

    #[test]
    fn pid_file_content_serialization() {
        let content = PidFileContent {
            pid: 12345,
            session_id: "test-session".to_string(),
            cwd: "/home/user/project".to_string(),
            started_at: 1700000000000,
            kind: SessionKind::Interactive,
            entrypoint: None,
            name: None,
            log_path: None,
            agent: None,
            status: Some(SessionStatus::Idle),
            waiting_for: None,
            updated_at: Some(1700000000000),
        };

        let json = serde_json::to_string(&content).unwrap();
        let parsed: PidFileContent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 12345);
        assert_eq!(parsed.session_id, "test-session");
        assert_eq!(parsed.kind, SessionKind::Interactive);
        assert_eq!(parsed.status, Some(SessionStatus::Idle));
    }

    #[test]
    fn pid_file_content_with_optional_fields() {
        let content = PidFileContent {
            pid: 1,
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            started_at: 0,
            kind: SessionKind::Bg,
            entrypoint: Some("cli".to_string()),
            name: Some("my-session".to_string()),
            log_path: Some("/tmp/log".to_string()),
            agent: Some("agent-1".to_string()),
            status: Some(SessionStatus::Busy),
            waiting_for: Some("user input".to_string()),
            updated_at: Some(100),
        };

        let json = serde_json::to_string_pretty(&content).unwrap();
        assert!(json.contains("my-session"));
        assert!(json.contains("agent-1"));
        assert!(json.contains("user input"));
    }

    #[test]
    fn pid_file_content_deserialize_kebab_case() {
        let json = r#"{
            "pid": 42,
            "sessionId": "s2",
            "cwd": "/tmp",
            "startedAt": 0,
            "kind": "daemon-worker",
            "status": "waiting"
        }"#;
        let content: PidFileContent = serde_json::from_str(json).unwrap();
        assert_eq!(content.kind, SessionKind::DaemonWorker);
        assert_eq!(content.status, Some(SessionStatus::Waiting));
    }

    #[test]
    fn register_and_drop_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();

        let pid = std::process::id();
        let pid_path = dir.join(format!("{}.json", pid));

        // Simulate registration
        let content = PidFileContent {
            pid,
            session_id: "test".to_string(),
            cwd: "/tmp".to_string(),
            started_at: 0,
            kind: SessionKind::Interactive,
            entrypoint: None,
            name: None,
            log_path: None,
            agent: None,
            status: Some(SessionStatus::Idle),
            waiting_for: None,
            updated_at: None,
        };
        std::fs::write(&pid_path, serde_json::to_string(&content).unwrap()).unwrap();

        let guard = SessionGuard {
            pid_file: pid_path.clone(),
            active: Arc::new(AtomicBool::new(true)),
        };

        assert!(pid_path.exists());
        drop(guard);
        assert!(!pid_path.exists());
    }

    #[test]
    fn guard_double_drop_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_path = tmp.path().join("99999.json");
        std::fs::write(&pid_path, "{}").unwrap();

        let guard = SessionGuard {
            pid_file: pid_path.clone(),
            active: Arc::new(AtomicBool::new(true)),
        };

        // Manually deactivate
        guard.active.store(false, Ordering::SeqCst);
        drop(guard);
        // File still exists because guard was deactivated
        assert!(pid_path.exists());
    }

    #[test]
    fn is_bg_session_default() {
        // Without env var set, should not be bg
        // (Can't reliably test env vars in parallel tests)
        // Just verify the function doesn't panic
        let _ = is_bg_session();
    }

    #[test]
    fn current_process_is_running() {
        assert!(is_process_running(std::process::id()));
    }

    #[test]
    fn dead_process_not_running() {
        // PID 99999999 is almost certainly not running
        assert!(!is_process_running(99_999_999));
    }
}
