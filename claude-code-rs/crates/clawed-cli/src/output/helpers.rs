use crate::theme::{self, RESET};
use clawed_core::tool::AbortSignal;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Stall detection thresholds.
const STALL_WARN_SECS: u64 = 30;
const STALL_CRIT_SECS: u64 = 60;

/// Spinner tick characters — braille animation.
const TICK_STRINGS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];

/// An animated spinner with stall detection and live elapsed timer.
/// Changes color when no progress updates occur.
pub(super) struct Spinner {
    bar: ProgressBar,
    last_activity: Arc<std::sync::Mutex<Instant>>,
    stall_stop: Arc<AtomicBool>,
    _stall_handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub(super) fn start(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} {elapsed:.cyan}")
                .unwrap()
                .tick_strings(TICK_STRINGS),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(80));

        let last_activity = Arc::new(std::sync::Mutex::new(Instant::now()));
        let stall_stop = Arc::new(AtomicBool::new(false));

        // Spawn stall detector thread
        let bar_clone = bar.clone();
        let activity_clone = last_activity.clone();
        let stop_clone = stall_stop.clone();
        let orig_msg = message.to_string();
        let handle = std::thread::spawn(move || {
            let mut warned = false;
            let mut critical = false;
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = activity_clone.lock().unwrap().elapsed();
                if elapsed.as_secs() >= STALL_CRIT_SECS && !critical {
                    critical = true;
                    bar_clone.set_style(
                        ProgressStyle::with_template("{spinner:.red} {msg} {elapsed:.red}")
                            .unwrap()
                            .tick_strings(TICK_STRINGS),
                    );
                    bar_clone.set_message(format!("{} {}(stalled {}s){}", orig_msg, theme::c_err(), elapsed.as_secs(), RESET));
                } else if elapsed.as_secs() >= STALL_WARN_SECS && !warned {
                    warned = true;
                    bar_clone.set_style(
                        ProgressStyle::with_template("{spinner:.yellow} {msg} {elapsed:.yellow}")
                            .unwrap()
                            .tick_strings(TICK_STRINGS),
                    );
                    bar_clone.set_message(format!("{} {}(waiting...){}", orig_msg, theme::c_warn(), RESET));
                }
            }
        });

        Self {
            bar,
            last_activity,
            stall_stop,
            _stall_handle: Some(handle),
        }
    }

    /// Record activity to reset stall detection timer.
    pub(super) fn tick_activity(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub(super) fn set_message(&self, msg: &str) {
        self.tick_activity();
        self.bar.set_message(msg.to_string());
    }

    pub(super) fn stop(&self) {
        self.stall_stop.store(true, Ordering::Relaxed);
        self.bar.finish_and_clear();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Format task/todo tool results with a richer inline display.
pub(super) fn format_tool_result_inline(name: &str, text: &str) -> Option<String> {
    match name {
        "task_create" | "task_update" | "task_get" | "task_list" |
        "TodoWrite" | "TodoRead" => {
            let first_line = text.lines().next().unwrap_or(text);
            let truncated = if first_line.chars().count() > 120 {
                let s: String = first_line.chars().take(117).collect();
                format!("{}…", s)
            } else {
                first_line.to_string()
            };
            Some(format!("\x1b[2m  │ {}\x1b[0m", truncated))
        }
        "Edit" | "FileEdit" | "MultiEdit" | "MultiEditTool" => {
            if let Some(stats) = parse_edit_stats(text) {
                Some(format!("  │ {}", stats))
            } else {
                let first_line = text.lines().next().unwrap_or(text);
                Some(format!("\x1b[2m  │ {}\x1b[0m", first_line))
            }
        }
        "Write" | "FileWrite" => {
            let first_line = text.lines().next().unwrap_or(text);
            Some(format!("\x1b[2m  │ {}\x1b[0m", first_line))
        }
        "Read" | "FileRead" => {
            let line_count = text.lines().count();
            Some(format!("\x1b[2m  │ {} lines\x1b[0m", line_count))
        }
        "Glob" | "GlobTool" => {
            let count = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("\x1b[2m  │ {} files matched\x1b[0m", count))
        }
        "Grep" | "GrepTool" => {
            let count = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("\x1b[2m  │ {} matches\x1b[0m", count))
        }
        "Bash" | "PowerShell" => {
            let line_count = text.lines().count();
            // Check for exit code in the last line
            let exit_info = text.lines().last()
                .and_then(|l| l.strip_prefix("Exit code: "))
                .map(|c| format!(" (exit {})", c.trim()))
                .unwrap_or_default();
            if line_count <= 1 && exit_info.is_empty() {
                None
            } else {
                Some(format!("\x1b[2m  │ {} lines{}\x1b[0m", line_count, exit_info))
            }
        }
        "Ls" | "LsTool" => {
            let count = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("\x1b[2m  │ {} entries\x1b[0m", count))
        }
        "WebFetch" => {
            let char_count = text.len();
            let display = if char_count > 1000 {
                format!("{:.1}K chars", char_count as f64 / 1000.0)
            } else {
                format!("{} chars", char_count)
            };
            Some(format!("\x1b[2m  │ {}\x1b[0m", display))
        }
        _ => None,
    }
}

