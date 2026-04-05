//! Session compaction — mirrors claude-code's `services/compact/compact.ts`.
//!
//! When conversation history grows large (past the token threshold), we call
//! Claude with a structured prompt that produces an `<analysis>` scratchpad
//! plus a `<summary>` block.  The analysis is stripped; the summary replaces
//! the old messages, giving a fresh context window while preserving intent.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let summary = compact_conversation(&client, &messages, model, max_tokens, None).await?;
//! ```
//!
//! The summary string can then be injected as a system message at the top of
//! the new conversation history.

use claude_api::client::AnthropicClient;
use claude_api::types::{ApiContentBlock, ApiMessage, MessagesRequest, SystemBlock};
use claude_core::message::{Message, ToolResultContent};
use serde::{Deserialize, Serialize};

// ── Token threshold ──────────────────────────────────────────────────────────

/// Auto-compact when accumulated input tokens exceed this threshold.
/// Matches the original (~90 % of claude-sonnet-4's 200k context window).
pub const AUTO_COMPACT_THRESHOLD: u64 = 80_000;

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
    let re = regex::Regex::new(r"\n{3,}").unwrap();
    re.replace_all(result.trim(), "\n\n").to_string()
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
    client: &AnthropicClient,
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

// ── Micro-Compact: Tool Result Clearing ──────────────────────────────────────

/// Marker text that replaces cleared tool result content.
pub const TOOL_RESULT_CLEARED: &str = "[Old tool result content cleared]";

/// Maximum size (in chars) for a single tool result before truncation.
pub const MAX_TOOL_RESULT_CHARS: usize = 50_000;

/// Tools whose results are compactable (safe to clear after they've been consumed).
const COMPACTABLE_TOOLS: &[&str] = &[
    "Read", "Bash", "PowerShell", "Grep", "Glob",
    "WebSearch", "WebFetch", "Edit", "Write", "MultiEdit",
    "ListDir", "FileRead", "FileEdit", "FileWrite",
    "GlobTool", "GrepTool", "BashTool", "WebSearchTool", "WebFetchTool",
];

/// Clear tool results from older messages, keeping the `keep_recent` most recent.
///
/// This is the Rust equivalent of TS `microcompactMessages()` time-based path.
/// Tool results from compactable tools are replaced with `TOOL_RESULT_CLEARED`.
///
/// Returns the number of tool results cleared.
pub fn clear_old_tool_results(messages: &mut [Message], keep_recent: usize) -> usize {
    use claude_core::message::{ContentBlock, ToolResultContent};

    // Collect all compactable tool result IDs, newest first.
    // We need to know which tool_use_id maps to which tool name.
    let mut tool_use_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for msg in messages.iter() {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    tool_use_names.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // Find all compactable tool_result blocks (by index in message array)
    let mut compactable_ids: Vec<String> = Vec::new();
    for msg in messages.iter() {
        if let Message::User(u) = msg {
            for block in &u.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    if let Some(name) = tool_use_names.get(tool_use_id) {
                        if COMPACTABLE_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(name)) {
                            compactable_ids.push(tool_use_id.clone());
                        }
                    }
                }
            }
        }
    }

    if compactable_ids.len() <= keep_recent {
        return 0;
    }

    // IDs to clear = all except the last `keep_recent`
    let clear_count = compactable_ids.len() - keep_recent;
    let clear_set: std::collections::HashSet<&str> = compactable_ids[..clear_count]
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut cleared = 0;
    for msg in messages.iter_mut() {
        if let Message::User(u) = msg {
            for block in u.content.iter_mut() {
                if let ContentBlock::ToolResult { tool_use_id, content, .. } = block {
                    if clear_set.contains(tool_use_id.as_str()) {
                        // Check if already cleared
                        let already_cleared = content.len() == 1
                            && matches!(&content[0], ToolResultContent::Text { text } if text == TOOL_RESULT_CLEARED);
                        if !already_cleared {
                            *content = vec![ToolResultContent::Text {
                                text: TOOL_RESULT_CLEARED.to_string(),
                            }];
                            cleared += 1;
                        }
                    }
                }
            }
        }
    }

    cleared
}

