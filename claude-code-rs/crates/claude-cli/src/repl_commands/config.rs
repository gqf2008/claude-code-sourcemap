//! /config, /context, /login, /logout command handlers.

use claude_agent::engine::QueryEngine;

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

/// Show loaded context details.
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

    // 2. Settings sources
    println!("\n\x1b[1;33m── Settings ──\x1b[0m");
    let loaded = claude_core::config::Settings::load_merged(cwd);
    if loaded.layers.is_empty() {
        println!("  \x1b[2m(defaults only)\x1b[0m");
    } else {
        for (source, _) in &loaded.layers {
            println!("  ✓ {}", source);
        }
    }
    println!("{}", loaded.settings.summary());

    // 3. Tools
    println!("\n\x1b[1;33m── Tools ──\x1b[0m");
    let tool_count = engine.tool_count();
    println!("  {} tool(s) registered", tool_count);

    // 4. CLAUDE.md files
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

    // 5. Memory files
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

    // 6. Skills
    println!("\n\x1b[1;33m── Skills ──\x1b[0m");
    let skills = claude_core::skills::load_skills(cwd);
    if skills.is_empty() {
        println!("  \x1b[2m(no skills)\x1b[0m");
    } else {
        for s in &skills {
            println!("  /{}: {}", s.name, s.description);
        }
    }

    // 7. Active hooks
    println!("\n\x1b[1;33m── Hooks ──\x1b[0m");
    let hooks = &loaded.settings.hooks;
    let hook_counts = [
        ("PreToolUse", hooks.pre_tool_use.len()),
        ("PostToolUse", hooks.post_tool_use.len()),
        ("Stop", hooks.stop.len()),
        ("SessionStart", hooks.session_start.len()),
        ("SessionEnd", hooks.session_end.len()),
        ("UserPromptSubmit", hooks.user_prompt_submit.len()),
    ];
    let total_hooks: usize = hook_counts.iter().map(|(_, c)| c).sum();
    if total_hooks == 0 {
        println!("  \x1b[2m(no hooks configured)\x1b[0m");
    } else {
        for (name, count) in &hook_counts {
            if *count > 0 {
                println!("  {}: {} rule(s)", name, count);
            }
        }
    }

    // 8. Token estimate
    let state = engine.state().read().await;
    let system_tokens = claude_core::token_estimation::estimate_text_tokens(&claude_md);
    let msg_tokens = claude_core::token_estimation::estimate_messages_tokens(&state.messages);
    println!("\n\x1b[1;33m── Token Estimates ──\x1b[0m");
    println!("  System prompt: ~{} tokens", system_tokens);
    println!("  Conversation:  ~{} tokens", msg_tokens);
    println!("  Total:         ~{} tokens", system_tokens + msg_tokens);
}

pub(crate) fn handle_login() {
    let Some(config_dir) = claude_core::config::Settings::config_dir() else {
        eprintln!("\x1b[31mCannot determine config directory\x1b[0m");
        return;
    };
    let settings_path = config_dir.join("settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

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

/// Reload CLAUDE.md, memory, and settings without restarting the session.
pub(crate) async fn handle_reload_context(engine: &QueryEngine, cwd: &std::path::Path) {
    println!("\x1b[33mReloading context…\x1b[0m");

    // 1. Reload settings
    let loaded = claude_core::config::Settings::load_merged(cwd);
    println!("  ✓ Settings reloaded ({} source(s))", loaded.layers.len());

    // 2. Reload CLAUDE.md
    let claude_md = claude_core::claude_md::load_claude_md(cwd);
    let md_lines = claude_md.lines().count();
    if md_lines > 0 {
        println!("  ✓ CLAUDE.md reloaded ({} lines)", md_lines);
    } else {
        println!("  \x1b[2m(no CLAUDE.md found)\x1b[0m");
    }

    // 3. Reload memory
    let mem_files = claude_core::memory::list_memory_files(cwd);
    println!("  ✓ Memory: {} file(s)", mem_files.len());

    // 4. Reload skills
    let skills = claude_core::skills::load_skills(cwd);
    println!("  ✓ Skills: {} loaded", skills.len());

    // 5. Reload hooks from settings
    let hooks = &loaded.settings.hooks;
    let total_hooks = hooks.pre_tool_use.len()
        + hooks.post_tool_use.len()
        + hooks.stop.len()
        + hooks.session_start.len()
        + hooks.session_end.len()
        + hooks.user_prompt_submit.len();
    println!("  ✓ Hooks: {} rule(s)", total_hooks);

    // 6. Update engine system prompt with fresh CLAUDE.md
    engine.update_system_prompt_context(&claude_md).await;

    // 7. Summary
    let state = engine.state().read().await;
    let system_tokens = claude_core::token_estimation::estimate_text_tokens(&claude_md);
    let msg_tokens = claude_core::token_estimation::estimate_messages_tokens(&state.messages);
    println!("\n\x1b[32m✓ Context reloaded ({} system + {} conversation tokens)\x1b[0m",
        system_tokens, msg_tokens);
}
