use claude_core::skills::SkillEntry;

pub enum SlashCommand {
    Help,
    Clear,
    Model(String),
    Compact { instructions: String },
    Cost { window: String },
    Skills,
    Memory { sub: String },
    Session { sub: String },
    Diff,
    Status,
    Permissions { mode: String },
    Config,
    Undo,
    Review { prompt: String },
    PrComments { pr_number: u64 },
    Branch { name: String },
    Doctor,
    Init,
    Commit { message: String },
    CommitPushPr { message: String },
    Pr { prompt: String },
    Bug { prompt: String },
    Search { query: String },
    History { page: usize },
    Retry,
    Version,
    Login,
    Logout,
    Context,
    Export { format: String },
    RunSkill { name: String, prompt: String },
    ReloadContext,
    Mcp { sub: String },
    Plugin { sub: String },
    /// Run a command defined by a plugin.
    RunPluginCommand { name: String, prompt: String },
    /// Agent definitions management.
    Agents { sub: String },
    /// Switch terminal theme.
    Theme { name: String },
    /// Plan mode management.
    Plan { args: String },
    /// Toggle extended thinking.
    Think { args: String },
    /// Force next request to skip prompt cache.
    BreakCache,
    /// Rewind conversation by N turns.
    Rewind { turns: String },
    /// Quick model toggle (/fast → haiku, /fast off → restore previous).
    Fast { toggle: String },
    /// Add context directory at runtime.
    AddDir { path: String },
    /// Generate a session summary.
    Summary,
    /// Rename current session.
    Rename { name: String },
    /// Copy last assistant response to clipboard.
    Copy,
    /// Export session to shareable markdown file.
    Share,
    /// List files in current context/working directory.
    Files { pattern: String },
    /// Show environment information.
    Env,
    /// Toggle vim editing mode.
    Vim { toggle: String },
    /// Open stickers page in browser.
    Stickers,
    /// Set effort level (low|medium|high|max|auto).
    Effort { level: String },
    /// Tag current session.
    Tag { name: String },
    /// Show release notes / changelog.
    ReleaseNotes,
    /// Submit feedback.
    Feedback { text: String },
    /// Show detailed session statistics.
    Stats,
    Exit,
    Unknown(String),
}

impl SlashCommand {
    pub fn parse(input: &str, known_skills: &[SkillEntry]) -> Option<Self> {
        let input = input.trim();
        if !input.starts_with('/') { return None; }
        let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let args = parts.get(1).map(|s| s.trim().to_string()).unwrap_or_default();
        Some(match cmd.as_str() {
            "help" | "?" => Self::Help,
            "clear" => Self::Clear,
            "model" => Self::Model(args),
            "compact" => Self::Compact { instructions: args },
            "cost" => Self::Cost { window: args },
            "skills" => Self::Skills,
            "memory" => Self::Memory { sub: args },
            "session" | "resume" => Self::Session { sub: args },
            "diff" => Self::Diff,
            "status" => Self::Status,
            "permissions" | "perms" => Self::Permissions { mode: args },
            "config" | "settings" => Self::Config,
            "undo" => Self::Undo,
            "review" => Self::Review { prompt: args },
            "pr-comments" | "prc" => {
                let num = args.trim_start_matches('#').parse::<u64>().unwrap_or(0);
                Self::PrComments { pr_number: num }
            }
            "branch" | "fork" => Self::Branch { name: args },
            "doctor" => Self::Doctor,
            "init" => Self::Init,
            "commit" => Self::Commit { message: args },
            "commit-push-pr" | "cpp" => Self::CommitPushPr { message: args },
            "pr" => Self::Pr { prompt: args },
            "bug" | "debug" => Self::Bug { prompt: args },
            "search" | "find" | "grep" => Self::Search { query: args },
            "history" => Self::History { page: args.parse().unwrap_or(1) },
            "retry" | "redo" => Self::Retry,
            "version" => Self::Version,
            "login" => Self::Login,
            "logout" => Self::Logout,
            "context" | "ctx" => Self::Context,
            "export" => Self::Export { format: if args.is_empty() { "markdown".into() } else { args } },
            "reload-context" | "reload" => Self::ReloadContext,
            "mcp" => Self::Mcp { sub: args },
            "plugin" | "plugins" => Self::Plugin { sub: args },
            "agents" | "agent" => Self::Agents { sub: args },
            "theme" => Self::Theme { name: args },
            "plan" => Self::Plan { args },
            "think" | "thinking" => Self::Think { args },
            "break-cache" | "breakcache" => Self::BreakCache,
            "rewind" => Self::Rewind { turns: args },
            "fast" => Self::Fast { toggle: args },
            "add-dir" | "adddir" => Self::AddDir { path: args },
            "summary" => Self::Summary,
            "rename" => Self::Rename { name: args },
            "copy" | "yank" => Self::Copy,
            "share" => Self::Share,
            "files" | "ls" => Self::Files { pattern: args },
            "env" | "environment" => Self::Env,
            "vim" => Self::Vim { toggle: args },
            "stickers" => Self::Stickers,
            "effort" => Self::Effort { level: args },
            "tag" => Self::Tag { name: args },
            "release-notes" | "changelog" => Self::ReleaseNotes,
            "feedback" => Self::Feedback { text: args },
            "stats" | "usage" => Self::Stats,
            "exit" | "quit" => Self::Exit,
            name => {
                // Check if it matches a loaded skill
                if known_skills.iter().any(|s| s.name == name) {
                    Self::RunSkill { name: name.to_string(), prompt: args }
                } else {
                    Self::Unknown(name.to_string())
                }
            }
        })
    }