/// Parse "+N -N lines" from edit result text and return a colored string.
pub(super) fn parse_edit_stats(text: &str) -> Option<String> {
    // Match pattern: "(+N -N lines)"
    let paren_start = text.find("(+")?;
    let paren_end = text[paren_start..].find(')')? + paren_start;
    let inner = &text[paren_start + 1..paren_end]; // "+N -N lines"
    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.len() >= 2 {
        let added = parts[0]; // "+N"
        let removed = parts[1]; // "-N"
        let path = text.split(" (+").next().unwrap_or("");
        let path_short = short_path(path.trim_start_matches("Edited ").trim_start_matches("Wrote "));
        Some(format!("\x1b[2m{}\x1b[0m {}{}\x1b[0m {}{}\x1b[0m", path_short, theme::c_ok(), added, theme::c_err(), removed))
    } else {
        None
    }
}

/// Format tool start with key parameter info for better UX.
pub(super) fn format_tool_start(name: &str, input: &serde_json::Value) -> String {
    let detail = match name {
        "Read" | "FileRead" => input["file_path"].as_str()
            .or_else(|| input["path"].as_str())
            .map(|p| format!(" \x1b[2m{}\x1b[0m", short_path(p)))
            .unwrap_or_default(),
        "Edit" | "FileEdit" => input["file_path"].as_str()
            .or_else(|| input["path"].as_str())
            .map(|p| format!(" \x1b[2m{}\x1b[0m", short_path(p)))
            .unwrap_or_default(),
        "Write" | "FileWrite" => input["file_path"].as_str()
            .or_else(|| input["path"].as_str())
            .map(|p| format!(" \x1b[2m{}\x1b[0m", short_path(p)))
            .unwrap_or_default(),
        "MultiEdit" | "MultiEditTool" => {
            let files = input["edits"].as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e["file_path"].as_str().or_else(|| e["path"].as_str()))
                        .map(short_path)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if files.is_empty() { String::new() } else { format!(" \x1b[2m[{}]\x1b[0m", files) }
        }
        "Bash" => input["command"].as_str()
            .map(|c| {
                let short = if c.len() > 60 { format!("{}…", &c[..57]) } else { c.to_string() };
                format!(" \x1b[2m`{}`\x1b[0m", short)
            })
            .unwrap_or_default(),
        "PowerShell" => input["command"].as_str()
            .map(|c| {
                let short = if c.len() > 60 { format!("{}…", &c[..57]) } else { c.to_string() };
                format!(" \x1b[2m`{}`\x1b[0m", short)
            })
            .unwrap_or_default(),
        "REPL" | "ReplTool" => {
            let lang = input["language"].as_str().unwrap_or("?");
            let code = input["code"].as_str().unwrap_or("");
            let first_line = code.lines().next().unwrap_or("");
            let short = if first_line.len() > 50 { format!("{}…", &first_line[..47]) } else { first_line.to_string() };
            format!(" \x1b[2m[{}] {}\x1b[0m", lang, short)
        }
        "Glob" | "GlobTool" => input["pattern"].as_str()
            .map(|p| format!(" \x1b[2m{}\x1b[0m", p))
            .unwrap_or_default(),
        "Grep" | "GrepTool" => input["pattern"].as_str()
            .map(|p| format!(" \x1b[2m/{}/\x1b[0m", p))
            .unwrap_or_default(),
        "Git" | "GitTool" => {
            let sub = input["subcommand"].as_str().unwrap_or("");
            let args = input["args"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            format!(" \x1b[2m{} {}\x1b[0m", sub, args)
        }
        "GitStatus" | "GitStatusTool" => String::new(),
        "Agent" => input["agent_type"].as_str()
            .map(|t| {
                let desc = input["description"].as_str().unwrap_or("");
                if desc.is_empty() {
                    format!(" \x1b[2m({})\x1b[0m", t)
                } else {
                    format!(" \x1b[2m({}: {})\x1b[0m", t, desc)
                }
            })
            .unwrap_or_default(),
        "WebFetch" => input["url"].as_str()
            .map(|u| format!(" \x1b[2m{}\x1b[0m", u))
            .unwrap_or_default(),
        "WebSearch" => input["query"].as_str()
            .map(|q| {
                let short = if q.len() > 50 { format!("{}…", &q[..47]) } else { q.to_string() };
                format!(" \x1b[2m\"{}\"\x1b[0m", short)
            })
            .unwrap_or_default(),
        "Skill" | "SkillTool" => input["skill_name"].as_str()
            .map(|n| format!(" \x1b[2m{}\x1b[0m", n))
            .unwrap_or_default(),
        "Ls" | "LsTool" => input["path"].as_str()
            .map(|p| format!(" \x1b[2m{}\x1b[0m", short_path(p)))
            .unwrap_or_default(),
        "TodoWrite" | "TodoRead" => input["action"].as_str()
            .map(|a| format!(" \x1b[2m{}\x1b[0m", a))
            .unwrap_or_default(),
        _ => String::new(),
    };
    format!("{}⚙ {}{}\x1b[0m", theme::c_tool(), name, detail)
}

