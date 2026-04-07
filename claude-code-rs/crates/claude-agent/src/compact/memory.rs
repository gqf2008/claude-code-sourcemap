//! Session memory extraction — extract reusable facts from compacted summaries.
//!
//! During compaction, we can ask Claude to identify key facts (user preferences,
//! project conventions, architecture decisions) and persist them as memory files
//! for future sessions.
//!
//! Uses the 4-type taxonomy from `claude_core::memory::MemoryType`:
//! user, feedback, project, reference.

use serde::{Deserialize, Serialize};

use claude_core::memory::{self, MemoryType};

/// A memory fact extracted from conversation during compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemory {
    /// Short fact (< 200 chars).
    pub fact: String,
    /// Source/citation (e.g., "user mentioned", "discovered during task X").
    pub source: String,
    /// Category tag matching MemoryType (user, feedback, project, reference).
    pub category: String,
}

impl ExtractedMemory {
    /// Map `category` string to the formal `MemoryType` enum.
    /// Falls back to `Feedback` for unrecognized categories.
    pub fn memory_type(&self) -> MemoryType {
        MemoryType::parse(&self.category).unwrap_or(MemoryType::Feedback)
    }
}

/// Prompt template for session memory extraction.
/// Called with the conversation summary to ask Claude to extract key facts.
pub fn build_memory_extraction_prompt(summary: &str) -> String {
    format!(
        r#"Below is a compacted summary of a conversation session. Extract any important, reusable facts that should be remembered for future sessions.

Focus on these memory types:
- **user**: User's role, goals, preferences, knowledge level
- **feedback**: Guidance about work approach (corrections AND confirmations)
- **project**: Ongoing work, goals, incidents NOT derivable from code/git
- **reference**: Pointers to external systems (Linear, Grafana, Slack channels)

Do NOT save:
- Code patterns, architecture, file paths (derivable from current state)
- Git history, recent changes, debugging solutions
- Ephemeral task details or in-progress work

Return a JSON array of objects, each with "fact", "source", and "category" fields.
Category must be one of: user, feedback, project, reference.
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

/// Write extracted memories to the given memory directory using proper frontmatter.
///
/// Each memory gets its own file with YAML frontmatter (name, description, type).
/// The MEMORY.md index is updated after all writes.
pub fn save_extracted_memories(
    memories: &[ExtractedMemory],
    memory_dir: &std::path::Path,
) -> anyhow::Result<usize> {
    if memories.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(memory_dir)?;

    let mut saved = 0;
    for mem in memories {
        let slug: String = mem.fact.chars()
            .take(40)
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let slug = slug.trim_matches('-').to_string();
        let filename = format!(
            "session-{}-{}.md",
            slug,
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        if memory::write_memory_file(
            memory_dir,
            &filename,
            &mem.fact,
            &format!("{} (source: {})", mem.fact, mem.source),
            mem.memory_type(),
            &format!("{}\n\nSource: {}", mem.fact, mem.source),
        ).is_ok() {
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
        let json = r#"[{"fact":"User prefers Chinese","source":"user said so","category":"user"}]"#;
        let memories = parse_extracted_memories(json);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].fact, "User prefers Chinese");
        assert_eq!(memories[0].category, "user");
        assert_eq!(memories[0].memory_type(), MemoryType::User);
    }

    #[test]
    fn test_parse_extracted_memories_wrapped_in_markdown() {
        let response = "```json\n[{\"fact\":\"uses Rust\",\"source\":\"project\",\"category\":\"project\"}]\n```";
        let memories = parse_extracted_memories(response);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].fact, "uses Rust");
        assert_eq!(memories[0].memory_type(), MemoryType::Project);
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
        assert!(prompt.contains("user"));
        assert!(prompt.contains("feedback"));
        assert!(prompt.contains("project"));
        assert!(prompt.contains("reference"));
    }

    #[test]
    fn test_memory_type_fallback() {
        let mem = ExtractedMemory {
            fact: "test".to_string(),
            source: "test".to_string(),
            category: "unknown_category".to_string(),
        };
        // Unknown categories fall back to Feedback
        assert_eq!(mem.memory_type(), MemoryType::Feedback);
    }

    #[test]
    fn test_save_extracted_memories_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let memories = vec![
            ExtractedMemory {
                fact: "User prefers Rust".to_string(),
                source: "user said so".to_string(),
                category: "user".to_string(),
            },
            ExtractedMemory {
                fact: "Always run clippy".to_string(),
                source: "feedback".to_string(),
                category: "feedback".to_string(),
            },
        ];

        let saved = save_extracted_memories(&memories, tmp.path()).unwrap();
        assert_eq!(saved, 2);

        // Check files were created with frontmatter
        let files: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter(|e| e.file_name() != "MEMORY.md")
            .collect();
        assert_eq!(files.len(), 2);

        // Check MEMORY.md index was created
        let index = tmp.path().join("MEMORY.md");
        assert!(index.exists());
    }

    #[test]
    fn test_save_extracted_memories_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save_extracted_memories(&[], tmp.path()).unwrap();
        assert_eq!(saved, 0);
    }
}
