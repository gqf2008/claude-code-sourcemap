//! REPL command handlers — extracted from repl.rs for clarity.
//!
//! Each pub(crate) function handles one slash command or sub-command.

use claude_agent::engine::QueryEngine;
use claude_core::memory::list_memory_files;
use claude_core::skills::SkillEntry;
use rustyline::DefaultEditor;

use crate::output::print_stream;

/// Handle /memory subcommands.
pub(crate) fn handle_memory_command(sub: &str, cwd: &std::path::Path) {
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
pub(crate) async fn handle_session_command(sub: &str, engine: &QueryEngine) {
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
pub(crate) fn handle_diff_command(cwd: &std::path::Path) {
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
pub(crate) async fn handle_status_command(engine: &QueryEngine, cwd: &std::path::Path) {
    let s = engine.state().read().await;
    println!("Session:  {}", &engine.session_id()[..8]);
    println!("Model:    {} ({})", claude_core::model::display_name(&s.model), s.model);
    println!("Turns:    {}", s.turn_count);
    println!("Messages: {}", s.messages.len());
    println!("Tokens:   {}↑ {}↓", format_tokens(s.total_input_tokens), format_tokens(s.total_output_tokens));

    // Cache statistics
    if s.total_cache_read_tokens > 0 || s.total_cache_creation_tokens > 0 {
        let cache_total = s.total_cache_read_tokens + s.total_cache_creation_tokens;
        let hit_rate = if cache_total > 0 {
            s.total_cache_read_tokens as f64 / cache_total as f64 * 100.0
        } else { 0.0 };
        println!("Cache:    {} read, {} write ({:.0}% hit rate)",
            format_tokens(s.total_cache_read_tokens),
            format_tokens(s.total_cache_creation_tokens),
            hit_rate);
    }

    // Cost
    let cost = engine.cost_tracker().total_usd();
    if cost > 0.0 {
        println!("Cost:     {}", format_cost(cost));
    }

    // Errors
    if s.total_errors > 0 {
        let breakdown: Vec<String> = s.error_counts.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect();
        println!("Errors:   {} ({})", s.total_errors, breakdown.join(", "));
    }

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

pub(crate) fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub(crate) fn format_cost(cost: f64) -> String {
    if cost >= 0.5 {
        format!("${:.2}", cost)
    } else if cost >= 0.0001 {
        format!("${:.4}", cost)
    } else {
        "$0.00".to_string()
    }
}

/// Show current configuration.
pub(crate) fn handle_config_command(cwd: &std::path::Path) {
    let loaded = claude_core::config::Settings::load_merged(cwd);

    println!("\x1b[1mConfiguration\x1b[0m");
    println!("{}", loaded.display_sources());

    // CLAUDE.md status
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let size = std::fs::metadata(&claude_md).map(|m| m.len()).unwrap_or(0);
        println!("  CLAUDE.md: {} ({} bytes)", claude_md.display(), size);
    }

    // Settings file paths
    println!("\n\x1b[1mSettings files:\x1b[0m");
    if let Some(user_path) = dirs::home_dir().map(|h| h.join(".claude").join("settings.json")) {
        let exists = if user_path.exists() { "✓" } else { "✗" };
        println!("  {} User:    {}", exists, user_path.display());
    }
    let proj_path = cwd.join(".claude").join("settings.json");
    let proj_exists = if proj_path.exists() { "✓" } else { "✗" };
    println!("  {} Project: {}", proj_exists, proj_path.display());
    let local_path = cwd.join(".claude").join("settings.local.json");
    let local_exists = if local_path.exists() { "✓" } else { "✗" };
    println!("  {} Local:   {}", local_exists, local_path.display());
}

/// Run a skill as a single-shot sub-agent conversation.
pub(crate) async fn run_skill(
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
pub(crate) async fn handle_undo(engine: &QueryEngine) {
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
pub(crate) async fn handle_doctor(engine: &QueryEngine, cwd: &std::path::Path) {
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
pub(crate) async fn handle_review(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
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

// ── /init ────────────────────────────────────────────────────────────────────

/// Initialize CLAUDE.md for the current project.
pub(crate) async fn handle_init(engine: &QueryEngine, cwd: &std::path::Path) {
    let claude_md_path = cwd.join("CLAUDE.md");
    let existing = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path).ok()
    } else {
        None
    };

    // Gather project context
    let mut context_parts: Vec<String> = Vec::new();

    // Check for common manifest files
    for manifest in &[
        "package.json", "Cargo.toml", "pyproject.toml", "go.mod",
        "pom.xml", "build.gradle", "Makefile", "CMakeLists.txt",
    ] {
        let p = cwd.join(manifest);
        if p.exists() {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let truncated: String = content.lines().take(50).collect::<Vec<_>>().join("\n");
                context_parts.push(format!("--- {} ---\n{}", manifest, truncated));
            }
        }
    }

    // Check for README
    for readme in &["README.md", "README.rst", "README.txt", "README"] {
        let p = cwd.join(readme);
        if p.exists() {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let truncated: String = content.lines().take(80).collect::<Vec<_>>().join("\n");
                context_parts.push(format!("--- {} ---\n{}", readme, truncated));
            }
            break;
        }
    }

    // Check for CI config
    for ci in &[".github/workflows", ".gitlab-ci.yml", "Jenkinsfile", ".circleci/config.yml"] {
        let p = cwd.join(ci);
        if p.exists() {
            context_parts.push(format!("CI config found: {}", ci));
        }
    }

    let context = if context_parts.is_empty() {
        "No manifest or README files found.".to_string()
    } else {
        context_parts.join("\n\n")
    };

    let prompt = if let Some(ref existing_content) = existing {
        format!(
            "The project at {} already has a CLAUDE.md. Analyze the current content and the project \
             context below. Suggest specific improvements as diffs. Do NOT silently overwrite.\n\n\
             Existing CLAUDE.md:\n```\n{}\n```\n\nProject context:\n{}\n\n\
             Propose concrete changes to improve the CLAUDE.md.",
            cwd.display(), existing_content, context
        )
    } else {
        format!(
            "Create a CLAUDE.md file for the project at {}. Analyze the project context below \
             and generate a concise CLAUDE.md that includes ONLY:\n\
             - Build, test, and lint commands (especially non-obvious ones)\n\
             - Code style rules that differ from language defaults\n\
             - Repo conventions (branch naming, commit style, PR process)\n\
             - Required env vars or setup steps\n\
             - Non-obvious architectural decisions or gotchas\n\n\
             Do NOT include: file-by-file structure, standard language conventions, generic advice.\n\n\
             Project context:\n{}\n\n\
             Use the Write tool to create CLAUDE.md in the project root.",
            cwd.display(), context
        )
    };

    println!("\x1b[35m[Init]\x1b[0m Analyzing project…");
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
        eprintln!("\x1b[31mInit error: {}\x1b[0m", e);
    }
}

// ── /commit ──────────────────────────────────────────────────────────────────

/// Stage changes and commit with an AI-generated message.
pub(crate) async fn handle_commit(engine: &QueryEngine, cwd: &std::path::Path, user_message: &str) {
    // 1. Check git status
    let status_out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output();

    let status = match status_out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(e) => {
            eprintln!("\x1b[31mNot a git repository or git not found: {}\x1b[0m", e);
            return;
        }
    };

    if status.trim().is_empty() {
        println!("No changes to commit.");
        return;
    }

    // 2. Get diff for context
    let diff = std::process::Command::new("git")
        .args(["diff", "--staged"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let unstaged_diff = std::process::Command::new("git")
        .args(["diff"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    // 3. Get recent commit style
    let log = std::process::Command::new("git")
        .args(["log", "--oneline", "-10"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    // 4. Build the prompt
    let combined_diff = if diff.is_empty() { &unstaged_diff } else { &diff };
    let has_staged = !diff.is_empty();

    let prompt = format!(
        "Commit the current changes in this git repository.\n\n\
         Rules:\n\
         - Analyze the changes and create a clear commit message\n\
         - Follow the commit style from recent commits shown below\n\
         - Focus on the \"why\" not the \"what\"\n\
         - Keep the message concise (1 line summary, optional body)\n\
         - {stage_instruction}\n\
         - NEVER use --amend, --no-verify, or --force\n\
         - NEVER commit secrets or credentials\n\
         - Use `git add` to stage specific files, then `git commit -m \"message\"`\n\
         {user_note}\n\
         Recent commits:\n```\n{log}\n```\n\n\
         git status:\n```\n{status}\n```\n\n\
         Diff:\n```diff\n{diff}\n```",
        stage_instruction = if has_staged {
            "Changes are already staged — commit them directly"
        } else {
            "Stage the relevant changed files with `git add <file>` (NOT `git add -A` unless all changes are related)"
        },
        user_note = if user_message.is_empty() {
            String::new()
        } else {
            format!("\nUser's note about this commit: {}\n", user_message)
        },
        log = log.trim(),
        status = status.trim(),
        diff = if combined_diff.len() > 8000 {
            format!("{}…\n[truncated, {} total bytes]", &combined_diff[..8000], combined_diff.len())
        } else {
            combined_diff.to_string()
        },
    );

    println!("\x1b[35m[Commit]\x1b[0m Analyzing changes…");
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
        eprintln!("\x1b[31mCommit error: {}\x1b[0m", e);
    }
}

// ── /login ───────────────────────────────────────────────────────────────────

pub(crate) fn handle_login() {
    let Some(config_dir) = claude_core::config::Settings::config_dir() else {
        eprintln!("\x1b[31mCannot determine config directory\x1b[0m");
        return;
    };
    let settings_path = config_dir.join("settings.json");

    // Read existing settings or create new
    let mut settings: serde_json::Value = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Prompt for API key (hide input)
    print!("Enter your Anthropic API key: ");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut key = String::new();
    if std::io::stdin().read_line(&mut key).is_err() {
        eprintln!("\x1b[31mFailed to read input\x1b[0m");
        return;
    }
    let key = key.trim().to_string();

    if key.is_empty() {
        println!("No key provided. Cancelled.");
        return;
    }

    if !key.starts_with("sk-ant-") && !key.starts_with("sk-") {
        println!("\x1b[33mWarning: API key doesn't start with 'sk-ant-' — this may not be a valid Anthropic key.\x1b[0m");
    }

    // Save to settings
    settings["api_key"] = serde_json::Value::String(key);
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        eprintln!("\x1b[31mFailed to create config dir: {}\x1b[0m", e);
        return;
    }
    match serde_json::to_string_pretty(&settings) {
        Ok(json) => match std::fs::write(&settings_path, json) {
            Ok(_) => println!("\x1b[32m✓ API key saved to {}\x1b[0m", settings_path.display()),
            Err(e) => eprintln!("\x1b[31mFailed to save settings: {}\x1b[0m", e),
        },
        Err(e) => eprintln!("\x1b[31mFailed to serialize settings: {}\x1b[0m", e),
    }
}

// ── /logout ──────────────────────────────────────────────────────────────────

pub(crate) fn handle_logout() {
    let Some(config_dir) = claude_core::config::Settings::config_dir() else {
        eprintln!("\x1b[31mCannot determine config directory\x1b[0m");
        return;
    };
    let settings_path = config_dir.join("settings.json");
    if !settings_path.exists() {
        println!("No saved settings found.");
        return;
    }

    let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if settings.get("api_key").is_none() {
        println!("No saved API key found.");
        return;
    }

    if let Some(obj) = settings.as_object_mut() {
        obj.remove("api_key");
    }
    match serde_json::to_string_pretty(&settings) {
        Ok(json) => match std::fs::write(&settings_path, json) {
            Ok(_) => println!("\x1b[32m✓ API key removed from settings\x1b[0m"),
            Err(e) => eprintln!("\x1b[31mFailed to update settings: {}\x1b[0m", e),
        },
        Err(e) => eprintln!("\x1b[31mFailed to serialize settings: {}\x1b[0m", e),
    }
}

// ── /context ─────────────────────────────────────────────────────────────────

pub(crate) async fn handle_context(engine: &QueryEngine, cwd: &std::path::Path) {
    println!("\x1b[1;36m── Loaded Context ──\x1b[0m\n");

    // 1. Model info
    let state = engine.state().read().await;
    let display = claude_core::model::display_name(&state.model);
    println!("\x1b[1mModel:\x1b[0m {} ({})", display, state.model);
    println!("\x1b[1mPermission mode:\x1b[0m {:?}", state.permission_mode);
    println!("\x1b[1mTurns:\x1b[0m {}", state.turn_count);
    println!("\x1b[1mMessages:\x1b[0m {}", state.messages.len());
    drop(state);

    // 2. CLAUDE.md files
    println!("\n\x1b[1;33m── CLAUDE.md ──\x1b[0m");
    let claude_md = claude_core::claude_md::load_claude_md(cwd);
    if claude_md.is_empty() {
        println!("  \x1b[2m(none found)\x1b[0m");
    } else {
        let preview: String = claude_md.lines().take(20).collect::<Vec<_>>().join("\n");
        println!("{}", preview);
        let total_lines = claude_md.lines().count();
        if total_lines > 20 {
            println!("  \x1b[2m… ({} more lines)\x1b[0m", total_lines - 20);
        }
    }

    // 3. Memory files
    println!("\n\x1b[1;33m── Memory ──\x1b[0m");
    let mem_files = claude_core::memory::list_memory_files(cwd);
    if mem_files.is_empty() {
        println!("  \x1b[2m(no memory files)\x1b[0m");
    } else {
        for f in &mem_files {
            let type_tag = f.memory_type.as_ref()
                .map(|t| format!("[{}] ", t.as_str()))
                .unwrap_or_default();
            println!("  {}{}", type_tag, f.filename);
        }
    }

    // 4. Skills
    println!("\n\x1b[1;33m── Skills ──\x1b[0m");
    let skills = claude_core::skills::load_skills(cwd);
    if skills.is_empty() {
        println!("  \x1b[2m(no skills)\x1b[0m");
    } else {
        for s in &skills {
            println!("  /{}: {}", s.name, s.description);
        }
    }

    // 5. Token estimate
    let state = engine.state().read().await;
    let system_tokens = claude_core::token_estimation::estimate_text_tokens(&claude_md);
    let msg_tokens = claude_core::token_estimation::estimate_messages_tokens(&state.messages);
    println!("\n\x1b[1;33m── Token Estimates ──\x1b[0m");
    println!("  System prompt: ~{} tokens", system_tokens);
    println!("  Conversation:  ~{} tokens", msg_tokens);
    println!("  Total:         ~{} tokens", system_tokens + msg_tokens);
}

// ── /export ──────────────────────────────────────────────────────────────────

pub(crate) async fn handle_export(engine: &QueryEngine, cwd: &std::path::Path, format: &str) {
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        println!("No conversation to export.");
        return;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");

    match format {
        "json" => {
            let filename = format!("claude_export_{}.json", timestamp);
            let path = cwd.join(&filename);
            let export = serde_json::json!({
                "model": state.model,
                "turn_count": state.turn_count,
                "messages": state.messages.iter().map(|m| match m {
                    claude_core::message::Message::User(u) => serde_json::json!({
                        "role": "user",
                        "content": u.content.iter().filter_map(|b| match b {
                            claude_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    claude_core::message::Message::Assistant(a) => serde_json::json!({
                        "role": "assistant",
                        "content": a.content.iter().filter_map(|b| match b {
                            claude_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            claude_core::message::ContentBlock::ToolUse { name, input, .. } =>
                                Some(serde_json::json!({"tool": name, "input": input})),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    claude_core::message::Message::System(s) => serde_json::json!({
                        "role": "system",
                        "content": s.message,
                    }),
                }).collect::<Vec<_>>(),
            });
            let json = serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".into());
            match std::fs::write(&path, json) {
                Ok(_) => println!("\x1b[32m✓ Exported to {}\x1b[0m", path.display()),
                Err(e) => eprintln!("\x1b[31mExport failed: {}\x1b[0m", e),
            }
        }
        _ => {
            // Default: markdown
            let filename = format!("claude_export_{}.md", timestamp);
            let path = cwd.join(&filename);
            let mut md = format!("# Claude Conversation Export\n\nModel: {}\nTurns: {}\n\n---\n\n",
                state.model, state.turn_count);

            for msg in &state.messages {
                match msg {
                    claude_core::message::Message::User(u) => {
                        md.push_str("## 🧑 User\n\n");
                        for block in &u.content {
                            if let claude_core::message::ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    claude_core::message::Message::Assistant(a) => {
                        md.push_str("## 🤖 Assistant\n\n");
                        for block in &a.content {
                            match block {
                                claude_core::message::ContentBlock::Text { text } => {
                                    md.push_str(text);
                                    md.push_str("\n\n");
                                }
                                claude_core::message::ContentBlock::ToolUse { name, .. } => {
                                    md.push_str(&format!("*Used tool: {}*\n\n", name));
                                }
                                _ => {}
                            }
                        }
                    }
                    claude_core::message::Message::System(_) => {}
                }
                md.push_str("---\n\n");
            }

            match std::fs::write(&path, &md) {
                Ok(_) => println!("\x1b[32m✓ Exported to {}\x1b[0m", path.display()),
                Err(e) => eprintln!("\x1b[31mExport failed: {}\x1b[0m", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_tokens ────────────────────────────────────────────────

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(99_999), "100.0K");
        assert_eq!(format_tokens(999_999), "1000.0K");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        assert_eq!(format_tokens(10_000_000), "10.0M");
    }

    // ── format_cost ──────────────────────────────────────────────────

    #[test]
    fn test_format_cost_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
    }

    #[test]
    fn test_format_cost_tiny() {
        assert_eq!(format_cost(0.00001), "$0.00");
    }

    #[test]
    fn test_format_cost_small() {
        assert_eq!(format_cost(0.001), "$0.0010");
        assert_eq!(format_cost(0.0123), "$0.0123");
    }

    #[test]
    fn test_format_cost_medium() {
        assert_eq!(format_cost(0.5), "$0.50");
        assert_eq!(format_cost(1.23), "$1.23");
    }

    #[test]
    fn test_format_cost_large() {
        assert_eq!(format_cost(10.0), "$10.00");
        assert_eq!(format_cost(99.99), "$99.99");
    }
}
