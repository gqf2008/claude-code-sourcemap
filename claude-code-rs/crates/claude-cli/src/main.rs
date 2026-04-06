mod config;
mod repl;
mod repl_commands;
mod commands;
mod output;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use claude_core::skills::load_skills;

#[derive(Parser, Debug)]
#[command(name = "claude", version, about = "Claude Code — AI coding assistant (Rust)")]
struct Cli {
    /// Initial prompt — run non-interactively and exit
    prompt: Option<String>,

    /// API key (or set ANTHROPIC_API_KEY)
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    api_key: Option<String>,

    /// Model
    #[arg(long, short, default_value = "claude-sonnet-4-20250514")]
    model: String,

    /// Permission mode: default | bypass | acceptEdits | plan
    #[arg(long, default_value = "default")]
    permission_mode: String,

    /// Custom system prompt
    #[arg(long)]
    system_prompt: Option<String>,

    /// Working directory
    #[arg(long, short = 'd')]
    cwd: Option<String>,

    /// Max conversation turns
    #[arg(long, default_value = "100")]
    max_turns: u32,

    /// Disable CLAUDE.md injection
    #[arg(long)]
    no_claude_md: bool,

    /// Print final output only (suppress progress, suitable for piping)
    #[arg(long, short = 'p')]
    print: bool,

    /// Output format for non-interactive mode: text (default) or json
    #[arg(long, default_value = "text")]
    output_format: String,

    /// Resume the most recent session
    #[arg(long, alias = "continue")]
    resume: bool,

    /// Resume a specific session by ID
    #[arg(long)]
    session_id: Option<String>,

    /// Initialize CLAUDE.md and settings in the current project
    #[arg(long)]
    init: bool,

    /// Additional context directories (files are read and included)
    #[arg(long = "add-dir")]
    add_dirs: Vec<String>,

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,

    /// Enable coordinator (multi-agent orchestration) mode
    #[arg(long)]
    coordinator: bool,

    /// Restrict available tools (comma-separated or repeatable)
    #[arg(long = "allowed-tools")]
    allowed_tools: Vec<String>,

    /// Maximum output tokens per response
    #[arg(long, default_value = "16384")]
    max_tokens: u32,

    /// Enable extended thinking (chain-of-thought reasoning)
    #[arg(long)]
    thinking: bool,

    /// Token budget for extended thinking (default 10000)
    #[arg(long, default_value = "10000")]
    thinking_budget: u32,

    /// Additional system prompt text appended to the default prompt
    #[arg(long)]
    append_system_prompt: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { EnvFilter::new("debug") } else { EnvFilter::new("warn") };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let settings = config::load_settings()?;

