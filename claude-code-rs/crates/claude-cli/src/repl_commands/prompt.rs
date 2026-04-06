//! /review, /init, /commit command handlers.

use claude_agent::engine::QueryEngine;
use crate::output::print_stream;

/// Launch a code review on recent git changes.
pub(crate) async fn handle_review(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    let diff_output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    let diff = match diff_output {
        Ok(out) => {
            let d = String::from_utf8_lossy(&out.stdout).to_string();
            if d.is_empty() {
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

/// Initialize CLAUDE.md for the current project.
pub(crate) async fn handle_init(engine: &QueryEngine, cwd: &std::path::Path) {
    let claude_md_path = cwd.join("CLAUDE.md");
    let existing = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path).ok()
    } else {
        None
    };

    let mut context_parts: Vec<String> = Vec::new();

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

/// Stage changes and commit with an AI-generated message.
pub(crate) async fn handle_commit(engine: &QueryEngine, cwd: &std::path::Path, user_message: &str) {
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

    let log = std::process::Command::new("git")
        .args(["log", "--oneline", "-10"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

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
