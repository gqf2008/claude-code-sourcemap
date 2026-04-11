//! /doctor diagnostics command handler.

use clawed_agent::engine::QueryEngine;

/// Run /doctor diagnostics.
pub(crate) async fn handle_doctor(engine: &QueryEngine, cwd: &std::path::Path) {
    println!("\x1b[1;36m╭───────────────────────────╮\x1b[0m");
    println!("\x1b[1;36m│    Claude Code Doctor     │\x1b[0m");
    println!("\x1b[1;36m╰───────────────────────────╯\x1b[0m\n");

    let mut warnings = 0u32;
    let mut errors = 0u32;

    // 1. API key (instant)
    let api_ok = std::env::var("ANTHROPIC_API_KEY").is_ok();
    if api_ok {
        println!("  \x1b[32m✓\x1b[0m API key configured");
    } else {
        println!("  \x1b[31m✗\x1b[0m ANTHROPIC_API_KEY not set");
        errors += 1;
    }

    // Parallel external tool checks (git, rg, node)
    let cwd_owned = cwd.to_path_buf();
    let (git_ver, git_repo, rg_ver, node_ver) = tokio::join!(
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("git").arg("--version").output()
        }),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(&cwd_owned)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }),
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("rg").arg("--version").output()
        }),
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("node").arg("--version").output()
        }),
    );

    // 2. Git version
    match git_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  \x1b[32m✓\x1b[0m {}", ver);
        }
        _ => {
            println!("  \x1b[31m✗\x1b[0m git not found in PATH");
            errors += 1;
        }
    }

    // 3. Git repo
    let in_repo = git_repo.unwrap_or(false);
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
    let mem_files = clawed_core::memory::list_memory_files(cwd);
    if !mem_files.is_empty() {
        println!("  \x1b[32m✓\x1b[0m {} memory file(s)", mem_files.len());
    }

    // 8. Sessions
    let sessions = clawed_core::session::list_sessions();
    if !sessions.is_empty() {
        let latest_age = clawed_core::session::format_age(&sessions[0].updated_at);
        println!("  \x1b[32m✓\x1b[0m {} saved session(s), latest: {}", sessions.len(), latest_age);
    }

    // 9. Settings file (multi-layer)
    let loaded = clawed_core::config::Settings::load_merged(cwd);
    if loaded.layers.is_empty() {
        println!("  \x1b[2m·\x1b[0m Using default settings (no config files found)");
    } else {
        let sources: Vec<String> = loaded.sources.iter().map(|s| s.to_string()).collect();
        println!("  \x1b[32m✓\x1b[0m Settings loaded from: {}", sources.join(", "));
    }

    // 10. Ripgrep (result from parallel check)
    match rg_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string();
            println!("  \x1b[32m✓\x1b[0m {}", ver);
        }
        _ => {
            println!("  \x1b[33m⚠\x1b[0m ripgrep (rg) not found — GrepTool may not work");
            warnings += 1;
        }
    }

    // 11. Node.js (result from parallel check)
    match node_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  \x1b[32m✓\x1b[0m Node.js {}", ver);
        }
        _ => {
            println!("  \x1b[2m·\x1b[0m Node.js not found (optional, for MCP servers)");
        }
    }

    // 12. Model + token info
    {
        let s = engine.state().read().await;
        let display = clawed_core::model::display_name_any(&s.model);
        println!("  \x1b[2m·\x1b[0m Model: {} ({})", display, s.model);
        println!("  \x1b[2m·\x1b[0m Permission mode: {:?}", s.permission_mode);
        println!("  \x1b[2m·\x1b[0m Tools: {} registered", engine.tool_count());
        if let Some(pct) = engine.context_usage_percent().await {
            println!("  \x1b[2m·\x1b[0m Context usage: {}%", pct);
        }
    }

    // 13. MCP configuration
    let mcp_config = cwd.join(".claude").join("mcp.json");
    if mcp_config.exists() {
        if let Ok(content) = std::fs::read_to_string(&mcp_config) {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(val) => {
                    let count = val.get("mcpServers")
                        .and_then(|s| s.as_object())
                        .map(|o| o.len())
                        .unwrap_or(0);
                    println!("  \x1b[32m✓\x1b[0m MCP config: {} server(s) defined", count);
                }
                Err(_) => {
                    println!("  \x1b[33m⚠\x1b[0m .claude/mcp.json exists but is invalid JSON");
                    warnings += 1;
                }
            }
        }
    }

    // 14. API base URL (custom provider check)
    if let Ok(base_url) = std::env::var("ANTHROPIC_BASE_URL") {
        println!("  \x1b[2m·\x1b[0m Custom API URL: {}", base_url);
    }
    if let Ok(provider) = std::env::var("CLAUDE_CODE_PROVIDER") {
        println!("  \x1b[2m·\x1b[0m Provider: {}", provider);
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
