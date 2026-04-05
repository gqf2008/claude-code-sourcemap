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
                                let display = claude_core::model::display_name(&resolved);
                                println!("Model set to: {} ({})", display, resolved);
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
                            CommandResult::RunSkill { name, prompt } => {
                                run_skill(&engine, &skills, &name, &prompt, &mut rl).await;
                            }
                        }
                    }
                    continue;
                }

                // Non-slash input: support multiline with trailing `\`
                let mut input_buf = line;
                while input_buf.ends_with('\\') {
                    input_buf.pop(); // remove trailing backslash
                    input_buf.push('\n');
                    match rl.readline("\x1b[2m. \x1b[0m") {
                        Ok(cont) => input_buf.push_str(&cont),
                        _ => break,
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

                let model = { engine.state().read().await.model.clone() };
                let stream = engine.submit(input).await;
                if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
                    eprintln!("\x1b[31mError: {}\x1b[0m", e);
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
            // Validate: reject path traversal attempts
            if rel_path.contains("..") || rel_path.starts_with('/') || rel_path.starts_with('\\') || rel_path.contains(':') {
                println!("Invalid filename: must be a simple name without path separators or '..'");
                return;
            }
            // Try to find the file in memory dirs
            let mem_dirs = claude_core::memory::memory_dirs(cwd);
            let mut found = false;
            for dir in &mem_dirs {
                let p = dir.join(rel_path);
                // Verify resolved path stays inside the memory directory
                if let Ok(canonical) = p.canonicalize() {
                    if !canonical.starts_with(dir) {
                        continue;
                    }
                }
                if p.exists() {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".into());
                    let _ = std::process::Command::new(&editor).arg(&p).status();
                    found = true;
                    break;
                }
            }
            if !found {
                // Create new file in user memory dir
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
        "delete" | "rm" => {
            let id = parts.get(1).copied().unwrap_or("").trim();
            if id.is_empty() {
                println!("Usage: /session delete <id>");
                return;
            }
            let sessions = claude_core::session::list_sessions();
            let found = sessions.iter().find(|s| s.id.starts_with(id));
            match found {
                Some(meta) => {
                    match claude_core::session::delete_session(&meta.id) {
                        Ok(()) => println!("\x1b[32m✓ Deleted session {:.8} ({})\x1b[0m", meta.id, meta.title),
                        Err(e) => eprintln!("\x1b[31mFailed to delete: {}\x1b[0m", e),
                    }
                }
                None => println!("No session found matching '{}'. Use /session list.", id),
            }
        }
        other => {
            println!("Unknown session subcommand: '{}'. Use save, list, load <id>, or delete <id>.", other);
        }
    }
}

/// Show git diff (staged + unstaged).
fn handle_diff_command(cwd: &std::path::Path) {
    let output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    match output {
        Ok(out) => {
            let diff = String::from_utf8_lossy(&out.stdout);
            if diff.is_empty() {
                println!("No changes (working tree is clean).");
            } else {
                println!("{}", diff);
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.is_empty() && !out.status.success() {
                eprintln!("\x1b[31m{}\x1b[0m", stderr.trim());
            }
        }
        Err(e) => eprintln!("\x1b[31mFailed to run git diff: {}\x1b[0m", e),
    }
}

/// Show session and git status.
async fn handle_status_command(engine: &QueryEngine, cwd: &std::path::Path) {
    let s = engine.state().read().await;
    println!("Session:  {}", &engine.session_id()[..8]);
    println!("Model:    {} ({})", claude_core::model::display_name(&s.model), s.model);
    println!("Turns:    {}", s.turn_count);
    println!("Messages: {}", s.messages.len());
    println!("Tokens:   {}↑ {}↓", s.total_input_tokens, s.total_output_tokens);
    println!("Mode:     {:?}", s.permission_mode);
    println!("CWD:      {}", cwd.display());

    // Git branch + status
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output();
    if let Ok(out) = branch {
        let branch_name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !branch_name.is_empty() {
            println!("Branch:   {}", branch_name);
        }
    }

    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output();
    if let Ok(out) = status {
        let lines = String::from_utf8_lossy(&out.stdout);
        let count = lines.lines().count();
        if count == 0 {
            println!("Git:      clean");
        } else {
            println!("Git:      {} changed file(s)", count);
        }
    }
}

/// Show current configuration.
fn handle_config_command(cwd: &std::path::Path) {
    let settings = crate::config::load_settings();
    match settings {
        Ok(s) => {
            println!("Configuration:");
            if let Some(ref model) = s.model {
                println!("  model: {}", model);
            }
            if let Some(ref mode) = s.permission_mode {
                println!("  permission_mode: {}", mode);
            }
            if !s.allowed_tools.is_empty() {
                println!("  allowed_tools: {:?}", s.allowed_tools);
            }
            if !s.denied_tools.is_empty() {
                println!("  denied_tools: {:?}", s.denied_tools);
            }
            if !s.permission_rules.is_empty() {
                println!("  permission_rules: {} rule(s)", s.permission_rules.len());
            }
            let hooks_count = s.hooks.pre_tool_use.len()
                + s.hooks.post_tool_use.len()
                + s.hooks.stop.len()
                + s.hooks.session_start.len()
                + s.hooks.session_end.len();
            if hooks_count > 0 {
                println!("  hooks: {} rule(s)", hooks_count);
            }
            if let Some(ref p) = s.custom_system_prompt {
                println!("  custom_system_prompt: {}...", &p[..p.len().min(60)]);
            }
            if let Some(ref p) = s.append_system_prompt {
                println!("  append_system_prompt: {}...", &p[..p.len().min(60)]);
            }

            // Show config file path
            let config_path = dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("claude")
                .join("settings.json");
            println!("\n  Config file: {}", config_path.display());
        }
        Err(e) => eprintln!("Failed to load config: {}", e),
    }

    // CLAUDE.md status
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let size = std::fs::metadata(&claude_md).map(|m| m.len()).unwrap_or(0);
        println!("  CLAUDE.md: {} ({} bytes)", claude_md.display(), size);
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

    let model = { parent_engine.state().read().await.model.clone() };
    let stream = parent_engine.submit(&augmented).await;
    if let Err(e) = print_stream(stream, &model, Some(parent_engine.cost_tracker())).await {
        eprintln!("\x1b[31mSkill error: {}\x1b[0m", e);
    }
}

/// Undo the last assistant turn — remove trailing assistant+user message pair.
async fn handle_undo(engine: &QueryEngine) {
    let mut s = engine.state().write().await;
    let len = s.messages.len();
    if len < 2 {
        println!("Nothing to undo.");
        return;
    }

    // Remove messages from the end until we've popped one assistant message.
    // Then also remove the preceding user message (if any) to keep the
    // conversation in a valid state (user→assistant pairs).
    let mut removed_assistant = false;
    while let Some(last) = s.messages.last() {
        let is_assistant = matches!(last, claude_core::message::Message::Assistant(_));
        s.messages.pop();
        if is_assistant {
            removed_assistant = true;
            break;
        }
    }

    // Also remove the preceding user message that triggered the assistant turn
    if removed_assistant {
        if let Some(last) = s.messages.last() {
            if matches!(last, claude_core::message::Message::User(_)) {
                s.messages.pop();
            }
        }
    }

    if removed_assistant {
        let new_len = s.messages.len();
        println!("\x1b[32m✓ Undone (removed {} message(s), {} remaining)\x1b[0m", len - new_len, new_len);
    } else {
        println!("Nothing to undo.");
    }
}

/// Run /doctor diagnostics.
async fn handle_doctor(engine: &QueryEngine, cwd: &std::path::Path) {
    println!("\x1b[1;36m╭───────────────────────────╮\x1b[0m");
    println!("\x1b[1;36m│    Claude Code Doctor     │\x1b[0m");
    println!("\x1b[1;36m╰───────────────────────────╯\x1b[0m\n");

    let mut warnings = 0u32;
    let mut errors = 0u32;

    // 1. API key
    let api_ok = std::env::var("ANTHROPIC_API_KEY").is_ok();
    if api_ok {
        println!("  \x1b[32m✓\x1b[0m API key configured");
    } else {
        println!("  \x1b[31m✗\x1b[0m ANTHROPIC_API_KEY not set");
        errors += 1;
    }

    // 2. Git
    let git_version = std::process::Command::new("git")
        .arg("--version")
        .output();
    match git_version {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  \x1b[32m✓\x1b[0m {}", ver);
        }
        _ => {
            println!("  \x1b[31m✗\x1b[0m git not found in PATH");
            errors += 1;
        }
    }

    // 3. Git repo
    let in_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if in_repo {
        println!("  \x1b[32m✓\x1b[0m Inside git repository");
    } else {
        println!("  \x1b[33m⚠\x1b[0m Not inside a git repository");
        warnings += 1;
    }

    // 4. CLAUDE.md
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let size = std::fs::metadata(&claude_md).map(|m| m.len()).unwrap_or(0);
        println!("  \x1b[32m✓\x1b[0m CLAUDE.md found ({} bytes)", size);
    } else {
        println!("  \x1b[33m⚠\x1b[0m No CLAUDE.md — run --init to create one");
        warnings += 1;
    }

    // 5. Rules directory
    let rules_dir = cwd.join(".claude").join("rules");
    if rules_dir.is_dir() {
        let count = std::fs::read_dir(&rules_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .count();
        if count > 0 {
            println!("  \x1b[32m✓\x1b[0m .claude/rules/: {} rule file(s)", count);
        } else {
            println!("  \x1b[2m·\x1b[0m .claude/rules/ exists (empty)");
        }
    }

    // 6. Skills directory
    let skills_dir = cwd.join(".claude").join("skills");
    if skills_dir.is_dir() {
        let count = std::fs::read_dir(&skills_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .count();
        if count > 0 {
            println!("  \x1b[32m✓\x1b[0m .claude/skills/: {} skill(s)", count);
        } else {
            println!("  \x1b[2m·\x1b[0m .claude/skills/ exists (empty)");
        }
    }

    // 7. Memory files
    let mem_files = claude_core::memory::list_memory_files(cwd);
    if !mem_files.is_empty() {
        println!("  \x1b[32m✓\x1b[0m {} memory file(s)", mem_files.len());
    }

    // 8. Sessions
    let sessions = claude_core::session::list_sessions();
    if !sessions.is_empty() {
        let latest_age = claude_core::session::format_age(&sessions[0].updated_at);
        println!("  \x1b[32m✓\x1b[0m {} saved session(s), latest: {}", sessions.len(), latest_age);
    }

    // 9. Settings file
    let settings = crate::config::load_settings();
    match settings {
        Ok(_) => println!("  \x1b[32m✓\x1b[0m Settings loaded OK"),
        Err(e) => {
            println!("  \x1b[31m✗\x1b[0m Settings error: {}", e);
            errors += 1;
        }
    }

    // 10. Model + token info
    {
        let s = engine.state().read().await;
        println!("  \x1b[2m·\x1b[0m Model: {}", s.model);
        println!("  \x1b[2m·\x1b[0m Permission mode: {:?}", s.permission_mode);
    }

    // Summary
    println!();
    if errors == 0 && warnings == 0 {
        println!("  \x1b[32m🎉 All checks passed!\x1b[0m");
    } else {
        if errors > 0 {
            println!("  \x1b[31m{} error(s)\x1b[0m", errors);
        }
        if warnings > 0 {
            println!("  \x1b[33m{} warning(s)\x1b[0m", warnings);
        }
    }
}

/// Launch a code review on recent git changes.
async fn handle_review(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    // Get the diff to review
    let diff_output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    let diff = match diff_output {
        Ok(out) => {
            let d = String::from_utf8_lossy(&out.stdout).to_string();
            if d.is_empty() {
                // Try staged changes
                let staged = std::process::Command::new("git")
                    .args(["diff", "--cached"])
                    .current_dir(cwd)
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if staged.is_empty() {
                    println!("No changes to review. Make some changes first.");
                    return;
                }
                staged
            } else {
                d
            }
        }
        Err(e) => {
            eprintln!("\x1b[31mFailed to get git diff: {}\x1b[0m", e);
            return;
        }
    };

    let review_prompt = if custom_prompt.is_empty() {
        format!(
            "Review the following code changes for bugs, style issues, security concerns, \
             and potential improvements. Be specific about file paths and line numbers.\n\n\
             ```diff\n{}\n```",
            diff
        )
    } else {
        format!("{}\n\n```diff\n{}\n```", custom_prompt, diff)
    };

    println!("\x1b[35m[Code Review]\x1b[0m");
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&review_prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
        eprintln!("\x1b[31mReview error: {}\x1b[0m", e);
    }
}