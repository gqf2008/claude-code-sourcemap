//! Modular system prompt assembly — aligned with TS `prompts.ts` + `systemPromptSections.ts`.
//!
//! The system prompt is composed from named sections in a defined order.
//! A **dynamic boundary marker** separates the static prefix (cacheable across
//! organizations) from the per-session dynamic suffix.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────── Static prefix (global cache) ────────────────┐
//! │ identity  │ system_guidelines │ doing_tasks │ actions │ ...  │
//! ├──────────────── DYNAMIC BOUNDARY ────────────────────────────┤
//! │ environment │ memory │ tool_guidance │ claude_md │ ...       │
//! └──────────────────────────────────────────────────────────────┘
//! ```

pub mod sections;

use std::path::Path;

use sections::*;

/// Marker that separates globally-cacheable prefix from session-specific suffix.
/// The API prompt-caching layer uses this to apply different cache scopes.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

// ── Builder types ───────────────────────────────────────────────────────────

/// Assembled system prompt with cache boundary information.
#[derive(Debug, Clone)]
pub struct SystemPrompt {
    /// Full text of the system prompt.
    pub text: String,
    /// Byte offset where the dynamic boundary starts (for cache splitting).
    pub dynamic_boundary_offset: usize,
}

impl SystemPrompt {
    /// The globally-cacheable prefix (before the dynamic boundary).
    pub fn static_prefix(&self) -> &str {
        &self.text[..self.dynamic_boundary_offset]
    }

    /// The per-session dynamic suffix (after the dynamic boundary).
    pub fn dynamic_suffix(&self) -> &str {
        &self.text[self.dynamic_boundary_offset..]
    }
}

/// Optional dynamic sections for the system prompt.
#[derive(Debug)]
pub struct DynamicSections<'a> {
    /// Language preference (e.g. "中文", "English")
    pub language: Option<&'a str>,
    /// Output style name + prompt
    pub output_style: Option<(&'a str, &'a str)>,
    /// MCP server (name, instructions) pairs
    pub mcp_instructions: Vec<(String, String)>,
    /// Scratchpad directory path
    pub scratchpad_dir: Option<&'a str>,
    /// Token budget (0 = unlimited)
    pub token_budget: u64,
    /// Enable proactive/autonomous mode section
    pub proactive_mode: bool,
    /// Enable coordinator mode section
    pub coordinator_mode: bool,
    /// Include file editing best practices (default: true)
    pub include_editing_guidance: bool,
    /// Include git operations guidance (default: true)
    pub include_git_guidance: bool,
    /// Include testing guidance (default: true)
    pub include_testing_guidance: bool,
    /// Include debugging guidance (default: true)
    pub include_debugging_guidance: bool,
    /// Memory directory path (for behavioral prompt injection)
    pub memory_dir: Option<&'a str>,
}

impl<'a> Default for DynamicSections<'a> {
    fn default() -> Self {
        Self {
            language: None,
            output_style: None,
            mcp_instructions: Vec::new(),
            scratchpad_dir: None,
            token_budget: 0,
            proactive_mode: false,
            coordinator_mode: false,
            include_editing_guidance: true,
            include_git_guidance: true,
            include_testing_guidance: true,
            include_debugging_guidance: true,
            memory_dir: None,
        }
    }
}

// ── Build functions ─────────────────────────────────────────────────────────

/// Build the default system prompt from modular sections.
///
/// # Arguments
/// - `cwd` — Current working directory
/// - `model` — Model name (for environment info + knowledge cutoff)
/// - `enabled_tools` — Names of enabled tools (for tool-specific guidance)
/// - `claude_md_content` — Pre-loaded CLAUDE.md content (empty string if none)
/// - `memory_content` — Pre-loaded memory content (empty string if none)
pub fn build_system_prompt(
    cwd: &Path,
    model: &str,
    enabled_tools: &[String],
    claude_md_content: &str,
    memory_content: &str,
) -> SystemPrompt {
    build_system_prompt_ext(cwd, model, enabled_tools, claude_md_content, memory_content, &DynamicSections::default())
}