    /// Execute built-in commands that don't need an engine.
    pub fn execute(&self, known_skills: &[SkillEntry], plugin_commands: &[PluginCommandEntry]) -> CommandResult {
        match self {
            Self::Help => CommandResult::Print(build_help_text(known_skills, plugin_commands)),
            Self::Clear => CommandResult::ClearHistory,
            Self::Model(name) if name.is_empty() => {
                let aliases = claude_core::model::list_aliases();
                let mut out = String::from("Usage: /model <name|alias>\n\nAliases:\n");
                for (alias, resolved) in &aliases {
                    let display = claude_core::model::display_name_any(resolved);
                    out.push_str(&format!("  {:<10} → {} ({})\n", alias, display, resolved));
                }
                out.push_str(&format!(
                    "\nSmall/fast model: {} (for compaction)\n",
                    claude_core::model::display_name_any(&claude_core::model::small_fast_model()),
                ));
                out.push_str("\nExamples: /model sonnet  /model opus  /model haiku  /model gpt-4o");
                CommandResult::Print(out)
            }
            Self::Model(name) => CommandResult::SetModel(name.clone()),
            Self::Compact { instructions } => CommandResult::Compact {
                instructions: if instructions.is_empty() { None } else { Some(instructions.clone()) },
            },
            Self::Cost { window } => CommandResult::ShowCost { window: window.clone() },
            Self::Skills => {
                let invocable: Vec<_> = known_skills
                    .iter()
                    .filter(|s| s.user_invocable)
                    .collect();
                if invocable.is_empty() {
                    CommandResult::Print("No skills found. Add .md files to .claude/skills/".into())
                } else {
                    let list = invocable.iter()
                        .map(|s| {
                            let name_display = s.display_name.as_deref().unwrap_or(&s.name);
                            let mut line = format!("  /{:<20} {}", name_display, s.description);
                            if let Some(ref hint) = s.argument_hint {
                                line.push_str(&format!("  \x1b[2m{}\x1b[0m", hint));
                            }
                            line
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let cond_count = claude_core::skills::conditional_skill_count();
                    let mut out = format!("Available skills:\n{}", list);
                    if cond_count > 0 {
                        out.push_str(&format!(
                            "\n\n  \x1b[2m({} conditional skill{} pending — will activate when matching files are touched)\x1b[0m",
                            cond_count,
                            if cond_count == 1 { "" } else { "s" }
                        ));
                    }
                    CommandResult::Print(out)
                }
            }
            Self::Memory { sub } => CommandResult::Memory { sub: sub.clone() },
            Self::Session { sub } => CommandResult::Session { sub: sub.clone() },
            Self::Diff => CommandResult::Diff,
            Self::Status => CommandResult::Status,
            Self::Permissions { mode } => CommandResult::Permissions { mode: mode.clone() },
            Self::Config => CommandResult::Config,
            Self::Undo => CommandResult::Undo,
            Self::Review { prompt } => CommandResult::Review { prompt: prompt.clone() },
            Self::PrComments { pr_number } => CommandResult::PrComments { pr_number: *pr_number },
            Self::Branch { name } => CommandResult::Branch { name: name.clone() },
            Self::Doctor => CommandResult::Doctor,
            Self::Init => CommandResult::Init,
            Self::Commit { message } => CommandResult::Commit { message: message.clone() },
            Self::CommitPushPr { message } => CommandResult::CommitPushPr { message: message.clone() },
            Self::Pr { prompt } => CommandResult::Pr { prompt: prompt.clone() },
            Self::Bug { prompt } => CommandResult::Bug { prompt: prompt.clone() },
            Self::Search { query } => CommandResult::Search { query: query.clone() },
            Self::History { page } => CommandResult::History { page: *page },
            Self::Retry => CommandResult::Retry,
            Self::Version => CommandResult::Print(format!("claude-code-rs v{}", env!("CARGO_PKG_VERSION"))),
            Self::Login => CommandResult::Login,
            Self::Logout => CommandResult::Logout,
            Self::Context => CommandResult::Context,
            Self::Export { format } => CommandResult::Export { format: format.clone() },
            Self::ReloadContext => CommandResult::ReloadContext,
            Self::Mcp { sub } => CommandResult::Mcp { sub: sub.clone() },
            Self::Plugin { sub } => CommandResult::Plugin { sub: sub.clone() },
            Self::RunSkill { name, prompt } => CommandResult::RunSkill {
                name: name.clone(),
                prompt: prompt.clone(),
            },
            Self::RunPluginCommand { name, prompt } => CommandResult::RunPluginCommand {
                name: name.clone(),
                prompt: prompt.clone(),
            },
            Self::Agents { sub } => CommandResult::Agents { sub: sub.clone() },
            Self::Theme { name } => CommandResult::Theme { name: name.clone() },
            Self::Plan { args } => CommandResult::Plan { args: args.clone() },
            Self::Think { args } => CommandResult::Think { args: args.clone() },
            Self::BreakCache => CommandResult::BreakCache,
            Self::Rewind { turns } => CommandResult::Rewind { turns: turns.clone() },
            Self::Fast { toggle } => CommandResult::Fast { toggle: toggle.clone() },
            Self::AddDir { path } => CommandResult::AddDir { path: path.clone() },
            Self::Summary => CommandResult::Summary,
            Self::Rename { name } => CommandResult::Rename { name: name.clone() },
            Self::Copy => CommandResult::Copy,
            Self::Share => CommandResult::Share,
            Self::Files { pattern } => CommandResult::Files { pattern: pattern.clone() },
            Self::Env => CommandResult::Env,
            Self::Vim { toggle } => CommandResult::Vim { toggle: toggle.clone() },
            Self::Stickers => CommandResult::Stickers,
            Self::Effort { level } => CommandResult::Effort { level: level.clone() },
            Self::Tag { name } => CommandResult::Tag { name: name.clone() },
            Self::ReleaseNotes => CommandResult::ReleaseNotes,
            Self::Feedback { text } => {
                if text.is_empty() {
                    CommandResult::Print("Usage: /feedback <your feedback text>".into())
                } else {
                    CommandResult::Feedback { text: text.clone() }
                }
            }
            Self::Stats => CommandResult::Stats,
            Self::Exit => CommandResult::Exit,
            Self::Unknown(cmd) => {
                CommandResult::Print(format!("Unknown command: /{}. Type /help.", cmd))
            }
        }
    }
}

pub enum CommandResult {
    Print(String),
    ClearHistory,
    SetModel(String),
    ShowCost { window: String },
    Compact { instructions: Option<String> },
    Memory { sub: String },
    Session { sub: String },
    Diff,
    Status,
    Permissions { mode: String },
    Config,
    Undo,
    Review { prompt: String },
    PrComments { pr_number: u64 },
    Branch { name: String },
    Doctor,
    Init,
    Commit { message: String },
    CommitPushPr { message: String },
    Pr { prompt: String },
    Bug { prompt: String },
    Search { query: String },
    History { page: usize },
    Retry,
    Login,
    Logout,
    Context,
    Export { format: String },
    RunSkill { name: String, prompt: String },
    ReloadContext,
    Mcp { sub: String },
    Plugin { sub: String },
    /// Execute a plugin-defined command (prompt sent to engine).
    RunPluginCommand { name: String, prompt: String },
    /// Agent definitions management (/agents list, info, create, delete).
    Agents { sub: String },
    /// Theme switching (/theme [name]).
    Theme { name: String },
    /// Plan mode (/plan [open|description]).
    Plan { args: String },
    /// Toggle extended thinking (/think [on|off|<budget>]).
    Think { args: String },
    /// Force next request to skip prompt cache (/break-cache).
    BreakCache,
    /// Rewind conversation by N turns (/rewind [N]).
    Rewind { turns: String },
    /// Toggle fast/cheap model (/fast [off]).
    Fast { toggle: String },
    /// Add context directory at runtime (/add-dir <path>).
    AddDir { path: String },
    /// Generate a summary of the current conversation (/summary).
    Summary,
    /// Rename the current session (/rename <name>).
    Rename { name: String },
    /// Copy last assistant response to clipboard (/copy).
    Copy,
    /// Export session to shareable markdown file (/share).
    Share,
    /// List files in current working directory (/files [pattern]).
    Files { pattern: String },
    /// Show environment information (/env).
    Env,
    /// Toggle vim editing mode (/vim [on|off]).
    Vim { toggle: String },
    /// Open stickers page (/stickers).
    Stickers,
    /// Set effort level (/effort [low|medium|high|max|auto]).
    Effort { level: String },
    /// Tag current session (/tag <name>).
    Tag { name: String },
    /// Show release notes (/release-notes).
    ReleaseNotes,
    /// Submit feedback (/feedback <text>).
    Feedback { text: String },
    /// Show detailed session statistics (/stats).
    Stats,
    Exit,
}

/// A plugin command entry for help display.
pub struct PluginCommandEntry {
    pub plugin_name: String,
    pub command_name: String,
}

fn build_help_text(skills: &[SkillEntry], plugin_commands: &[PluginCommandEntry]) -> String {
    let mut text = HELP_TEXT_BASE.to_string();
    // Skills are shown via /skills command, not in /help (matches TS behavior)
    if !skills.is_empty() {
        text.push_str(&format!(
            "\n\n  \x1b[2m({} skill{} loaded — use /skills to list)\x1b[0m",
            skills.len(),
            if skills.len() == 1 { "" } else { "s" },
        ));
    }
    if !plugin_commands.is_empty() {
        text.push_str("\n\n\x1b[1mPlugins\x1b[0m:");
        for pc in plugin_commands {
            text.push_str(&format!("\n  /{:<20} (from {})", pc.command_name, pc.plugin_name));
        }
    }
    text
}

const HELP_TEXT_BASE: &str = "\
\x1b[1mConversation\x1b[0m
  /help              Show this help
  /clear             Clear conversation history
  /compact [instr]   Compact conversation to free tokens
  /undo              Undo last assistant turn
  /rewind [N]        Rewind by N turns (default 1)
  /search <query>    Search conversation history
  /history [page]    Browse conversation turns
  /retry             Retry the last failed prompt (alias: /redo)
  /cost [today|week|month]  Show token usage and costs
  /exit              Exit the CLI

\x1b[1mGit & Code\x1b[0m
  /diff              Show git diff (staged + unstaged)
  /status            Show session and git status
  /commit [msg]      Stage and commit (AI-generated message)
  /commit-push-pr    Commit → push → create PR (alias: /cpp)
  /pr [prompt]       Create/review a pull request
  /bug [prompt]      Debug a problem with AI assistance
  /review [prompt]   AI code review on recent changes
  /pr-comments <#>   Fetch and analyze PR review comments (alias: /prc)
  /branch [name]     Fork conversation into a new branch (alias: /fork)
  /init              Initialize CLAUDE.md for the project

\x1b[1mConfiguration\x1b[0m
  /model <name>      Switch model (aliases: sonnet, opus, haiku, best)
  /fast [off]        Toggle fast/cheap model (haiku)
  /think [on|off|N]  Toggle extended thinking (N = token budget)
  /effort [level]    Set effort (low|medium|high|max|auto)
  /vim [on|off]      Toggle vim editing mode
  /theme [name]      Switch terminal theme (dark, light, dark-ansi, etc.)
  /break-cache       Force next request to skip prompt cache
  /login             Set API key interactively
  /logout            Clear saved API key
  /config            Show current configuration
  /permissions       Show permission mode and rules (default|bypass|acceptEdits|plan)
  /context           Show loaded context (CLAUDE.md, memory, model)
  /reload-context    Reload CLAUDE.md, memory, and settings
  /mcp               Show discovered MCP servers
  /plugin            List loaded plugins (alias: /plugins)

\x1b[1mSession & Memory\x1b[0m
  /session save      Save current session
  /session list      List saved sessions
  /session load <q>  Resume session (ID prefix or keyword search)
  /session delete <id> Delete a saved session
  /summary           Generate a conversation summary
  /rename <name>     Rename current session
  /tag <name>        Tag or untag current session
  /copy              Copy last assistant response to clipboard
  /share             Export session to shareable markdown file
  /export [format]   Export session (markdown or json)
  /memory list       List memory files
  /memory open <f>   Open a memory file

\x1b[1mPlanning\x1b[0m
  /plan              Toggle plan mode (read-only tools, structured planning)
  /plan open         Open plan file in external editor

\x1b[1mSystem\x1b[0m
  /doctor            Check environment health
  /skills            List available skills
  /agents            Manage agent definitions
  /add-dir <path>    Add context directory at runtime
  /files [pattern]   List files in current directory
  /env               Show environment information
  /version           Show version info
  /release-notes     Show version and release notes
  /stickers          Order Claude Code stickers!
  /feedback <text>   Submit feedback about Claude Code
  /stats             Show detailed session statistics

\x1b[1mTips\x1b[0m
  • End a line with \\ to continue on the next line (multiline)
  • Shift+Enter or Alt+Enter to insert a newline
  • Ctrl+R to search command history
  • Left/Right arrows to navigate within a line
  • Alt+Left/Right to jump by word, Alt+Backspace to delete word
  • Ctrl+A/Home to go to start, Ctrl+E/End to go to end
  • Attach images: type @path/to/image.png on its own line
  • Use --resume to restore the most recent session on startup
  • Use --init to create CLAUDE.md and project scaffolding";


#[cfg(test)]
mod tests {
    use super::*;

    fn no_skills() -> Vec<SkillEntry> { Vec::new() }
    fn no_plugins() -> Vec<PluginCommandEntry> { Vec::new() }

    fn test_skills() -> Vec<SkillEntry> {
        vec![SkillEntry {
            name: "review".into(),
            description: "Code review skill".into(),
            system_prompt: "You are a reviewer".into(),
            allowed_tools: vec!["Read".into()],
            model: None,
            display_name: None,
            when_to_use: None,
            paths: vec![],
            argument_names: vec![],
            argument_hint: None,
            version: None,
            context: None,
            agent: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            skill_root: None,
        }]
    }

    // ── parse ────────────────────────────────────────────────────────

    #[test]
    fn test_parse_not_slash() {
        assert!(SlashCommand::parse("hello", &no_skills()).is_none());
        assert!(SlashCommand::parse("", &no_skills()).is_none());
    }

    #[test]
    fn test_parse_basic_commands() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/help", &s), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/?", &s), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/clear", &s), Some(SlashCommand::Clear)));
        assert!(matches!(SlashCommand::parse("/exit", &s), Some(SlashCommand::Exit)));
        assert!(matches!(SlashCommand::parse("/quit", &s), Some(SlashCommand::Exit)));
        assert!(matches!(SlashCommand::parse("/version", &s), Some(SlashCommand::Version)));
        assert!(matches!(SlashCommand::parse("/diff", &s), Some(SlashCommand::Diff)));
        assert!(matches!(SlashCommand::parse("/status", &s), Some(SlashCommand::Status)));
        assert!(matches!(SlashCommand::parse("/undo", &s), Some(SlashCommand::Undo)));
        assert!(matches!(SlashCommand::parse("/doctor", &s), Some(SlashCommand::Doctor)));
        assert!(matches!(SlashCommand::parse("/init", &s), Some(SlashCommand::Init)));
        assert!(matches!(SlashCommand::parse("/login", &s), Some(SlashCommand::Login)));
        assert!(matches!(SlashCommand::parse("/logout", &s), Some(SlashCommand::Logout)));
        assert!(matches!(SlashCommand::parse("/cost", &s), Some(SlashCommand::Cost { .. })));
        assert!(matches!(SlashCommand::parse("/skills", &s), Some(SlashCommand::Skills)));
    }

    #[test]
    fn test_parse_case_insensitive() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/HELP", &s), Some(SlashCommand::Help)));
        assert!(matches!(SlashCommand::parse("/Model sonnet", &s), Some(SlashCommand::Model(_))));
    }

    #[test]
    fn test_parse_with_args() {
        let s = no_skills();
        match SlashCommand::parse("/model opus", &s) {
            Some(SlashCommand::Model(name)) => assert_eq!(name, "opus"),
            _ => panic!("expected Model"),
        }
        match SlashCommand::parse("/compact focus on code", &s) {
            Some(SlashCommand::Compact { instructions }) => assert_eq!(instructions, "focus on code"),
            _ => panic!("expected Compact"),
        }
        match SlashCommand::parse("/commit fix: typo", &s) {
            Some(SlashCommand::Commit { message }) => assert_eq!(message, "fix: typo"),
            _ => panic!("expected Commit"),
        }
        match SlashCommand::parse("/review check security", &s) {
            Some(SlashCommand::Review { prompt }) => assert_eq!(prompt, "check security"),
            _ => panic!("expected Review"),
        }
    }

    #[test]
    fn test_parse_aliases() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/perms", &s), Some(SlashCommand::Permissions { .. })));
        assert!(matches!(SlashCommand::parse("/permissions", &s), Some(SlashCommand::Permissions { .. })));
        assert!(matches!(SlashCommand::parse("/ctx", &s), Some(SlashCommand::Context)));
        assert!(matches!(SlashCommand::parse("/context", &s), Some(SlashCommand::Context)));
        assert!(matches!(SlashCommand::parse("/resume", &s), Some(SlashCommand::Session { .. })));
    }

    #[test]
    fn test_parse_memory_session_subcommands() {
        let s = no_skills();
        match SlashCommand::parse("/memory list", &s) {
            Some(SlashCommand::Memory { sub }) => assert_eq!(sub, "list"),
            _ => panic!("expected Memory"),
        }
        match SlashCommand::parse("/session save", &s) {
            Some(SlashCommand::Session { sub }) => assert_eq!(sub, "save"),
            _ => panic!("expected Session"),
        }
    }

    #[test]
    fn test_parse_export_default_format() {
        let s = no_skills();
        match SlashCommand::parse("/export", &s) {
            Some(SlashCommand::Export { format }) => assert_eq!(format, "markdown"),
            _ => panic!("expected Export"),
        }
        match SlashCommand::parse("/export json", &s) {
            Some(SlashCommand::Export { format }) => assert_eq!(format, "json"),
            _ => panic!("expected Export json"),
        }
    }

    #[test]
    fn test_parse_unknown_command() {
        let s = no_skills();
        match SlashCommand::parse("/foobar", &s) {
            Some(SlashCommand::Unknown(name)) => assert_eq!(name, "foobar"),
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn test_parse_skill_match() {
        let skills = test_skills();
        match SlashCommand::parse("/review do a review", &skills) {
            Some(SlashCommand::Review { .. }) => {} // /review is a built-in, takes precedence
            _ => panic!("expected Review"),
        }

        // A custom skill name that doesn't conflict with built-ins
        let skills = vec![SkillEntry {
            name: "myskill".into(),
            description: "My custom skill".into(),
            system_prompt: "".into(),
            allowed_tools: vec![],
            model: None,
            display_name: None,
            when_to_use: None,
            paths: vec![],
            argument_names: vec![],
            argument_hint: None,
            version: None,
            context: None,
            agent: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            skill_root: None,
        }];
        match SlashCommand::parse("/myskill do stuff", &skills) {
            Some(SlashCommand::RunSkill { name, prompt }) => {
                assert_eq!(name, "myskill");
                assert_eq!(prompt, "do stuff");
            }
            _ => panic!("expected RunSkill"),
        }
    }

    // ── execute ──────────────────────────────────────────────────────

    #[test]
    fn test_execute_help() {
        let cmd = SlashCommand::Help;
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Print(text) => assert!(text.contains("/help")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_help_with_skills() {
        let cmd = SlashCommand::Help;
        let skills = test_skills();
        match cmd.execute(&skills, &no_plugins()) {
            CommandResult::Print(text) => {
                assert!(text.contains("/help"));
                // Skills are no longer listed in /help (matches TS behavior)
                // Instead, a count hint is shown
                assert!(text.contains("skill"));
                assert!(text.contains("/skills"));
                assert!(!text.contains("Code review skill"));
            }
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_clear() {
        let cmd = SlashCommand::Clear;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::ClearHistory));
    }

    #[test]
    fn test_execute_model_empty() {
        let cmd = SlashCommand::Model(String::new());
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Print(text) => assert!(text.contains("Usage")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_model_set() {
        let cmd = SlashCommand::Model("opus".into());
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::SetModel(name) => assert_eq!(name, "opus"),
            _ => panic!("expected SetModel"),
        }
    }

    #[test]
    fn test_execute_version() {
        let cmd = SlashCommand::Version;
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Print(text) => assert!(text.contains("claude-code-rs")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_skills_empty() {
        let cmd = SlashCommand::Skills;
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Print(text) => assert!(text.contains("No skills")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_skills_list() {
        let cmd = SlashCommand::Skills;
        let skills = test_skills();
        match cmd.execute(&skills, &no_plugins()) {
            CommandResult::Print(text) => {
                assert!(text.contains("/review"));
                assert!(text.contains("Code review skill"));
            }
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_compact_with_instructions() {
        let cmd = SlashCommand::Compact { instructions: "focus on code".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Compact { instructions } => {
                assert_eq!(instructions.as_deref(), Some("focus on code"));
            }
            _ => panic!("expected Compact"),
        }
    }

    #[test]
    fn test_execute_compact_empty() {
        let cmd = SlashCommand::Compact { instructions: String::new() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Compact { instructions } => assert!(instructions.is_none()),
            _ => panic!("expected Compact"),
        }
    }

    #[test]
    fn test_execute_unknown() {
        let cmd = SlashCommand::Unknown("xyz".into());
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Print(text) => assert!(text.contains("Unknown")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_exit() {
        let cmd = SlashCommand::Exit;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Exit));
    }

    // ── new P27 commands ─────────────────────────────────────────────

    #[test]
    fn test_parse_pr() {
        let s = no_skills();
        match SlashCommand::parse("/pr fix auth", &s) {
            Some(SlashCommand::Pr { prompt }) => assert_eq!(prompt, "fix auth"),
            _ => panic!("expected Pr"),
        }
    }

    #[test]
    fn test_parse_bug() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/bug login broken", &s), Some(SlashCommand::Bug { .. })));
        assert!(matches!(SlashCommand::parse("/debug crash", &s), Some(SlashCommand::Bug { .. })));
    }

    #[test]
    fn test_execute_pr() {
        let cmd = SlashCommand::Pr { prompt: "review security".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Pr { prompt } => assert_eq!(prompt, "review security"),
            _ => panic!("expected Pr"),
        }
    }

    #[test]
    fn test_execute_bug() {
        let cmd = SlashCommand::Bug { prompt: "OOM crash".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Bug { prompt } => assert_eq!(prompt, "OOM crash"),
            _ => panic!("expected Bug"),
        }
    }

    #[test]
    fn test_help_text_includes_new_commands() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/pr"));
        assert!(text.contains("/bug"));
        assert!(text.contains("/search"));
    }

    #[test]
    fn test_parse_search() {
        let s = no_skills();
        match SlashCommand::parse("/search hello world", &s) {
            Some(SlashCommand::Search { query }) => assert_eq!(query, "hello world"),
            _ => panic!("expected Search"),
        }
        // aliases
        assert!(matches!(SlashCommand::parse("/find foo", &s), Some(SlashCommand::Search { .. })));
        assert!(matches!(SlashCommand::parse("/grep bar", &s), Some(SlashCommand::Search { .. })));
    }

    #[test]
    fn test_execute_search() {
        let cmd = SlashCommand::Search { query: "token".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Search { query } => assert_eq!(query, "token"),
            _ => panic!("expected Search"),
        }
    }

    // ── P34: /mcp command ────────────────────────────────────────────

    #[test]
    fn test_parse_mcp() {
        let s = no_skills();
        match SlashCommand::parse("/mcp", &s) {
            Some(SlashCommand::Mcp { sub }) => assert!(sub.is_empty()),
            _ => panic!("expected Mcp"),
        }
        match SlashCommand::parse("/mcp list", &s) {
            Some(SlashCommand::Mcp { sub }) => assert_eq!(sub, "list"),
            _ => panic!("expected Mcp list"),
        }
        match SlashCommand::parse("/mcp status", &s) {
            Some(SlashCommand::Mcp { sub }) => assert_eq!(sub, "status"),
            _ => panic!("expected Mcp status"),
        }
    }

    #[test]
    fn test_execute_mcp() {
        let cmd = SlashCommand::Mcp { sub: "list".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Mcp { sub } => assert_eq!(sub, "list"),
            _ => panic!("expected Mcp"),
        }
    }

    #[test]
    fn test_help_text_includes_mcp() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/mcp"));
    }

    // ── P40 commands ───────────────────────────────────────────────

    #[test]
    fn test_parse_commit_push_pr() {
        let s = no_skills();
        match SlashCommand::parse("/commit-push-pr add feature", &s) {
            Some(SlashCommand::CommitPushPr { message }) => assert_eq!(message, "add feature"),
            _ => panic!("expected CommitPushPr"),
        }
    }

    #[test]
    fn test_parse_cpp_alias() {
        let s = no_skills();
        match SlashCommand::parse("/cpp", &s) {
            Some(SlashCommand::CommitPushPr { message }) => assert!(message.is_empty()),
            _ => panic!("expected CommitPushPr via /cpp alias"),
        }
    }

    #[test]
    fn test_execute_commit_push_pr() {
        let cmd = SlashCommand::CommitPushPr { message: "new feature".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::CommitPushPr { message } => assert_eq!(message, "new feature"),
            _ => panic!("expected CommitPushPr"),
        }
    }

    #[test]
    fn test_help_text_includes_cpp() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/commit-push-pr"));
        assert!(text.contains("/cpp"));
    }

    #[test]
    fn test_execute_run_plugin_command() {
        let cmd = SlashCommand::RunPluginCommand {
            name: "my-cmd".into(),
            prompt: "Do something special".into(),
        };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::RunPluginCommand { name, prompt } => {
                assert_eq!(name, "my-cmd");
                assert_eq!(prompt, "Do something special");
            }
            _ => panic!("expected RunPluginCommand"),
        }
    }

    #[test]
    fn test_unknown_command_falls_through_to_unknown() {
        // A name that is not a builtin or skill should be Unknown
        let cmd = SlashCommand::parse("/my-custom-plugin-cmd", &no_skills());
        assert!(matches!(cmd, Some(SlashCommand::Unknown(_))));
    }

    #[test]
    fn test_help_text_includes_plugin_commands() {
        let plugins = vec![
            PluginCommandEntry { plugin_name: "my-plugin".into(), command_name: "deploy".into() },
        ];
        let text = build_help_text(&no_skills(), &plugins);
        assert!(text.contains("Plugins"));
        assert!(text.contains("/deploy"));
        assert!(text.contains("my-plugin"));
    }

    #[test]
    fn test_help_text_no_plugin_section_when_empty() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(!text.contains("Plugins"));
    }

    // ── /history command ──────────────────────────────────────────────

    #[test]
    fn test_parse_history_default() {
        let s = no_skills();
        match SlashCommand::parse("/history", &s) {
            Some(SlashCommand::History { page }) => assert_eq!(page, 1),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn test_parse_history_with_page() {
        let s = no_skills();
        match SlashCommand::parse("/history 3", &s) {
            Some(SlashCommand::History { page }) => assert_eq!(page, 3),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn test_execute_history() {
        let cmd = SlashCommand::History { page: 2 };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::History { page } => assert_eq!(page, 2),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn test_help_text_includes_history() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/history"));
    }

    // ── /retry command ────────────────────────────────────────────────

    #[test]
    fn test_parse_retry() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/retry", &s), Some(SlashCommand::Retry)));
    }

    #[test]
    fn test_parse_redo_alias() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/redo", &s), Some(SlashCommand::Retry)));
    }

    #[test]
    fn test_execute_retry() {
        let cmd = SlashCommand::Retry;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Retry));
    }

    #[test]
    fn test_help_text_includes_retry() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/retry"));
    }

    // ── comprehensive: every parse alias ────────────────────────────

    #[test]
    fn test_parse_all_aliases() {
        let s = no_skills();
        // /config aliases
        assert!(matches!(SlashCommand::parse("/config", &s), Some(SlashCommand::Config)));
        assert!(matches!(SlashCommand::parse("/settings", &s), Some(SlashCommand::Config)));
        // /branch aliases
        assert!(matches!(SlashCommand::parse("/branch feat", &s), Some(SlashCommand::Branch { .. })));
        assert!(matches!(SlashCommand::parse("/fork feat", &s), Some(SlashCommand::Branch { .. })));
        // /pr-comments aliases
        assert!(matches!(SlashCommand::parse("/pr-comments 42", &s), Some(SlashCommand::PrComments { .. })));
        assert!(matches!(SlashCommand::parse("/prc 42", &s), Some(SlashCommand::PrComments { .. })));
        // /reload-context aliases
        assert!(matches!(SlashCommand::parse("/reload-context", &s), Some(SlashCommand::ReloadContext)));
        assert!(matches!(SlashCommand::parse("/reload", &s), Some(SlashCommand::ReloadContext)));
        // /plugin aliases
        assert!(matches!(SlashCommand::parse("/plugin", &s), Some(SlashCommand::Plugin { .. })));
        assert!(matches!(SlashCommand::parse("/plugins", &s), Some(SlashCommand::Plugin { .. })));
        // /agents aliases
        assert!(matches!(SlashCommand::parse("/agents", &s), Some(SlashCommand::Agents { .. })));
        assert!(matches!(SlashCommand::parse("/agent", &s), Some(SlashCommand::Agents { .. })));
    }

    // ── comprehensive: every command parse+execute round-trip ────────

    #[test]
    fn test_permissions_parse_with_mode() {
        let s = no_skills();
        match SlashCommand::parse("/permissions bypass", &s) {
            Some(SlashCommand::Permissions { mode }) => assert_eq!(mode, "bypass"),
            _ => panic!("expected Permissions"),
        }
        match SlashCommand::parse("/perms plan", &s) {
            Some(SlashCommand::Permissions { mode }) => assert_eq!(mode, "plan"),
            _ => panic!("expected Permissions"),
        }
        // no arg
        match SlashCommand::parse("/permissions", &s) {
            Some(SlashCommand::Permissions { mode }) => assert!(mode.is_empty()),
            _ => panic!("expected Permissions"),
        }
    }

    #[test]
    fn test_execute_permissions() {
        let cmd = SlashCommand::Permissions { mode: "bypass".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Permissions { mode } => assert_eq!(mode, "bypass"),
            _ => panic!("expected Permissions"),
        }
        let cmd = SlashCommand::Permissions { mode: String::new() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Permissions { mode } => assert!(mode.is_empty()),
            _ => panic!("expected Permissions"),
        }
    }

    #[test]
    fn test_parse_and_execute_branch() {
        let s = no_skills();
        let cmd = SlashCommand::parse("/branch feature-x", &s).unwrap();
        match cmd.execute(&s, &no_plugins()) {
            CommandResult::Branch { name } => assert_eq!(name, "feature-x"),
            _ => panic!("expected Branch"),
        }
    }

    #[test]
    fn test_parse_pr_comments_hash_prefix() {
        let s = no_skills();
        match SlashCommand::parse("/prc #123", &s) {
            Some(SlashCommand::PrComments { pr_number }) => assert_eq!(pr_number, 123),
            _ => panic!("expected PrComments"),
        }
        match SlashCommand::parse("/pr-comments 456", &s) {
            Some(SlashCommand::PrComments { pr_number }) => assert_eq!(pr_number, 456),
            _ => panic!("expected PrComments"),
        }
        // invalid number → 0
        match SlashCommand::parse("/prc abc", &s) {
            Some(SlashCommand::PrComments { pr_number }) => assert_eq!(pr_number, 0),
            _ => panic!("expected PrComments"),
        }
    }

    #[test]
    fn test_execute_config() {
        let cmd = SlashCommand::Config;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Config));
    }

    #[test]
    fn test_execute_undo() {
        let cmd = SlashCommand::Undo;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Undo));
    }

    #[test]
    fn test_execute_diff() {
        let cmd = SlashCommand::Diff;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Diff));
    }

    #[test]
    fn test_execute_status() {
        let cmd = SlashCommand::Status;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Status));
    }

    #[test]
    fn test_execute_login_logout() {
        let login = SlashCommand::Login;
        assert!(matches!(login.execute(&no_skills(), &no_plugins()), CommandResult::Login));
        let logout = SlashCommand::Logout;
        assert!(matches!(logout.execute(&no_skills(), &no_plugins()), CommandResult::Logout));
    }

    #[test]
    fn test_execute_context() {
        let cmd = SlashCommand::Context;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Context));
    }

    #[test]
    fn test_execute_reload_context() {
        let cmd = SlashCommand::ReloadContext;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::ReloadContext));
    }

    #[test]
    fn test_execute_doctor() {
        let cmd = SlashCommand::Doctor;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Doctor));
    }