    let cwd = match cli.cwd {
        Some(ref dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // ── Handle --init: create CLAUDE.md and settings ────────────────────────
    if cli.init {
        return run_init(&cwd);
    }

    let api_key = cli.api_key.or(settings.api_key).ok_or_else(|| {
        anyhow::anyhow!("API key required. Set ANTHROPIC_API_KEY or use --api-key.")
    })?;

    // Build system prompt: if user specified --system-prompt, use that.
    // Otherwise the engine will build the full modular prompt via system_prompt.rs.
    let system_prompt = cli.system_prompt
        .or(settings.custom_system_prompt)
        .unwrap_or_default();

    let permission_mode = config::parse_permission_mode(&cli.permission_mode);
    let skills = load_skills(&cwd);

    let engine = claude_agent::engine::QueryEngine::builder(api_key, &cwd)
        .model(&cli.model)
        .system_prompt(system_prompt)
        .max_turns(cli.max_turns)
        .permission_checker(claude_agent::permissions::PermissionChecker::new(
            permission_mode,
            settings.permission_rules,
        ))
        .hooks_config(settings.hooks)
        .load_claude_md(!cli.no_claude_md)
        .load_memory(true)
        .coordinator_mode(cli.coordinator)
        .max_tokens(cli.max_tokens)
        .allowed_tools(cli.allowed_tools)
        .thinking(if cli.thinking {
            Some(claude_api::types::ThinkingConfig {
                thinking_type: "enabled".into(),
                budget_tokens: Some(cli.thinking_budget),
            })
        } else {
            None
        })
        .append_system_prompt(cli.append_system_prompt)
        .build();

    // ── Ctrl-C → abort signal (second press → force exit) ──────────────────
    {
        let abort = engine.abort_signal();
        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_ok() {
                    if abort.is_aborted() {
                        // Second Ctrl-C: force exit
                        eprintln!("\n\x1b[31m[Force exit]\x1b[0m");
                        std::process::exit(130);
                    }
                    eprintln!("\n\x1b[33m[Interrupted — press Ctrl-C again to force exit]\x1b[0m");
                    abort.abort();
                }
            }
        });
    }

    // Run SessionStart hook once at startup
    if let Some(extra) = engine.run_session_start().await {
        if !extra.is_empty() {
            eprintln!("\x1b[33m[SessionStart hook]: {}\x1b[0m", extra.trim());
        }
    }

    // ── Handle --resume / --session-id ──────────────────────────────────────
    if let Some(ref sid) = cli.session_id {
        match engine.restore_session(sid).await {
            Ok(title) => eprintln!("\x1b[32m✓ Resumed session: {}\x1b[0m", title),
            Err(e) => eprintln!("\x1b[31mFailed to restore session {}: {}\x1b[0m", sid, e),
        }
    } else if cli.resume {
        match resume_latest_session(&engine).await {
            Ok(Some(title)) => eprintln!("\x1b[32m✓ Resumed: {}\x1b[0m", title),
            Ok(None) => eprintln!("\x1b[33mNo saved sessions found.\x1b[0m"),
            Err(e) => eprintln!("\x1b[31mResume failed: {}\x1b[0m", e),
        }
    }

    if let Some(prompt) = cli.prompt {
        // Combine explicit prompt with any piped stdin
        let full_prompt = if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let mut stdin_buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin_buf)?;
            if stdin_buf.is_empty() {
                prompt
            } else {
                format!("{}\n\n<stdin>\n{}</stdin>", prompt, stdin_buf.trim())
            }
        } else {
            prompt
        };

        // Append --add-dir context
        let full_prompt = if !cli.add_dirs.is_empty() {
            let mut ctx = full_prompt;
            for dir in &cli.add_dirs {
                let dir_path = std::path::Path::new(dir);
                if dir_path.is_dir() {
                    ctx.push_str(&format!("\n\n<context source=\"{}\">\n", dir));
                    if let Ok(entries) = std::fs::read_dir(dir_path) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.is_file() {
                                if let Ok(content) = std::fs::read_to_string(&p) {
                                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                                    ctx.push_str(&format!("--- {} ---\n{}\n\n", name, content.trim()));
                                }
                            }
                        }
                    }
                    ctx.push_str("</context>");
                } else {
                    eprintln!("\x1b[33mWarning: --add-dir '{}' not found\x1b[0m", dir);
                }
            }
            ctx
        } else {
            full_prompt
        };

        if cli.output_format == "json" {
            output::run_json(&engine, &full_prompt).await?;
        } else if cli.print {
            output::run_single(&engine, &full_prompt).await?;
        } else {
            output::run_task_interactive(&engine, &full_prompt).await?;
        }
    } else if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        // Stdin-only mode: read from pipe with no explicit prompt
        let mut stdin_buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin_buf)?;
        let stdin_buf = stdin_buf.trim();
        if !stdin_buf.is_empty() {
            if cli.output_format == "json" {
                output::run_json(&engine, stdin_buf).await?;
            } else if cli.print {
                output::run_single(&engine, stdin_buf).await?;
            } else {
                output::run_task_interactive(&engine, stdin_buf).await?;
            }
        } else {
            eprintln!("No input provided. Use `claude \"prompt\"` or pipe via stdin.");
        }
    } else {
        repl::run(engine, skills, cwd).await?;
    }

    Ok(())
}