pub(super) fn short_path(path: &str) -> &str {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 3 { return path; }
    // Find the byte offset of the Nth-from-last separator
    let keep = 3;
    let mut sep_count = 0;
    for (i, b) in path.bytes().enumerate().rev() {
        if b == b'/' || b == b'\\' {
            sep_count += 1;
            if sep_count == keep {
                return &path[i + 1..];
            }
        }
    }
    path
}

/// Format a status line shown after each turn completes (single line).
///
/// Example: `[claude-sonnet-4 | 23% ctx | 22.3K↓ 1.2K↑ | $0.0142 | 3.2s]`
pub(super) fn format_status_line(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    elapsed_secs: f64,
    context_window: u64,
) -> String {
    // Context usage percentage
    let context_pct = if context_window > 0 {
        let used = input_tokens + output_tokens;
        (used as f64 / context_window as f64 * 100.0).min(100.0)
    } else {
        0.0
    };

    // Color context % based on usage level
    let context_color = if context_pct >= 80.0 {
        theme::c_err()
    } else if context_pct >= 60.0 {
        theme::c_warn()
    } else {
        "\x1b[2m" // dim (normal)
    };

    // Format cost
    let cost_str = if cost_usd >= 0.01 {
        format!("${:.2}", cost_usd)
    } else if cost_usd >= 0.0001 {
        format!("${:.4}", cost_usd)
    } else {
        "$0".to_string()
    };

    // Shorten model name for display
    let model_short = shorten_model_name(model);

    // Compact token counts
    let in_tok = compact_tokens(input_tokens);
    let out_tok = compact_tokens(output_tokens);

    format!(
        "\x1b[2m[{} | {}{}%\x1b[2m ctx | {}↓ {}↑ | {} | {:.1}s]\x1b[0m",
        model_short,
        context_color,
        context_pct as u32,
        in_tok,
        out_tok,
        cost_str,
        elapsed_secs,
    )
}

fn compact_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{n}")
    } else if n < 100_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}K", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Shorten common model names for status line display.
fn shorten_model_name(model: &str) -> &str {
    // Strip common prefixes
    let m = model.strip_prefix("anthropic.").unwrap_or(model);
    if m.len() > 30 {
        // Truncate very long model IDs
        &m[..30]
    } else {
        m
    }
}

