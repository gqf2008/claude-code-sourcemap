use claude_agent::engine::QueryEngine;
use claude_core::skills::SkillEntry;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::commands::{CommandResult, SlashCommand};
use crate::output::print_stream;
use crate::repl_commands::*;

/// Snapshot of config file modification times for auto-reload detection.
struct ConfigMtimes {
    claude_md: Option<std::time::SystemTime>,
    settings: Option<std::time::SystemTime>,
}

impl ConfigMtimes {
    fn capture(cwd: &std::path::Path) -> Self {
        Self {
            claude_md: Self::mtime(&cwd.join("CLAUDE.md")),
            settings: claude_core::config::settings_path()
                .and_then(|p| Self::mtime(&p)),
        }
    }

    fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    /// Returns true if any watched file has changed since the last snapshot.
    fn changed_since(&self, cwd: &std::path::Path) -> bool {
        let current = Self::capture(cwd);
        self.claude_md != current.claude_md || self.settings != current.settings
    }
}

pub async fn run(engine: QueryEngine, skills: Vec<SkillEntry>, cwd: std::path::PathBuf) -> anyhow::Result<()> {
    let current_model = engine.state().read().await.model.clone();
    let display = claude_core::model::display_name_any(&current_model);
    println!("\x1b[1;34m╭─────────────────────────────────╮\x1b[0m");
    println!("\x1b[1;34m│      Claude Code (Rust)         │\x1b[0m");
    println!("\x1b[1;34m│  Model: {:<23} │\x1b[0m", display);
    println!("\x1b[1;34m│  cwd: {:<25} │\x1b[0m", truncate_path(&cwd, 25));
    println!("\x1b[1;34m│  Type /help for commands        │\x1b[0m");
    println!("\x1b[1;34m╰─────────────────────────────────╯\x1b[0m\n");

    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        println!("\x1b[33mSkills loaded: {}\x1b[0m\n", names.join(", "));
    }

    let mut rl = DefaultEditor::new()?;

    // Load persistent history
    let history_path = history_file_path();
    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    // Track config file modification times for auto-reload
    let mut config_mtimes = ConfigMtimes::capture(&cwd);

    loop {
        let readline = rl.readline("\x1b[1;32m> \x1b[0m");
        match readline {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }

                // Parse slash commands BEFORE multiline expansion
                if trimmed.starts_with('/') {
                    let _ = rl.add_history_entry(trimmed);
                    if let Some(cmd) = SlashCommand::parse(trimmed, &skills) {
                        match cmd.execute(&skills) {
                            CommandResult::Print(text) => println!("{}", text),
                            CommandResult::Exit => { println!("Goodbye!"); break; }
                            CommandResult::ClearHistory => {
                                engine.clear_history().await;
                                println!("Conversation history cleared.");
                            }
                            CommandResult::SetModel(input) => {
                                let resolved = claude_core::model::resolve_model_string(&input);
                                let state = engine.state();
                                let mut s = state.write().await;
                                s.model = resolved.clone();
                                let display = claude_core::model::display_name_any(&resolved);
                                println!("Model set to: {} ({})", display, resolved);

                                // Persist to user settings
                                if let Err(e) = claude_core::config::Settings::update_field(
                                    claude_core::config::SettingsSource::User,
                                    &cwd,
                                    |s| { s.model = Some(resolved.clone()); },
                                ) {
                                    eprintln!("\x1b[33mNote: Could not persist model choice: {}\x1b[0m", e);
                                }
                            }
                            CommandResult::ShowCost => {
                                let state = engine.state();
                                let s = state.read().await;
                                let summary = engine.cost_tracker().format_summary(
                                    s.total_input_tokens,
                                    s.total_output_tokens,
                                    s.turn_count,
                                );
                                println!("{}", summary);
                            }
                            CommandResult::Compact { instructions } => {
                                println!("\x1b[33mCompacting conversation…\x1b[0m");
                                match engine.compact("manual", instructions.as_deref()).await {
                                    Ok(summary) => {
                                        println!("\x1b[32m✓ Compacted.\x1b[0m");
                                        let preview: String = summary.lines().take(5).collect::<Vec<_>>().join("\n");
                                        println!("\x1b[2m{}\x1b[0m", preview);
                                    }
                                    Err(e) => eprintln!("\x1b[31mCompact failed: {}\x1b[0m", e),
                                }
                            }
                            CommandResult::Memory { sub } => {
                                handle_memory_command(&sub, &cwd);
                            }
                            CommandResult::Session { sub } => {
                                handle_session_command(&sub, &engine).await;
                            }
                            CommandResult::Diff => {
                                handle_diff_command(&cwd);
                            }
                            CommandResult::Status => {
                                handle_status_command(&engine, &cwd).await;
                            }
                            CommandResult::Permissions => {
                                let s = engine.state().read().await;
                                println!("Permission mode: {:?}", s.permission_mode);
                            }
                            CommandResult::Config => {
                                handle_config_command(&cwd);
                            }
                            CommandResult::Undo => {
                                handle_undo(&engine).await;
                            }
                            CommandResult::Review { prompt } => {
                                handle_review(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::Doctor => {
                                handle_doctor(&engine, &cwd).await;
                            }
                            CommandResult::Init => {
                                handle_init(&engine, &cwd).await;
                            }
                            CommandResult::Commit { message } => {
                                handle_commit(&engine, &cwd, &message).await;
                            }
                            CommandResult::Pr { prompt } => {
                                handle_pr(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::Bug { prompt } => {
                                handle_bug(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::Search { query } => {
                                handle_search(&engine, &query).await;
                            }
                            CommandResult::Login => {
                                handle_login();
                            }
                            CommandResult::Logout => {
                                handle_logout();
                            }
                            CommandResult::Context => {
                                handle_context(&engine, &cwd).await;
                            }
                            CommandResult::Export { format } => {
                                handle_export(&engine, &cwd, &format).await;
                            }
                            CommandResult::ReloadContext => {
                                handle_reload_context(&engine, &cwd).await;
                            }
                            CommandResult::RunSkill { name, prompt } => {
                                run_skill(&engine, &skills, &name, &prompt, &mut rl).await;
                            }
                        }
                    }
                    continue;
                }

                // Non-slash input: support multiline
                // 1. Trailing `\` continues on next line
                // 2. Triple-backtick ``` starts a code block (read until closing ```)
                let mut input_buf = line;

                // Check if input starts with ``` (code block mode)
                if input_buf.trim_start().starts_with("```") {
                    // Read until we find a line that is just ```
                    input_buf.push('\n');
                    while let Ok(cont) = rl.readline("\x1b[2m` \x1b[0m") {
                        if cont.trim() == "```" {
                            break;
                        }
                        input_buf.push_str(&cont);
                        input_buf.push('\n');
                    }
                } else {
                    // Standard trailing-backslash continuation
                    while input_buf.ends_with('\\') {
                        input_buf.pop(); // remove trailing backslash
                        input_buf.push('\n');
                        match rl.readline("\x1b[2m. \x1b[0m") {
                            Ok(cont) => input_buf.push_str(&cont),
                            _ => break,
                        }
                    }
                }
                let input = input_buf.trim();
                if input.is_empty() { continue; }
                let _ = rl.add_history_entry(input);

                // Check auto-compact before submitting
                if engine.should_auto_compact().await {
                    println!("\x1b[33m[Context limit approaching — auto-compacting…]\x1b[0m");
                    if let Err(e) = engine.compact("auto", None).await {
                        eprintln!("\x1b[31mAuto-compact failed: {}\x1b[0m", e);
                    } else {
                        println!("\x1b[32m[Auto-compact complete]\x1b[0m");
                    }
                }

                // Auto-reload config if files changed on disk
                if config_mtimes.changed_since(&cwd) {
                    println!("\x1b[2m[Config changed on disk — reloading…]\x1b[0m");
                    handle_reload_context(&engine, &cwd).await;
                    config_mtimes = ConfigMtimes::capture(&cwd);
                }

                let model = { engine.state().read().await.model.clone() };

                // Extract @image.png references from input
                let (text, images) = claude_core::image::extract_image_refs(input);
                let stream = if images.is_empty() {
                    engine.submit(&text).await
                } else {
                    let img_count = images.len();
                    println!(
                        "\x1b[2m📎 {} image{} attached\x1b[0m",
                        img_count,
                        if img_count == 1 { "" } else { "s" }
                    );
                    let mut content = Vec::new();
                    if !text.is_empty() {
                        content.push(claude_core::message::ContentBlock::Text { text });
                    }
                    content.extend(images);
                    engine.submit_with_content(content).await
                };

                // The background Ctrl+C handler (main.rs) will call engine.abort()
                // when the user presses Ctrl+C. print_stream checks abort internally.
                if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
                    if engine.abort_signal().is_aborted() {
                        eprintln!("\x1b[33m⏹ Interrupted\x1b[0m");
                        engine.abort_signal().reset();
                    } else {
                        eprintln!("\x1b[31mError: {}\x1b[0m", e);
                    }
                }
                // Reset abort signal after each turn
                if engine.abort_signal().is_aborted() {
                    engine.abort_signal().reset();
                }

                // Show turn stats + context usage warning
                print_turn_stats(&engine).await;

                // Context usage warning (80% threshold)
                if let Some(pct) = engine.context_usage_percent().await {
                    if pct >= 90 {
                        eprintln!("\x1b[31m⚠ Context {pct}% full — consider /compact or /clear\x1b[0m");
                    } else if pct >= 80 {
                        eprintln!("\x1b[33m⚠ Context {pct}% full\x1b[0m");
                    }
                }

                // In coordinator mode: drain background agent notifications and
                // re-submit them so the coordinator can react to completed tasks.
                if engine.is_coordinator() {
                    const MAX_NOTIFICATION_ROUNDS: u32 = 10;
                    let mut rounds = 0;
                    loop {
                        let notifications = engine.drain_notifications().await;
                        if notifications.is_empty() || rounds >= MAX_NOTIFICATION_ROUNDS {
                            break;
                        }
                        rounds += 1;
                        for notif in &notifications {
                            if let claude_core::message::Message::User(u) = notif {
                                // Concatenate all text blocks from the notification
                                let text: String = u.content.iter().filter_map(|b| {
                                    if let claude_core::message::ContentBlock::Text { text } = b {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                }).collect::<Vec<_>>().join("\n");
                                if text.is_empty() { continue; }
                                eprintln!("\x1b[33m[Task notification received]\x1b[0m");
                                let stream = engine.submit(&text).await;
                                if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
                                    eprintln!("\x1b[31mError: {}\x1b[0m", e);
                                }
                            }
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => { println!("^C"); continue; }
            Err(ReadlineError::Eof) => { println!("Goodbye!"); break; }
            Err(err) => { eprintln!("Error: {:?}", err); break; }
        }
    }

    // Save persistent history
    if let Some(ref path) = history_path {
        let _ = rl.save_history(path);
    }

    // Auto-save session on exit (if there's history)
    let has_messages = { engine.state().read().await.messages.len() > 1 };
    if has_messages {
        if let Err(e) = engine.save_session().await {
            eprintln!("\x1b[2m(Session auto-save failed: {})\x1b[0m", e);
        } else {
            eprintln!("\x1b[2m(Session saved: {})\x1b[0m", &engine.session_id()[..8]);
        }
    }

    Ok(())
}

/// Get the path to the persistent history file (~/.claude/history).
fn history_file_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| {
        let dir = home.join(".claude");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("history")
    })
}

/// Display token usage and cost after each turn.
async fn print_turn_stats(engine: &QueryEngine) {
    let state = engine.state().read().await;
    let cost = engine.cost_tracker();
    let total_cost = cost.total_usd();

    let input_k = format_compact_tokens(state.total_input_tokens);
    let output_k = format_compact_tokens(state.total_output_tokens);

    if total_cost > 0.0 {
        eprintln!(
            "\x1b[2m  tokens: {}↓ {}↑ · cost: ${:.4} · turns: {}\x1b[0m",
            input_k, output_k, total_cost, state.turn_count
        );
    }
}

/// Format tokens compactly: 1234 → "1.2K", 12345 → "12K", 1234567 → "1.2M"
fn format_compact_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{}", n)
    } else if n < 100_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}K", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Truncate a path for display (keep last components within `max_len` chars).
fn truncate_path(path: &std::path::Path, max_len: usize) -> String {
    let s = path.display().to_string();
    if s.chars().count() <= max_len {
        return s;
    }
    let skip = s.chars().count() - max_len + 1;
    let tail: String = s.chars().skip(skip).collect();
    format!("…{}", tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_compact_tokens_below_1k() {
        assert_eq!(format_compact_tokens(0), "0");
        assert_eq!(format_compact_tokens(1), "1");
        assert_eq!(format_compact_tokens(999), "999");
    }

    #[test]
    fn format_compact_tokens_kilos() {
        assert_eq!(format_compact_tokens(1_000), "1.0K");
        assert_eq!(format_compact_tokens(1_234), "1.2K");
        assert_eq!(format_compact_tokens(15_500), "15.5K");
        assert_eq!(format_compact_tokens(99_999), "100.0K");
    }

    #[test]
    fn format_compact_tokens_large_kilos() {
        assert_eq!(format_compact_tokens(100_000), "100K");
        assert_eq!(format_compact_tokens(500_000), "500K");
        assert_eq!(format_compact_tokens(999_999), "999K");
    }

    #[test]
    fn format_compact_tokens_megas() {
        assert_eq!(format_compact_tokens(1_000_000), "1.0M");
        assert_eq!(format_compact_tokens(1_500_000), "1.5M");
        assert_eq!(format_compact_tokens(12_345_678), "12.3M");
    }

    #[test]
    fn truncate_path_short() {
        let p = std::path::Path::new("src");
        assert_eq!(truncate_path(p, 25), "src");
    }

    #[test]
    fn truncate_path_long() {
        let p = std::path::Path::new("/very/long/path/that/exceeds/limit");
        let result = truncate_path(p, 15);
        assert!(result.starts_with('…'));
        // Display length matters, not byte length
        assert!(result.chars().count() <= 16);
    }
}
