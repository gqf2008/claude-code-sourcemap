//! `/agents` command handler — list, inspect, create and delete agent definitions.

use std::path::Path;

use claude_core::agents::{
    AgentDefinition, AgentSource,
    delete_agent, get_agents, save_agent, validate_agent,
};

/// Handle `/agents [subcommand]`.
pub(crate) fn handle_agents_command(sub: &str, cwd: &Path) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.trim()).unwrap_or("");
    let args = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match subcmd {
        "" | "list" => list_agents(cwd),
        "info" => {
            if args.is_empty() {
                println!("Usage: /agents info <name>");
            } else {
                show_agent(args, cwd);
            }
        }
        "create" => {
            if args.is_empty() {
                println!("Usage: /agents create <name>");
                println!("\x1b[2mCreates an agent definition in .claude/agents/<name>.md\x1b[0m");
            } else {
                create_agent_scaffold(args, cwd);
            }
        }
        "delete" | "rm" => {
            if args.is_empty() {
                println!("Usage: /agents delete <name>");
            } else {
                delete_agent_cmd(args, cwd);
            }
        }
        "help" => {
            println!("\x1b[1mAgent Definitions\x1b[0m\n");
            println!("  /agents               List all agent definitions");
            println!("  /agents list           Same as above");
            println!("  /agents info <name>    Show details of an agent");
            println!("  /agents create <name>  Create a new agent scaffold");
            println!("  /agents delete <name>  Delete an agent definition");
            println!();
            println!("\x1b[2mAgents are .md files in .claude/agents/ with YAML frontmatter.");
            println!("They define specialized sub-agents with custom tools, models, and prompts.\x1b[0m");
        }
        other => {
            println!("Unknown subcommand: {}. Try /agents help", other);
        }
    }
}

fn list_agents(cwd: &Path) {
    let all = get_agents(cwd);
    if all.is_empty() {
        println!("No agent definitions found.");
        println!("\x1b[2mCreate one with: /agents create <name>\x1b[0m");
        println!("\x1b[2mOr add .md files to .claude/agents/\x1b[0m");
        return;
    }

    println!("\x1b[1mAgent Definitions\x1b[0m ({} total)\n", all.len());

    // Group by source
    let mut by_source: std::collections::BTreeMap<String, Vec<&AgentDefinition>> =
        std::collections::BTreeMap::new();
    for agent in &all {
        let label = format!("{}", agent.source);
        by_source.entry(label).or_default().push(agent);
    }

    for (source, agents) in &by_source {
        println!("  \x1b[1;36m{}\x1b[0m", source);
        for agent in agents {
            let color = agent.color.as_deref().unwrap_or("");
            let color_dot = if !color.is_empty() {
                format!("\x1b[38;5;{}m●\x1b[0m ", color_code(color))
            } else {
                String::new()
            };
            let bg = if agent.background { " \x1b[2m[bg]\x1b[0m" } else { "" };
            println!(
                "    {}{:<20} {}{}",
                color_dot,
                agent.agent_type,
                agent.description,
                bg
            );
            if !agent.allowed_tools.is_empty() {
                let tool_list = if agent.allowed_tools.len() <= 5 {
                    agent.allowed_tools.join(", ")
                } else {
                    format!(
                        "{}, ... (+{})",
                        agent.allowed_tools[..4].join(", "),
                        agent.allowed_tools.len() - 4
                    )
                };
                println!("    {:<20} \x1b[2mtools: {}\x1b[0m", "", tool_list);
            }
        }
        println!();
    }
}