/// Categorize an error message and return (icon, optional hint).
pub(super) fn categorize_error(msg: &str) -> (&'static str, Option<&'static str>) {
    let lower = msg.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized")
        || lower.contains("invalid key") || lower.contains("invalid api key") || lower.contains("invalid_key") {
        ("🔑", Some("Check your API key with `/login` or set ANTHROPIC_API_KEY"))
    } else if lower.contains("403") || lower.contains("forbidden") || lower.contains("permission") {
        ("🚫", Some("Your API key may lack the required permissions"))
    } else if lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests") {
        ("⏳", Some("Rate limited — the request will be retried automatically"))
    } else if lower.contains("quota") || lower.contains("billing") || lower.contains("credit") {
        ("💳", Some("Quota exceeded — check your billing at console.anthropic.com"))
    } else if lower.contains("529") || lower.contains("overloaded") {
        ("🔥", Some("API is overloaded — try again in a moment"))
    } else if lower.contains("model not found") || lower.contains("invalid_model") || lower.contains("does not exist") {
        ("🔍", Some("Model not found — check the model name with `/model`"))
    } else if lower.contains("context_length") || lower.contains("too many tokens") || lower.contains("max_tokens") {
        ("📏", Some("Input too long — try `/compact` to reduce context size"))
    } else if lower.contains("timeout") || lower.contains("timed out") {
        ("⏱", Some("Connection timed out — check your network"))
    } else if lower.contains("connection") || lower.contains("dns") || lower.contains("network")
        || lower.contains("connect error") {
        ("🌐", Some("Network error — check your internet connection"))
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        ("💥", Some("Server error — this is usually temporary"))
    } else {
        ("❌", None)
    }
}

/// Spawn a background thread that listens for ESC key press and triggers abort.
/// Returns a guard that stops the listener when dropped.
pub(crate) fn spawn_esc_listener(abort: AbortSignal) -> EscListenerGuard {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::spawn(move || {
        // Enable raw mode to capture individual key presses
        if crossterm::terminal::enable_raw_mode().is_err() {
            return;
        }
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            // Poll for events with a short timeout
            if crossterm::event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                    if key.code == crossterm::event::KeyCode::Esc {
                        abort.abort();
                        break;
                    }
                }
            }
        }
        let _ = crossterm::terminal::disable_raw_mode();
    });
    EscListenerGuard { stop, handle: Some(handle) }
}

pub(crate) struct EscListenerGuard {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for EscListenerGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── short_path ───────────────────────────────────────────────────