/// Extended build accepting additional dynamic sections.
pub fn build_system_prompt_ext(
    cwd: &Path,
    model: &str,
    enabled_tools: &[String],
    claude_md_content: &str,
    memory_content: &str,
    dynamic: &DynamicSections<'_>,
) -> SystemPrompt {
    let parts: Vec<String> = vec![
        DEFAULT_PREFIX.to_string(),
        section_system_guidelines().to_string(),
        section_doing_tasks().to_string(),
        section_actions().to_string(),
        section_using_tools().to_string(),
        section_tone_style().to_string(),
        section_output_efficiency().to_string(),
    ];

    let static_text = parts.join("\n");
    let dynamic_boundary_offset = static_text.len() + 1 + SYSTEM_PROMPT_DYNAMIC_BOUNDARY.len() + 1;

    // ── Dynamic suffix (per-session) ─────────────────────────────────────
    let mut dynamic_parts: Vec<String> = Vec::new();

    // Environment
    dynamic_parts.push(section_environment(cwd, model));

    // Language preference
    if let Some(lang) = section_language(dynamic.language) {
        dynamic_parts.push(lang);
    }

    // Output style
    if let Some((name, prompt)) = dynamic.output_style {
        if let Some(s) = section_output_style(Some(name), Some(prompt)) {
            dynamic_parts.push(s);
        }
    }

    // Tool guidance
    if !enabled_tools.is_empty() {
        dynamic_parts.push(section_tool_guidance(enabled_tools));
    }

    // MCP instructions
    if let Some(mcp) = section_mcp_instructions(&dynamic.mcp_instructions) {
        dynamic_parts.push(mcp);
    }

    // Memory system: behavioral instructions + file contents
    if let Some(mem_dir) = dynamic.memory_dir {
        dynamic_parts.push(section_memory_behavioral(mem_dir));
    }
    if !memory_content.is_empty() {
        dynamic_parts.push(format!(
            "\n## Memory Contents\n\n<memory>\n{}\n</memory>",
            memory_content
        ));
    }

    // CLAUDE.md project context
    if !claude_md_content.is_empty() {
        dynamic_parts.push(format!(
            "\n## Project Instructions (CLAUDE.md)\n\n<project-instructions>\n{}\n</project-instructions>",
            claude_md_content
        ));
    }

    // Scratchpad
    if let Some(sp) = section_scratchpad(dynamic.scratchpad_dir) {
        dynamic_parts.push(sp);
    }

    // Token budget
    if let Some(tb) = section_token_budget(dynamic.token_budget) {
        dynamic_parts.push(tb);
    }

    // Proactive/autonomous mode
    if dynamic.proactive_mode {
        dynamic_parts.push(section_proactive_mode().to_string());
    }

    // Coordinator mode (worker orchestration)
    if dynamic.coordinator_mode {
        dynamic_parts.push(section_coordinator().to_string());
    }

    // Best-practice guidance sections
    if dynamic.include_editing_guidance {
        dynamic_parts.push(section_file_editing().to_string());
    }
    if dynamic.include_git_guidance {
        dynamic_parts.push(section_git_guidance().to_string());
    }
    if dynamic.include_testing_guidance {
        dynamic_parts.push(section_testing_guidance().to_string());
    }
    if dynamic.include_debugging_guidance {
        dynamic_parts.push(section_debugging_guidance().to_string());
    }

    // Summarize tool results reminder
    dynamic_parts.push(format!("\n{}", SUMMARIZE_TOOL_RESULTS));

    let text = format!(
        "{}\n{}\n{}",
        static_text,
        SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
        dynamic_parts.join("\n")
    );

    SystemPrompt {
        text,
        dynamic_boundary_offset,
    }
}

/// Build an effective system prompt respecting overrides and agent definitions.
///
/// Priority order (first wins):
/// 1. `override_prompt` — replaces everything (loop mode)
/// 2. `coordinator_prompt` — replaces default (coordinator mode)
/// 3. `agent_prompt` — replaces default (sub-agent mode)
/// 4. `custom_prompt` — replaces default (--system-prompt flag)
/// 5. Default — built from sections
///
/// `append_prompt` is always added at the end (unless override is set).
#[allow(clippy::too_many_arguments)]
pub fn build_effective_system_prompt(
    cwd: &Path,
    model: &str,
    enabled_tools: &[String],
    claude_md_content: &str,
    memory_content: &str,
    custom_prompt: Option<&str>,
    append_prompt: Option<&str>,
    override_prompt: Option<&str>,
    coordinator_prompt: Option<&str>,
    agent_prompt: Option<&str>,
) -> String {
    // Priority 0: Override replaces everything
    if let Some(ovr) = override_prompt {
        return ovr.to_string();
    }

    let base = if let Some(coord) = coordinator_prompt {
        coord.to_string()
    } else if let Some(agent) = agent_prompt {
        agent.to_string()
    } else if let Some(custom) = custom_prompt {
        custom.to_string()
    } else {
        build_system_prompt(cwd, model, enabled_tools, claude_md_content, memory_content).text
    };

    match append_prompt {
        Some(append) => format!("{}\n\n{}", base, append),
        None => base,
    }
}

