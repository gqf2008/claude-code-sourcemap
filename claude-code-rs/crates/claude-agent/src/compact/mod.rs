//! Session compaction — mirrors claude-code's `services/compact/compact.ts`.
//!
//! When conversation history grows large (past the token threshold), we call
//! Claude with a structured prompt that produces an `<analysis>` scratchpad
//! plus a `<summary>` block.  The analysis is stripped; the summary replaces
//! the old messages, giving a fresh context window while preserving intent.
//!
//! ## Submodules
//!
//! - [`micro`] — lightweight message trimming without calling Claude
//! - [`memory`] — extract and persist reusable facts from compacted summaries
//!
//! ## Usage
//!
//! ```rust,ignore
//! let summary = compact_conversation(&client, &messages, model, max_tokens, None).await?;
//! ```

pub mod micro;
pub mod memory;

// Re-export commonly used items from submodules
pub use micro::{
    clear_old_tool_results, truncate_large_tool_results, snip_old_messages,
    TOOL_RESULT_CLEARED, MAX_TOOL_RESULT_CHARS,
};
pub use memory::{
    ExtractedMemory, build_memory_extraction_prompt, parse_extracted_memories,
    save_extracted_memories,
};

use claude_api::client::ApiClient;
use claude_api::types::{ApiContentBlock, ApiMessage, MessagesRequest, SystemBlock};
use claude_core::message::{Message, ToolResultContent};

// ── Token threshold ──────────────────────────────────────────────────────────

/// Legacy constant — prefer `get_auto_compact_threshold(context_window)`.
pub const AUTO_COMPACT_THRESHOLD: u64 = 80_000;

/// Calculate the auto-compact threshold dynamically from the model's context window.
/// Matches TS: `effectiveContextWindow - AUTOCOMPACT_BUFFER_TOKENS`.
pub fn get_auto_compact_threshold(context_window: u64) -> u64 {
    let effective = context_window.saturating_sub(20_000); // reserve for output
    effective.saturating_sub(AUTOCOMPACT_BUFFER_TOKENS)
}

// ── Prompt ───────────────────────────────────────────────────────────────────

const NO_TOOLS_PREAMBLE: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\n\
    - Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.\n\
    - You already have all the context you need in the conversation above.\n\
    - Tool calls will be REJECTED and will waste your only turn — you will fail the task.\n\
    - Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\n";

const COMPACT_PROMPT: &str = "Your task is to create a detailed summary of the conversation so far, \
paying close attention to the user's explicit requests and your previous actions.\n\
This summary should be thorough in capturing technical details, code patterns, and architectural \
decisions that would be essential for continuing development work without losing context.\n\n\
Before providing your final summary, wrap your analysis in <analysis> tags to organize your \
thoughts and ensure you've covered all necessary points.\n\n\
Your summary should include the following sections:\n\n\
1. Primary Request and Intent: Capture all of the user's explicit requests and intents in detail\n\
2. Key Technical Concepts: List all important technical concepts, technologies, and frameworks discussed.\n\
3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or created. \
   Include full code snippets where applicable.\n\
4. Errors and fixes: List all errors encountered and how you fixed them. Include user feedback.\n\
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.\n\
6. All user messages: List ALL user messages that are not tool results.\n\
7. Pending Tasks: Outline any pending tasks explicitly requested.\n\
8. Current Work: Describe precisely what was being worked on immediately before this summary.\n\
9. Optional Next Step: The next step directly in line with the most recent work. Include verbatim quotes.\n\n\
Structure your response as:\n\
<analysis>\n\
[Your analysis]\n\
</analysis>\n\n\
<summary>\n\
[Your structured summary]\n\
</summary>\n\n\
REMINDER: Do NOT call any tools. Respond with plain text only.";

// ── Summary formatting ────────────────────────────────────────────────────────

