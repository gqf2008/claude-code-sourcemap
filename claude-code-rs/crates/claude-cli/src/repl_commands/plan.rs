//! `/plan` command handler — plan mode and plan file management.

use std::path::{Path, PathBuf};

use claude_agent::engine::QueryEngine;
use claude_core::permissions::PermissionMode;

use crate::theme;

/// Handle `/plan [args]`.
///
/// - No args: toggle plan mode (enter/exit).
/// - `open`: open plan file in `$EDITOR`.
/// - `show`/`view`: display current plan.
/// - Other text: enable plan mode with that description as initial plan.
pub(crate) async fn handle_plan_command(args: &str, engine: &QueryEngine, cwd: &Path) {
    let args = args.trim();

    // Check if already in plan mode
    let in_plan_mode = {
        let state = engine.state().read().await;
        state.permission_mode == PermissionMode::Plan
    };

    if args.is_empty() {
        // Toggle plan mode
        if in_plan_mode {
            {
                let mut state = engine.state().write().await;
                state.exit_plan_mode();
            }
            println!("{}📋 Plan mode disabled{}", theme::c_tool(), theme::RESET);
            println!("{}  Switched back to previous permission mode.{}", theme::DIM, theme::RESET);
        } else {
            {
                let mut state = engine.state().write().await;
                state.enter_plan_mode();
            }
            println!("{}📋 Plan mode enabled{}", theme::c_tool(), theme::RESET);
            println!("{}  Tools restricted to read-only. Describe your goal and the AI will create a plan.{}", theme::DIM, theme::RESET);
            println!("{}  Use /plan again to exit plan mode.{}", theme::DIM, theme::RESET);
        }
        return;
    }

    if args == "open" {
        let plan_path = get_plan_path(cwd);
        if !plan_path.exists() {
            let initial = "# Plan\n\n_Describe your goals here._\n";
            if let Some(parent) = plan_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&plan_path, initial);
        }

        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| {
                if cfg!(windows) { "notepad".to_string() } else { "vi".to_string() }
            });

        println!("{}Opening {} in {}...{}", theme::DIM, plan_path.display(), editor, theme::RESET);
        match std::process::Command::new(&editor).arg(&plan_path).status() {
            Ok(status) if status.success() => {
                println!("{}✓ Plan file saved: {}{}", theme::c_ok(), plan_path.display(), theme::RESET);
            }
            Ok(status) => {
                eprintln!("{}Editor exited with: {}{}", theme::c_err(), status, theme::RESET);
            }
            Err(e) => {
                eprintln!("{}Failed to open editor '{}': {}{}", theme::c_err(), editor, e, theme::RESET);
                eprintln!("{}  Set $EDITOR to your preferred editor.{}", theme::DIM, theme::RESET);
            }
        }
        return;
    }

    if args == "show" || args == "view" {
        let plan_path = get_plan_path(cwd);
        if plan_path.exists() {
            match std::fs::read_to_string(&plan_path) {
                Ok(content) => {
                    println!("{}Current Plan{}", theme::BOLD, theme::RESET);
                    println!("{}{}{}", theme::DIM, plan_path.display(), theme::RESET);
                    println!();
                    println!("{}", content);
                }
                Err(e) => {
                    eprintln!("{}Failed to read plan: {}{}", theme::c_err(), e, theme::RESET);
                }
            }
        } else {
            println!("{}No plan file found. Use /plan open to create one.{}", theme::DIM, theme::RESET);
        }
        return;
    }

    // Any other text: enable plan mode with description
    if !in_plan_mode {
        let mut state = engine.state().write().await;
        state.enter_plan_mode();
    }

    let plan_path = get_plan_path(cwd);
    if let Some(parent) = plan_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!("# Plan\n\n{}\n", args);
    let _ = std::fs::write(&plan_path, &content);
    println!("{}📋 Plan mode enabled{}", theme::c_tool(), theme::RESET);
    println!("{}  Plan: {}{}", theme::DIM, args, theme::RESET);
    println!("{}  Saved to: {}{}", theme::DIM, plan_path.display(), theme::RESET);
}

/// Get the plan file path for the current project.
fn get_plan_path(cwd: &Path) -> PathBuf {
    let base = claude_core::config::Settings::claude_dir()
        .unwrap_or_else(|| PathBuf::from(".claude"));
    let plans_dir = base.join("plans");
    // Use a slug derived from the cwd path for uniqueness
    let slug = cwd_to_slug(cwd);
    plans_dir.join(format!("{}.md", slug))
}

/// Convert a cwd path to a filesystem-safe slug.
fn cwd_to_slug(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cwd_to_slug() {
        let slug = cwd_to_slug(Path::new("/home/user/project"));
        assert!(!slug.is_empty());
        assert!(!slug.contains('/'));
        assert!(!slug.contains('\\'));
    }

    #[test]
    fn test_cwd_to_slug_windows() {
        let slug = cwd_to_slug(Path::new("C:\\Users\\gxh\\project"));
        assert!(!slug.is_empty());
        assert!(!slug.contains('\\'));
        assert!(!slug.contains(':'));
    }
}
