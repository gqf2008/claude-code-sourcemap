//! REPL command handlers — split into focused submodules.
//!
//! Each submodule handles one category of slash commands:
//! - `memory` — /memory list, open, add
//! - `session` — /session save, load, list, delete; /undo; /export
//! - `config` — /config, /context, /login, /logout
//! - `doctor` — /doctor diagnostics
//! - `review` — /review (code review on git changes / PRs)
//! - `prompt` — /init, /commit, /pr, /bug (AI-driven)
//! - `skill` — skill runner

mod memory;
mod session;
mod config;
mod doctor;
mod review;
mod prompt;
mod skill;
mod mcp;
mod pr_comments;
mod branch;
mod agents;
mod theme;
mod plan;

// Re-export all handlers so callers can `use crate::repl_commands::*`
pub(crate) use memory::handle_memory_command;
pub(crate) use session::{handle_session_command, handle_undo, handle_export, handle_search, handle_history};
pub(crate) use config::{handle_config_command, handle_context, handle_login, handle_logout, handle_reload_context};
pub(crate) use doctor::handle_doctor;
pub(crate) use review::handle_review;
pub(crate) use prompt::{handle_init, handle_commit, handle_pr, handle_bug, handle_commit_push_pr};
pub(crate) use skill::run_skill;
pub(crate) use mcp::handle_mcp_command;
pub(crate) use pr_comments::handle_pr_comments;
pub(crate) use branch::handle_branch;
pub(crate) use agents::handle_agents_command;
pub(crate) use theme::handle_theme_command;
pub(crate) use plan::handle_plan_command;

use claude_agent::engine::QueryEngine;
use claude_agent::plugin::PluginLoader;

/// Show loaded plugins and their status.
pub(crate) fn handle_plugin_command(sub: &str, cwd: &std::path::Path) {
    let loader = PluginLoader::discover(cwd);

    match sub.split_whitespace().next().unwrap_or("list") {
        "list" | "" => {
            if loader.count() == 0 {
                println!("No plugins found.");
                println!("\x1b[2mAdd plugins to .claude/plugins/ or ~/.claude/plugins/\x1b[0m");
                return;
            }

            println!("\x1b[1mLoaded Plugins\x1b[0m ({} total, {} enabled)\n",
                loader.count(), loader.enabled_count());

            for plugin in loader.plugins() {
                let status = if plugin.manifest.enabled {
                    "\x1b[32m●\x1b[0m"
                } else {
                    "\x1b[31m○\x1b[0m"
                };
                println!("  {} \x1b[1m{}\x1b[0m v{} \x1b[2m({})\x1b[0m",
                    status,
                    plugin.manifest.name,
                    plugin.manifest.version,
                    plugin.source,
                );
                if !plugin.manifest.description.is_empty() {
                    println!("    {}", plugin.manifest.description);
                }
                if !plugin.manifest.commands.is_empty() {
                    let cmd_names: Vec<&str> = plugin.manifest.commands.iter()
                        .map(|c| c.name.as_str())
                        .collect();
                    println!("    Commands: /{}", cmd_names.join(", /"));
                }
                if !plugin.manifest.skills.is_empty() {
                    let skill_names: Vec<&str> = plugin.manifest.skills.iter()
                        .map(|s| s.name.as_str())
                        .collect();
                    println!("    Skills: {}", skill_names.join(", "));
                }
                if !plugin.manifest.hooks.is_empty() {
                    println!("    Hooks: {}", plugin.manifest.hooks.len());
                }
            }
        }
        "info" => {
            let name = sub.split_whitespace().nth(1);
            match name {
                Some(name) => {
                    match loader.get(name) {
                        Some(plugin) => {
                            println!("\x1b[1m{}\x1b[0m v{}", plugin.manifest.name, plugin.manifest.version);
                            println!("Source:  {}", plugin.source);
                            println!("Path:    {}", plugin.dir.display());
                            println!("Enabled: {}", plugin.manifest.enabled);
                            if !plugin.manifest.description.is_empty() {
                                println!("Desc:    {}", plugin.manifest.description);
                            }
                            for cmd in &plugin.manifest.commands {
                                println!("\n  Command: /{}", cmd.name);
                                if !cmd.description.is_empty() {
                                    println!("  Desc:    {}", cmd.description);
                                }
                                if let Some(ref file) = cmd.prompt_file {
                                    println!("  Prompt:  {}", file);
                                }
                            }
                            for skill in &plugin.manifest.skills {
                                println!("\n  Skill: {}", skill.name);
                                println!("  File:  {}", skill.prompt_file);
                            }
                        }
                        None => println!("Plugin '{}' not found.", name),
                    }
                }
                None => println!("Usage: /plugin info <name>"),
            }
        }
        "enable" => {
            let name = sub.split_whitespace().nth(1);
            match name {
                Some(name) => println!("Enable not yet persistent. Plugin '{}' noted.", name),
                None => println!("Usage: /plugin enable <name>"),
            }
        }
        "disable" => {
            let name = sub.split_whitespace().nth(1);
            match name {
                Some(name) => println!("Disable not yet persistent. Plugin '{}' noted.", name),
                None => println!("Usage: /plugin disable <name>"),
            }
        }
        "install" => {
            let path_str = sub.split_whitespace().nth(1);
            match path_str {
                Some(path_str) => {
                    let source = std::path::Path::new(path_str);
                    // Resolve relative paths against cwd
                    let source = if source.is_relative() {
                        cwd.join(source)
                    } else {
                        source.to_path_buf()
                    };
                    match PluginLoader::install_from_path(&source) {
                        Ok(name) => {
                            println!("\x1b[32m✓\x1b[0m Plugin '\x1b[1m{}\x1b[0m' installed successfully.", name);
                            println!("\x1b[2mUse /plugin list to verify, /plugin reload to activate.\x1b[0m");
                        }
                        Err(e) => {
                            eprintln!("\x1b[31m✗ Install failed: {}\x1b[0m", e);
                        }
                    }
                }
                None => println!("Usage: /plugin install <path-to-plugin-dir>"),
            }
        }
        "reload" => {
            let loader = PluginLoader::discover(cwd);
            println!("\x1b[32m✓\x1b[0m Reloaded {} plugin(s) ({} enabled).",
                loader.count(), loader.enabled_count());
        }
        other => {
            println!("Unknown subcommand: {}. Use: /plugin [list|info|enable|disable|install|reload] <name>", other);
        }
    }
}

