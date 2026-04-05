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

use std::path::Path;

/// Marker that separates globally-cacheable prefix from session-specific suffix.
/// The API prompt-caching layer uses this to apply different cache scopes.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

/// Identity prefix for the default interactive CLI mode.
const DEFAULT_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

// ── Section definitions ─────────────────────────────────────────────────────

/// Static: system guidelines on tool execution, permissions, tags.
fn section_system_guidelines() -> &'static str {
    r#"
## System Guidelines

- You are an interactive CLI agent specializing in software engineering tasks.
- You have tools to read/write files, execute commands, search code, and interact with the user.
- Tools are executed in a user-selected permission mode. When you attempt to call a tool that is not automatically allowed by the user's permission mode, the user will be prompted so that they can approve or deny the execution. If the user denies a tool call, do not re-attempt the exact same tool call.
- IMPORTANT: When a tool result includes `<feedback>` tags, the contents are direct instructions from the user that you must follow.
- When referring to content that appeared earlier in the conversation, reference it directly rather than re-reading the file/resource.
- When output of a tool call is truncated, do not retry with modified arguments to get the full output unless the truncated content is clearly insufficient."#
}

/// Static: coding task guidelines.
fn section_doing_tasks() -> &'static str {
    r#"
## Doing Tasks

- ALWAYS look at existing code conventions and follow them. Match the style of the codebase.
- NEVER assume a library or module is available unless you can verify it in the codebase.
- When modifying code, ALWAYS consider the impact on other parts of the codebase.
- NEVER generate extremely long hashes, binary content, or other non-human-readable content.
- When creating or editing files, ensure they end with a newline.
- Prefer making focused, surgical changes over large-scale refactors.
- When fixing a bug, look at the actual error/test output carefully before writing code.
- Run existing tests or builds to verify changes when available — do not create new testing infrastructure unless asked.
- Don't gold-plate — implement what's needed, not more.
- When writing tests, verify they actually pass rather than assuming."#
}

/// Static: when to ask for confirmation.
fn section_actions() -> &'static str {
    r#"
## Actions Requiring Confirmation

ALWAYS ask the user for confirmation before performing these actions:
- **Destructive operations**: deleting files/directories, dropping databases, rm -rf
- **Hard-to-reverse changes**: force-push, git reset --hard, overwriting without backup
- **External visibility**: pushing to remote, posting PR comments, sending messages
- **Large-scale changes**: modifying more than 5 files, changing public APIs
- **External uploads**: sending content to pastebins, external services, diagram renderers"#
}

/// Static: tool usage best practices.
fn section_using_tools() -> &'static str {
    r#"
## Using Your Tools

- **Prefer specialized tools over shell commands**: Use FileReadTool instead of `cat`, GrepTool instead of `grep`, GlobTool instead of `find`, FileEditTool instead of `sed`.
- **Parallel tool calls**: When multiple independent operations are needed, make them in a single response to maximize efficiency.
- **Search order**: Prefer semantic search > glob patterns > grep for finding code.
- **File edits**: Use FileEditTool for surgical edits (replace one occurrence). Use FileWriteTool only for new files or complete rewrites.
- **Batch operations**: Chain related shell commands with `&&` instead of separate tool calls.
- **Read before edit**: Always read a file before editing to understand context and verify assumptions."#
}

/// Static: tone and style guidelines.
fn section_tone_style() -> &'static str {
    r#"
## Tone & Style

- Be concise. Lead with the answer or action, then explain if needed.
- Do not use emojis or exclamation points in prose.
- When referring to code, use backtick-quoted identifiers: `functionName`, `ClassName`.
- When referring to files, use the path: `src/utils/foo.rs`.
- When referring to issues/PRs, use `owner/repo#123` format.
- Focus on decisions, blockers, and findings rather than narrating your process.
- Avoid repeating information the user already provided."#
}

/// Dynamic: environment information (CWD, platform, git status, model).
fn section_environment(cwd: &Path, model: &str) -> String {
    let platform = std::env::consts::OS;
    let shell = if cfg!(windows) { "PowerShell" } else { "bash" };
    let is_git = cwd.join(".git").exists()
        || std::process::Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let model_desc = canonical_model_description(model);
    let cutoff = knowledge_cutoff(model);

    let mut env = format!(
        r#"
## Environment

- Working directory: {}
- Platform: {}
- Shell: {}
- Is git repository: {}"#,
        cwd.display(),
        platform,
        shell,
        is_git,
    );

    if !model_desc.is_empty() {
        env.push_str(&format!("\n- Model: {}", model_desc));
    }
    if !cutoff.is_empty() {
        env.push_str(&format!("\n- Knowledge cutoff: {}", cutoff));
    }

    env
}

