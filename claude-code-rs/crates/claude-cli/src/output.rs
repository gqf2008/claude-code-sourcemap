use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
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
                println!("\n\x1b[36m⚙ Tool: {}\x1b[0m", name);
            }
            AgentEvent::ToolResult { is_error, .. } => {
                if is_error {
                    println!("\x1b[31m✗ Tool failed\x1b[0m");
                } else {
                    println!("\x1b[32m✓ Done\x1b[0m");
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
