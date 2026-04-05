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
                            ApiContentBlock::Text { text: text.clone() }
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
                        },
                        _ => ApiContentBlock::Text {
                            text: "[content block]".to_string(),
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
                            Some(ApiContentBlock::Text { text: text.clone() })
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

    Ok(format_compact_summary(&raw_text))
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