/// Strip the `<analysis>` scratchpad and unwrap `<summary>` tags.
pub fn format_compact_summary(raw: &str) -> String {
    // Remove <analysis>...</analysis>
    let without_analysis = if let (Some(start), Some(end)) = (
        raw.find("<analysis>"),
        raw.find("</analysis>"),
    ) {
        let before = &raw[..start];
        let after = &raw[end + "</analysis>".len()..];
        format!("{}{}", before, after)
    } else {
        raw.to_string()
    };

    // Extract <summary>...</summary> content
    let result = if let (Some(start), Some(end)) = (
        without_analysis.find("<summary>"),
        without_analysis.find("</summary>"),
    ) {
        let content = &without_analysis[start + "<summary>".len()..end];
        format!("Summary:\n{}", content.trim())
    } else {
        without_analysis
    };

    // Collapse excessive blank lines
    claude_core::text_util::collapse_blank_lines(&result)
}

// ── Message serialisation for compact call ───────────────────────────────────

/// Convert our internal messages to API messages, stripping images.
fn messages_for_compact(messages: &[Message]) -> Vec<ApiMessage> {
    messages
        .iter()
        .filter_map(|msg| match msg {
            Message::User(u) => {
                let content: Vec<ApiContentBlock> = u
                    .content
                    .iter()
                    .map(|b| match b {
                        claude_core::message::ContentBlock::Text { text } => {
                            ApiContentBlock::Text { text: text.clone(), cache_control: None }
                        }
                        claude_core::message::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => ApiContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: content
                                .iter()
                                .map(|c| match c {
                                    ToolResultContent::Text { text } => {
                                        claude_api::types::ToolResultContent::Text {
                                            text: text.clone(),
                                        }
                                    }
                                    ToolResultContent::Image { .. } => {
                                        claude_api::types::ToolResultContent::Text {
                                            text: "[image]".to_string(),
                                        }
                                    }
                                })
                                .collect(),
                            is_error: *is_error,
                            cache_control: None,
                        },
                        _ => ApiContentBlock::Text {
                            text: "[content block]".to_string(),
                            cache_control: None,
                        },
                    })
                    .collect();
                Some(ApiMessage {
                    role: "user".into(),
                    content,
                })
            }
            Message::Assistant(a) => {
                let content: Vec<ApiContentBlock> = a
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        claude_core::message::ContentBlock::Text { text } => {
                            Some(ApiContentBlock::Text { text: text.clone(), cache_control: None })
                        }
                        claude_core::message::ContentBlock::ToolUse { id, name, input } => {
                            Some(ApiContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            })
                        }
                        claude_core::message::ContentBlock::Thinking { thinking } => {
                            Some(ApiContentBlock::Text {
                                text: format!("<thinking>{}</thinking>", thinking),
                                cache_control: None,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                if content.is_empty() {
                    None
                } else {
                    Some(ApiMessage {
                        role: "assistant".into(),
                        content,
                    })
                }
            }
            Message::System(_) => None,
        })
        .collect()
}

// ── Public compaction API ────────────────────────────────────────────────────

/// Compact a conversation history into a structured summary.
///
/// Returns the formatted summary string.  The caller is responsible for
/// replacing the old `messages` slice with a compact boundary + this summary.
pub async fn compact_conversation(
    client: &ApiClient,
    messages: &[Message],
    model: &str,
    custom_instructions: Option<&str>,
) -> anyhow::Result<String> {
    let api_messages = messages_for_compact(messages);

    if api_messages.is_empty() {
        anyhow::bail!("No messages to compact");
    }

    // Build the compact prompt
    let mut compact_prompt = format!("{}{}", NO_TOOLS_PREAMBLE, COMPACT_PROMPT);
    if let Some(instructions) = custom_instructions {
        if !instructions.trim().is_empty() {
            compact_prompt.push_str(&format!("\n\nAdditional Instructions:\n{}", instructions));
        }
    }

    let system = vec![SystemBlock {
        block_type: "text".into(),
        text: compact_prompt,
        cache_control: None,
    }];

    let request = MessagesRequest {
        model: model.to_string(),
        max_tokens: 8192,
        messages: api_messages,
        system: Some(system),
        tools: None,
        stream: false,
        stop_sequences: None,
        temperature: None,
        top_p: None,
        thinking: None,
    };

    let response = client.messages(&request).await
        .map_err(|e| anyhow::anyhow!("Compact API call failed: {}", e))?;

    // Extract text from response
    let raw_text: String = response
        .content
        .iter()
        .filter_map(|b| {
            if let claude_api::types::ResponseContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    if raw_text.is_empty() {
        anyhow::bail!("Compact response was empty");
    }

    let summary = format_compact_summary(&raw_text);

    // Validate that we actually got a meaningful summary.
    if !raw_text.contains("<summary>") || !raw_text.contains("</summary>") {
        tracing::warn!("Compaction response missing <summary> tags — may be unreliable");
    }
    if summary.trim().is_empty() || summary.len() < 30 {
        anyhow::bail!("Compaction produced an empty or too-short summary — keeping original messages");
    }

    Ok(summary)
}

/// Build the system message text that replaces old conversation history.
pub fn compact_context_message(summary: &str, transcript_note: Option<&str>) -> String {
    let mut msg = format!(
        "This session is being continued from a previous conversation that ran out of context.\n\
        The summary below covers the earlier portion of the conversation.\n\n{}",
        summary
    );
    if let Some(note) = transcript_note {
        msg.push_str(&format!("\n\n{}", note));
    }
    msg.push_str("\n\nContinue the conversation from where it left off without asking \
        the user any further questions. Resume directly — do not acknowledge the summary, \
        do not recap what was happening. Pick up the last task as if the break never happened.");
    msg
}

// ── Tool Use Summary ─────────────────────────────────────────────────────────

/// Generate a concise summary of tool uses in a message sequence.
/// This is used to condense long tool use chains during compaction.
pub fn summarize_tool_uses(messages: &[Message]) -> String {
    use std::collections::HashMap;
    let mut tool_counts: HashMap<String, u32> = HashMap::new();
    let mut files_modified: Vec<String> = Vec::new();
    let mut files_read: Vec<String> = Vec::new();

    for msg in messages {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let claude_core::message::ContentBlock::ToolUse { name, input, .. } = block {
                    *tool_counts.entry(name.clone()).or_insert(0) += 1;

                    // Track files
                    if let Some(path) = input["file_path"].as_str() {
                        match name.as_str() {
                            "Read" => {
                                if !files_read.contains(&path.to_string()) {
                                    files_read.push(path.to_string());
                                }
                            }
                            "Edit" | "Write" | "MultiEdit" => {
                                if !files_modified.contains(&path.to_string()) {
                                    files_modified.push(path.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    if tool_counts.is_empty() {
        return String::new();
    }

    let mut summary = String::from("Tool usage summary:\n");

    // Sort by count descending
    let mut sorted: Vec<_> = tool_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    for (tool, count) in &sorted {
        summary.push_str(&format!("  {} — {} call(s)\n", tool, count));
    }

    if !files_modified.is_empty() {
        summary.push_str(&format!(
            "Files modified: {}\n",
            files_modified.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
        ));
        if files_modified.len() > 10 {
            summary.push_str(&format!("  ... and {} more\n", files_modified.len() - 10));
        }
    }

    if !files_read.is_empty() {
        summary.push_str(&format!(
            "Files read: {}\n",
            files_read.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
        ));
        if files_read.len() > 10 {
            summary.push_str(&format!("  ... and {} more\n", files_read.len() - 10));
        }
    }

    summary
}

// ── Post-Compact Cleanup ─────────────────────────────────────────────────────

/// Remove duplicate or redundant content from post-compact messages.
/// This cleans up memory injections and context that got duplicated.
pub fn post_compact_cleanup(messages: &mut Vec<Message>) {
    // Remove consecutive duplicate system messages
    let mut i = 0;
    while i + 1 < messages.len() {
        let is_dup = match (&messages[i], &messages[i + 1]) {
            (Message::System(a), Message::System(b)) => a.message == b.message,
            _ => false,
        };
        if is_dup {
            messages.remove(i + 1);
        } else {
            i += 1;
        }
    }

    // Trim empty assistant messages (can happen after compaction)
    messages.retain(|msg| {
        if let Message::Assistant(a) = msg {
            !a.content.is_empty()
        } else {
            true
        }
    });
}

// ── Token Warning State ──────────────────────────────────────────────────────

/// Calculate token usage warning level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenWarningState {
    /// Under 50% of threshold — normal
    Normal,
    /// 50-75% — approaching limit
    Warning,
    /// 75-90% — nearly full
    Critical,
    /// Over 90% — auto-compact imminent
    Imminent,
}

pub fn calculate_token_warning(current_tokens: u64, threshold: u64) -> TokenWarningState {
    if threshold == 0 { return TokenWarningState::Normal; }
    let ratio = current_tokens as f64 / threshold as f64;
    if ratio >= 0.9 { TokenWarningState::Imminent }
    else if ratio >= 0.75 { TokenWarningState::Critical }
    else if ratio >= 0.5 { TokenWarningState::Warning }
    else { TokenWarningState::Normal }
}

// ── Auto-compact trigger ────────────────────────────────────────────────────

/// Buffer tokens between auto-compact threshold and context window.
const AUTOCOMPACT_BUFFER_TOKENS: u64 = 13_000;

/// Maximum consecutive auto-compact failures before circuit-breaker trips.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// State for auto-compact trigger logic.
pub struct AutoCompactState {
    /// How many compactions have failed in a row.
    consecutive_failures: u32,
    /// Disable flag (can be set by user or env var).
    pub disabled: bool,
    /// Last compaction summary message id (for dedup).
    pub last_summary_id: Option<String>,
}

impl AutoCompactState {
    pub fn new() -> Self {
        Self {
            consecutive_failures: 0,
            disabled: false,
            last_summary_id: None,
        }
    }

    /// Should we trigger auto-compact given the current token count and model's context window?
    pub fn should_auto_compact(&self, current_tokens: u64, context_window: u64) -> bool {
        if self.disabled { return false; }
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES { return false; }
        if context_window == 0 { return false; }

        // Effective window = context - reserved output tokens (20k)
        let effective = context_window.saturating_sub(20_000);
        let threshold = effective.saturating_sub(AUTOCOMPACT_BUFFER_TOKENS);
        current_tokens >= threshold
    }

    /// Call after a successful compaction.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Call after a failed compaction attempt.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
    }

    /// Whether the circuit breaker has tripped.
    pub fn is_circuit_broken(&self) -> bool {
        self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES
    }
}

impl Default for AutoCompactState {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::message::{
        AssistantMessage, ContentBlock, SystemMessage,
    };

    #[test]
    fn test_token_warning_levels() {
        assert_eq!(calculate_token_warning(0, 100_000), TokenWarningState::Normal);
        assert_eq!(calculate_token_warning(40_000, 100_000), TokenWarningState::Normal);
        assert_eq!(calculate_token_warning(55_000, 100_000), TokenWarningState::Warning);
        assert_eq!(calculate_token_warning(80_000, 100_000), TokenWarningState::Critical);
        assert_eq!(calculate_token_warning(95_000, 100_000), TokenWarningState::Imminent);
        assert_eq!(calculate_token_warning(1_000_000, 0), TokenWarningState::Normal);
    }

    #[test]
    fn test_summarize_tool_uses_empty() {
        let messages: Vec<Message> = Vec::new();
        let summary = summarize_tool_uses(&messages);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_summarize_tool_uses_with_tools() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"file_path": "src/main.rs"}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "Edit".into(),
                        input: serde_json::json!({"file_path": "src/lib.rs"}),
                    },
                    ContentBlock::ToolUse {
                        id: "t3".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"file_path": "Cargo.toml"}),
                    },
                ],
                stop_reason: None,
                usage: None,
            }),
        ];
        let summary = summarize_tool_uses(&messages);
        assert!(summary.contains("Read"));
        assert!(summary.contains("Edit"));
        assert!(summary.contains("src/main.rs"));
        assert!(summary.contains("src/lib.rs"));
    }

    #[test]
    fn test_post_compact_cleanup_removes_duplicates() {
        let mut messages = vec![
            Message::System(SystemMessage { uuid: "s1".into(), message: "Hello".into() }),
            Message::System(SystemMessage { uuid: "s2".into(), message: "Hello".into() }),
            Message::System(SystemMessage { uuid: "s3".into(), message: "World".into() }),
        ];
        post_compact_cleanup(&mut messages);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_post_compact_cleanup_removes_empty_assistant() {
        let mut messages = vec![
            Message::System(SystemMessage { uuid: "s1".into(), message: "Ctx".into() }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![],
                stop_reason: None,
                usage: None,
            }),
        ];
        post_compact_cleanup(&mut messages);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_auto_compact_trigger() {
        let state = AutoCompactState::new();
        assert!(!state.should_auto_compact(100_000, 200_000));
        assert!(state.should_auto_compact(170_000, 200_000));
        assert!(state.should_auto_compact(200_000, 200_000));
    }

    #[test]
    fn test_auto_compact_disabled() {
        let mut state = AutoCompactState::new();
        state.disabled = true;
        assert!(!state.should_auto_compact(200_000, 200_000));
    }

    #[test]
    fn test_auto_compact_circuit_breaker() {
        let mut state = AutoCompactState::new();
        assert!(!state.is_circuit_broken());

        state.record_failure();
        state.record_failure();
        assert!(!state.is_circuit_broken());

        state.record_failure();
        assert!(state.is_circuit_broken());
        assert!(!state.should_auto_compact(200_000, 200_000));

        state.record_success();
        assert!(!state.is_circuit_broken());
        assert!(state.should_auto_compact(200_000, 200_000));
    }

    #[test]
    fn test_auto_compact_zero_context() {
        let state = AutoCompactState::new();
        assert!(!state.should_auto_compact(100_000, 0));
    }

    // ── format_compact_summary ──────────────────────────────────────────────

    #[test]
    fn format_summary_strips_analysis_and_unwraps_summary() {
        let raw = "<analysis>thinking...</analysis>\n<summary>Hello world</summary>";
        let result = format_compact_summary(raw);
        assert!(!result.contains("<analysis>"));
        assert!(!result.contains("<summary>"));
        assert!(result.contains("Hello world"));
        assert!(result.starts_with("Summary:"));
    }

    #[test]
    fn format_summary_no_tags() {
        let raw = "Just plain text, no XML tags.";
        let result = format_compact_summary(raw);
        assert_eq!(result, "Just plain text, no XML tags.");
    }

    #[test]
    fn format_summary_analysis_only() {
        let raw = "<analysis>thinking stuff</analysis>\nLeftover text";
        let result = format_compact_summary(raw);
        assert!(!result.contains("thinking stuff"));
        assert!(result.contains("Leftover text"));
    }

    #[test]
    fn format_summary_summary_only() {
        let raw = "<summary>Only summary here</summary>";
        let result = format_compact_summary(raw);
        assert!(result.contains("Only summary here"));
        assert!(result.starts_with("Summary:"));
    }

    // ── compact_context_message ─────────────────────────────────────────────

    #[test]
    fn compact_context_message_basic() {
        let msg = compact_context_message("Task was X.", None);
        assert!(msg.contains("Task was X."));
        assert!(msg.contains("continued from a previous conversation"));
        assert!(msg.contains("Resume directly"));
    }

    #[test]
    fn compact_context_message_with_note() {
        let msg = compact_context_message("Summary here.", Some("Note: check config."));
        assert!(msg.contains("Summary here."));
        assert!(msg.contains("Note: check config."));
    }

    // ── AutoCompactState default ────────────────────────────────────────────

    #[test]
    fn auto_compact_state_default() {
        let state = AutoCompactState::default();
        assert!(!state.disabled);
        assert!(!state.is_circuit_broken());
        assert!(state.last_summary_id.is_none());
    }
}
