use crate::theme;
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
    let stream_start = std::time::Instant::now();

    let _esc_guard = abort_signal.map(|a| spawn_esc_listener(a.clone()));

    let spinner = Spinner::start("Thinking...");
    let mut tool_spinner: Option<Spinner> = None;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                if first_content {
                    first_content = false;
                    spinner.stop();
                }
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
                if thinking_started {
                    thinking_started = false;
                    eprintln!("\x1b[0m");
                }
                // Tick spinner activity for stall detection
                spinner.tick_activity();
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
                let tool_msg = format!("🔧 Running {}...", name);
                tool_spinner = Some(Spinner::start(&tool_msg));
                last_tool_name = name.clone();
                tool_start_time = Some(std::time::Instant::now());
            }
            AgentEvent::ToolUseReady { name, input, .. } => {
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
                eprintln!("\n{}", format_tool_start(&name, &input));
            }
            AgentEvent::ToolResult { is_error, text, .. } => {
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
                let elapsed = tool_start_time
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                tool_start_time = None;

                if is_error {
                    eprintln!("{}  ✗ failed\x1b[0m \x1b[36m({:.1}s)\x1b[0m", theme::c_err(), elapsed.as_secs_f64());
                } else {
                    eprintln!("{}  ✓ done\x1b[0m \x1b[36m({:.1}s)\x1b[0m", theme::c_ok(), elapsed.as_secs_f64());
                }
                if let Some(ref result_text) = text {
                    if let Some(inline) = format_tool_result_inline(&last_tool_name, result_text) {
                        eprintln!("{}", inline);
                    }
                }
            }
            AgentEvent::AssistantMessage(_) => {}
            AgentEvent::TurnComplete { .. } => {
                md.finish();

                // Show status line with context info
                let cost = cost_tracker.map_or(0.0, CostTracker::total_usd);
                let elapsed = stream_start.elapsed().as_secs_f64();
                // Estimate context window based on model
                let context_window = estimate_context_window(model);
                let status = format_status_line(
                    model,
                    total_input_tokens,
                    total_output_tokens,
                    cost,
                    elapsed,
                    context_window,
                );
                eprintln!("{}", status);
                println!();
            }
            AgentEvent::UsageUpdate(u) => {
                total_input_tokens += u.input_tokens;
                total_output_tokens += u.output_tokens;
                if let Some(tracker) = cost_tracker {
                    tracker.add(model, &u);
                }
                // Reset stall detection on usage update
                spinner.tick_activity();
                tracing::debug!("Tokens: in={}, out={}", u.input_tokens, u.output_tokens);
            }
            AgentEvent::Error(msg) => {
                spinner.stop();
                if let Some(ts) = tool_spinner.take() {
                    ts.stop();
                }
                let (icon, hint) = categorize_error(&msg);
                eprintln!("{}{}  {}\x1b[0m", theme::c_err(), icon, msg);
                if let Some(h) = hint {
                    eprintln!("\x1b[2m  💡 {}\x1b[0m", h);
                }
            }
            AgentEvent::MaxTurns { limit } => {
                eprintln!("{}Max turns ({}) reached\x1b[0m", theme::c_warn(), limit);
            }
            AgentEvent::TurnTokens { input_tokens, output_tokens } => {
                tracing::debug!("Turn tokens: in={}, out={}", input_tokens, output_tokens);
            }
            AgentEvent::ContextWarning { usage_pct, message } => {
                eprintln!("{}⚠ Context {:.0}%: {}\x1b[0m", theme::c_warn(), usage_pct * 100.0, message);
            }
            AgentEvent::CompactStart => {
                eprintln!("{}🗜 Compacting conversation...\x1b[0m", theme::c_tool());
            }
            AgentEvent::CompactComplete { summary_len } => {
                eprintln!("{}✓ Compacted ({} chars)\x1b[0m", theme::c_tool(), summary_len);
            }
        }
    }
    md.finish();
    Ok(())
}

/// Estimate context window size based on model name.
pub fn estimate_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("gpt-4") || m.contains("gpt-5") {
        128_000
    } else if m.contains("deepseek") {
        64_000
    } else {
        // Anthropic models (opus/sonnet/haiku) and unknown default to 200k
        200_000
    }
}
