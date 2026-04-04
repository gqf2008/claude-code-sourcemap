use claude_core::config::Settings;
use claude_core::permissions::PermissionMode;

pub fn load_settings() -> anyhow::Result<Settings> {
    Settings::load()
}

pub fn parse_permission_mode(mode: &str) -> PermissionMode {
    match mode {
        "bypass" | "bypassPermissions" => PermissionMode::BypassAll,
        "acceptEdits" | "accept-edits" => PermissionMode::AcceptEdits,
        "plan" => PermissionMode::Plan,
        _ => PermissionMode::Default,
    }
}

pub fn build_system_prompt(
    cli_prompt: Option<&str>,
    settings_prompt: Option<&str>,
    append_prompt: Option<&str>,
) -> String {
    let base = cli_prompt.or(settings_prompt).unwrap_or(DEFAULT_SYSTEM_PROMPT);
    match append_prompt {
        Some(extra) => format!("{}\n\n{}", base, extra),
        None => base.to_string(),
    }
}

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are Claude, an AI assistant made by Anthropic, running as a CLI coding agent.
You have access to tools for reading, writing, and searching files, executing shell commands, and more.

Key guidelines:
- Read files before editing them
- Make precise, surgical edits
- Use Bash for complex operations
- Verify changes after making them
- Be concise in responses"#;