/// Resume the most recent session.
async fn resume_latest_session(
    engine: &claude_agent::engine::QueryEngine,
) -> anyhow::Result<Option<String>> {
    let sessions = claude_core::session::list_sessions();
    if let Some(latest) = sessions.first() {
        let title = engine.restore_session(&latest.id).await?;
        Ok(Some(title))
    } else {
        Ok(None)
    }
}

/// Initialize a new project with CLAUDE.md and optional settings.
fn run_init(cwd: &std::path::Path) -> anyhow::Result<()> {
    let claude_md_path = cwd.join("CLAUDE.md");
    if claude_md_path.exists() {
        println!("CLAUDE.md already exists at {}", claude_md_path.display());
        println!("  Use `/init` in the REPL for AI-powered improvements.");
    } else {
        let content = generate_claude_md_template(cwd);
        std::fs::write(&claude_md_path, &content)?;
        println!("✓ Created {}", claude_md_path.display());
    }

    // Create .claude/ directory for skills, memory, and rules
    let claude_dir = cwd.join(".claude");
    let skills_dir = claude_dir.join("skills");
    let rules_dir = claude_dir.join("rules");
    if !skills_dir.exists() {
        std::fs::create_dir_all(&skills_dir)?;
        println!("✓ Created {}", skills_dir.display());
    }
    if !rules_dir.exists() {
        std::fs::create_dir_all(&rules_dir)?;
        println!("✓ Created {}", rules_dir.display());
    }

    // Create settings directory if it doesn't exist
    if let Some(config_dir) = dirs::config_dir() {
        let settings_dir = config_dir.join("claude");
        if !settings_dir.exists() {
            std::fs::create_dir_all(&settings_dir)?;
            println!("✓ Created config dir: {}", settings_dir.display());
        }
        let settings_path = settings_dir.join("settings.json");
        if !settings_path.exists() {
            std::fs::write(&settings_path, "{}\n")?;
            println!("✓ Created {}", settings_path.display());
        }
    }

    println!("\n🎉 Project initialized! Edit CLAUDE.md to customize Claude's behavior.");
    println!("   Run `claude` to start a conversation.");
    println!("   Run `claude` then `/init` for AI-powered CLAUDE.md generation.");
    Ok(())
}

