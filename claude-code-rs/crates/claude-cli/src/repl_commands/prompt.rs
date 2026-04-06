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

/// Create or review a pull request.
pub(crate) async fn handle_pr(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    // Get current branch and default branch
    let current_branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let default_branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            s.strip_prefix("origin/").unwrap_or(&s).to_string()
        })
        .unwrap_or_else(|| "main".into());

    let diff = std::process::Command::new("git")
        .args(["diff", &format!("origin/{}...HEAD", default_branch)])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let log = std::process::Command::new("git")
        .args(["log", "--oneline", &format!("origin/{}..HEAD", default_branch)])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    if diff.is_empty() && log.is_empty() {
        println!("No commits ahead of {}. Push some changes first.", default_branch);
        return;
    }

    let user_note = if custom_prompt.is_empty() {
        String::new()
    } else {
        format!("\nUser's instructions: {}\n", custom_prompt)
    };

    let prompt = format!(
        "Help me create a pull request for the branch `{branch}` targeting `{base}`.\n\n\
         Rules:\n\
         - Analyze the commits and diff below\n\
         - Generate a clear PR title and description\n\
         - PR title should be concise and descriptive\n\
         - PR description should include: summary of changes, motivation, testing notes\n\
         - Use markdown formatting in the description\n\
         {user_note}\n\
         Commits:\n```\n{log}\n```\n\n\
         Diff:\n```diff\n{diff}\n```",
        branch = current_branch,
        base = default_branch,
        user_note = user_note,
        log = log.trim(),
        diff = if diff.len() > 12000 {
            format!("{}…\n[truncated, {} total bytes]", &diff[..12000], diff.len())
        } else {
            diff
        },
    );

    println!("\x1b[35m[PR]\x1b[0m Analyzing {} → {}…", current_branch, default_branch);
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
        eprintln!("\x1b[31mPR error: {}\x1b[0m", e);
    }
}

/// Debug a problem with AI assistance.
pub(crate) async fn handle_bug(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    let mut context_parts: Vec<String> = Vec::new();

    // Collect recent git log for context
    if let Ok(out) = std::process::Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(cwd)
        .output()
    {
        let log = String::from_utf8_lossy(&out.stdout).to_string();
        if !log.is_empty() {
            context_parts.push(format!("Recent commits:\n```\n{}\n```", log.trim()));
        }
    }

    // Collect recent diff
    if let Ok(out) = std::process::Command::new("git")
        .args(["diff", "HEAD~1"])
        .current_dir(cwd)
        .output()
    {
        let diff = String::from_utf8_lossy(&out.stdout).to_string();
        if !diff.is_empty() {
            let truncated = if diff.len() > 6000 {
                format!("{}…\n[truncated]", &diff[..6000])
            } else {
                diff
            };
            context_parts.push(format!("Recent changes:\n```diff\n{}\n```", truncated));
        }
    }

    let context = if context_parts.is_empty() {
        "No git context available.".to_string()
    } else {
        context_parts.join("\n\n")
    };

    let user_note = if custom_prompt.is_empty() {
        "Help me identify and fix bugs in the recent changes.".to_string()
    } else {
        custom_prompt.to_string()
    };

    let prompt = format!(
        "Debug the following problem:\n\n{user_note}\n\n\
         Instructions:\n\
         - Read the relevant source files to understand the code\n\
         - Identify the root cause of the problem\n\
         - Suggest a specific fix with code changes\n\
         - If the problem description is vague, ask clarifying questions\n\n\
         {context}",
        user_note = user_note,
        context = context,
    );

    println!("\x1b[35m[Debug]\x1b[0m Investigating…");
    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker())).await {
        eprintln!("\x1b[31mDebug error: {}\x1b[0m", e);
    }
}
