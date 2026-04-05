use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
use claude_agent::task_runner::{run_task, CompletionReason, TaskProgress};
use tokio_stream::StreamExt;

pub async fn print_stream(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                print!("{}", text);
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            AgentEvent::ThinkingDelta(_) => {}
            AgentEvent::ToolUseStart { name, .. } => {
                eprintln!("\n\x1b[36m⚙ {}\x1b[0m", name);
            }
            AgentEvent::ToolResult { is_error, .. } => {
                if is_error {
                    eprintln!("\x1b[31m  ✗ failed\x1b[0m");
                } else {
                    eprintln!("\x1b[32m  ✓ done\x1b[0m");
                }
            }
            AgentEvent::AssistantMessage(_) => {}
            AgentEvent::TurnComplete { .. } => { println!(); }
            AgentEvent::UsageUpdate(u) => {
                tracing::debug!("Tokens: in={}, out={}", u.input_tokens, u.output_tokens);
            }
            AgentEvent::Error(msg) => {
                eprintln!("\x1b[31mError: {}\x1b[0m", msg);
            }
        }
    }
    Ok(())
}

pub async fn run_single(engine: &QueryEngine, prompt: &str) -> anyhow::Result<()> {
    let stream = engine.submit(prompt).await;
    print_stream(stream).await
}

/// Run a task non-interactively with a rich progress display.
///
/// This is the primary path for `claude -p "task"` mode.  It shows:
///   • Tool invocations with names as they start/finish
///   • Turn separators
///   • Final summary with token/timing stats
pub async fn run_task_interactive(engine: &QueryEngine, task: &str) -> anyhow::Result<()> {
    use std::io::Write;

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
                eprintln!("\n\x1b[36m⚙ {}\x1b[0m", name);
            }
            TaskProgress::ToolDone { is_error, .. } => {
                if is_error {
                    eprintln!("\x1b[31m  ✗\x1b[0m");
                } else {
                    eprintln!("\x1b[32m  ✓\x1b[0m");
                }
            }
            TaskProgress::Tokens { .. } => {}
            TaskProgress::Done(_) => {}
        }
    }).await;

    // Final newline + summary to stderr
    println!();
    eprint!(
        "\x1b[2m[{} | {} turns | {} tool calls | {}↑ {}↓ tokens | {:.1}s]\x1b[0m",
        result.reason,
        result.turns,
        result.tool_uses,
        result.input_tokens,
        result.output_tokens,
        result.elapsed.as_secs_f64(),
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