/// Dynamic: tool-specific guidance based on which tools are enabled.
fn section_tool_guidance(enabled_tools: &[String]) -> String {
    let mut guidance = String::from("\n## Tool-Specific Guidance\n");
    let has = |name: &str| enabled_tools.iter().any(|t| t.eq_ignore_ascii_case(name));

    if has("DispatchAgent") {
        guidance.push_str(
            "\n- **Agent tool**: Use for complex, independent tasks that benefit from \
             a separate context. Explore agents are for research; use general-purpose \
             agents for implementation tasks.",
        );
    }

    if has("SkillTool") || has("Skill") {
        guidance.push_str(
            "\n- **Skills**: Check available skills before starting unfamiliar tasks. \
             Skills provide domain-specific workflows.",
        );
    }

    if has("AskUser") || has("AskUserQuestion") {
        guidance.push_str(
            "\n- **AskUser**: When you are uncertain about requirements, scope, or \
             approach, use AskUserQuestion to clarify rather than guessing. \
             If the user denies a tool call you don't understand, ask them why.",
        );
    }

    if has("TodoWrite") || has("TodoRead") {
        guidance.push_str(
            "\n- **Todos**: Use TodoWrite/TodoRead to track complex multi-step tasks. \
             Break work into small, actionable items.",
        );
    }

    if has("WebSearch") || has("WebSearchTool") {
        guidance.push_str(
            "\n- **Web search**: Use for current events, recent API docs, or information \
             likely to have changed since your knowledge cutoff.",
        );
    }

    guidance
}