/// Truncate individual tool results that exceed `max_chars`.
///
/// Returns the number of tool results truncated.
pub fn truncate_large_tool_results(messages: &mut [Message], max_chars: usize) -> usize {
    use claude_core::message::{ContentBlock, ToolResultContent};

    let mut truncated = 0;
    for msg in messages.iter_mut() {
        if let Message::User(u) = msg {
            for block in u.content.iter_mut() {
                if let ContentBlock::ToolResult { content, .. } = block {
                    for item in content.iter_mut() {
                        if let ToolResultContent::Text { text } = item {
                            if text.len() > max_chars {
                                // UTF-8 safe truncation
                                let mut end = max_chars;
                                while !text.is_char_boundary(end) && end > 0 {
                                    end -= 1;
                                }
                                let truncated_text = format!(
                                    "{}\n\n[… truncated {} chars]",
                                    &text[..end],
                                    text.len() - end,
                                );
                                *text = truncated_text;
                                truncated += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    truncated
}

/// Snip old message pairs from the beginning of conversation history.
///
/// Removes user+assistant pairs from the front until `target_pairs` remain.
/// Inserts a boundary message at the snip point.
/// Returns the number of messages removed.
pub fn snip_old_messages(messages: &mut Vec<Message>, keep_recent_pairs: usize) -> usize {
    // Count user+assistant pairs
    let mut pair_count = 0;
    for msg in messages.iter() {
        if matches!(msg, Message::User(_)) {
            pair_count += 1;
        }
    }

    if pair_count <= keep_recent_pairs {
        return 0;
    }

    let pairs_to_remove = pair_count - keep_recent_pairs;

    // Remove messages from front: each "pair" is one user msg + one assistant msg
    let mut removed = 0;
    let mut pairs_removed = 0;
    let mut i = 0;
    while pairs_removed < pairs_to_remove && i < messages.len() {
        match &messages[i] {
            Message::User(_) => {
                messages.remove(i);
                removed += 1;
                pairs_removed += 1;
                // Also remove the following assistant message if present
                if i < messages.len() && matches!(&messages[i], Message::Assistant(_)) {
                    messages.remove(i);
                    removed += 1;
                }
            }
            Message::Assistant(_) => {
                // Orphaned assistant without a preceding user — remove it
                messages.remove(i);
                removed += 1;
            }
            Message::System(_) => {
                i += 1; // Skip system messages
            }
        }
    }

    // Insert boundary message at the start
    if removed > 0 {
        use claude_core::message::SystemMessage;
        messages.insert(0, Message::System(SystemMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            message: format!(
                "[{} earlier messages snipped to manage context size]",
                removed
            ),
        }));
    }

    removed
}

// ── Session Memory Extraction ────────────────────────────────────────────────

/// A memory fact extracted from conversation during compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemory {
    /// Short fact (< 200 chars).
    pub fact: String,
    /// Source/citation (e.g., "user mentioned", "discovered during task X").
    pub source: String,
    /// Category tag (e.g., "preference", "convention", "architecture").
    pub category: String,
}

/// Prompt template for session memory extraction.
/// Called with the conversation summary to ask Claude to extract key facts.
pub fn build_memory_extraction_prompt(summary: &str) -> String {
    format!(
        r#"Below is a compacted summary of a conversation session. Extract any important, reusable facts that should be remembered for future sessions.

Focus on:
- User preferences (language, style, workflow)
- Project conventions or architecture decisions
- Important paths, commands, or configuration
- Recurring patterns or anti-patterns

Return a JSON array of objects, each with "fact", "source", and "category" fields.
Only include facts that are:
1. Likely to remain true across sessions
2. Actionable for future tasks
3. Not obvious from reading the code

If no memorable facts are found, return an empty array: []

<summary>
{summary}
</summary>

Respond with ONLY the JSON array, no other text."#
    )
}

/// Parse extracted memories from Claude's JSON response.
pub fn parse_extracted_memories(response: &str) -> Vec<ExtractedMemory> {
    // Try to parse directly
    if let Ok(memories) = serde_json::from_str::<Vec<ExtractedMemory>>(response) {
        return memories;
    }
    // Try to find JSON array in response (Claude sometimes wraps in markdown)
    if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            if let Ok(memories) = serde_json::from_str::<Vec<ExtractedMemory>>(&response[start..=end]) {
                return memories;
            }
        }
    }
    Vec::new()
}

/// Write extracted memories to the user memory directory.
pub fn save_extracted_memories(memories: &[ExtractedMemory]) -> anyhow::Result<usize> {
    if memories.is_empty() {
        return Ok(0);
    }
    let dir = claude_core::memory::ensure_user_memory_dir()?;
    let mut saved = 0;
    for mem in memories {
        let slug: String = mem.fact.chars()
            .take(40)
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let slug = slug.trim_matches('-').to_string();
        let filename = format!("session-{}-{}.md", slug, &uuid::Uuid::new_v4().to_string()[..8]);
        let path = dir.join(&filename);

        let content = format!(
            "---\ntype: feedback\ndescription: {}\n---\n\n{}\n\nSource: {}\n",
            mem.category, mem.fact, mem.source
        );

        if std::fs::write(&path, &content).is_ok() {
            saved += 1;
        }
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::message::{
        AssistantMessage, ContentBlock, ToolResultContent, UserMessage,
    };

    #[test]
    fn test_token_warning_levels() {
        assert_eq!(calculate_token_warning(0, 100_000), TokenWarningState::Normal);
        assert_eq!(calculate_token_warning(40_000, 100_000), TokenWarningState::Normal);
        assert_eq!(calculate_token_warning(55_000, 100_000), TokenWarningState::Warning);
        assert_eq!(calculate_token_warning(80_000, 100_000), TokenWarningState::Critical);
        assert_eq!(calculate_token_warning(95_000, 100_000), TokenWarningState::Imminent);
        // Zero threshold always Normal
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
        use claude_core::message::{AssistantMessage, ContentBlock};
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
        use claude_core::message::SystemMessage;
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
        use claude_core::message::{AssistantMessage, SystemMessage};
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
        // 200k context, 20k output reserved, 13k buffer → threshold = 167k
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

        state.record_failure(); // 3rd failure
        assert!(state.is_circuit_broken());
        assert!(!state.should_auto_compact(200_000, 200_000));

        // Success resets
        state.record_success();
        assert!(!state.is_circuit_broken());
        assert!(state.should_auto_compact(200_000, 200_000));
    }

    #[test]
    fn test_auto_compact_zero_context() {
        let state = AutoCompactState::new();
        assert!(!state.should_auto_compact(100_000, 0));
    }

    // ── Micro-compact tests ──────────────────────────────────

    fn make_tool_use_msg(id: &str, name: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
            stop_reason: None,
            usage: None,
        })
    }

    fn make_tool_result_msg(tool_use_id: &str, text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContent::Text { text: text.into() }],
                is_error: false,
            }],
        })
    }

    #[test]
    fn test_clear_old_tool_results_basic() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "file contents 1"),
            make_tool_use_msg("t2", "Bash"),
            make_tool_result_msg("t2", "command output"),
            make_tool_use_msg("t3", "Read"),
            make_tool_result_msg("t3", "file contents 3"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(cleared, 2); // t1, t2 cleared; t3 kept

        // Verify t3 still has content
        if let Message::User(u) = &msgs[5] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert_eq!(text, "file contents 3");
                }
            }
        }

        // Verify t1 was cleared
        if let Message::User(u) = &msgs[1] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert_eq!(text, TOOL_RESULT_CLEARED);
                }
            }
        }
    }

    #[test]
    fn test_clear_old_tool_results_non_compactable_skipped() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "AgentTool"),
            make_tool_result_msg("t1", "agent result"),
            make_tool_use_msg("t2", "Read"),
            make_tool_result_msg("t2", "file content"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 0);
        // AgentTool is not compactable, only Read is cleared
        assert_eq!(cleared, 1);
    }

    #[test]
    fn test_clear_old_tool_results_idempotent() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "data"),
            make_tool_use_msg("t2", "Read"),
            make_tool_result_msg("t2", "data2"),
        ];

        let c1 = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(c1, 1);

        // Run again — should not re-clear already cleared
        let c2 = clear_old_tool_results(&mut msgs, 1);
        assert_eq!(c2, 0);
    }

    #[test]
    fn test_clear_old_tool_results_keep_all() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "data"),
        ];

        let cleared = clear_old_tool_results(&mut msgs, 5);
        assert_eq!(cleared, 0);
    }

    #[test]
    fn test_truncate_large_tool_results() {
        let long_text = "x".repeat(100);
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", &long_text),
        ];

        let truncated = truncate_large_tool_results(&mut msgs, 50);
        assert_eq!(truncated, 1);

        if let Message::User(u) = &msgs[1] {
            if let ContentBlock::ToolResult { content, .. } = &u.content[0] {
                if let ToolResultContent::Text { text } = &content[0] {
                    assert!(text.len() < 100);
                    assert!(text.contains("truncated"));
                }
            }
        }
    }

    #[test]
    fn test_truncate_no_change_under_limit() {
        let mut msgs = vec![
            make_tool_use_msg("t1", "Read"),
            make_tool_result_msg("t1", "short"),
        ];

        let truncated = truncate_large_tool_results(&mut msgs, 1000);
        assert_eq!(truncated, 0);
    }

    #[test]
    fn test_snip_old_messages() {
        let mut msgs: Vec<Message> = Vec::new();
        for i in 0..5 {
            msgs.push(Message::User(UserMessage {
                uuid: format!("u{i}"),
                content: vec![ContentBlock::Text { text: format!("question {i}") }],
            }));
            msgs.push(Message::Assistant(AssistantMessage {
                uuid: format!("a{i}"),
                content: vec![ContentBlock::Text { text: format!("answer {i}") }],
                stop_reason: None,
                usage: None,
            }));
        }

        assert_eq!(msgs.len(), 10);
        let removed = snip_old_messages(&mut msgs, 2);
        assert_eq!(removed, 6); // 3 pairs × 2 = 6 messages

        // First message should be the snip marker
        assert!(matches!(&msgs[0], Message::System(_)));

        // Remaining should be 2 user+assistant pairs + 1 system = 5
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    fn test_snip_no_change_under_limit() {
        let mut msgs = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "q".into() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![ContentBlock::Text { text: "a".into() }],
                stop_reason: None,
                usage: None,
            }),
        ];

        let removed = snip_old_messages(&mut msgs, 5);
        assert_eq!(removed, 0);
        assert_eq!(msgs.len(), 2);
    }

    // ── Memory extraction tests ──────────────────────────────

    #[test]
    fn test_parse_extracted_memories_valid_json() {
        let json = r#"[{"fact":"User prefers Chinese","source":"user said so","category":"preference"}]"#;
        let memories = parse_extracted_memories(json);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].fact, "User prefers Chinese");
        assert_eq!(memories[0].category, "preference");
    }

    #[test]
    fn test_parse_extracted_memories_wrapped_in_markdown() {
        let response = "```json\n[{\"fact\":\"uses Rust\",\"source\":\"project\",\"category\":\"tech\"}]\n```";
        let memories = parse_extracted_memories(response);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].fact, "uses Rust");
    }

    #[test]
    fn test_parse_extracted_memories_empty() {
        assert!(parse_extracted_memories("[]").is_empty());
        assert!(parse_extracted_memories("no json here").is_empty());
    }

    #[test]
    fn test_build_memory_extraction_prompt() {
        let prompt = build_memory_extraction_prompt("User discussed Rust porting");
        assert!(prompt.contains("User discussed Rust porting"));
        assert!(prompt.contains("JSON array"));
        assert!(prompt.contains("<summary>"));
    }
}
