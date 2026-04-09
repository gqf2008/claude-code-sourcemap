mod helpers;
mod renderer;
mod stream;

pub use renderer::OutputRenderer;
pub use stream::print_stream;

use claude_agent::engine::QueryEngine;
use claude_agent::task_runner::{run_task, CompletionReason};
use helpers::format_tool_result_inline;

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
            claude_agent::task_runner::TaskProgress::TurnStart { turn } if turn > 0 => {
                eprintln!("\x1b[2m── turn {} ──\x1b[0m", turn);
            }
            claude_agent::task_runner::TaskProgress::Text(t) => {
                print!("{}", t);
                std::io::stdout().flush().ok();
            }
            claude_agent::task_runner::TaskProgress::ToolUse { name, .. } => {
                last_tool = name.clone();
                eprintln!("\n\x1b[36m⚙ {}\x1b[0m", name);
            }
            claude_agent::task_runner::TaskProgress::ToolDone { is_error, text, .. } => {
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
            claude_agent::task_runner::TaskProgress::TurnStart { .. }
            | claude_agent::task_runner::TaskProgress::Tokens { .. }
            | claude_agent::task_runner::TaskProgress::Done(_) => {}
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
