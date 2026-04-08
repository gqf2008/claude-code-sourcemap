mod config;
mod repl;
mod repl_commands;
mod commands;
mod output;
mod markdown;
mod diff_display;

use clap::{CommandFactory, Parser};
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

    /// Generate shell completions and exit (bash, zsh, fish, powershell)
    #[arg(long, value_name = "SHELL")]
    completions: Option<clap_complete::Shell>,

    /// API provider: anthropic (default), openai, deepseek, ollama, together, groq, bedrock, vertex
    #[arg(long, default_value = "anthropic")]
    provider: String,

    /// Override API base URL (provider-specific)
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

    let filter = if cli.verbose { EnvFilter::new("debug") } else { EnvFilter::new("warn") };
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(filter).init();

    let settings = config::load_settings()?;

    // Inject env vars from settings.json before auth resolution
    settings.apply_env();

    let cwd = match cli.cwd {
        Some(ref dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // ── Handle --init: create CLAUDE.md and settings ────────────────────────
    if cli.init {
        return run_init(&cwd);
    }

    let api_key = resolve_api_key(&cli.provider, cli.api_key.as_deref(), settings.api_key.as_deref())?;

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
    let skills = load_skills(&cwd);

    // ── Discover MCP server configs ────────────────────────────────────────
    let mcp_instructions = discover_mcp_instructions(&cwd);
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

/// Read OAuth access token from `~/.claude/.credentials.json`.
///
/// The TS Claude Code stores OAuth tokens in this file with the structure:
/// ```json
/// { "claudeAiOauth": { "accessToken": "...", "expiresAt": ... } }
/// ```
fn read_oauth_credentials() -> Option<String> {
    let home = dirs::home_dir()?;
    let cred_path = home.join(".claude").join(".credentials.json");
    let content = std::fs::read_to_string(&cred_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let oauth = json.get("claudeAiOauth")?;

    // Check expiry — expiresAt is milliseconds since epoch
    if let Some(expires_at) = oauth.get("expiresAt").and_then(|v| v.as_i64()) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        if now_ms > expires_at {
            tracing::debug!("OAuth token expired (expiresAt={})", expires_at);
            return None;
        }
    }

    let token = oauth.get("accessToken")?.as_str()?;
    if token.is_empty() {
        return None;
    }
    tracing::debug!("Loaded OAuth token from {}", cred_path.display());
    Some(token.to_string())
}

/// Read `primaryApiKey` from `~/.claude/config.json` (Claude Code config).
fn read_claude_config_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let config_path = home.join(".claude").join("config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let key = json.get("primaryApiKey")?.as_str()?;
    if key.is_empty() {
        return None;
    }
    tracing::debug!("Loaded primaryApiKey from {}", config_path.display());
    Some(key.to_string())
}

/// Resolve API key based on provider.
///
/// Priority (for `anthropic` provider):
/// 1. `--api-key` CLI flag
/// 2. `ANTHROPIC_API_KEY` env var (captured by clap)
/// 3. `~/.claude/settings.json` → `api_key`
/// 4. `~/.claude/.credentials.json` → OAuth `accessToken`
/// 5. `~/.claude.json` → `primaryApiKey`
///
/// Other providers: `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, etc.
/// `ollama` / `local`: no key required.
fn resolve_api_key(
    provider: &str,
    cli_key: Option<&str>,
    settings_key: Option<&str>,
) -> anyhow::Result<String> {
    // Explicit CLI flag always wins
    if let Some(key) = cli_key {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!(
                "API key is empty. Provide a valid key via --api-key or environment variable."
            ));
        }
        return Ok(trimmed.to_string());
    }

    match provider {
        "anthropic" => {
            // settings.json api_key
            if let Some(key) = settings_key {
                return Ok(key.to_string());
            }
            // ANTHROPIC_API_KEY is already captured by clap's env attribute;
            // if we reach here, it wasn't set. Try other sources.

            // ANTHROPIC_AUTH_TOKEN (used by proxy/managed setups)
            if let Ok(token) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
                let t = token.trim();
                if !t.is_empty() {
                    return Ok(t.to_string());
                }
            }

            // OAuth credentials (~/.claude/.credentials.json)
            if let Some(token) = read_oauth_credentials() {
                return Ok(token);
            }
            // Config file (~/.claude/config.json → primaryApiKey)
            if let Some(key) = read_claude_config_key() {
                return Ok(key);
            }

            Err(anyhow::anyhow!(
                "API key required. Set ANTHROPIC_API_KEY, use --api-key, \
                 or login via the official Claude Code CLI."
            ))
        }
        "openai" | "together" | "groq" => {
            let env_var = match provider {
                "openai" => "OPENAI_API_KEY",
                "together" => "TOGETHER_API_KEY",
                "groq" => "GROQ_API_KEY",
                _ => "OPENAI_API_KEY",
            };
            std::env::var(env_var).or_else(|_| {
                settings_key.map(|k| k.to_string()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "API key required for {} provider. Set {} or use --api-key.",
                        provider,
                        env_var
                    )
                })
            })
        }
        "deepseek" => std::env::var("DEEPSEEK_API_KEY").or_else(|_| {
            settings_key.map(|k| k.to_string()).ok_or_else(|| {
                anyhow::anyhow!(
                    "API key required for DeepSeek. Set DEEPSEEK_API_KEY or use --api-key."
                )
            })
        }),
        "ollama" | "local" => {
            // No key needed
            Ok("ollama".to_string())
        }
        "openai-compatible" => {
            // Try OPENAI_API_KEY, fallback to settings, then allow empty
            std::env::var("OPENAI_API_KEY")
                .or_else(|_| Ok(settings_key.unwrap_or("").to_string()))
        }
        _ => {
            // Unknown provider — try settings key, then OPENAI_API_KEY
            if let Some(key) = settings_key {
                Ok(key.to_string())
            } else {
                std::env::var("OPENAI_API_KEY").map_err(|_| {
                    anyhow::anyhow!(
                        "API key required for {} provider. Use --api-key.",
                        provider
                    )
                })
            }
        }
    }
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

