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

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { EnvFilter::new("debug") } else { EnvFilter::new("warn") };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let settings = config::load_settings()?;

    let api_key = cli.api_key.or(settings.api_key).ok_or_else(|| {
        anyhow::anyhow!("API key required. Set ANTHROPIC_API_KEY or use --api-key.")
    })?;

    let cwd = match cli.cwd {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let system_prompt = config::build_system_prompt(
        cli.system_prompt.as_deref(),
        settings.custom_system_prompt.as_deref(),
        settings.append_system_prompt.as_deref(),
    );

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
        .build();

    // ── Ctrl-C → abort signal ────────────────────────────────────────────────
    {
        let abort = engine.abort_signal();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\n\x1b[33m[Interrupted — aborting task…]\x1b[0m");
                abort.abort();
            }
        });
    }

    // Run SessionStart hook once at startup
    if let Some(extra) = engine.run_session_start().await {
        if !extra.is_empty() {
            eprintln!("\x1b[33m[SessionStart hook]: {}\x1b[0m", extra.trim());
        }
    }

    if let Some(prompt) = cli.prompt {
        if cli.print {
            // --print mode: only emit final text to stdout, progress to stderr
            output::run_single(&engine, &prompt).await?;
        } else {
            // Default non-interactive: rich task progress
            output::run_task_interactive(&engine, &prompt).await?;
        }
    } else {
        repl::run(engine, skills, cwd).await?;
    }

    Ok(())
}