// ── Builder ─────────────────────────────────────────────────────────────────

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
    let mut parts: Vec<String> = Vec::new();

    // ── Static prefix (globally cacheable) ───────────────────────────────
    parts.push(DEFAULT_PREFIX.to_string());
    parts.push(section_system_guidelines().to_string());
    parts.push(section_doing_tasks().to_string());
    parts.push(section_actions().to_string());
    parts.push(section_using_tools().to_string());
    parts.push(section_tone_style().to_string());

    let static_text = parts.join("\n");
    // Offset = static text + newline + boundary marker + newline
    let dynamic_boundary_offset = static_text.len() + 1 + SYSTEM_PROMPT_DYNAMIC_BOUNDARY.len() + 1;

    // ── Dynamic suffix (per-session) ─────────────────────────────────────
    let mut dynamic_parts: Vec<String> = Vec::new();

    // Environment
    dynamic_parts.push(section_environment(cwd, model));

    // Tool guidance
    if !enabled_tools.is_empty() {
        dynamic_parts.push(section_tool_guidance(enabled_tools));
    }

    // Memory files
    if !memory_content.is_empty() {
        dynamic_parts.push(format!(
            "\n## Agent Memory\n\n<memory>\n{}\n</memory>",
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
        // Priority 1: Coordinator mode
        coord.to_string()
    } else if let Some(agent) = agent_prompt {
        // Priority 2: Agent-specific prompt
        agent.to_string()
    } else if let Some(custom) = custom_prompt {
        // Priority 3: Custom --system-prompt
        custom.to_string()
    } else {
        // Priority 4: Default assembled prompt
        build_system_prompt(cwd, model, enabled_tools, claude_md_content, memory_content).text
    };

    // Append prompt (always added unless override was used)
    match append_prompt {
        Some(append) => format!("{}\n\n{}", base, append),
        None => base,
    }
}

// ── Coordinator prompt ──────────────────────────────────────────────────────

/// Build the coordinator-mode system prompt.
pub fn coordinator_system_prompt() -> String {
    format!(
        r#"You are Claude Code, an AI assistant that orchestrates software engineering tasks across multiple workers.

## Role

You are a **coordinator** that breaks down complex tasks and delegates them to specialized worker agents.
You do NOT write code directly — you plan, decompose, and coordinate.

## Available Tools

- **DispatchAgent** — Spawn worker agents (explore, plan, general-purpose, verification)
- **SendMessage** — Send follow-up messages to running agents
- **TaskStop** — Cancel a running agent
- **TodoWrite/TodoRead** — Track task progress

## Workflow

1. **Research Phase**: Use explore agents to understand the codebase
2. **Planning Phase**: Break the task into independent, parallelizable subtasks
3. **Implementation Phase**: Spawn general-purpose agents for each subtask
4. **Verification Phase**: Review results, run tests, fix issues

## Concurrency

- Launch independent agents in parallel (don't wait for one to finish before starting another)
- Each agent should be self-contained with enough context to work independently
- Minimize context overlap between agents to reduce redundant work

## Worker Prompts

When spawning workers, provide:
- Clear, specific task description
- Relevant file paths and context
- Expected deliverables
- Constraints and conventions to follow

{DEFAULT_PREFIX}"#
    )
}

// ── Default agent prompt ────────────────────────────────────────────────────

/// Default system prompt for sub-agents (explore, general-purpose, etc.).
pub const DEFAULT_AGENT_PROMPT: &str = "\
You are an agent for Claude Code, Anthropic's official CLI for Claude. \
Given the user's message, you should use the tools available to complete the task. \
Complete the task fully — don't gold-plate, but don't leave it half-done. \
When you complete the task, respond with a concise report covering what was done \
and any key findings — the caller will relay this to the user, \
so it only needs the essentials.";

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Map a model ID to a human-readable description.
fn canonical_model_description(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("opus-4-6") || m.contains("opus-4.6") {
        "Claude Opus 4.6"
    } else if m.contains("opus-4-5") || m.contains("opus-4.5") {
        "Claude Opus 4.5"
    } else if m.contains("opus-4") || m.contains("opus4") {
        "Claude Opus 4"
    } else if m.contains("sonnet-4-6") || m.contains("sonnet-4.6") {
        "Claude Sonnet 4.6"
    } else if m.contains("sonnet-4-5") || m.contains("sonnet-4.5") {
        "Claude Sonnet 4.5"
    } else if m.contains("sonnet-4") || m.contains("sonnet4") {
        "Claude Sonnet 4"
    } else if m.contains("haiku-4-5") || m.contains("haiku-4.5") {
        "Claude Haiku 4.5"
    } else if m.contains("haiku-3-5") || m.contains("haiku-3.5") {
        "Claude Haiku 3.5"
    } else {
        ""
    }
}

/// Knowledge cutoff date by model family.
fn knowledge_cutoff(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("sonnet-4-6") || m.contains("sonnet-4.6") {
        "August 2025"
    } else if m.contains("opus-4-6") || m.contains("opus-4.6")
        || m.contains("opus-4-5") || m.contains("opus-4.5")
    {
        "May 2025"
    } else if m.contains("haiku-4") {
        "February 2025"
    } else if m.contains("opus-4") || m.contains("sonnet-4") {
        "January 2025"
    } else {
        ""
    }
}

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
        assert!(prompt.text.contains("System Guidelines"));
        assert!(prompt.text.contains("Doing Tasks"));
        assert!(prompt.text.contains("Actions Requiring Confirmation"));
        assert!(prompt.text.contains("Using Your Tools"));
        assert!(prompt.text.contains("Tone & Style"));
        assert!(prompt.text.contains("Environment"));
        assert!(prompt.dynamic_boundary_offset > 0);
        assert!(prompt.dynamic_boundary_offset < prompt.text.len());
    }

    #[test]
    fn test_dynamic_boundary_split() {
        let cwd = PathBuf::from(".");
        let prompt = build_system_prompt(&cwd, "claude-sonnet-4-20250514", &[], "", "");

        let prefix = prompt.static_prefix();
        let suffix = prompt.dynamic_suffix();

        // Static prefix should contain identity and guidelines
        assert!(prefix.contains(DEFAULT_PREFIX));
        assert!(prefix.contains("Tone & Style"));
        // Boundary marker should NOT appear in either part's visible content
        assert!(!suffix.starts_with(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));

        // Dynamic suffix should contain environment info
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

        assert!(prompt.text.contains("Agent Memory"));
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
        // Override replaces everything, including append
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
        // Coordinator takes priority over custom
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

        // Agent takes priority over custom but not coordinator
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
    fn test_knowledge_cutoff() {
        assert_eq!(knowledge_cutoff("claude-sonnet-4-6-20250820"), "August 2025");
        assert_eq!(knowledge_cutoff("claude-opus-4-5-20250515"), "May 2025");
        assert_eq!(knowledge_cutoff("claude-haiku-4-5-20250210"), "February 2025");
        assert_eq!(knowledge_cutoff("claude-sonnet-4-20250514"), "January 2025");
        assert_eq!(knowledge_cutoff("some-unknown-model"), "");
    }

    #[test]
    fn test_canonical_model_description() {
        assert_eq!(canonical_model_description("claude-sonnet-4-20250514"), "Claude Sonnet 4");
        assert_eq!(canonical_model_description("claude-opus-4-5-20250515"), "Claude Opus 4.5");
        assert_eq!(canonical_model_description("claude-haiku-4-5-20250210"), "Claude Haiku 4.5");
        assert_eq!(canonical_model_description("unknown"), "");
    }

    #[test]
    fn test_tool_guidance_generation() {
        let tools = vec![
            "DispatchAgent".to_string(),
            "AskUser".to_string(),
            "TodoWrite".to_string(),
        ];
        let guidance = section_tool_guidance(&tools);

        assert!(guidance.contains("Agent tool"));
        assert!(guidance.contains("AskUser"));
        assert!(guidance.contains("Todos"));
        assert!(!guidance.contains("Web search")); // not enabled
    }
}
