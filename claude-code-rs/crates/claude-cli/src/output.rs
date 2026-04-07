use claude_agent::cost::CostTracker;
use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
use claude_agent::task_runner::{run_task, CompletionReason, TaskProgress};
use tokio_stream::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Write as _;

/// An animated spinner shown while waiting for the first streaming token.
struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Spinner {
    fn start() -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let handle = tokio::spawn(async move {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while r.load(Ordering::Relaxed) {
                eprint!("\r\x1b[2m{} Thinking...\x1b[0m", frames[i % frames.len()]);
                use std::io::Write;
                std::io::stderr().flush().ok();
                i += 1;
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
            // Clear the spinner line
            eprint!("\r\x1b[2K");
            std::io::stderr().flush().ok();
        });
        Self { running, handle: Some(handle) }
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
        // Give the task a moment to print the clear sequence before aborting
        if let Some(h) = self.handle.take() {
            // Spin-wait briefly for the task to notice the flag (max ~100ms)
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
            while !h.is_finished() && std::time::Instant::now() < deadline {
                std::hint::spin_loop();
            }
            if !h.is_finished() {
                h.abort();
                // Ensure spinner line is cleared even if task didn't finish
                eprint!("\r\x1b[2K");
                std::io::stderr().flush().ok();
            }
        }
    }
}

/// Format task/todo tool results with a richer inline display.
fn format_tool_result_inline(name: &str, text: &str) -> Option<String> {
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
        _ => None,
    }
}

/// Format tool start with key parameter info for better UX.
fn format_tool_start(name: &str, input: &serde_json::Value) -> String {
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
        "dispatch_agent" => input["agent_type"].as_str()
            .map(|t| format!(" \x1b[2m({})\x1b[0m", t))
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
    format!("\x1b[36m⚙ {}{}\x1b[0m", name, detail)
}

fn short_path(path: &str) -> &str {
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

/// Categorize an error message and return (icon, optional hint).
fn categorize_error(msg: &str) -> (&'static str, Option<&'static str>) {
    let lower = msg.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized")
        || lower.contains("invalid key") || lower.contains("invalid api key") || lower.contains("invalid_key") {
        ("🔑", Some("Check your API key with `/login` or set ANTHROPIC_API_KEY"))
    } else if lower.contains("403") || lower.contains("forbidden") || lower.contains("permission") {
        ("🚫", Some("Your API key may lack the required permissions"))
    } else if lower.contains("429") || lower.contains("rate limit") {
        ("⏳", Some("Rate limited — the request will be retried automatically"))
    } else if lower.contains("529") || lower.contains("overloaded") {
        ("🔥", Some("API is overloaded — try again in a moment"))
    } else if lower.contains("timeout") || lower.contains("timed out") {
        ("⏱", Some("Connection timed out — check your network"))
    } else if lower.contains("connection") || lower.contains("dns") || lower.contains("network") {
        ("🌐", Some("Network error — check your internet connection"))
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        ("💥", Some("Server error — this is usually temporary"))
    } else {
        ("❌", None)
    }
}

pub async fn print_stream(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>,
    model: &str,
    cost_tracker: Option<&CostTracker>,
) -> anyhow::Result<()> {
    let mut last_tool_name = String::new();
    let mut tool_start_time: Option<std::time::Instant> = None;
    let mut thinking_started = false;
    let mut first_content = true;
    let mut md = crate::markdown::MarkdownRenderer::new();

    // Start the thinking spinner
    let spinner = Spinner::start();

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                if thinking_started {
                    thinking_started = false;
                    eprintln!("\x1b[0m");
                }
                md.push(&text);
            }
            AgentEvent::ThinkingDelta(text) => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                if !thinking_started {
                    thinking_started = true;
                    eprint!("\x1b[2;3m💭 ");
                }
                eprint!("{}", text);
                use std::io::Write;
                std::io::stderr().flush().ok();
            }
            AgentEvent::ToolUseStart { name, .. } => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                last_tool_name = name.clone();
                tool_start_time = Some(std::time::Instant::now());
            }
            AgentEvent::ToolUseReady { name, input, .. } => {
                eprintln!("\n{}", format_tool_start(&name, &input));
            }
            AgentEvent::ToolResult { is_error, text, .. } => {
                let elapsed = tool_start_time
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                tool_start_time = None;

                if is_error {
                    eprintln!("\x1b[31m  ✗ failed\x1b[0m \x1b[2m({:.1}s)\x1b[0m", elapsed.as_secs_f64());
                } else {
                    eprintln!("\x1b[32m  ✓ done\x1b[0m \x1b[2m({:.1}s)\x1b[0m", elapsed.as_secs_f64());
                }
                // Show inline summary for task/todo tools
                if let Some(ref result_text) = text {
                    if let Some(inline) = format_tool_result_inline(&last_tool_name, result_text) {
                        eprintln!("{}", inline);
                    }
                }
            }
            AgentEvent::AssistantMessage(_) => {}
            AgentEvent::TurnComplete { .. } => {
                // Flush any remaining markdown content
                md.finish();
                // Show per-turn cost summary
                if let Some(tracker) = cost_tracker {
                    let cost = tracker.total_usd();
                    if cost > 0.0 {
                        let cost_str = if cost >= 0.5 {
                            format!("${:.2}", cost)
                        } else if cost >= 0.0001 {
                            format!("${:.4}", cost)
                        } else {
                            "$0.00".to_string()
                        };
                        eprintln!("\x1b[2m  [{}]\x1b[0m", cost_str);
                    }
                }
                println!();
            }
            AgentEvent::UsageUpdate(u) => {
                if let Some(tracker) = cost_tracker {
                    tracker.add(model, &u);
                }
                tracing::debug!("Tokens: in={}, out={}", u.input_tokens, u.output_tokens);
            }
            AgentEvent::Error(msg) => {
                spinner.stop();
                let (icon, hint) = categorize_error(&msg);
                eprintln!("\x1b[31m{} {}\x1b[0m", icon, msg);
                if let Some(h) = hint {
                    eprintln!("\x1b[2m  💡 {}\x1b[0m", h);
                }
            }
            AgentEvent::MaxTurns { limit } => {
                eprintln!("\x1b[33mMax turns ({}) reached\x1b[0m", limit);
            }
            AgentEvent::TurnTokens { input_tokens, output_tokens } => {
                tracing::debug!("Turn tokens: in={}, out={}", input_tokens, output_tokens);
            }
            AgentEvent::ContextWarning { usage_pct, message } => {
                eprintln!("\x1b[33m⚠ Context {:.0}%: {}\x1b[0m", usage_pct * 100.0, message);
            }
            AgentEvent::CompactStart => {
                eprintln!("\x1b[36m🗜 Compacting conversation...\x1b[0m");
            }
            AgentEvent::CompactComplete { summary_len } => {
                eprintln!("\x1b[36m✓ Compacted ({} chars)\x1b[0m", summary_len);
            }
        }
    }
    md.finish();
    Ok(())
}