/// Execute a plugin command by sending its prompt to the engine.
pub(crate) async fn handle_plugin_run(engine: &QueryEngine, name: &str, prompt: &str) {
    eprintln!("\x1b[36m🔌 Running plugin command: /{}\x1b[0m", name);
    let stream = engine.submit(prompt).await;
    let cost = engine.cost_tracker();
    let model = {
        let s = engine.state().read().await;
        s.model.clone()
    };
    if let Err(e) = crate::output::print_stream(stream, &model, Some(cost), None).await {
        eprintln!("\x1b[31mPlugin command error: {}\x1b[0m", e);
    }
}

/// Show git diff (staged + unstaged) with structured coloring.
pub(crate) fn handle_diff_command(cwd: &std::path::Path) {
    // Get list of changed files for per-file diff
    let files_out = std::process::Command::new("git")
        .args(["diff", "HEAD", "--name-only"])
        .current_dir(cwd)
        .output();

    let files: Vec<String> = match files_out {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect(),
        Err(e) => {
            eprintln!("\x1b[31mFailed to run git diff: {}\x1b[0m", e);
            return;
        }
    };

    if files.is_empty() {
        println!("No changes (working tree is clean).");
        return;
    }

    println!("\x1b[1m{} file(s) changed:\x1b[0m\n", files.len());

    for file in &files {
        // Get old (HEAD) and new (working tree) content
        let old = std::process::Command::new("git")
            .args(["show", &format!("HEAD:{}", file)])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let new_path = cwd.join(file);
        let new = std::fs::read_to_string(&new_path).unwrap_or_default();

        if old == new {
            continue;
        }

        crate::diff_display::print_diff(&old, &new, Some(file));
        let stats = crate::diff_display::diff_stats(&old, &new);
        eprintln!("  {}\n", stats);
    }
}

/// Show session and git status.
pub(crate) async fn handle_status_command(engine: &QueryEngine, cwd: &std::path::Path) {
    let s = engine.state().read().await;
    println!("Session:  {}", &engine.session_id()[..8]);
    println!("Model:    {} ({})", claude_core::model::display_name_any(&s.model), s.model);
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

    // Plugins
    {
        let loader = claude_agent::plugin::PluginLoader::discover(cwd);
        let plugins = loader.plugins();
        if !plugins.is_empty() {
            let cmd_count: usize = plugins.iter().map(|p| p.manifest.commands.len()).sum();
            println!("Plugins:  {} loaded ({} commands)", plugins.len(), cmd_count);
        }
    }

    // Context usage
    if let Some(pct) = engine.context_usage_percent().await {
        let color = if pct >= 90 { "\x1b[31m" } else if pct >= 80 { "\x1b[33m" } else { "" };
        let reset = if !color.is_empty() { "\x1b[0m" } else { "" };
        println!("Context:  {}{pct}%{} of window used", color, reset);
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
