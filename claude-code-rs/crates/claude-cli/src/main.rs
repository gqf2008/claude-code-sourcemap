mod auth;
mod config;
mod init;
mod input;
mod repl;
mod repl_commands;
mod commands;
mod output;
mod markdown;
mod diff_display;
mod session;
pub mod theme;
mod ui;

use std::sync::Arc;

use clap::{CommandFactory, Parser};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "claude", version, about = "Claude Code — AI coding assistant (Rust)")]
struct Cli {
    /// Initial prompt — run non-interactively and exit.
    /// If omitted, starts an interactive REPL session
    prompt: Option<String>,

    /// API key for authentication.
    /// Can also be set via ANTHROPIC_API_KEY env var.
    /// For non-Anthropic providers, use the provider's key format
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    api_key: Option<String>,

    /// Model identifier or alias.
    /// Aliases: sonnet, opus, haiku, best.
    /// Full names: claude-sonnet-4-20250514, claude-opus-4-20250514, etc.
    /// Third-party: gpt-4o, deepseek-chat, qwen-plus, etc.
    #[arg(long, short, default_value = "claude-sonnet-4-20250514")]
    model: String,

    /// Permission mode: default | bypass | acceptEdits | plan.
    ///   default       — ask before risky operations.
    ///   bypass        — skip all permission checks (dangerzone).
    ///   acceptEdits   — auto-approve file edits, still ask for shell commands.
    ///   plan          — read-only, no tool execution
    #[arg(long, default_value = "default")]
    permission_mode: String,

    /// Replace the entire system prompt with custom text.
    /// Overrides built-in prompt, CLAUDE.md, and all injected sections
    #[arg(long)]
    system_prompt: Option<String>,

    /// Working directory for the session.
    /// Tools resolve paths relative to this directory.
    /// Defaults to current working directory
    #[arg(long, short = 'd')]
    cwd: Option<String>,

    /// Max conversation turns before auto-exit (non-interactive mode).
    /// Each user→assistant round-trip counts as one turn
    #[arg(long, default_value = "100")]
    max_turns: u32,

    /// Disable CLAUDE.md injection.
    /// Skips loading project-level (.claude/CLAUDE.md) and user-level CLAUDE.md files
    #[arg(long)]
    no_claude_md: bool,

    /// Print final assistant response only.
    /// Suppresses progress indicators — suitable for piping to other commands
    #[arg(long, short = 'p')]
    print: bool,

    /// Output format for non-interactive mode: text | json.
    ///   text — plain text output (default).
    ///   json — structured JSON with messages, tool calls, and metadata
    #[arg(long, default_value = "text")]
    output_format: String,

    /// Resume the most recent conversation session.
    /// Alias: --continue
    #[arg(long, alias = "continue")]
    resume: bool,

    /// Resume a specific session by its UUID.
    /// Use /session list in REPL to find session IDs
    #[arg(long)]
    session_id: Option<String>,

    /// Initialize project configuration.
    /// Creates .claude/CLAUDE.md and .claude/settings.json interactively
    #[arg(long)]
    init: bool,

    /// Additional context directories.
    /// Files in these directories are read and included as context.
    /// Can be specified multiple times: --add-dir src --add-dir docs
    #[arg(long = "add-dir")]
    add_dirs: Vec<String>,

    /// Enable verbose/debug logging output.
    /// Shows API calls, token usage, tool execution details
    #[arg(long, short)]
    verbose: bool,

    /// Enable coordinator (multi-agent orchestration) mode.
    /// Spawns sub-agents via AgentTool for parallel task execution
    #[arg(long)]
    coordinator: bool,

    /// Restrict available tools.
    /// Comma-separated list or repeatable: --allowed-tools Bash,FileRead.
    /// Available tools: Bash, FileRead, FileEdit, FileWrite, Glob, Grep,
    /// LS, WebFetch, WebSearch, AskUser, MultiEdit, Notebook, TodoRead,
    /// TodoWrite, Agent, Task, Skill, MCP, etc.
    #[arg(long = "allowed-tools")]
    allowed_tools: Vec<String>,

    /// Maximum output tokens per model response.
    /// Higher values allow longer responses but cost more
    #[arg(long, default_value = "16384")]
    max_tokens: u32,