// ── Coordinator prompt ──────────────────────────────────────────────────────

/// Build the coordinator-mode system prompt.
pub fn coordinator_system_prompt() -> String {
    format!(
        r#"You are Clawed Code, an AI assistant that orchestrates software engineering tasks across multiple workers.

## Role

You are a **coordinator** that breaks down complex tasks and delegates them to specialized worker agents.
You do NOT write code directly — you plan, decompose, and coordinate.

## Available Tools

- **Agent** — Spawn worker agents (explore, plan, general-purpose, verification)
- **SendMessage** — Send follow-up messages to running agents
- **TaskStop** — Cancel a running agent
- **TodoWrite/TodoRead** — Track task progress

## Workflow

1. **Research Phase**: Use explore agents to understand the codebase
2. **Planning Phase**: Break the task into independent, parallelizable subtasks
3. **Implementation Phase**: Spawn general-purpose agents for each subtask
4. **Verification Phase**: Review results, run tests, fix issues

## Concurrency Rules

- Launch independent agents in **parallel** (don't wait for one to finish before starting another)
- Each agent should be **self-contained** with enough context to work independently
- Minimize context overlap between agents to reduce redundant work
- Maximum 8 concurrent agents; queue additional work until a slot opens
- Monitor progress via task notifications; re-launch failed agents with adjusted prompts

## Worker Prompts

When spawning workers, provide:
- Clear, specific task description
- Relevant file paths and context
- Expected deliverables
- Constraints and conventions to follow

## Scratchpad

Use TodoWrite to maintain a shared scratchpad:
- Track which subtasks are in progress, completed, or blocked
- Record intermediate findings from explore agents
- Note decisions and rationale for implementation choices

## Error Handling

- If a worker fails, read its error output and determine if retrying with different parameters would help
- If a task is blocked, use SendMessage to provide additional context to the worker
- If multiple workers conflict (e.g., editing the same file), serialize those tasks

## Result Assembly

After all workers complete:
- Verify the combined output is consistent
- Run a verification agent to test the integrated result
- Report a concise summary to the user

{}"#,
        DEFAULT_PREFIX
    )
}

// ── Default agent prompt ────────────────────────────────────────────────────

/// Default system prompt for sub-agents (explore, general-purpose, etc.).
pub const DEFAULT_AGENT_PROMPT: &str = "\
You are an agent for Clawed Code, a Rust-based open-source AI coding assistant. \
Given the user's message, you should use the tools available to complete the task. \
Complete the task fully — don't gold-plate, but don't leave it half-done. \
When you complete the task, respond with a concise report covering what was done \
and any key findings — the caller will relay this to the user, \
so it only needs the essentials.";

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_system_prompt_contains_sections() {
        let cwd = PathBuf::from(".");
        let tools = vec!["FileReadTool".to_string(), "BashTool".to_string()];
        let prompt = build_system_prompt(&cwd, "claude-sonnet-4-20250514", &tools, "", "");

        assert!(prompt.text.contains(DEFAULT_PREFIX));
        assert!(prompt.text.contains("# System"));
        assert!(prompt.text.contains("# Doing tasks"));
        assert!(prompt.text.contains("# Executing actions"));
        assert!(prompt.text.contains("# Using tools"));
        assert!(prompt.text.contains("# Tone and style"));
        assert!(prompt.text.contains("# Output efficiency"));
        assert!(prompt.text.contains("Environment"));
        assert!(prompt.text.contains(SUMMARIZE_TOOL_RESULTS));
        assert!(prompt.dynamic_boundary_offset > 0);
        assert!(prompt.dynamic_boundary_offset < prompt.text.len());
    }

    #[test]
    fn test_dynamic_boundary_split() {
        let cwd = PathBuf::from(".");
        let prompt = build_system_prompt(&cwd, "claude-sonnet-4-20250514", &[], "", "");

        let prefix = prompt.static_prefix();
        let suffix = prompt.dynamic_suffix();

        assert!(prefix.contains(DEFAULT_PREFIX));
        assert!(prefix.contains("Tone and style"));
        assert!(!suffix.starts_with(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
        assert!(suffix.contains("Environment"));
    }

    #[test]
    fn test_claude_md_injection() {
        let cwd = PathBuf::from(".");
        let prompt = build_system_prompt(
            &cwd,
            "claude-sonnet-4-20250514",
            &[],
            "Always use tabs for indentation.",
            "",
        );

        assert!(prompt.text.contains("Project Instructions (CLAUDE.md)"));
        assert!(prompt.text.contains("Always use tabs for indentation."));
    }

    #[test]
    fn test_memory_injection() {
        let cwd = PathBuf::from(".");
        let prompt = build_system_prompt(
            &cwd,
            "claude-sonnet-4-20250514",
            &[],
            "",
            "Remember: user prefers Python 3.12",
        );

        assert!(prompt.text.contains("Memory Contents"));
        assert!(prompt.text.contains("Remember: user prefers Python 3.12"));
    }

    #[test]
    fn test_effective_prompt_override() {
        let result = build_effective_system_prompt(
            Path::new("."),
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            Some("custom"),
            Some("append"),
            Some("OVERRIDE"),
            None,
            None,
        );
        assert_eq!(result, "OVERRIDE");
    }

    #[test]
    fn test_effective_prompt_custom_with_append() {
        let result = build_effective_system_prompt(
            Path::new("."),
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            Some("my custom prompt"),
            Some("extra instructions"),
            None,
            None,
            None,
        );
        assert!(result.contains("my custom prompt"));
        assert!(result.contains("extra instructions"));
    }

    #[test]
    fn test_effective_prompt_priority_order() {
        let result = build_effective_system_prompt(
            Path::new("."),
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            Some("custom"),
            None,
            None,
            Some("coordinator"),
            None,
        );
        assert_eq!(result, "coordinator");

        let result = build_effective_system_prompt(
            Path::new("."),
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            Some("custom"),
            None,
            None,
            None,
            Some("agent"),
        );
        assert_eq!(result, "agent");
    }

    #[test]
    fn test_memory_behavioral_prompt_with_dir() {
        let cwd = PathBuf::from(".");
        let dynamic = DynamicSections {
            memory_dir: Some("/home/user/.claude/memory"),
            ..Default::default()
        };
        let prompt = build_system_prompt_ext(
            &cwd,
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            &dynamic,
        );
        // Behavioral prompt is injected even without memory content
        assert!(prompt.text.contains("# Auto Memory"));
        assert!(prompt.text.contains("Types of memory"));
        assert!(prompt.text.contains("<name>user</name>"));
        assert!(prompt.text.contains("<name>feedback</name>"));
        assert!(prompt.text.contains("<name>project</name>"));
        assert!(prompt.text.contains("<name>reference</name>"));
        assert!(prompt.text.contains("What NOT to save"));
        assert!(prompt.text.contains("How to save memories"));
        assert!(prompt.text.contains("frontmatter"));
        assert!(prompt.text.contains("Before recommending from memory"));
        assert!(prompt.text.contains("/home/user/.claude/memory"));
    }

    #[test]
    fn test_memory_behavioral_not_injected_without_dir() {
        let cwd = PathBuf::from(".");
        let dynamic = DynamicSections {
            memory_dir: None,
            ..Default::default()
        };
        let prompt = build_system_prompt_ext(
            &cwd,
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            &dynamic,
        );
        assert!(!prompt.text.contains("# Auto Memory"));
    }

    #[test]
    fn test_guidance_defaults_are_true() {
        let d = DynamicSections::default();
        assert!(d.include_editing_guidance);
        assert!(d.include_git_guidance);
        assert!(d.include_testing_guidance);
        assert!(d.include_debugging_guidance);
    }

    #[test]
    fn test_guidance_sections_included_by_default() {
        let cwd = PathBuf::from(".");
        let prompt = build_system_prompt(&cwd, "claude-sonnet-4-20250514", &[], "", "");
        assert!(prompt.text.contains("File editing best practices"));
        assert!(prompt.text.contains("Git operations"));
        assert!(prompt.text.contains("Testing"));
        assert!(prompt.text.contains("Debugging"));
        // Coordinator NOT included by default
        assert!(!prompt.text.contains("Coordinator Mode"));
    }

    #[test]
    fn test_coordinator_mode_section() {
        let cwd = PathBuf::from(".");
        let dynamic = DynamicSections {
            coordinator_mode: true,
            ..Default::default()
        };
        let prompt = build_system_prompt_ext(
            &cwd,
            "claude-sonnet-4-20250514",
            &[],
            "",
            "",
            &dynamic,
        );
        assert!(prompt.text.contains("Coordinator Mode"));
        assert!(prompt.text.contains("task-notification"));
        assert!(prompt.text.contains("SendMessage"));
    }
}
