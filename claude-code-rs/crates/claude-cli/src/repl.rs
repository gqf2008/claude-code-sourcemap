use claude_agent::engine::QueryEngine;
use claude_core::memory::list_memory_files;
use claude_core::skills::SkillEntry;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::commands::{CommandResult, SlashCommand};
use crate::output::print_stream;

pub async fn run(engine: QueryEngine, skills: Vec<SkillEntry>, cwd: std::path::PathBuf) -> anyhow::Result<()> {
    println!("\x1b[1;34m╭─────────────────────────────────╮\x1b[0m");
    println!("\x1b[1;34m│      Claude Code (Rust)         │\x1b[0m");
    println!("\x1b[1;34m│  Type /help for commands        │\x1b[0m");
    println!("\x1b[1;34m│  Type /exit to quit             │\x1b[0m");
    println!("\x1b[1;34m╰─────────────────────────────────╯\x1b[0m\n");

    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        println!("\x1b[33mSkills loaded: {}\x1b[0m\n", names.join(", "));
    }

    let mut rl = DefaultEditor::new()?;

    loop {
        let readline = rl.readline("\x1b[1;32m> \x1b[0m");
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() { continue; }
                let _ = rl.add_history_entry(input);

                if let Some(cmd) = SlashCommand::parse(input, &skills) {
                    match cmd.execute(&skills) {
                        CommandResult::Print(text) => println!("{}", text),
                        CommandResult::Exit => { println!("Goodbye!"); break; }
                        CommandResult::ClearHistory => {
                            engine.clear_history().await;
                            println!("Conversation history cleared.");
                        }
                        CommandResult::SetModel(model) => {
                            let state = engine.state();
                            let mut s = state.write().await;
                            s.model = model.clone();
                            println!("Model set to: {}", model);
                        }
                        CommandResult::ShowCost => {
                            let state = engine.state();
                            let s = state.read().await;
                            println!(
                                "Tokens: input={}, output={}, turns={}",
                                s.total_input_tokens, s.total_output_tokens, s.turn_count
                            );
                        }
                        CommandResult::Compact { instructions } => {
                            println!("\x1b[33mCompacting conversation…\x1b[0m");
                            match engine.compact("manual", instructions.as_deref()).await {
                                Ok(summary) => {
                                    println!("\x1b[32m✓ Compacted.\x1b[0m");
                                    // Print first few lines of summary as preview
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
                        CommandResult::RunSkill { name, prompt } => {
                            run_skill(&engine, &skills, &name, &prompt, &mut rl).await;
                        }
                    }
                    continue;
                }

                // Check auto-compact before submitting
                if engine.should_auto_compact().await {
                    println!("\x1b[33m[Context limit approaching — auto-compacting…]\x1b[0m");
                    if let Err(e) = engine.compact("auto", None).await {
                        eprintln!("\x1b[31mAuto-compact failed: {}\x1b[0m", e);
                    } else {
                        println!("\x1b[32m[Auto-compact complete]\x1b[0m");
                    }
                }

                let stream = engine.submit(input).await;
                if let Err(e) = print_stream(stream).await {
                    eprintln!("\x1b[31mError: {}\x1b[0m", e);
                }
            }
            Err(ReadlineError::Interrupted) => { println!("^C"); continue; }
            Err(ReadlineError::Eof) => { println!("Goodbye!"); break; }
            Err(err) => { eprintln!("Error: {:?}", err); break; }
        }
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

/// Handle /memory subcommands.
fn handle_memory_command(sub: &str, cwd: &std::path::Path) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let files = list_memory_files(cwd);
            if files.is_empty() {
                println!("No memory files found.");
                println!("Create .md files in ~/.claude/memory/ or .claude/memory/ to use memory.");
            } else {
                println!("Memory files ({}):", files.len());
                for f in &files {
                    let type_tag = f.memory_type.as_ref()
                        .map(|t| format!("[{}] ", t.as_str()))
                        .unwrap_or_default();
                    let desc = f.description.as_deref().unwrap_or("");
                    println!("  {}{:<40} {}", type_tag, f.filename, desc);
                }
            }
        }
        "open" => {
            let rel_path = parts.get(1).copied().unwrap_or("").trim();
            if rel_path.is_empty() {
                println!("Usage: /memory open <filename>");
                return;
            }
            // Try to find the file in memory dirs
            let mem_dirs = claude_core::memory::memory_dirs(cwd);
            let mut found = false;
            for dir in &mem_dirs {
                let p = dir.join(rel_path);
                if p.exists() {
                    // Open in $EDITOR or just print the path
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".into());
                    let _ = std::process::Command::new(&editor).arg(&p).status();
                    found = true;
                    break;
                }
            }
            if !found {
                // Create new file
                match claude_core::memory::ensure_user_memory_dir() {
                    Ok(dir) => {
                        let p = dir.join(rel_path);
                        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".into());
                        let _ = std::process::Command::new(&editor).arg(&p).status();
                    }
                    Err(e) => eprintln!("Cannot open memory dir: {}", e),
                }
            }
        }
        other => {
            println!("Unknown memory subcommand: '{}'. Use 'list' or 'open <file>'.", other);
        }
    }
}