fn show_agent(name: &str, cwd: &Path) {
    let all = get_agents(cwd);
    let agent = all.iter().find(|a| a.agent_type.eq_ignore_ascii_case(name));
    match agent {
        None => println!("Agent '{}' not found. Use /agents list to see available.", name),
        Some(a) => {
            println!("\x1b[1m{}\x1b[0m", a.agent_type);
            println!("Description: {}", a.description);
            println!("Source:      {}", a.source);
            if let Some(ref model) = a.model {
                println!("Model:       {}", model);
            }
            if let Some(ref effort) = a.effort {
                println!("Effort:      {}", effort);
            }
            if let Some(ref color) = a.color {
                println!("Color:       {}", color);
            }
            if let Some(ref perm) = a.permission_mode {
                println!("Permissions: {}", perm);
            }
            if let Some(turns) = a.max_turns {
                println!("Max turns:   {}", turns);
            }
            if a.background {
                println!("Background:  yes");
            }
            if let Some(ref mem) = a.memory {
                println!("Memory:      {}", mem);
            }
            if !a.allowed_tools.is_empty() {
                println!("Tools:       {}", a.allowed_tools.join(", "));
            }
            if !a.disallowed_tools.is_empty() {
                println!("Disallowed:  {}", a.disallowed_tools.join(", "));
            }
            if !a.skills.is_empty() {
                println!("Skills:      {}", a.skills.join(", "));
            }
            if let Some(ref path) = a.file_path {
                println!("File:        {}", path.display());
            }
            // Show first 200 chars of system prompt
            let prompt_preview = if a.system_prompt.len() > 200 {
                format!("{}...", &a.system_prompt[..200])
            } else {
                a.system_prompt.clone()
            };
            println!("\n\x1b[2m--- System Prompt ---\x1b[0m");
            println!("{}", prompt_preview);
        }
    }
}

fn create_agent_scaffold(name: &str, cwd: &Path) {
    let agent = AgentDefinition {
        agent_type: name.to_string(),
        description: format!("{} agent", name),
        system_prompt: format!("You are a specialized {} assistant.", name),
        allowed_tools: vec![],
        disallowed_tools: vec![],
        model: None,
        effort: None,
        memory: None,
        color: None,
        permission_mode: None,
        max_turns: None,
        background: false,
        skills: vec![],
        initial_prompt: None,
        source: AgentSource::Local,
        file_path: None,
        base_dir: None,
    };

    // Validate before saving
    let existing = get_agents(cwd);
    let validation = validate_agent(&agent, &existing);
    if !validation.is_valid() {
        println!("\x1b[31mInvalid agent definition:\x1b[0m");
        for e in &validation.errors {
            println!("  - {}", e);
        }
        return;
    }
    for w in &validation.warnings {
        println!("\x1b[33m⚠ {}\x1b[0m", w);
    }

    match save_agent(&agent, cwd) {
        Ok(path) => {
            println!("\x1b[32m✓\x1b[0m Created agent scaffold: {}", path.display());
            println!("\x1b[2mEdit the file to customize tools, model, and system prompt.\x1b[0m");
        }
        Err(e) => {
            println!("\x1b[31mFailed to create agent: {}\x1b[0m", e);
        }
    }
}

fn delete_agent_cmd(name: &str, cwd: &Path) {
    let all = get_agents(cwd);
    let agent = all.iter().find(|a| a.agent_type.eq_ignore_ascii_case(name));
    match agent {
        None => {
            println!("\x1b[31mAgent '{}' not found.\x1b[0m Use /agents list to see available.", name);
        }
        Some(a) => {
            if a.source == AgentSource::BuiltIn {
                println!("\x1b[31mCannot delete built-in agent '{}'.\x1b[0m", name);
                return;
            }
            match delete_agent(a) {
                Ok(()) => {
                    println!("\x1b[32m✓\x1b[0m Deleted agent: {}", name);
                }
                Err(e) => {
                    println!("\x1b[31mFailed to delete agent '{}': {}\x1b[0m", name, e);
                }
            }
        }
    }
}

/// Convert a color name to a 256-color code for terminal display.
fn color_code(color: &str) -> u8 {
    match color.to_lowercase().as_str() {
        "red" => 196,
        "green" => 46,
        "blue" => 33,
        "yellow" => 226,
        "orange" => 208,
        "purple" | "violet" => 135,
        "cyan" => 51,
        "magenta" | "pink" => 198,
        "white" => 15,
        "gray" | "grey" => 245,
        _ => 7, // default white
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_code_known_colors() {
        assert_eq!(color_code("red"), 196);
        assert_eq!(color_code("blue"), 33);
        assert_eq!(color_code("GREEN"), 46);
    }

    #[test]
    fn color_code_unknown_defaults() {
        assert_eq!(color_code("rainbow"), 7);
        assert_eq!(color_code(""), 7);
    }
}
