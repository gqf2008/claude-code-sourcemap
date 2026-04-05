mod config;
mod repl;
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
    } else {
        let default_content = format!(
            "# Project Guidelines\n\n\
             ## Overview\n\
             <!-- Describe your project here -->\n\n\
             ## Code Style\n\
             <!-- Add coding conventions, preferred patterns, etc. -->\n\n\
             ## Build & Test\n\
             <!-- Add build, lint, and test commands -->\n\
             ```bash\n\
             # Example:\n\
             # npm run build\n\
             # npm test\n\
             ```\n\n\
             ## Important Notes\n\
             <!-- Add any critical information Claude should know -->\n"
        );
        std::fs::write(&claude_md_path, default_content)?;
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
    Ok(())
}