pub async fn run_single(engine: &QueryEngine, prompt: &str) -> anyhow::Result<()> {
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(prompt).await;
    print_stream(stream, &model, Some(engine.cost_tracker())).await
}

/// Run a single prompt and output structured JSON result.
///
/// JSON format:
/// ```json
/// {
///   "text": "assistant response text",
///   "tool_uses": [...],
///   "input_tokens": 1234,
///   "output_tokens": 567,
///   "turns": 3,
///   "stop_reason": "end_turn"
/// }
/// ```
pub async fn run_json(engine: &QueryEngine, prompt: &str) -> anyhow::Result<()> {
    let result = run_task(engine, prompt, |_| {}).await;

    let json = serde_json::json!({
        "text": result.output,
        "tool_uses": result.tool_uses,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "turns": result.turns,
        "stop_reason": format!("{}", result.reason),
        "duration_ms": result.elapsed.as_millis(),
        "success": result.success(),
    });

    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// Run a task non-interactively with a rich progress display.
///
/// This is the primary path for `claude -p "task"` mode.  It shows:
///   • Tool invocations with names as they start/finish
///   • Inline task/todo summaries
///   • Turn separators
///   • Final summary with token/timing stats
pub async fn run_task_interactive(engine: &QueryEngine, task: &str) -> anyhow::Result<()> {
    use std::io::Write;

    let mut last_tool = String::new();

    let result = run_task(engine, task, |event| {
        match event {
            TaskProgress::TurnStart { turn } if turn > 0 => {
                eprintln!("\x1b[2m── turn {} ──\x1b[0m", turn);
            }
            TaskProgress::TurnStart { .. } => {}
            TaskProgress::Text(t) => {
                print!("{}", t);
                std::io::stdout().flush().ok();
            }
            TaskProgress::ToolUse { name, .. } => {
                last_tool = name.clone();
                eprintln!("\n\x1b[36m⚙ {}\x1b[0m", name);
            }
            TaskProgress::ToolDone { is_error, text, .. } => {
                if is_error {
                    eprintln!("\x1b[31m  ✗\x1b[0m");
                } else {
                    eprintln!("\x1b[32m  ✓\x1b[0m");
                }
                if let Some(ref result_text) = text {
                    if let Some(inline) = format_tool_result_inline(&last_tool, result_text) {
                        eprintln!("{}", inline);
                    }
                }
            }
            TaskProgress::Tokens { .. } => {}
            TaskProgress::Done(_) => {}
        }
    }).await;

    // Final newline + summary to stderr
    println!();
    let cost = engine.cost_tracker().total_usd();
    let cost_str = if cost >= 0.5 {
        format!(" | ${:.2}", cost)
    } else if cost >= 0.0001 {
        format!(" | ${:.4}", cost)
    } else {
        String::new()
    };
    eprint!(
        "\x1b[2m[{} | {} turns | {} tool calls | {}↑ {}↓ tokens | {:.1}s{}]\x1b[0m",
        result.reason,
        result.turns,
        result.tool_uses,
        result.input_tokens,
        result.output_tokens,
        result.elapsed.as_secs_f64(),
        cost_str,
    );
    eprintln!();

    if !result.success() {
        if let CompletionReason::Error(ref e) = result.reason {
            eprintln!("\x1b[31mTask failed: {}\x1b[0m", e);
            return Err(anyhow::anyhow!("{}", e));
        }
    }

    Ok(())
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
        let result = format_tool_result_inline("Read", "file contents...");
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
}