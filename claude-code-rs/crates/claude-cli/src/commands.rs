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
    RunSkill { name: String, prompt: String },
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
            "config" => Self::Config,
            "undo" => Self::Undo,
            "review" => Self::Review { prompt: args },
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
                CommandResult::Print("Usage: /model <name>".to_string())
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
    RunSkill { name: String, prompt: String },
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
Available commands:
  /help              Show this help
  /clear             Clear conversation history
  /model <name>      Switch model
  /compact [instr]   Compact conversation history
  /cost              Show token usage
  /diff              Show git diff (staged + unstaged)
  /status            Show session and git status
  /undo              Undo last assistant turn (remove last assistant+user pair)
  /review [prompt]   Launch code review on recent changes
  /permissions       Show current permission mode and rules
  /config            Show current configuration
  /skills            List available skills
  /memory list       List memory files
  /memory open <f>   Open a memory file
  /session save      Save current session
  /session list      List saved sessions
  /session load <id> Resume a saved session
  /exit              Exit the CLI

Tips:
  • End a line with \\ to continue on the next line (multiline input)
  • Use --resume to restore the most recent session on startup
  • Use --init to create CLAUDE.md and project scaffolding";