    #[test]
    fn test_short_path_already_short() {
        assert_eq!(short_path("src/main.rs"), "src/main.rs");
        assert_eq!(short_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn test_short_path_truncates_deep() {
        let p = "very/deep/nested/path/to/file.rs";
        let result = short_path(p);
        assert_eq!(result, "path/to/file.rs");
    }

    #[test]
    fn test_short_path_backslash() {
        let p = r"very\deep\nested\path\to\file.rs";
        let result = short_path(p);
        assert_eq!(result, r"path\to\file.rs");
    }

    #[test]
    fn test_short_path_mixed_separators() {
        // C:\Users\alice/repo/src/main.rs → 6 segments, keep last 3
        let p = r"C:\Users\alice/repo/src/main.rs";
        let result = short_path(p);
        assert_eq!(result, "repo/src/main.rs");
    }

    #[test]
    fn test_short_path_single_component() {
        assert_eq!(short_path("file.rs"), "file.rs");
    }

    // ── format_tool_start ────────────────────────────────────────────

    #[test]
    fn test_format_tool_start_read() {
        let result = format_tool_start("Read", &json!({"file_path": "src/main.rs"}));
        assert!(result.contains("Read"));
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn test_format_tool_start_bash() {
        let result = format_tool_start("Bash", &json!({"command": "ls -la"}));
        assert!(result.contains("Bash"));
        assert!(result.contains("ls -la"));
    }

    #[test]
    fn test_format_tool_start_bash_long_command() {
        let long = "x".repeat(100);
        let result = format_tool_start("Bash", &json!({"command": long}));
        assert!(result.contains("…")); // truncated
    }

    #[test]
    fn test_format_tool_start_glob() {
        let result = format_tool_start("Glob", &json!({"pattern": "**/*.rs"}));
        assert!(result.contains("**/*.rs"));
    }

    #[test]
    fn test_format_tool_start_grep() {
        let result = format_tool_start("Grep", &json!({"pattern": "fn main"}));
        assert!(result.contains("/fn main/"));
    }

    #[test]
    fn test_format_tool_start_web_fetch() {
        let result = format_tool_start("WebFetch", &json!({"url": "https://example.com"}));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_format_tool_start_unknown_tool() {
        let result = format_tool_start("CustomTool", &json!({}));
        assert!(result.contains("CustomTool"));
    }

    // ── format_tool_result_inline ────────────────────────────────────

    #[test]
    fn test_format_result_inline_task_tool() {
        let result = format_tool_result_inline("TodoWrite", "Task created: fix bug");
        assert!(result.is_some());
        assert!(result.unwrap().contains("Task created: fix bug"));
    }

    #[test]
    fn test_format_result_inline_non_task_tool() {
        // "Read" now has inline formatting (shows line count)
        let result = format_tool_result_inline("Read", "line1\nline2\nline3");
        assert!(result.is_some());
        assert!(result.unwrap().contains("3 lines"));

        // Unknown tools still return None
        let result = format_tool_result_inline("SomeUnknownTool", "stuff");
        assert!(result.is_none());
    }

    #[test]
    fn test_format_result_inline_long_text_truncated() {
        let long = "x".repeat(200);
        let result = format_tool_result_inline("task_create", &long);
        assert!(result.is_some());
        assert!(result.unwrap().contains("…"));
    }

    // ── categorize_error ─────────────────────────────────────────────

    #[test]
    fn test_categorize_error_auth() {
        let (icon, hint) = categorize_error("401 Unauthorized");
        assert_eq!(icon, "🔑");
        assert!(hint.is_some());
    }

    #[test]
    fn test_categorize_error_rate_limit() {
        let (icon, hint) = categorize_error("429 rate limit exceeded");
        assert_eq!(icon, "⏳");
        assert!(hint.unwrap().contains("retried"));
    }

    #[test]
    fn test_categorize_error_overloaded() {
        let (icon, _) = categorize_error("529 API overloaded");
        assert_eq!(icon, "🔥");
    }

    #[test]
    fn test_categorize_error_timeout() {
        let (icon, hint) = categorize_error("connection timed out");
        assert_eq!(icon, "⏱");
        assert!(hint.unwrap().contains("network"));
    }

    #[test]
    fn test_categorize_error_network() {
        let (icon, _) = categorize_error("dns resolution failed");
        assert_eq!(icon, "🌐");
    }

    #[test]
    fn test_categorize_error_server() {
        let (icon, hint) = categorize_error("500 Internal Server Error");
        assert_eq!(icon, "💥");
        assert!(hint.unwrap().contains("temporary"));
    }

    #[test]
    fn test_categorize_error_unknown() {
        let (icon, hint) = categorize_error("something unexpected happened");
        assert_eq!(icon, "❌");
        assert!(hint.is_none());
    }

    #[test]
    fn test_categorize_error_quota() {
        let (icon, hint) = categorize_error("quota exceeded for this billing period");
        assert_eq!(icon, "💳");
        assert!(hint.unwrap().contains("billing"));
    }

    #[test]
    fn test_categorize_error_model_not_found() {
        let (icon, hint) = categorize_error("model not found: claude-nonexistent");
        assert_eq!(icon, "🔍");
        assert!(hint.unwrap().contains("model"));
    }

    #[test]
    fn test_categorize_error_context_length() {
        let (icon, hint) = categorize_error("context_length_exceeded: too many tokens");
        assert_eq!(icon, "📏");
        assert!(hint.unwrap().contains("compact"));
    }

    // ── parse_edit_stats ─────────────────────────────────────────────

    #[test]
    fn test_parse_edit_stats_normal() {
        let result = parse_edit_stats("Edited src/main.rs (+3 -1 lines)");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("+3"));
        assert!(s.contains("-1"));
    }

    #[test]
    fn test_parse_edit_stats_no_match() {
        let result = parse_edit_stats("Edited src/main.rs");
        assert!(result.is_none());
    }

    #[test]
    fn test_format_result_inline_edit_tool() {
        let result = format_tool_result_inline("Edit", "Edited src/main.rs (+5 -2 lines)");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("+5"));
        assert!(s.contains("-2"));
    }

    #[test]
    fn test_format_result_inline_write_tool() {
        let result = format_tool_result_inline("Write", "Wrote src/new.rs");
        assert!(result.is_some());
        assert!(result.unwrap().contains("Wrote src/new.rs"));
    }

    #[test]
    fn test_format_result_inline_multi_edit() {
        let result = format_tool_result_inline("MultiEdit", "Edited a.rs (+1 -1 lines), b.rs (+2 -0 lines)");
        assert!(result.is_some());
    }

    // ── parse_edit_stats edge cases ──────────────────────────────────

    #[test]
    fn test_parse_edit_stats_malformed_no_numbers() {
        // Missing numbers — the parser doesn't validate numeric format,
        // it just extracts the +/- tokens. So this returns Some (not a panic).
        let result = parse_edit_stats("Edited file.txt (+ - lines)");
        assert!(result.is_some(), "parser accepts malformed stats without panicking");
    }

    #[test]
    fn test_parse_edit_stats_zero_changes() {
        let result = parse_edit_stats("Edited src/main.rs (+0 -0 lines)");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("+0"));
        assert!(s.contains("-0"));
    }

    #[test]
    fn test_parse_edit_stats_large_numbers() {
        let result = parse_edit_stats("Edited huge.rs (+9999 -8888 lines)");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("+9999"));
        assert!(s.contains("-8888"));
    }

