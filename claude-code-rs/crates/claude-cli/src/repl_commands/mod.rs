//! REPL command handlers — split into focused submodules.
//!
//! Each submodule handles one category of slash commands:
//! - `memory` — /memory list, open, add
//! - `session` — /session save, load, list, delete; /undo; /export
//! - `config` — /config, /context, /login, /logout
//! - `doctor` — /doctor diagnostics
//! - `prompt` — /review, /init, /commit (AI-driven)
//! - `skill` — skill runner

mod memory;
mod session;
mod config;
mod doctor;
mod prompt;
mod skill;
mod mcp;

// Re-export all handlers so callers can `use crate::repl_commands::*`
pub(crate) use memory::handle_memory_command;
pub(crate) use session::{handle_session_command, handle_undo, handle_export, handle_search};
pub(crate) use config::{handle_config_command, handle_context, handle_login, handle_logout, handle_reload_context};
pub(crate) use doctor::handle_doctor;
pub(crate) use prompt::{handle_review, handle_init, handle_commit, handle_pr, handle_bug};
pub(crate) use skill::run_skill;
pub(crate) use mcp::handle_mcp_command;

use claude_agent::engine::QueryEngine;

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
