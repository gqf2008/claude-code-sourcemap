use claude_core::skills::SkillEntry;

pub enum SlashCommand {
    Help,
    Clear,
    Model(String),
    Compact { instructions: String },
    Cost,
    Skills,
    Memory { sub: String },
    Session { sub: String },
    Diff,
    Status,
    Permissions,
    Config,
    Undo,
    Review { prompt: String },
    Doctor,
    Init,
    Commit { message: String },
    CommitPushPr { message: String },
    Pr { prompt: String },
    Bug { prompt: String },
    Search { query: String },
    Version,
    Login,
    Logout,
    Context,
    Export { format: String },
    RunSkill { name: String, prompt: String },
    ReloadContext,
    Mcp { sub: String },
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
            "cost" => Self::Cost,
            "skills" => Self::Skills,
            "memory" => Self::Memory { sub: args },
            "session" | "resume" => Self::Session { sub: args },
            "diff" => Self::Diff,
            "status" => Self::Status,
            "permissions" | "perms" => Self::Permissions,
            "config" | "settings" => Self::Config,
            "undo" => Self::Undo,
            "review" => Self::Review { prompt: args },
            "doctor" => Self::Doctor,
            "init" => Self::Init,
            "commit" => Self::Commit { message: args },
            "commit-push-pr" | "cpp" => Self::CommitPushPr { message: args },
            "pr" => Self::Pr { prompt: args },
            "bug" | "debug" => Self::Bug { prompt: args },
            "search" | "find" | "grep" => Self::Search { query: args },
            "version" => Self::Version,
            "login" => Self::Login,
            "logout" => Self::Logout,
            "context" | "ctx" => Self::Context,
            "export" => Self::Export { format: if args.is_empty() { "markdown".into() } else { args } },
            "reload-context" | "reload" => Self::ReloadContext,
            "mcp" => Self::Mcp { sub: args },
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
    pub fn execute(&self, known_skills: &[SkillEntry]) -> CommandResult {
        match self {
            Self::Help => CommandResult::Print(build_help_text(known_skills)),
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
            Self::Cost => CommandResult::ShowCost,
            Self::Skills => {
                if known_skills.is_empty() {
                    CommandResult::Print("No skills found. Add .md files to .claude/skills/".into())
                } else {
                    let list = known_skills.iter()
                        .map(|s| format!("  /{:<20} {}", s.name, s.description))
                        .collect::<Vec<_>>()
                        .join("\n");
                    CommandResult::Print(format!("Available skills:\n{}", list))
                }
            }
            Self::Memory { sub } => CommandResult::Memory { sub: sub.clone() },
            Self::Session { sub } => CommandResult::Session { sub: sub.clone() },
            Self::Diff => CommandResult::Diff,
            Self::Status => CommandResult::Status,
            Self::Permissions => CommandResult::Permissions,
            Self::Config => CommandResult::Config,
            Self::Undo => CommandResult::Undo,
            Self::Review { prompt } => CommandResult::Review { prompt: prompt.clone() },
            Self::Doctor => CommandResult::Doctor,
            Self::Init => CommandResult::Init,
            Self::Commit { message } => CommandResult::Commit { message: message.clone() },
            Self::CommitPushPr { message } => CommandResult::CommitPushPr { message: message.clone() },
            Self::Pr { prompt } => CommandResult::Pr { prompt: prompt.clone() },
            Self::Bug { prompt } => CommandResult::Bug { prompt: prompt.clone() },
            Self::Search { query } => CommandResult::Search { query: query.clone() },
            Self::Version => CommandResult::Print(format!("claude-code-rs v{}", env!("CARGO_PKG_VERSION"))),
            Self::Login => CommandResult::Login,
            Self::Logout => CommandResult::Logout,
            Self::Context => CommandResult::Context,
            Self::Export { format } => CommandResult::Export { format: format.clone() },
            Self::ReloadContext => CommandResult::ReloadContext,
            Self::Mcp { sub } => CommandResult::Mcp { sub: sub.clone() },
            Self::RunSkill { name, prompt } => CommandResult::RunSkill {
                name: name.clone(),
                prompt: prompt.clone(),
            },
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
    ShowCost,
    Compact { instructions: Option<String> },
    Memory { sub: String },
    Session { sub: String },
    Diff,
    Status,
    Permissions,
    Config,
    Undo,
    Review { prompt: String },
    Doctor,
    Init,
    Commit { message: String },
    CommitPushPr { message: String },
    Pr { prompt: String },
    Bug { prompt: String },
    Search { query: String },
    Login,
    Logout,
    Context,
    Export { format: String },
    RunSkill { name: String, prompt: String },
    ReloadContext,
    Mcp { sub: String },
    Exit,
}

fn build_help_text(skills: &[SkillEntry]) -> String {
    let mut text = HELP_TEXT_BASE.to_string();
    if !skills.is_empty() {
        text.push_str("\n\nSkills (sub-agents with specialised prompts):");
        for s in skills {
            text.push_str(&format!("\n  /{:<20} {}", s.name, s.description));
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
  /search <query>    Search conversation history
  /cost              Show token usage and costs
  /exit              Exit the CLI

\x1b[1mGit & Code\x1b[0m
  /diff              Show git diff (staged + unstaged)
  /status            Show session and git status
  /commit [msg]      Stage and commit (AI-generated message)
  /commit-push-pr    Commit → push → create PR (alias: /cpp)
  /pr [prompt]       Create/review a pull request
  /bug [prompt]      Debug a problem with AI assistance
  /review [prompt]   AI code review on recent changes
  /init              Initialize CLAUDE.md for the project

\x1b[1mConfiguration\x1b[0m
  /model <name>      Switch model (aliases: sonnet, opus, haiku, best)
  /login             Set API key interactively
  /logout            Clear saved API key
  /config            Show current configuration
  /permissions       Show permission mode and rules
  /context           Show loaded context (CLAUDE.md, memory, model)
  /reload-context    Reload CLAUDE.md, memory, and settings
  /mcp               Show discovered MCP servers

\x1b[1mSession & Memory\x1b[0m
  /session save      Save current session
  /session list      List saved sessions
  /session load <id> Resume a saved session
  /session delete <id> Delete a saved session
  /export [format]   Export session (markdown or json)
  /memory list       List memory files
  /memory open <f>   Open a memory file

\x1b[1mSystem\x1b[0m
  /doctor            Check environment health
  /skills            List available skills
  /version           Show version info

\x1b[1mTips\x1b[0m
  • End a line with \\ to continue on the next line (multiline)
  • Attach images: type @path/to/image.png on its own line
  • Use --resume to restore the most recent session on startup
  • Use --init to create CLAUDE.md and project scaffolding";


#[cfg(test)]
mod tests {
    use super::*;

    fn no_skills() -> Vec<SkillEntry> { Vec::new() }

    fn test_skills() -> Vec<SkillEntry> {
        vec![SkillEntry {
            name: "review".into(),
            description: "Code review skill".into(),
            system_prompt: "You are a reviewer".into(),
            allowed_tools: vec!["Read".into()],
            model: None,
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
        assert!(matches!(SlashCommand::parse("/cost", &s), Some(SlashCommand::Cost)));
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
        assert!(matches!(SlashCommand::parse("/perms", &s), Some(SlashCommand::Permissions)));
        assert!(matches!(SlashCommand::parse("/permissions", &s), Some(SlashCommand::Permissions)));
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
        match cmd.execute(&no_skills()) {
            CommandResult::Print(text) => assert!(text.contains("/help")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_help_with_skills() {
        let cmd = SlashCommand::Help;
        let skills = test_skills();
        match cmd.execute(&skills) {
            CommandResult::Print(text) => {
                assert!(text.contains("/help"));
                assert!(text.contains("review"));
                assert!(text.contains("Code review skill"));
            }
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_clear() {
        let cmd = SlashCommand::Clear;
        assert!(matches!(cmd.execute(&no_skills()), CommandResult::ClearHistory));
    }

    #[test]
    fn test_execute_model_empty() {
        let cmd = SlashCommand::Model(String::new());
        match cmd.execute(&no_skills()) {
            CommandResult::Print(text) => assert!(text.contains("Usage")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_model_set() {
        let cmd = SlashCommand::Model("opus".into());
        match cmd.execute(&no_skills()) {
            CommandResult::SetModel(name) => assert_eq!(name, "opus"),
            _ => panic!("expected SetModel"),
        }
    }

    #[test]
    fn test_execute_version() {
        let cmd = SlashCommand::Version;
        match cmd.execute(&no_skills()) {
            CommandResult::Print(text) => assert!(text.contains("claude-code-rs")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_skills_empty() {
        let cmd = SlashCommand::Skills;
        match cmd.execute(&no_skills()) {
            CommandResult::Print(text) => assert!(text.contains("No skills")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_skills_list() {
        let cmd = SlashCommand::Skills;
        let skills = test_skills();
        match cmd.execute(&skills) {
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
        match cmd.execute(&no_skills()) {
            CommandResult::Compact { instructions } => {
                assert_eq!(instructions.as_deref(), Some("focus on code"));
            }
            _ => panic!("expected Compact"),
        }
    }

    #[test]
    fn test_execute_compact_empty() {
        let cmd = SlashCommand::Compact { instructions: String::new() };
        match cmd.execute(&no_skills()) {
            CommandResult::Compact { instructions } => assert!(instructions.is_none()),
            _ => panic!("expected Compact"),
        }
    }

    #[test]
    fn test_execute_unknown() {
        let cmd = SlashCommand::Unknown("xyz".into());
        match cmd.execute(&no_skills()) {
            CommandResult::Print(text) => assert!(text.contains("Unknown")),
            _ => panic!("expected Print"),
        }
    }

    #[test]
    fn test_execute_exit() {
        let cmd = SlashCommand::Exit;
        assert!(matches!(cmd.execute(&no_skills()), CommandResult::Exit));
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
        match cmd.execute(&no_skills()) {
            CommandResult::Pr { prompt } => assert_eq!(prompt, "review security"),
            _ => panic!("expected Pr"),
        }
    }

    #[test]
    fn test_execute_bug() {
        let cmd = SlashCommand::Bug { prompt: "OOM crash".into() };
        match cmd.execute(&no_skills()) {
            CommandResult::Bug { prompt } => assert_eq!(prompt, "OOM crash"),
            _ => panic!("expected Bug"),
        }
    }

    #[test]
    fn test_help_text_includes_new_commands() {
        let text = build_help_text(&no_skills());
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
        match cmd.execute(&no_skills()) {
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
        match cmd.execute(&no_skills()) {
            CommandResult::Mcp { sub } => assert_eq!(sub, "list"),
            _ => panic!("expected Mcp"),
        }
    }

    #[test]
    fn test_help_text_includes_mcp() {
        let text = build_help_text(&no_skills());
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
        match cmd.execute(&no_skills()) {
            CommandResult::CommitPushPr { message } => assert_eq!(message, "new feature"),
            _ => panic!("expected CommitPushPr"),
        }
    }

    #[test]
    fn test_help_text_includes_cpp() {
        let text = build_help_text(&no_skills());
        assert!(text.contains("/commit-push-pr"));
        assert!(text.contains("/cpp"));
    }
}