/// Handle /session subcommands.
async fn handle_session_command(sub: &str, engine: &QueryEngine) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let sessions = claude_core::session::list_sessions();
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions:");
                for s in &sessions {
                    let age = claude_core::session::format_age(&s.updated_at);
                    println!(
                        "  \x1b[36m{:.8}\x1b[0m  {:<50} ({} msgs, {} turns, {})",
                        s.id, s.title, s.message_count, s.turn_count, age,
                    );
                }
            }
        }
        "save" => {
            match engine.save_session().await {
                Ok(()) => {
                    println!("\x1b[32m✓ Session saved ({})\x1b[0m", &engine.session_id()[..8]);
                }
                Err(e) => eprintln!("\x1b[31mFailed to save session: {}\x1b[0m", e),
            }
        }
        "load" | "resume" => {
            let id = parts.get(1).copied().unwrap_or("").trim();
            if id.is_empty() {
                // Auto-resume latest session
                let sessions = claude_core::session::list_sessions();
                if sessions.is_empty() {
                    println!("No sessions to resume. Use /session list first.");
                    return;
                }
                let latest = &sessions[0];
                match engine.restore_session(&latest.id).await {
                    Ok(title) => {
                        println!("\x1b[32m✓ Resumed session: {}\x1b[0m", title);
                        println!("  ({} messages restored)", latest.message_count);
                    }
                    Err(e) => eprintln!("\x1b[31mFailed to resume: {}\x1b[0m", e),
                }
            } else {
                // Find session by prefix match
                let sessions = claude_core::session::list_sessions();
                let found = sessions.iter().find(|s| s.id.starts_with(id));
                match found {
                    Some(meta) => {
                        match engine.restore_session(&meta.id).await {
                            Ok(title) => {
                                println!("\x1b[32m✓ Resumed session: {}\x1b[0m", title);
                                println!("  ({} messages restored)", meta.message_count);
                            }
                            Err(e) => eprintln!("\x1b[31mFailed to resume: {}\x1b[0m", e),
                        }
                    }
                    None => println!("No session found matching '{}'. Use /session list.", id),
                }
            }
        }
        other => {
            println!("Unknown session subcommand: '{}'. Use save, list, or load <id>.", other);
        }
    }
}

/// Run a skill as a single-shot sub-agent conversation.
async fn run_skill(
    parent_engine: &QueryEngine,
    skills: &[SkillEntry],
    name: &str,
    prompt: &str,
    rl: &mut DefaultEditor,
) {
    let skill = match skills.iter().find(|s| s.name == name) {
        Some(s) => s,
        None => { eprintln!("Unknown skill: {}", name); return; }
    };

    // Determine the actual prompt: if empty, ask interactively
    let user_prompt = if prompt.is_empty() {
        match rl.readline(&format!("\x1b[1;35m[{}]> \x1b[0m", skill.name)) {
            Ok(p) if !p.trim().is_empty() => p,
            _ => return,
        }
    } else {
        prompt.to_string()
    };

    println!("\x1b[35m[Running skill: {}]\x1b[0m", skill.name);

    // Build the prompt augmented with the skill's system context
    let augmented = if skill.system_prompt.is_empty() {
        user_prompt
    } else {
        format!(
            "<skill_context>\n{}\n</skill_context>\n\n{}",
            skill.system_prompt, user_prompt
        )
    };

    // Submit to the parent engine — the skill's context is injected as part of the message.
    // For tool restrictions, we note them but don't enforce in the simple REPL case.
    if !skill.allowed_tools.is_empty() {
        println!(
            "\x1b[33m  (Skill restricts tools to: {})\x1b[0m",
            skill.allowed_tools.join(", ")
        );
    }

    let stream = parent_engine.submit(&augmented).await;
    if let Err(e) = print_stream(stream).await {
        eprintln!("\x1b[31mSkill error: {}\x1b[0m", e);
    }
}

