//! Session memory extraction — extract reusable facts from compacted summaries.
//!
//! During compaction, we can ask Claude to identify key facts (user preferences,
//! project conventions, architecture decisions) and persist them as memory files
//! for future sessions.

use serde::{Deserialize, Serialize};

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
