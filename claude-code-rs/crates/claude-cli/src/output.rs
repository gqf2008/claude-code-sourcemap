use claude_agent::cost::CostTracker;
use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
use claude_agent::task_runner::{run_task, CompletionReason, TaskProgress};
use tokio_stream::StreamExt;

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
    let start = parts.len() - 3;
    let idx = path.len() - parts[start..].join("/").len();
    &path[idx..]
}

pub async fn print_stream(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>,
    model: &str,
    cost_tracker: Option<&CostTracker>,
) -> anyhow::Result<()> {
    let mut last_tool_name = String::new();
    let mut tool_start_time: Option<std::time::Instant> = None;
    let mut thinking_started = false;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                if thinking_started {
                    // End of thinking phase
                    thinking_started = false;
                    eprintln!("\x1b[0m"); // reset italic
                }
                print!("{}", text);
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            AgentEvent::ThinkingDelta(text) => {
                if !thinking_started {
                    thinking_started = true;
                    eprint!("\x1b[2;3m💭 ");
                }
                eprint!("{}", text);
                use std::io::Write;
                std::io::stderr().flush().ok();
            }
            AgentEvent::ToolUseStart { name, .. } => {
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
                eprintln!("\x1b[31mError: {}\x1b[0m", msg);
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

