//! Tool execution audit logger.
//!
//! Writes structured JSONL entries to `~/.claude/audit.jsonl` for every tool
//! invocation. Each line is a self-contained JSON object with timestamp,
//! tool name, input summary, duration, and outcome.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// A single audit log entry.
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    /// ISO-8601 timestamp when execution started.
    pub timestamp: String,
    /// Session ID.
    pub session_id: String,
    /// Tool name.
    pub tool: String,
    /// Truncated input summary (max 200 chars).
    pub input_summary: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the tool succeeded.
    pub success: bool,
    /// Error message if failed (truncated to 200 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Tracks a tool invocation and writes the audit entry on completion.
pub struct AuditSpan {
    session_id: String,
    tool: String,
    input_summary: String,
    start: Instant,
}

impl AuditSpan {
    /// Create a new audit span when a tool execution begins.
    pub fn begin(session_id: &str, tool: &str, input: &serde_json::Value) -> Self {
        let summary = truncate(&serde_json::to_string(input).unwrap_or_default(), 200);
        Self {
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            input_summary: summary,
            start: Instant::now(),
        }
    }

    /// Complete the span and write the audit entry.
    pub fn finish(self, success: bool, error: Option<&str>) {
        let elapsed = self.start.elapsed();
        let entry = AuditEntry {
            timestamp: chrono_now(),
            session_id: self.session_id,
            tool: self.tool,
            input_summary: self.input_summary,
            duration_ms: elapsed.as_millis() as u64,
            success,
            error: error.map(|e| truncate(e, 200)),
        };
        AUDIT_LOG.write(entry);
    }
}

/// Global audit log writer (append-only JSONL file).
static AUDIT_LOG: std::sync::LazyLock<AuditLog> = std::sync::LazyLock::new(AuditLog::new);

struct AuditLog {
    #[allow(dead_code)]
    path: Option<PathBuf>,
    file: Mutex<Option<std::fs::File>>,
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    { std::env::var_os("USERPROFILE").map(PathBuf::from) }
    #[cfg(not(windows))]
    { std::env::var_os("HOME").map(PathBuf::from) }
}

impl AuditLog {
    fn new() -> Self {
        let path = home_dir().map(|h| h.join(".claude").join("audit.jsonl"));
        let file = path.as_ref().and_then(|p: &PathBuf| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
        Self {
            path,
            file: Mutex::new(file),
        }
    }

    fn write(&self, entry: AuditEntry) {
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                // Recover from poisoned mutex — still usable
                poisoned.into_inner()
            }
        };
        if let Some(ref mut f) = *guard {
            if let Ok(json) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{}", json);
                let _ = f.flush();
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Find the last valid char boundary at or before `max`
        let mut end = max.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    // Simple ISO-8601-ish: 2026-04-07T02:31:24Z
    let secs = now.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Convert days since epoch to Y-M-D (simplified)
    let (year, month, day) = epoch_days_to_ymd(days as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, hours, minutes, seconds)
}

fn epoch_days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let long = "a".repeat(300);
        let result = truncate(&long, 200);
        assert!(result.len() <= 204); // 200 + "…" in utf-8
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_truncate_multibyte_utf8() {
        // Emoji is 4 bytes — slicing at various positions must not panic
        let input = "hello🔥world🎉test";
        for max in 0..input.len() {
            let result = truncate(input, max);
            // Must be valid UTF-8 (no panic) and end with … if truncated
            assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        }
    }

    #[test]
    fn test_truncate_chinese() {
        // Chinese chars are 3 bytes each
        let input = "你好世界测试数据";
        let result = truncate(input, 6); // 6 bytes = 2 chars
        assert!(result.starts_with("你好"));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_truncate_exact_boundary() {
        let input = "abc";
        assert_eq!(truncate(input, 3), "abc"); // exact fit, no truncation
        assert_eq!(truncate(input, 2), "ab…"); // truncated
    }

    #[test]
    fn test_chrono_now_format() {
        let ts = chrono_now();
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
        assert_eq!(ts.len(), 20); // 2026-04-07T02:31:24Z
    }

    #[test]
    fn test_audit_entry_serialization() {
        let entry = AuditEntry {
            timestamp: "2026-04-07T00:00:00Z".to_string(),
            session_id: "test-session".to_string(),
            tool: "BashTool".to_string(),
            input_summary: r#"{"command":"ls"}"#.to_string(),
            duration_ms: 42,
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("BashTool"));
        assert!(!json.contains("error")); // skip_serializing_if
    }

    #[test]
    fn test_audit_entry_with_error() {
        let entry = AuditEntry {
            timestamp: "2026-04-07T00:00:00Z".to_string(),
            session_id: "test-session".to_string(),
            tool: "FileRead".to_string(),
            input_summary: "{}".to_string(),
            duration_ms: 5,
            success: false,
            error: Some("File not found".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("File not found"));
    }

    #[test]
    fn test_audit_span_timing() {
        let span = AuditSpan::begin("sess", "TestTool", &serde_json::json!({"key": "val"}));
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Just verify it doesn't panic — actual file write goes to global log
        span.finish(true, None);
    }

    #[test]
    fn test_epoch_days_to_ymd() {
        // 2026-04-07 is day 20550 since epoch (1970-01-01)
        let (y, m, d) = epoch_days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));

        let (y, m, d) = epoch_days_to_ymd(365);
        assert_eq!((y, m, d), (1971, 1, 1));
    }
}
