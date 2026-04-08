use claude_agent::cost::CostTracker;
use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
use claude_agent::task_runner::{run_task, CompletionReason, TaskProgress};
use claude_bus::bus::ClientHandle;
use claude_bus::events::AgentNotification;
use claude_core::tool::AbortSignal;
use tokio_stream::StreamExt;
use std::io::Write as _;

use indicatif::{ProgressBar, ProgressStyle};

/// An animated spinner shown while waiting for the first streaming token.
/// Uses indicatif for richer animation with elapsed time.
struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    fn start(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} {elapsed:.dim}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "]),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(80));
        Self { bar }
    }

    fn set_message(&self, msg: &str) {
        self.bar.set_message(msg.to_string());
    }

    fn stop(&self) {
        self.bar.finish_and_clear();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
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
        "Edit" | "FileEdit" | "MultiEdit" | "MultiEditTool" => {
            // Parse "+N -N lines" from result text and colorize
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
        _ => None,
    }
}

/// Parse "+N -N lines" from edit result text and return a colored string.
fn parse_edit_stats(text: &str) -> Option<String> {
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
        Some(format!("\x1b[2m{}\x1b[0m \x1b[32m{}\x1b[0m \x1b[31m{}\x1b[0m", path_short, added, removed))
    } else {
        None
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
fn spawn_esc_listener(abort: AbortSignal) -> EscListenerGuard {
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

struct EscListenerGuard {
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

// ── OutputRenderer: renders AgentNotification from bus ─────────────────────

/// Renders `AgentNotification` events received from the Event Bus to the terminal.
///
/// This is the bus-native rendering path. The existing `print_stream()` function
/// works with the legacy `AgentEvent` stream; `OutputRenderer` works with
/// `ClientHandle.recv_notification()` and produces identical output.
#[allow(dead_code)]
pub struct OutputRenderer {
    model: String,
    md: crate::markdown::MarkdownRenderer,
    spinner: Option<Spinner>,
    tool_spinner: Option<Spinner>,
    last_tool_name: String,
    tool_start_time: Option<std::time::Instant>,
    thinking_started: bool,
    first_content: bool,
    total_input_tokens: u64,
    total_output_tokens: u64,
}

#[allow(dead_code)]
impl OutputRenderer {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            md: crate::markdown::MarkdownRenderer::new(),
            spinner: Some(Spinner::start("Thinking...")),
            tool_spinner: None,
            last_tool_name: String::new(),
            tool_start_time: None,
            thinking_started: false,
            first_content: true,
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    /// Run a rendering loop: receive notifications from the bus client handle
    /// until the session ends or the channel closes.
    ///
    /// This is the primary entry point for bus-based rendering.
    pub async fn run(
        &mut self,
        client: &mut ClientHandle,
        cost_tracker: Option<&CostTracker>,
        abort_signal: Option<&AbortSignal>,
    ) {
        let _esc_guard = abort_signal.map(|a| spawn_esc_listener(a.clone()));

        while let Some(notification) = client.recv_notification().await {
            let done = self.render(notification, cost_tracker);
            if done {
                break;
            }
        }
        self.finish();
    }

    /// Render a single notification. Returns `true` if this was a terminal event
    /// (TurnComplete or SessionEnd) and the renderer should stop.
    pub fn render(
        &mut self,
        notification: AgentNotification,
        cost_tracker: Option<&CostTracker>,
    ) -> bool {
        match notification {
            AgentNotification::TextDelta { text } => {
                self.ensure_started();
                if let Some(ts) = self.tool_spinner.take() { ts.stop(); }
                if self.thinking_started {
                    self.thinking_started = false;
                    eprintln!("\x1b[0m");
                }
                self.md.push(&text);
            }
            AgentNotification::ThinkingDelta { text } => {
                if self.first_content {
                    self.first_content = false;
                    if let Some(ref s) = self.spinner { s.set_message("💭 Thinking..."); s.stop(); }
                    self.spinner = None;
                }
                if !self.thinking_started {
                    self.thinking_started = true;
                    eprint!("\x1b[2;3m💭 ");
                }
                eprint!("{}", text);
                std::io::stderr().flush().ok();
            }
            AgentNotification::ToolUseStart { tool_name, .. } => {
                self.ensure_started();
                let msg = format!("🔧 Running {}...", tool_name);
                self.tool_spinner = Some(Spinner::start(&msg));
                self.last_tool_name = tool_name;
                self.tool_start_time = Some(std::time::Instant::now());
            }
            AgentNotification::ToolUseReady { tool_name, input, .. } => {
                if let Some(ts) = self.tool_spinner.take() { ts.stop(); }
                eprintln!("\n{}", format_tool_start(&tool_name, &input));
            }
            AgentNotification::ToolUseComplete { is_error, result_preview, .. } => {
                if let Some(ts) = self.tool_spinner.take() { ts.stop(); }
                let elapsed = self.tool_start_time.take()
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                if is_error {
                    eprintln!("\x1b[31m  ✗ failed\x1b[0m \x1b[2m({:.1}s)\x1b[0m", elapsed.as_secs_f64());
                } else {
                    eprintln!("\x1b[32m  ✓ done\x1b[0m \x1b[2m({:.1}s)\x1b[0m", elapsed.as_secs_f64());
                }
                if let Some(ref text) = result_preview {
                    if let Some(inline) = format_tool_result_inline(&self.last_tool_name, text) {
                        eprintln!("{}", inline);
                    }
                }
            }
            AgentNotification::TurnComplete { usage, .. } => {
                self.md.finish();
                self.total_input_tokens += usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
                if let Some(tracker) = cost_tracker {
                    let core_usage = claude_core::message::Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_input_tokens: if usage.cache_creation_tokens > 0 {
                            Some(usage.cache_creation_tokens)
                        } else {
                            None
                        },
                        cache_read_input_tokens: if usage.cache_read_tokens > 0 {
                            Some(usage.cache_read_tokens)
                        } else {
                            None
                        },
                    };
                    tracker.add(&self.model, &core_usage);
                }
                let mut parts = Vec::new();
                if let Some(tracker) = cost_tracker {
                    let cost = tracker.total_usd();
                    if cost > 0.0 {
                        parts.push(if cost >= 0.5 {
                            format!("${:.2}", cost)
                        } else if cost >= 0.0001 {
                            format!("${:.4}", cost)
                        } else {
                            "$0.00".to_string()
                        });
                    }
                }
                if self.total_input_tokens > 0 || self.total_output_tokens > 0 {
                    parts.push(format!("{}↓ {}↑",
                        crate::repl_commands::format_tokens(self.total_input_tokens),
                        crate::repl_commands::format_tokens(self.total_output_tokens)));
                }
                if !parts.is_empty() {
                    eprintln!("\x1b[2m  [{}]\x1b[0m", parts.join(" · "));
                }
                println!();
                return true;
            }
            AgentNotification::AssistantMessage { .. } => {}
            AgentNotification::TurnStart { .. } => {}
            AgentNotification::SessionStart { .. } => {}
            AgentNotification::SessionEnd { .. } => {
                self.finish();
                return true;
            }
            AgentNotification::ContextWarning { usage_pct, message } => {
                eprintln!("\x1b[33m⚠ Context {:.0}%: {}\x1b[0m", usage_pct * 100.0, message);
            }
            AgentNotification::CompactStart => {
                eprintln!("\x1b[36m🗜 Compacting conversation...\x1b[0m");
            }
            AgentNotification::CompactComplete { summary_len } => {
                eprintln!("\x1b[36m✓ Compacted ({} chars)\x1b[0m", summary_len);
            }
            AgentNotification::AgentSpawned { name, agent_type, .. } => {
                let label = name.as_deref().unwrap_or(&agent_type);
                eprintln!("\x1b[36m🤖 Agent spawned: {}\x1b[0m", label);
            }
            AgentNotification::AgentProgress { text, .. } => {
                eprintln!("\x1b[2m  │ {}\x1b[0m", text);
            }
            AgentNotification::AgentComplete { is_error, .. } => {
                if is_error {
                    eprintln!("\x1b[31m  ✗ Agent failed\x1b[0m");
                } else {
                    eprintln!("\x1b[32m  ✓ Agent done\x1b[0m");
                }
            }
            AgentNotification::McpServerConnected { name, tool_count } => {
                eprintln!("\x1b[2m[MCP: {} connected, {} tools]\x1b[0m", name, tool_count);
            }
            AgentNotification::McpServerDisconnected { name } => {
                eprintln!("\x1b[2m[MCP: {} disconnected]\x1b[0m", name);
            }
            AgentNotification::McpServerError { name, error } => {
                eprintln!("\x1b[31m[MCP: {} error: {}]\x1b[0m", name, error);
            }
            AgentNotification::McpServerList { servers } => {
                for s in &servers {
                    let status = if s.connected { "connected" } else { "disconnected" };
                    eprintln!("\x1b[2m  {} ({})\x1b[0m", s.name, status);
                }
            }
            AgentNotification::Error { message, .. } => {
                self.stop_spinners();
                let (icon, hint) = categorize_error(&message);
                eprintln!("\x1b[31m{} {}\x1b[0m", icon, message);
                if let Some(h) = hint {
                    eprintln!("\x1b[2m  💡 {}\x1b[0m", h);
                }
            }
        }
        false
    }

    /// Reset renderer state for a new submission cycle.
    pub fn reset(&mut self) {
        self.first_content = true;
        self.thinking_started = false;
        self.tool_start_time = None;
        self.last_tool_name.clear();
        self.spinner = Some(Spinner::start("Thinking..."));
        self.tool_spinner = None;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
    }

    fn ensure_started(&mut self) {
        if self.first_content {
            self.first_content = false;
            if let Some(s) = self.spinner.take() { s.stop(); }
        }
    }

    fn stop_spinners(&mut self) {
        if let Some(s) = self.spinner.take() { s.stop(); }
        if let Some(ts) = self.tool_spinner.take() { ts.stop(); }
    }

    fn finish(&mut self) {
        self.stop_spinners();
        self.md.finish();
    }
}

pub async fn print_stream(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>,
    model: &str,
    cost_tracker: Option<&CostTracker>,
    abort_signal: Option<&AbortSignal>,
) -> anyhow::Result<()> {
    let mut last_tool_name = String::new();
    let mut tool_start_time: Option<std::time::Instant> = None;
    let mut thinking_started = false;
    let mut first_content = true;
    let mut md = crate::markdown::MarkdownRenderer::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    // Listen for ESC key press to cancel the running task
    let _esc_guard = abort_signal.map(|a| spawn_esc_listener(a.clone()));

    // Start the thinking spinner
    let spinner = Spinner::start("Thinking...");
    let mut tool_spinner: Option<Spinner> = None;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                // Stop tool spinner if text arrives
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
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
                    spinner.set_message("💭 Thinking...");
                    spinner.stop();
                }
                if !thinking_started {
                    thinking_started = true;
                    eprint!("\x1b[2;3m💭 ");
                }
                eprint!("{}", text);
                std::io::stderr().flush().ok();
            }
            AgentEvent::ToolUseStart { name, .. } => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                // Start a spinner for the tool execution
                let tool_msg = format!("🔧 Running {}...", name);
                tool_spinner = Some(Spinner::start(&tool_msg));
                last_tool_name = name.clone();
                tool_start_time = Some(std::time::Instant::now());
            }
            AgentEvent::ToolUseReady { name, input, .. } => {
                // Stop tool spinner before printing tool details
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
                eprintln!("\n{}", format_tool_start(&name, &input));
            }
            AgentEvent::ToolResult { is_error, text, .. } => {
                // Stop tool spinner
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
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
                // Show per-turn cost and token summary
                let mut parts = Vec::new();
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
                        parts.push(cost_str);
                    }
                }
                if total_input_tokens > 0 || total_output_tokens > 0 {
                    parts.push(format!("{}↓ {}↑",
                        crate::repl_commands::format_tokens(total_input_tokens),
                        crate::repl_commands::format_tokens(total_output_tokens)));
                }
                if !parts.is_empty() {
                    eprintln!("\x1b[2m  [{}]\x1b[0m", parts.join(" · "));
                }
                println!();
            }
            AgentEvent::UsageUpdate(u) => {
                total_input_tokens += u.input_tokens;
                total_output_tokens += u.output_tokens;
                if let Some(tracker) = cost_tracker {
                    tracker.add(model, &u);
                }
                tracing::debug!("Tokens: in={}, out={}", u.input_tokens, u.output_tokens);
            }
            AgentEvent::Error(msg) => {
                spinner.stop();
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
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
    print_stream(stream, &model, Some(engine.cost_tracker()), None).await
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

    // ── OutputRenderer ──────────────────────────────────────────────

    #[test]
    fn test_output_renderer_new() {
        let renderer = OutputRenderer::new("claude-sonnet");
        assert_eq!(renderer.model, "claude-sonnet");
        assert!(renderer.first_content);
        assert!(!renderer.thinking_started);
        assert_eq!(renderer.total_input_tokens, 0);
        assert_eq!(renderer.total_output_tokens, 0);
    }

    #[test]
    fn test_output_renderer_text_delta() {
        let mut renderer = OutputRenderer::new("claude-sonnet");
        let done = renderer.render(
            AgentNotification::TextDelta { text: "hello".into() },
            None,
        );
        assert!(!done);
        assert!(!renderer.first_content);
    }

    #[test]
    fn test_output_renderer_turn_complete_returns_true() {
        let mut renderer = OutputRenderer::new("claude-sonnet");
        let done = renderer.render(
            AgentNotification::TurnComplete {
                turn: 1,
                stop_reason: "end_turn".into(),
                usage: claude_bus::events::UsageInfo {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
            None,
        );
        assert!(done);
        assert_eq!(renderer.total_input_tokens, 100);
        assert_eq!(renderer.total_output_tokens, 50);
    }

    #[test]
    fn test_output_renderer_session_end_returns_true() {
        let mut renderer = OutputRenderer::new("claude-sonnet");
        let done = renderer.render(
            AgentNotification::SessionEnd { reason: "exit".into() },
            None,
        );
        assert!(done);
    }

    #[test]
    fn test_output_renderer_tool_lifecycle() {
        let mut renderer = OutputRenderer::new("test-model");

        // ToolUseStart
        let done = renderer.render(
            AgentNotification::ToolUseStart {
                id: "t1".into(),
                tool_name: "Bash".into(),
            },
            None,
        );
        assert!(!done);
        assert_eq!(renderer.last_tool_name, "Bash");
        assert!(renderer.tool_start_time.is_some());

        // ToolUseReady
        let done = renderer.render(
            AgentNotification::ToolUseReady {
                id: "t1".into(),
                tool_name: "Bash".into(),
                input: json!({"command": "ls"}),
            },
            None,
        );
        assert!(!done);

        // ToolUseComplete
        let done = renderer.render(
            AgentNotification::ToolUseComplete {
                id: "t1".into(),
                tool_name: "Bash".into(),
                is_error: false,
                result_preview: Some("output here".into()),
            },
            None,
        );
        assert!(!done);
        assert!(renderer.tool_start_time.is_none());
    }

    #[test]
    fn test_output_renderer_error_notification() {
        let mut renderer = OutputRenderer::new("test");
        let done = renderer.render(
            AgentNotification::Error {
                code: ErrorCode::ApiError,
                message: "401 Unauthorized".into(),
            },
            None,
        );
        assert!(!done);
    }

    #[test]
    fn test_output_renderer_reset() {
        let mut renderer = OutputRenderer::new("test");
        renderer.first_content = false;
        renderer.total_input_tokens = 100;
        renderer.total_output_tokens = 50;
        renderer.last_tool_name = "Bash".into();

        renderer.reset();
        assert!(renderer.first_content);
        assert_eq!(renderer.total_input_tokens, 0);
        assert_eq!(renderer.total_output_tokens, 0);
        assert!(renderer.last_tool_name.is_empty());
    }

    #[test]
    fn test_output_renderer_mcp_notifications() {
        let mut renderer = OutputRenderer::new("test");

        assert!(!renderer.render(
            AgentNotification::McpServerConnected { name: "sqlite".into(), tool_count: 3 },
            None,
        ));
        assert!(!renderer.render(
            AgentNotification::McpServerDisconnected { name: "sqlite".into() },
            None,
        ));
        assert!(!renderer.render(
            AgentNotification::McpServerError {
                name: "bad".into(),
                error: "timeout".into(),
            },
            None,
        ));
    }

    #[test]
    fn test_output_renderer_agent_notifications() {
        let mut renderer = OutputRenderer::new("test");

        assert!(!renderer.render(
            AgentNotification::AgentSpawned {
                agent_id: "a1".into(),
                name: Some("explorer".into()),
                agent_type: "explore".into(),
                background: true,
            },
            None,
        ));
        assert!(!renderer.render(
            AgentNotification::AgentProgress {
                agent_id: "a1".into(),
                text: "searching...".into(),
            },
            None,
        ));
        assert!(!renderer.render(
            AgentNotification::AgentComplete {
                agent_id: "a1".into(),
                result: "found it".into(),
                is_error: false,
            },
            None,
        ));
    }

    #[test]
    fn test_output_renderer_context_and_compact() {
        let mut renderer = OutputRenderer::new("test");

        assert!(!renderer.render(
            AgentNotification::ContextWarning {
                usage_pct: 0.85,
                message: "85% context used".into(),
            },
            None,
        ));
        assert!(!renderer.render(AgentNotification::CompactStart, None));
        assert!(!renderer.render(
            AgentNotification::CompactComplete { summary_len: 500 },
            None,
        ));
    }
}