    /// Enable extended thinking (chain-of-thought reasoning).
    /// Model shows its reasoning process before answering.
    /// Uses additional tokens from --thinking-budget
    #[arg(long)]
    thinking: bool,

    /// Token budget for extended thinking.
    /// Only effective when --thinking is enabled.
    /// Higher budgets allow deeper reasoning on complex problems
    #[arg(long, default_value = "10000")]
    thinking_budget: u32,

    /// Additional system prompt text appended after the default prompt.
    /// Unlike --system-prompt, this preserves built-in prompt and CLAUDE.md
    #[arg(long)]
    append_system_prompt: Option<String>,

    /// List all saved sessions and exit.
    /// Shows session ID, title, message count, and age.
    /// Useful for scripting: `claude --list-sessions | head -5`
    #[arg(long)]
    list_sessions: bool,

    /// Generate shell completions and exit.
    /// Supported shells: bash, zsh, fish, powershell, elvish.
    /// Example: claude --completions bash >> ~/.bashrc
    #[arg(long, value_name = "SHELL")]
    completions: Option<clap_complete::Shell>,

    /// API provider backend.
    /// Supported: anthropic, openai, deepseek, ollama, together, groq, bedrock, vertex.
    /// Each provider has different model availability and pricing
    #[arg(long, default_value = "anthropic")]
    provider: String,