    #[test]
    fn test_parse_edit_stats_wrote_prefix() {
        let result = parse_edit_stats("Wrote src/new.rs (+10 -0 lines)");
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("+10"));
    }

    // ── short_path edge cases ────────────────────────────────────────

    #[test]
    fn test_short_path_empty_string() {
        assert_eq!(short_path(""), "");
    }

    #[test]
    fn test_short_path_no_separators() {
        assert_eq!(short_path("file.txt"), "file.txt");
    }

    #[test]
    fn test_short_path_exactly_three_segments() {
        assert_eq!(short_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn test_short_path_windows_deep() {
        let p = "C:\\Users\\gxh\\Documents\\project\\src\\main.rs";
        let result = short_path(p);
        // keeps last 3 segments
        assert_eq!(result, "project\\src\\main.rs");
    }

    // ── categorize_error edge cases ──────────────────────────────────

    #[test]
    fn test_categorize_error_case_insensitive() {
        let (icon, _) = categorize_error("UNAUTHORIZED ACCESS");
        assert_eq!(icon, "🔑");
    }

    #[test]
    fn test_categorize_error_empty_string() {
        let (icon, hint) = categorize_error("");
        assert_eq!(icon, "❌");
        assert!(hint.is_none());
    }

    #[test]
    fn test_categorize_error_multiple_keywords() {
        // "401 timeout" — first match wins (401 checked before timeout)
        let (icon, _) = categorize_error("401 unauthorized timeout");
        assert_eq!(icon, "🔑");
    }

    #[test]
    fn test_categorize_error_forbidden() {
        let (icon, _) = categorize_error("403 Forbidden");
        assert_eq!(icon, "🚫");
    }

    #[test]
    fn test_categorize_error_502_503() {
        let (icon, _) = categorize_error("502 Bad Gateway");
        assert_eq!(icon, "💥");
        let (icon2, _) = categorize_error("503 Service Unavailable");
        assert_eq!(icon2, "💥");
    }

    // ── format_tool_start edge cases ─────────────────────────────────

    #[test]
    fn test_format_tool_start_repl() {
        let input = json!({"language": "python", "code": "print('hello')"});
        let s = format_tool_start("REPL", &input);
        assert!(s.contains("python"));
        assert!(s.contains("print"));
    }

    #[test]
    fn test_format_tool_start_git() {
        let input = json!({"subcommand": "log", "args": ["--oneline", "-5"]});
        let s = format_tool_start("Git", &input);
        assert!(s.contains("log"));
        assert!(s.contains("--oneline"));
    }

    #[test]
    fn test_format_tool_start_web_search() {
        let input = json!({"query": "rust async programming tutorial for beginners 2024 advanced"});
        let s = format_tool_start("WebSearch", &input);
        assert!(s.contains("rust async"));
    }

    #[test]
    fn test_format_tool_start_agent() {
        let input = json!({"agent_type": "explore"});
        let s = format_tool_start("Agent", &input);
        assert!(s.contains("explore"));
    }

    #[test]
    fn test_format_tool_start_agent_with_description() {
        let input = json!({"agent_type": "explore", "description": "Find config files"});
        let s = format_tool_start("Agent", &input);
        assert!(s.contains("explore"));
        assert!(s.contains("Find config files"));
    }
}
