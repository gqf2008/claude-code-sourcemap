use claude_agent::cost::CostTracker;
use claude_agent::query::AgentEvent;
use claude_core::tool::AbortSignal;
use tokio_stream::StreamExt;
use std::io::Write as _;

use super::helpers::*;

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