/// Auto-detect project type and generate a tailored CLAUDE.md template.
fn generate_claude_md_template(cwd: &std::path::Path) -> String {
    let mut sections = Vec::new();

    // Detect project type
    let has_cargo = cwd.join("Cargo.toml").exists();
    let has_package_json = cwd.join("package.json").exists();
    let has_pyproject = cwd.join("pyproject.toml").exists();
    let has_go_mod = cwd.join("go.mod").exists();
    let has_pom = cwd.join("pom.xml").exists();
    let has_makefile = cwd.join("Makefile").exists();

    // Header
    sections.push("# CLAUDE.md\n\nThis file provides guidance to Claude Code when working with this repository.".to_string());

    // Build & Test section based on detected project type
    let mut build_cmds = Vec::new();
    if has_cargo {
        build_cmds.push("cargo build           # Build the project");
        build_cmds.push("cargo test            # Run all tests");
        build_cmds.push("cargo test -p <crate> # Test a specific crate");
        build_cmds.push("cargo clippy          # Lint");
        build_cmds.push("cargo fmt --check     # Check formatting");
    }
    if has_package_json {
        build_cmds.push("npm install           # Install dependencies");
        build_cmds.push("npm run build         # Build");
        build_cmds.push("npm test              # Run tests");
        build_cmds.push("npm run lint          # Lint");
    }
    if has_pyproject {
        build_cmds.push("pip install -e .      # Install in dev mode");
        build_cmds.push("pytest                # Run tests");
        build_cmds.push("ruff check .          # Lint");
    }
    if has_go_mod {
        build_cmds.push("go build ./...        # Build");
        build_cmds.push("go test ./...         # Test");
        build_cmds.push("go vet ./...          # Lint");
    }
    if has_pom {
        build_cmds.push("mvn compile           # Build");
        build_cmds.push("mvn test              # Test");
    }
    if has_makefile {
        build_cmds.push("make                  # Build (see Makefile for targets)");
    }

    if build_cmds.is_empty() {
        sections.push("## Build & Test\n\n```bash\n# Add your build, test, and lint commands here\n```".to_string());
    } else {
        let cmds = build_cmds.join("\n");
        sections.push(format!("## Build & Test\n\n```bash\n{}\n```", cmds));
    }

    // Code style section
    sections.push("## Code Style\n\n<!-- Add coding conventions that differ from language defaults -->".to_string());

    // Architecture section
    sections.push("## Architecture\n\n<!-- Brief description of key directories and patterns -->".to_string());

    // Important notes
    sections.push("## Important Notes\n\n<!-- Add gotchas, required env vars, or non-obvious setup steps -->".to_string());

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::Path;

    // ── CLI arg parsing ──────────────────────────────────────────────

    #[test]
    fn test_cli_defaults() {
        let cli = Cli::try_parse_from(["claude"]).unwrap();
        assert!(cli.prompt.is_none());
        assert_eq!(cli.model, "claude-sonnet-4-20250514");
        assert_eq!(cli.permission_mode, "default");
        assert_eq!(cli.max_turns, 100);
        assert_eq!(cli.max_tokens, 16384);
        assert!(!cli.verbose);
        assert!(!cli.no_claude_md);
        assert!(!cli.print);
        assert!(!cli.resume);
        assert!(!cli.coordinator);
        assert!(!cli.thinking);
        assert!(!cli.init);
    }

    #[test]
    fn test_cli_with_prompt() {
        let cli = Cli::try_parse_from(["claude", "hello world"]).unwrap();
        assert_eq!(cli.prompt.as_deref(), Some("hello world"));
    }

    #[test]
    fn test_cli_model_flag() {
        let cli = Cli::try_parse_from(["claude", "-m", "claude-opus-4-20250514"]).unwrap();
        assert_eq!(cli.model, "claude-opus-4-20250514");
    }

    #[test]
    fn test_cli_verbose_and_print() {
        let cli = Cli::try_parse_from(["claude", "-v", "-p", "hi"]).unwrap();
        assert!(cli.verbose);
        assert!(cli.print);
    }

    #[test]
    fn test_cli_resume_alias() {
        let cli = Cli::try_parse_from(["claude", "--continue"]).unwrap();
        assert!(cli.resume);
    }

    #[test]
    fn test_cli_thinking_flags() {
        let cli = Cli::try_parse_from(["claude", "--thinking", "--thinking-budget", "20000"]).unwrap();
        assert!(cli.thinking);
        assert_eq!(cli.thinking_budget, 20000);
    }

    #[test]
    fn test_cli_allowed_tools() {
        let cli = Cli::try_parse_from(["claude", "--allowed-tools", "Read", "--allowed-tools", "Bash"]).unwrap();
        assert_eq!(cli.allowed_tools, vec!["Read", "Bash"]);
    }

    #[test]
    fn test_cli_permission_mode() {
        let cli = Cli::try_parse_from(["claude", "--permission-mode", "bypass"]).unwrap();
        assert_eq!(cli.permission_mode, "bypass");
    }

    #[test]
    fn test_cli_init_flag() {
        let cli = Cli::try_parse_from(["claude", "--init"]).unwrap();
        assert!(cli.init);
    }

    // ── generate_claude_md_template ──────────────────────────────────

    #[test]
    fn test_template_empty_dir() {
        let tmp = std::env::temp_dir().join("claude_test_empty_dir");
        let _ = std::fs::create_dir_all(&tmp);
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("# CLAUDE.md"));
        assert!(md.contains("## Build & Test"));
        assert!(md.contains("## Code Style"));
        assert!(md.contains("## Architecture"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_template_rust_project() {
        let tmp = std::env::temp_dir().join("claude_test_rust_proj");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("cargo build"));
        assert!(md.contains("cargo test"));
        assert!(md.contains("cargo clippy"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_template_node_project() {
        let tmp = std::env::temp_dir().join("claude_test_node_proj");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("package.json"), "{}").unwrap();
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("npm install"));
        assert!(md.contains("npm test"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}