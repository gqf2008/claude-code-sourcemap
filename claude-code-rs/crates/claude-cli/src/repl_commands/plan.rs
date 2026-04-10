//! `/plan` command handler — plan mode and plan file management.

use std::path::{Path, PathBuf};

use claude_agent::engine::QueryEngine;
use claude_core::permissions::PermissionMode;

/// Handle `/plan [args]`.
///
/// - No args: toggle plan mode (enter/exit).
/// - `open`: open plan file in `$EDITOR`.
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
                state.permission_mode = PermissionMode::Default;
            }
            println!("\x1b[36m📋 Plan mode disabled\x1b[0m");
            println!("\x1b[2m  Switched back to default permission mode.\x1b[0m");
        } else {
            {
                let mut state = engine.state().write().await;
                state.permission_mode = PermissionMode::Plan;
            }
            println!("\x1b[36m📋 Plan mode enabled\x1b[0m");
            println!("\x1b[2m  Tools restricted to read-only. Describe your goal and the AI will create a plan.\x1b[0m");
            println!("\x1b[2m  Use /plan again to exit plan mode.\x1b[0m");
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

        println!("\x1b[2mOpening {} in {}...\x1b[0m", plan_path.display(), editor);
        match std::process::Command::new(&editor).arg(&plan_path).status() {
            Ok(status) if status.success() => {
                println!("\x1b[32m✓ Plan file saved: {}\x1b[0m", plan_path.display());
            }
            Ok(status) => {
                eprintln!("\x1b[31mEditor exited with: {}\x1b[0m", status);
            }
            Err(e) => {
                eprintln!("\x1b[31mFailed to open editor '{}': {}\x1b[0m", editor, e);
                eprintln!("\x1b[2m  Set $EDITOR to your preferred editor.\x1b[0m");
            }
        }
        return;
    }

    if args == "show" || args == "view" {
        let plan_path = get_plan_path(cwd);
        if plan_path.exists() {
            match std::fs::read_to_string(&plan_path) {
                Ok(content) => {
                    println!("\x1b[1mCurrent Plan\x1b[0m");
                    println!("\x1b[2m{}\x1b[0m", plan_path.display());
                    println!();
                    println!("{}", content);
                }
                Err(e) => {
                    eprintln!("\x1b[31mFailed to read plan: {}\x1b[0m", e);
                }
            }
        } else {
            println!("\x1b[2mNo plan file found. Use /plan open to create one.\x1b[0m");
        }
        return;
    }

    // Any other text: enable plan mode with description
    if !in_plan_mode {
        let mut state = engine.state().write().await;
        state.permission_mode = PermissionMode::Plan;
    }

    let plan_path = get_plan_path(cwd);
    if let Some(parent) = plan_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!("# Plan\n\n{}\n", args);
    let _ = std::fs::write(&plan_path, &content);
    println!("\x1b[36m📋 Plan mode enabled\x1b[0m");
    println!("\x1b[2m  Plan: {}\x1b[0m", args);
    println!("\x1b[2m  Saved to: {}\x1b[0m", plan_path.display());
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