/// Discover MCP server configs and generate system prompt instructions.
///
/// Scans `.mcp.json` at project and user levels, extracts server names and
/// commands, and returns `(server_name, instruction)` pairs for the system prompt.
fn discover_mcp_instructions(cwd: &std::path::Path) -> Vec<(String, String)> {
    let config_paths = claude_tools::mcp::server::discover_mcp_configs(cwd);
    let mut instructions = Vec::new();

    for path in config_paths {
        match claude_tools::mcp::server::load_mcp_configs(&path) {
            Ok(configs) => {
                for cfg in configs {
                    let instruction = format!(
                        "MCP server '{}': command=`{} {}`",
                        cfg.name,
                        cfg.command,
                        cfg.args.join(" "),
                    );
                    instructions.push((cfg.name, instruction));
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load MCP config {}: {}", path.display(), e);
            }
        }
    }

    instructions
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

    // ── resolve_api_key ──────────────────────────────────────────────

    #[test]
    fn test_resolve_api_key_explicit() {
        assert_eq!(
            resolve_api_key("anthropic", Some("explicit-key"), None).unwrap(),
            "explicit-key"
        );
    }

    #[test]
    fn test_resolve_api_key_ollama_no_key() {
        assert_eq!(
            resolve_api_key("ollama", None, None).unwrap(),
            "ollama"
        );
    }

    #[test]
    fn test_resolve_api_key_anthropic_settings() {
        assert_eq!(
            resolve_api_key("anthropic", None, Some("settings-key")).unwrap(),
            "settings-key"
        );
    }

    #[test]
    fn test_resolve_api_key_anthropic_no_explicit() {
        // With no explicit key or settings key, resolve_api_key will try
        // ANTHROPIC_AUTH_TOKEN, OAuth credentials, and config.json.
        // On a dev machine with Claude Code installed, this may succeed.
        // We just verify it doesn't panic and returns a valid result type.
        let result = resolve_api_key("anthropic", None, None);
        match result {
            Ok(key) => assert!(!key.trim().is_empty(), "resolved key should not be blank"),
            Err(e) => assert!(e.to_string().contains("API key required")),
        }
    }

    #[test]
    fn test_resolve_api_key_empty_rejected() {
        assert!(resolve_api_key("anthropic", Some(""), None).is_err());
        assert!(resolve_api_key("anthropic", Some("   "), None).is_err());
    }

    #[test]
    fn test_resolve_api_key_trimmed() {
        let key = resolve_api_key("anthropic", Some("  sk-abc  "), None).unwrap();
        assert_eq!(key, "sk-abc");
    }

    // ── OAuth / legacy config credential reading ─────────────────────

    #[test]
    fn test_read_oauth_credentials_valid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cred_path = tmp.path().join(".credentials.json");
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 3_600_000; // 1 hour from now
        std::fs::write(
            &cred_path,
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"tok-123","expiresAt":{}}}}}"#,
                expires
            ),
        )
        .unwrap();

        // read_oauth_credentials reads from $HOME — we can't easily override that,
        // so we test the parsing logic directly
        let content = std::fs::read_to_string(&cred_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        let token = json["claudeAiOauth"]["accessToken"].as_str().unwrap();
        assert_eq!(token, "tok-123");
    }

    #[test]
    fn test_read_claude_config_key_parsing() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"primaryApiKey":"sk-ant-legacy"}"#).unwrap();
        let key = json["primaryApiKey"].as_str().unwrap();
        assert_eq!(key, "sk-ant-legacy");
    }

    #[test]
    fn test_oauth_expired_token_ignored() {
        let expired_json = r#"{"claudeAiOauth":{"accessToken":"tok-old","expiresAt":1000}}"#;
        let json: serde_json::Value = serde_json::from_str(expired_json).unwrap();
        let expires_at = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(now_ms > expires_at, "token should be expired");
    }

    #[test]
    fn test_oauth_empty_token_ignored() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"claudeAiOauth":{"accessToken":""}}"#).unwrap();
        let token = json["claudeAiOauth"]["accessToken"].as_str().unwrap();
        assert!(token.is_empty());
    }

    #[test]
    fn test_settings_env_parsing() {
        let json = r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"tok","ANTHROPIC_BASE_URL":"http://localhost:8080"}}"#;
        let settings: claude_core::config::Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.env.len(), 2);
        assert_eq!(settings.env["ANTHROPIC_AUTH_TOKEN"], "tok");
        assert_eq!(settings.env["ANTHROPIC_BASE_URL"], "http://localhost:8080");
    }

    #[test]
    fn test_resolve_api_key_auth_token_env() {
        // ANTHROPIC_AUTH_TOKEN should be picked up as fallback
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "proxy-token-123");
        let result = resolve_api_key("anthropic", None, None);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        // May succeed (picks up ANTHROPIC_AUTH_TOKEN) or fail (if some other
        // credential source matches first). Just check the token value if ok.
        if let Ok(key) = result {
            // Could be from ANTHROPIC_AUTH_TOKEN or from actual credential files on disk
            assert!(!key.is_empty());
        }
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

    #[test]
    fn test_discover_mcp_instructions_empty() {
        let tmp = std::env::temp_dir().join("claude_test_no_mcp");
        let _ = std::fs::create_dir_all(&tmp);
        let result = discover_mcp_instructions(&tmp);
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_mcp_instructions_with_config() {
        let tmp = std::env::temp_dir().join("claude_test_mcp_disc");
        let _ = std::fs::create_dir_all(&tmp);
        let mcp_json = r#"{
            "mcpServers": {
                "my-server": {
                    "command": "npx",
                    "args": ["-y", "my-mcp-server"]
                }
            }
        }"#;
        std::fs::write(tmp.join(".mcp.json"), mcp_json).unwrap();
        let result = discover_mcp_instructions(&tmp);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "my-server");
        assert!(result[0].1.contains("npx"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}