use crate::theme;
use clawed_agent::cost::CostTracker;
use clawed_bus::bus::ClientHandle;
use clawed_bus::events::AgentNotification;
use clawed_core::tool::AbortSignal;
use std::io::Write as _;

use super::helpers::*;

// ── OutputRenderer: renders AgentNotification from bus ─────────────────────

/// Renders `AgentNotification` events received from the Event Bus to the terminal.
///
/// This is the bus-native rendering path. The existing `print_stream()` function
/// works with the legacy `AgentEvent` stream; `OutputRenderer` works with
/// `ClientHandle.recv_notification()` and produces identical output.
pub struct OutputRenderer {
    pub(super) model: String,
    md: crate::markdown::MarkdownRenderer,
    spinner: Option<Spinner>,
    tool_spinner: Option<Spinner>,
    pub(super) last_tool_name: String,
    pub(super) tool_start_time: Option<std::time::Instant>,
    pub(super) thinking_started: bool,
    pub(super) first_content: bool,
    pub(super) total_input_tokens: u64,
    pub(super) total_output_tokens: u64,
    stream_start: std::time::Instant,
}

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
            stream_start: std::time::Instant::now(),
        }
    }

    /// Run a rendering loop: receive notifications from the bus client handle
    /// until the session ends or the channel closes.
    ///
    /// This is the primary entry point for bus-based rendering.
    #[allow(dead_code)]
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
                // Tick stall detection
                if let Some(ref s) = self.spinner { s.tick_activity(); }
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
                    eprintln!("{}  ✗ failed\x1b[0m \x1b[36m({:.1}s)\x1b[0m", theme::c_err(), elapsed.as_secs_f64());
                } else {
                    eprintln!("{}  ✓ done\x1b[0m \x1b[36m({:.1}s)\x1b[0m", theme::c_ok(), elapsed.as_secs_f64());
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
                    let core_usage = clawed_core::message::Usage {
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
                let cost = cost_tracker.map_or(0.0, CostTracker::total_usd);
                let elapsed = self.stream_start.elapsed().as_secs_f64();
                let context_window = super::stream::estimate_context_window(&self.model);
                let status = format_status_line(
                    &self.model,
                    self.total_input_tokens,
                    self.total_output_tokens,
                    cost,
                    elapsed,
                    context_window,
                );
                eprintln!("{}", status);
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
                eprintln!("{}⚠ Context {:.0}%: {}\x1b[0m", theme::c_warn(), usage_pct * 100.0, message);
            }
            AgentNotification::CompactStart => {
                eprintln!("{}🗜 Compacting conversation...\x1b[0m", theme::c_tool());
            }
            AgentNotification::CompactComplete { summary_len } => {
                eprintln!("{}✓ Compacted ({} chars)\x1b[0m", theme::c_tool(), summary_len);
            }
            AgentNotification::AgentSpawned { name, agent_type, .. } => {
                let label = name.as_deref().unwrap_or(&agent_type);
                eprintln!("{}🤖 Agent spawned: {}\x1b[0m", theme::c_tool(), label);
            }
            AgentNotification::AgentProgress { text, .. } => {
                eprintln!("\x1b[2m  │ {}\x1b[0m", text);
            }
            AgentNotification::AgentComplete { is_error, .. } => {
                if is_error {
                    eprintln!("{}  ✗ Agent failed\x1b[0m", theme::c_err());
                } else {
                    eprintln!("{}  ✓ Agent done\x1b[0m", theme::c_ok());
                }
            }
            AgentNotification::McpServerConnected { name, tool_count } => {
                eprintln!("\x1b[2m[MCP: {} connected, {} tools]\x1b[0m", name, tool_count);
            }
            AgentNotification::McpServerDisconnected { name } => {
                eprintln!("\x1b[2m[MCP: {} disconnected]\x1b[0m", name);
            }
            AgentNotification::McpServerError { name, error } => {
                eprintln!("{}[MCP: {} error: {}]\x1b[0m", theme::c_err(), name, error);
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
                eprintln!("{}{} {}\x1b[0m", theme::c_err(), icon, message);
                if let Some(h) = hint {
                    eprintln!("\x1b[2m  💡 {}\x1b[0m", h);
                }
            }
            AgentNotification::SessionSaved { session_id } => {
                eprintln!("\x1b[2m(Session saved: {})\x1b[0m", &session_id[..8.min(session_id.len())]);
            }
            AgentNotification::SessionStatus { model, total_turns, context_usage_pct, .. } => {
                eprintln!(
                    "\x1b[2m[Status: {} | {} turns | context {:.0}%]\x1b[0m",
                    model, total_turns, context_usage_pct
                );
            }
            AgentNotification::HistoryCleared => {
                println!("Conversation history cleared.");
            }
            AgentNotification::ModelChanged { model, display_name } => {
                println!("Model set to: {} ({})", display_name, model);
            }
            AgentNotification::MemoryExtracted { facts } => {
                if !facts.is_empty() {
                    eprintln!("\x1b[2m🧠 Extracted {} memory fact(s)\x1b[0m", facts.len());
                }
            }
            AgentNotification::ModelList { models } => {
                for m in &models {
                    eprintln!("  {} — {}", m.id, m.display_name);
                }
            }
            AgentNotification::ToolList { tools } => {
                let enabled = tools.iter().filter(|t| t.enabled).count();
                eprintln!("\x1b[2m[{} tools ({} enabled)]\x1b[0m", tools.len(), enabled);
            }
            AgentNotification::ThinkingChanged { .. } | AgentNotification::CacheBreakSet => {
                // Handled in REPL command dispatch, not here
            }

            // ── Swarm lifecycle events ──
            AgentNotification::SwarmTeamCreated { team_name, agent_count } => {
                eprintln!("\x1b[2m🐝 Swarm team '{}' created ({} agents)\x1b[0m", team_name, agent_count);
            }
            AgentNotification::SwarmTeamDeleted { team_name } => {
                eprintln!("\x1b[2m🐝 Swarm team '{}' deleted\x1b[0m", team_name);
            }
            AgentNotification::SwarmAgentSpawned { team_name, agent_id, model } => {
                eprintln!("\x1b[2m🐝 Agent '{}' spawned in '{}' ({})\x1b[0m", agent_id, team_name, model);
            }
            AgentNotification::SwarmAgentTerminated { team_name, agent_id } => {
                eprintln!("\x1b[2m🐝 Agent '{}' terminated in '{}'\x1b[0m", agent_id, team_name);
            }
            AgentNotification::SwarmAgentQuery { agent_id, prompt_preview, .. } => {
                eprintln!("\x1b[2m🐝 {} ← {}\x1b[0m", agent_id, prompt_preview);
            }
            AgentNotification::SwarmAgentReply { agent_id, text_preview, is_error, .. } => {
                let icon = if is_error { "❌" } else { "→" };
                eprintln!("\x1b[2m🐝 {} {} {}\x1b[0m", agent_id, icon, text_preview);
            }

            // ── Extended lifecycle events ──
            AgentNotification::AgentTerminated { agent_id, reason } => {
                eprintln!("\x1b[33m⚠ Agent '{}' terminated: {}\x1b[0m", agent_id, reason);
            }
            AgentNotification::ToolSelected { tool_name } => {
                // Quiet; ToolUseStart follows shortly with more detail
                tracing::debug!("Tool selected: {}", tool_name);
            }
            AgentNotification::ConflictDetected { file_path, agents } => {
                eprintln!(
                    "\x1b[31m⚠ File conflict: '{}' modified by: {}\x1b[0m",
                    file_path,
                    agents.join(", ")
                );
            }
        }
        false
    }

    /// Reset renderer state for a new submission cycle.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.first_content = true;
        self.thinking_started = false;
        self.tool_start_time = None;
        self.last_tool_name.clear();
        self.spinner = Some(Spinner::start("Thinking..."));
        self.tool_spinner = None;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.stream_start = std::time::Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_bus::events::ErrorCode;
    use serde_json::json;

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
                usage: clawed_bus::events::UsageInfo {
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