    /// Override API base URL.
    /// Useful for proxies, self-hosted instances, or custom endpoints.
    /// Example: --base-url http://localhost:11434/v1
    #[arg(long)]
    base_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ── Handle --completions: generate shell completions and exit ────────
    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "claude", &mut std::io::stdout());
        return Ok(());
    }

    // ── Handle --list-sessions: print sessions and exit ──────────────────
    if cli.list_sessions {
        let sessions = claude_core::session::list_sessions();
        if sessions.is_empty() {
            println!("No saved sessions.");
        } else {
            for s in &sessions {
                let age = claude_core::session::format_age(&s.updated_at);
                let title = s.custom_title.as_deref().unwrap_or(&s.title);
                println!(
                    "{:.8}\t{}\t{} msgs\t{} turns\t${:.4}\t{}",
                    s.id, title, s.message_count, s.turn_count, s.total_cost_usd, age,
                );
            }
        }
        return Ok(());
    }

    // RUST_LOG takes priority (e.g. RUST_LOG=claude_api=trace for raw packets)
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(filter).init();

    let settings = config::load_settings()?;

    // Initialize terminal theme from settings
    {
        let theme_setting = settings.theme.as_deref()
            .and_then(|s| s.parse::<theme::ThemeName>().ok().map(|n| match n {
                theme::ThemeName::Dark => theme::ThemeSetting::Dark,
                theme::ThemeName::Light => theme::ThemeSetting::Light,
                theme::ThemeName::DarkDaltonized => theme::ThemeSetting::DarkDaltonized,
                theme::ThemeName::LightDaltonized => theme::ThemeSetting::LightDaltonized,
                theme::ThemeName::DarkAnsi => theme::ThemeSetting::DarkAnsi,
                theme::ThemeName::LightAnsi => theme::ThemeSetting::LightAnsi,
            }))
            .unwrap_or(theme::ThemeSetting::Auto);
        theme::init_theme(theme_setting);
    }

    // Inject env vars from settings.json before auth resolution (single-threaded init)
    let _env_backup = settings.apply_env();

    let cwd = match cli.cwd {
        Some(ref dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // ── Handle --init: create CLAUDE.md and settings ────────────────────────
    if cli.init {
        return init::run_init(&cwd);
    }

    let api_key = auth::resolve_api_key(&cli.provider, cli.api_key.as_deref(), settings.api_key.as_deref())?;

    // For non-Anthropic providers, use provider-specific default model if user didn't override
    let model_input = if cli.model == "claude-sonnet-4-20250514" && cli.provider != "anthropic" {
        claude_core::model::default_model_for_provider(&cli.provider).to_string()
    } else {
        cli.model.clone()
    };

    // Resolve model aliases and validate (provider-aware).
    // When --base-url is specified, skip strict validation — the user is targeting
    // a compatible API (e.g. DashScope, LiteLLM) that may use non-Claude model names.
    let model = if cli.base_url.is_some() {
        let trimmed = model_input.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("Model name cannot be empty"));
        }
        trimmed
    } else {
        claude_core::model::validate_model_for_provider(&model_input, &cli.provider)
            .map_err(|e| anyhow::anyhow!(e))?
    };

    // Build system prompt: if user specified --system-prompt, use that.
    // Otherwise the engine will build the full modular prompt via system_prompt.rs.
    let system_prompt = cli.system_prompt
        .or(settings.custom_system_prompt)
        .unwrap_or_default();

    let permission_mode = config::parse_permission_mode(&cli.permission_mode);

    // ── Discover MCP server configs ────────────────────────────────────────
    let mcp_instructions = init::discover_mcp_instructions(&cwd);
    if !mcp_instructions.is_empty() {
        eprintln!(
            "\x1b[2m[MCP: {} server{} discovered]\x1b[0m",
            mcp_instructions.len(),
            if mcp_instructions.len() == 1 { "" } else { "s" }
        );
    }

    let engine = claude_agent::engine::QueryEngine::builder(api_key, &cwd)
        .model(&model)
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
        .provider(&cli.provider)
        .thinking(if cli.thinking {
            Some(claude_api::types::ThinkingConfig {
                thinking_type: "enabled".into(),
                budget_tokens: Some(cli.thinking_budget),
            })
        } else {
            None
        })
        .append_system_prompt(cli.append_system_prompt)
        .mcp_instructions(mcp_instructions);

    // Apply base URL override: CLI flag → ANTHROPIC_BASE_URL env → default
    let engine = if let Some(ref url) = cli.base_url {
        engine.base_url(url)
    } else if let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") {
        let u = url.trim().to_string();
        if !u.is_empty() { engine.base_url(&u) } else { engine }
    } else {
        engine
    };

    let engine = engine.build();
    let engine = Arc::new(engine);

    // ── Create Event Bus + AgentCoreAdapter ──────────────────────────────
    let (bus_handle, client_handle) = claude_bus::bus::EventBus::new(256);

    // Build MCP bus adapter from discovered configs
    let mcp_manager = claude_mcp::registry::McpManager::new();
    let mcp_adapter = claude_mcp::McpBusAdapter::new(mcp_manager);

    // Create the core adapter bridging QueryEngine ↔ EventBus
    let adapter = claude_agent::bus_adapter::AgentCoreAdapter::from_arc(
        Arc::clone(&engine),
        bus_handle,
        Some(mcp_adapter),
    );
    let _adapter_handle = adapter.spawn();
    tracing::debug!("Event Bus started, AgentCoreAdapter spawned");

    // ── Ctrl-C → abort signal (second press → force exit) ────────────────
    // We use a shared counter to track Ctrl-C presses.
    // First press: set abort signal (tools will check and exit early).
    // Second press: force exit (session save is attempted in REPL on normal exit).
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
        match auth::resume_latest_session(&engine).await {
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
        repl::run(engine, Some(client_handle), cwd).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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

    #[test]
    fn test_cli_provider_flag() {
        let cli = Cli::try_parse_from(["claude", "--provider", "openai", "--api-key", "sk-test"]).unwrap();
        assert_eq!(cli.provider, "openai");
    }

    #[test]
    fn test_cli_provider_default() {
        let cli = Cli::try_parse_from(["claude"]).unwrap();
        assert_eq!(cli.provider, "anthropic");
        assert!(cli.base_url.is_none());
    }

    #[test]
    fn test_cli_base_url_flag() {
        let cli = Cli::try_parse_from(["claude", "--base-url", "http://localhost:11434"]).unwrap();
        assert_eq!(cli.base_url.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn test_cli_list_sessions_flag() {
        let cli = Cli::try_parse_from(["claude", "--list-sessions"]).unwrap();
        assert!(cli.list_sessions);
    }

    #[test]
    fn test_cli_defaults_list_sessions_false() {
        let cli = Cli::try_parse_from(["claude"]).unwrap();
        assert!(!cli.list_sessions);
    }
}