    #[test]
    fn test_execute_init() {
        let cmd = SlashCommand::Init;
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Init));
    }

    #[test]
    fn test_execute_cost() {
        let cmd = SlashCommand::Cost { window: String::new() };
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::ShowCost { .. }));
    }

    #[test]
    fn test_execute_review() {
        let cmd = SlashCommand::Review { prompt: "check perf".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Review { prompt } => assert_eq!(prompt, "check perf"),
            _ => panic!("expected Review"),
        }
    }

    #[test]
    fn test_execute_commit() {
        let cmd = SlashCommand::Commit { message: "feat: new".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Commit { message } => assert_eq!(message, "feat: new"),
            _ => panic!("expected Commit"),
        }
    }

    #[test]
    fn test_execute_memory_session_passthrough() {
        let cmd = SlashCommand::Memory { sub: "list".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Memory { sub } => assert_eq!(sub, "list"),
            _ => panic!("expected Memory"),
        }
        let cmd = SlashCommand::Session { sub: "save".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Session { sub } => assert_eq!(sub, "save"),
            _ => panic!("expected Session"),
        }
    }

    #[test]
    fn test_execute_mcp_plugin_agents_passthrough() {
        let cmd = SlashCommand::Mcp { sub: "status".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Mcp { sub } => assert_eq!(sub, "status"),
            _ => panic!("expected Mcp"),
        }
        let cmd = SlashCommand::Plugin { sub: "list".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Plugin { sub } => assert_eq!(sub, "list"),
            _ => panic!("expected Plugin"),
        }
        let cmd = SlashCommand::Agents { sub: "show".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Agents { sub } => assert_eq!(sub, "show"),
            _ => panic!("expected Agents"),
        }
    }

    #[test]
    fn test_execute_export_format() {
        let cmd = SlashCommand::Export { format: "json".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Export { format } => assert_eq!(format, "json"),
            _ => panic!("expected Export"),
        }
    }

    #[test]
    fn test_execute_run_skill() {
        let cmd = SlashCommand::RunSkill { name: "deploy".into(), prompt: "to prod".into() };
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::RunSkill { name, prompt } => {
                assert_eq!(name, "deploy");
                assert_eq!(prompt, "to prod");
            }
            _ => panic!("expected RunSkill"),
        }
    }

    // ── help text completeness ──────────────────────────────────────

    #[test]
    fn test_help_text_covers_all_sections() {
        let text = build_help_text(&no_skills(), &no_plugins());
        // All section headers (match HELP_TEXT_BASE)
        assert!(text.contains("Conversation"));
        assert!(text.contains("Git & Code"));
        assert!(text.contains("Configuration"));
        assert!(text.contains("Session & Memory"));
        assert!(text.contains("System"));
        assert!(text.contains("Tips"));
        // Key commands from each section
        assert!(text.contains("/help"));
        assert!(text.contains("/compact"));
        assert!(text.contains("/model"));
        assert!(text.contains("/permissions"));
        assert!(text.contains("/commit"));
        assert!(text.contains("/session"));
        assert!(text.contains("/diff"));
        assert!(text.contains("/doctor"));
        assert!(text.contains("/export"));
        assert!(text.contains("/context"));
        assert!(text.contains("/config"));
        assert!(text.contains("/login"));
        assert!(text.contains("/logout"));
        assert!(text.contains("/undo"));
        assert!(text.contains("/rewind"));
        assert!(text.contains("/init"));
        assert!(text.contains("/agents"));
        assert!(text.contains("/mcp"));
    }

    // ── edge cases ──────────────────────────────────────────────────

    #[test]
    fn test_parse_whitespace_handling() {
        let s = no_skills();
        // leading/trailing whitespace
        match SlashCommand::parse("  /model   opus  ", &s) {
            Some(SlashCommand::Model(name)) => assert_eq!(name, "opus"),
            _ => panic!("expected Model"),
        }
        // just a slash
        match SlashCommand::parse("/", &s) {
            Some(SlashCommand::Unknown(name)) => assert!(name.is_empty()),
            _ => panic!("expected Unknown for bare slash"),
        }
    }

    #[test]
    fn test_parse_history_invalid_page() {
        let s = no_skills();
        // non-numeric page falls back to 1
        match SlashCommand::parse("/history abc", &s) {
            Some(SlashCommand::History { page }) => assert_eq!(page, 1),
            _ => panic!("expected History with default page"),
        }
    }

    #[test]
    fn test_parse_rewind() {
        let s = no_skills();
        match SlashCommand::parse("/rewind", &s) {
            Some(SlashCommand::Rewind { turns }) => assert!(turns.is_empty()),
            _ => panic!("expected Rewind"),
        }
        match SlashCommand::parse("/rewind 3", &s) {
            Some(SlashCommand::Rewind { turns }) => assert_eq!(turns, "3"),
            _ => panic!("expected Rewind with arg"),
        }
    }

    #[test]
    fn test_execute_rewind() {
        let cmd = SlashCommand::parse("/rewind 5", &no_skills()).unwrap();
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Rewind { turns } => assert_eq!(turns, "5"),
            _ => panic!("expected Rewind result"),
        }
    }

    // ── /fast ────────────────────────────────────────────────────────

    #[test]
    fn test_parse_fast() {
        let s = no_skills();
        match SlashCommand::parse("/fast", &s) {
            Some(SlashCommand::Fast { toggle }) => assert!(toggle.is_empty()),
            _ => panic!("expected Fast"),
        }
        match SlashCommand::parse("/fast off", &s) {
            Some(SlashCommand::Fast { toggle }) => assert_eq!(toggle, "off"),
            _ => panic!("expected Fast with off"),
        }
    }

    #[test]
    fn test_execute_fast() {
        let cmd = SlashCommand::parse("/fast", &no_skills()).unwrap();
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Fast { toggle } => assert!(toggle.is_empty()),
            _ => panic!("expected Fast result"),
        }
        let cmd = SlashCommand::parse("/fast off", &no_skills()).unwrap();
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::Fast { toggle } => assert_eq!(toggle, "off"),
            _ => panic!("expected Fast off result"),
        }
    }

    // ── /add-dir ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_add_dir() {
        let s = no_skills();
        match SlashCommand::parse("/add-dir ./src", &s) {
            Some(SlashCommand::AddDir { path }) => assert_eq!(path, "./src"),
            _ => panic!("expected AddDir"),
        }
        // alias
        match SlashCommand::parse("/adddir /tmp/docs", &s) {
            Some(SlashCommand::AddDir { path }) => assert_eq!(path, "/tmp/docs"),
            _ => panic!("expected AddDir alias"),
        }
    }

    #[test]
    fn test_execute_add_dir() {
        let cmd = SlashCommand::parse("/add-dir ./test", &no_skills()).unwrap();
        match cmd.execute(&no_skills(), &no_plugins()) {
            CommandResult::AddDir { path } => assert_eq!(path, "./test"),
            _ => panic!("expected AddDir result"),
        }
    }

    // ── new commands: /share, /files, /env ──────────────────────────

    #[test]
    fn test_parse_share() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/share", &s), Some(SlashCommand::Share)));
    }

    #[test]
    fn test_parse_files() {
        let s = no_skills();
        match SlashCommand::parse("/files", &s) {
            Some(SlashCommand::Files { pattern }) => assert!(pattern.is_empty()),
            _ => panic!("expected Files"),
        }
        match SlashCommand::parse("/files *.rs", &s) {
            Some(SlashCommand::Files { pattern }) => assert_eq!(pattern, "*.rs"),
            _ => panic!("expected Files with pattern"),
        }
        // alias
        assert!(matches!(SlashCommand::parse("/ls", &s), Some(SlashCommand::Files { .. })));
    }

    #[test]
    fn test_parse_env() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/env", &s), Some(SlashCommand::Env)));
        assert!(matches!(SlashCommand::parse("/environment", &s), Some(SlashCommand::Env)));
    }

    #[test]
    fn test_execute_share() {
        let cmd = SlashCommand::parse("/share", &no_skills()).unwrap();
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Share));
    }

    #[test]
    fn test_execute_env() {
        let cmd = SlashCommand::parse("/env", &no_skills()).unwrap();
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Env));
    }

    #[test]
    fn test_help_text_includes_new_features() {
        let text = build_help_text(&no_skills(), &no_plugins());
        assert!(text.contains("/share"));
        assert!(text.contains("/files"));
        assert!(text.contains("/env"));
        assert!(text.contains("/vim"));
        assert!(text.contains("/effort"));
        assert!(text.contains("/stickers"));
        assert!(text.contains("/tag"));
        assert!(text.contains("/release-notes"));
        assert!(text.contains("/feedback"));
        assert!(text.contains("Alt+Left"));
        assert!(text.contains("Alt+Backspace"));
    }

    // ── New command parse tests ─────────────────────────────────────

    #[test]
    fn test_parse_vim() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/vim", &s), Some(SlashCommand::Vim { toggle }) if toggle.is_empty()));
        assert!(matches!(SlashCommand::parse("/vim on", &s), Some(SlashCommand::Vim { toggle }) if toggle == "on"));
        assert!(matches!(SlashCommand::parse("/vim off", &s), Some(SlashCommand::Vim { toggle }) if toggle == "off"));
    }

    #[test]
    fn test_parse_stickers() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/stickers", &s), Some(SlashCommand::Stickers)));
    }

    #[test]
    fn test_parse_effort() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/effort", &s), Some(SlashCommand::Effort { level }) if level.is_empty()));
        assert!(matches!(SlashCommand::parse("/effort high", &s), Some(SlashCommand::Effort { level }) if level == "high"));
    }

    #[test]
    fn test_parse_tag() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/tag", &s), Some(SlashCommand::Tag { name }) if name.is_empty()));
        assert!(matches!(SlashCommand::parse("/tag important", &s), Some(SlashCommand::Tag { name }) if name == "important"));
    }

    #[test]
    fn test_parse_release_notes() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/release-notes", &s), Some(SlashCommand::ReleaseNotes)));
        assert!(matches!(SlashCommand::parse("/changelog", &s), Some(SlashCommand::ReleaseNotes)));
    }

    #[test]
    fn test_parse_feedback() {
        let s = no_skills();
        assert!(matches!(SlashCommand::parse("/feedback", &s), Some(SlashCommand::Feedback { text }) if text.is_empty()));
        assert!(matches!(SlashCommand::parse("/feedback great tool!", &s), Some(SlashCommand::Feedback { text }) if text == "great tool!"));
    }

    #[test]
    fn test_execute_effort_empty() {
        let cmd = SlashCommand::parse("/effort", &no_skills()).unwrap();
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Effort { level } if level.is_empty()));
    }

    #[test]
    fn test_execute_feedback_empty() {
        let cmd = SlashCommand::parse("/feedback", &no_skills()).unwrap();
        // Empty feedback should yield a Print hint
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Print(_)));
    }

    #[test]
    fn test_execute_feedback_with_text() {
        let cmd = SlashCommand::parse("/feedback hello", &no_skills()).unwrap();
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Feedback { text } if text == "hello"));
    }

    #[test]
    fn test_parse_stats() {
        assert!(matches!(
            SlashCommand::parse("/stats", &no_skills()).unwrap(),
            SlashCommand::Stats
        ));
        assert!(matches!(
            SlashCommand::parse("/usage", &no_skills()).unwrap(),
            SlashCommand::Stats
        ));
    }

    #[test]
    fn test_execute_stats() {
        let cmd = SlashCommand::parse("/stats", &no_skills()).unwrap();
        assert!(matches!(cmd.execute(&no_skills(), &no_plugins()), CommandResult::Stats));
    }
}
