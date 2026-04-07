//! Memory extraction pipeline — extracts reusable facts from conversation
//! and persists them as memory files.
//!
//! Aligned with TS `services/extractMemories/extractMemories.ts`:
//! - Cursor-based incremental extraction (only new messages since last run)
//! - Overlap guard (only one extraction at a time)
//! - Mutual exclusion with main agent's direct memory writes
//! - Throttle support (extract every N turns)
//! - Tool permission matrix for the extraction sub-agent

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tracing::{debug, info, warn};

use claude_core::memory;
use claude_core::message::Message;

use crate::compact::memory as compact_mem;

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum turns the extraction agent can take (prevents rabbit-holes).
pub const MAX_EXTRACTION_TURNS: u32 = 5;

/// Default throttle: extract every N turns (1 = every turn).
const DEFAULT_THROTTLE_INTERVAL: usize = 1;

// ── Tool permission for extraction agent ────────────────────────────────────

/// Tools the memory extraction agent is allowed to use.
const ALLOWED_READONLY_TOOLS: &[&str] = &[
    "Read", "Grep", "Glob", "Bash",
];

/// Tools allowed for writing (restricted to memory directory only).
const ALLOWED_WRITE_TOOLS: &[&str] = &[
    "Edit", "Write",
];

/// Check if a tool is allowed for the memory extraction agent.
///
/// Read-only tools are unrestricted. Write tools are only allowed when
/// the file path is within the memory directory.
pub fn is_tool_allowed(tool_name: &str, file_path: Option<&str>, memory_dir: &Path) -> bool {
    if ALLOWED_READONLY_TOOLS.contains(&tool_name) {
        return true;
    }
    if ALLOWED_WRITE_TOOLS.contains(&tool_name) {
        if let Some(path) = file_path {
            let path = Path::new(path);
            return path.starts_with(memory_dir);
        }
    }
    false
}

// ── Extraction state ────────────────────────────────────────────────────────

/// State for the memory extraction pipeline.
///
/// Tracks cursor position, overlap guard, and throttle counter.
pub struct MemoryExtractor {
    /// Memory directory to write to.
    memory_dir: PathBuf,
    /// UUID of the last message processed (cursor for incremental extraction).
    last_cursor_uuid: Option<String>,
    /// Whether an extraction is currently in progress (overlap guard).
    in_progress: Arc<AtomicBool>,
    /// Turns since last extraction (throttle counter).
    turns_since_last: AtomicUsize,
    /// Throttle interval (extract every N turns).
    throttle_interval: usize,
    /// Whether auto-memory extraction is enabled.
    pub enabled: bool,
}

impl MemoryExtractor {
    /// Create a new extractor for the given memory directory.
    pub fn new(memory_dir: PathBuf) -> Self {
        Self {
            memory_dir,
            last_cursor_uuid: None,
            in_progress: Arc::new(AtomicBool::new(false)),
            turns_since_last: AtomicUsize::new(0),
            throttle_interval: DEFAULT_THROTTLE_INTERVAL,
            enabled: true,
        }
    }

    /// Create with a custom throttle interval.
    pub fn with_throttle(mut self, interval: usize) -> Self {
        self.throttle_interval = interval.max(1);
        self
    }

    /// Check whether extraction should run.
    ///
    /// Gates (all must pass):
    /// 1. Enabled flag is true
    /// 2. Not a sub-agent (caller must check this)
    /// 3. No overlap (not already extracting)
    /// 4. Throttle interval reached
    /// 5. Main agent hasn't written to memory since last extraction
    pub fn should_extract(
        &self,
        messages: &[Message],
        is_subagent: bool,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        if is_subagent {
            debug!("Memory extraction skipped: sub-agent context");
            return false;
        }
        if self.in_progress.load(Ordering::Relaxed) {
            debug!("Memory extraction skipped: already in progress");
            return false;
        }
        if self.turns_since_last.load(Ordering::Relaxed) < self.throttle_interval {
            return false;
        }
        // Check mutual exclusion: has the main agent written to memory?
        if self.has_memory_writes_since(messages) {
            debug!("Memory extraction skipped: main agent wrote to memory directly");
            return false;
        }
        true
    }

    /// Record that a turn has passed (increment throttle counter).
    pub fn record_turn(&self) {
        self.turns_since_last.fetch_add(1, Ordering::Relaxed);
    }

    /// Count new messages since the last extraction cursor.
    pub fn count_new_messages(&self, messages: &[Message]) -> usize {
        match &self.last_cursor_uuid {
            None => messages.len(),
            Some(cursor) => {
                let cursor_idx = messages.iter().position(|m| m.uuid() == cursor);
                match cursor_idx {
                    Some(idx) => messages.len().saturating_sub(idx + 1),
                    None => messages.len(), // cursor not found, process all
                }
            }
        }
    }

    /// Build the extraction prompt from new messages since cursor.
    pub fn build_extraction_prompt(
        &self,
        messages: &[Message],
    ) -> Option<String> {
        let new_count = self.count_new_messages(messages);
        if new_count == 0 {
            return None;
        }

        // Build a text summary of new messages
        let start_idx = messages.len().saturating_sub(new_count);
        let mut summary = String::new();
        for msg in &messages[start_idx..] {
            match msg {
                Message::User(u) => {
                    for block in &u.content {
                        if let claude_core::message::ContentBlock::Text { text } = block {
                            summary.push_str(&format!("User: {}\n\n", text));
                        }
                    }
                }
                Message::Assistant(a) => {
                    for block in &a.content {
                        match block {
                            claude_core::message::ContentBlock::Text { text } => {
                                summary.push_str(&format!("Assistant: {}\n\n", text));
                            }
                            claude_core::message::ContentBlock::ToolUse { name, .. } => {
                                summary.push_str(&format!("Assistant used tool: {}\n", name));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if summary.trim().is_empty() {
            return None;
        }

        Some(compact_mem::build_memory_extraction_prompt(&summary))
    }

    /// Process extraction results: parse, save, update cursor and index.
    ///
    /// Returns the number of memories saved.
    pub fn process_results(
        &mut self,
        response: &str,
        messages: &[Message],
    ) -> usize {
        let memories = compact_mem::parse_extracted_memories(response);
        if memories.is_empty() {
            self.advance_cursor(messages);
            self.reset_throttle();
            return 0;
        }

        match compact_mem::save_extracted_memories(&memories, &self.memory_dir) {
            Ok(saved) => {
                info!(
                    "Memory extraction saved {} memories to {:?}",
                    saved, self.memory_dir
                );
                self.advance_cursor(messages);
                self.reset_throttle();
                saved
            }
            Err(e) => {
                warn!("Memory extraction save failed: {}", e);
                // Don't advance cursor on failure — retry next time
                0
            }
        }
    }

    /// Build a system notification message for memories saved.
    pub fn build_notification(saved_count: usize, file_names: &[String]) -> String {
        if file_names.is_empty() {
            format!("{} memories saved", saved_count)
        } else {
            format!(
                "{} memories saved: {}",
                saved_count,
                file_names.join(", ")
            )
        }
    }

    /// Get the memory directory path.
    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }

    /// Acquire the overlap guard. Returns false if already in progress.
    pub fn try_acquire(&self) -> bool {
        self.in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the overlap guard.
    pub fn release(&self) {
        self.in_progress.store(false, Ordering::SeqCst);
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Check if the main agent has written to memory files since the last cursor.
    fn has_memory_writes_since(&self, messages: &[Message]) -> bool {
        let start_idx = match &self.last_cursor_uuid {
            None => 0,
            Some(cursor) => {
                messages.iter()
                    .position(|m| m.uuid() == cursor)
                    .map(|i| i + 1)
                    .unwrap_or(0)
            }
        };

        let memory_dir_str = self.memory_dir.to_string_lossy();

        for msg in &messages[start_idx..] {
            if let Message::Assistant(a) = msg {
                for block in &a.content {
                    if let claude_core::message::ContentBlock::ToolUse { name, input, .. } = block {
                        if name == "Write" || name == "Edit" {
                            if let Some(path) = input["file_path"].as_str() {
                                if path.contains(memory_dir_str.as_ref()) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Advance cursor to the last message.
    fn advance_cursor(&mut self, messages: &[Message]) {
        if let Some(last) = messages.last() {
            self.last_cursor_uuid = Some(last.uuid().to_string());
        }
    }

    /// Reset throttle counter after extraction.
    fn reset_throttle(&self) {
        self.turns_since_last.store(0, Ordering::Relaxed);
    }
}

// ── Stop hooks integration ──────────────────────────────────────────────────

/// Decision from stop hooks (TS parity: handleStopHooks).
#[derive(Debug, Clone)]
pub enum StopDecision {
    /// Normal stop — no intervention.
    Stop,
    /// Inject feedback and continue the query loop.
    Continue { feedback: String },
    /// Memory extraction was triggered (fire-and-forget).
    ExtractMemories,
}

/// Handle stop hooks at the end of a query loop iteration.
///
/// Called when the model produces a final response with no tool calls.
/// Checks stop hooks and triggers memory extraction if appropriate.
pub fn handle_stop_hooks(
    extractor: Option<&mut MemoryExtractor>,
    messages: &[Message],
    is_subagent: bool,
    hook_decision: Option<crate::hooks::HookDecision>,
) -> StopDecision {
    // 1. Check hook decision first
    if let Some(decision) = hook_decision {
        match decision {
            crate::hooks::HookDecision::FeedbackAndContinue { feedback } => {
                return StopDecision::Continue { feedback };
            }
            crate::hooks::HookDecision::Block { reason } => {
                return StopDecision::Continue { feedback: reason };
            }
            _ => {}
        }
    }

    // 2. Check memory extraction
    if let Some(extractor) = extractor {
        if extractor.should_extract(messages, is_subagent) {
            return StopDecision::ExtractMemories;
        }
    }

    StopDecision::Stop
}

// ── Find relevant memories (simple keyword-based recall) ────────────────────

/// Find up to `max_results` relevant memory files based on keyword overlap.
///
/// This is a simplified version of TS's `findRelevantMemories` which uses
/// a Sonnet selector model. Our version uses basic keyword matching on
/// the memory description and filename.
pub fn find_relevant_memories(
    query: &str,
    memory_dir: &Path,
    max_results: usize,
) -> Vec<memory::MemoryHeader> {
    let headers = memory::scan_memory_dir(memory_dir);
    if headers.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }

    // Tokenize query into lowercase keywords (>= 3 chars)
    let keywords: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| w.len() >= 3)
        .collect();

    if keywords.is_empty() {
        // No meaningful keywords — return newest memories
        return headers.into_iter().take(max_results).collect();
    }

    // Score each header by keyword overlap
    let mut scored: Vec<(usize, memory::MemoryHeader)> = headers
        .into_iter()
        .map(|h| {
            let searchable = format!(
                "{} {} {}",
                h.filename,
                h.name.as_deref().unwrap_or(""),
                h.description.as_deref().unwrap_or("")
            ).to_lowercase();

            let score = keywords.iter()
                .filter(|kw| searchable.contains(kw.as_str()))
                .count();
            (score, h)
        })
        .collect();

    // Sort by score (descending), then by mtime (newest first)
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.mtime.cmp(&a.1.mtime))
    });

    scored
        .into_iter()
        .filter(|(score, _)| *score > 0)
        .take(max_results)
        .map(|(_, h)| h)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::message::{AssistantMessage, ContentBlock, UserMessage};

    fn make_user_msg(uuid: &str, text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid.to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
        })
    }

    fn make_assistant_msg(uuid: &str, text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid.to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: None,
            usage: None,
        })
    }

    fn make_assistant_tool_use(uuid: &str, tool: &str, path: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid.to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: tool.to_string(),
                input: serde_json::json!({"file_path": path}),
            }],
            stop_reason: None,
            usage: None,
        })
    }

    // ── MemoryExtractor ──────────────────────────────────────────────

    #[test]
    fn new_extractor_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        assert!(ext.enabled);
        assert!(ext.last_cursor_uuid.is_none());
        assert!(!ext.in_progress.load(Ordering::Relaxed));
    }

    #[test]
    fn count_new_messages_no_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        let msgs = vec![make_user_msg("u1", "hello"), make_assistant_msg("a1", "hi")];
        assert_eq!(ext.count_new_messages(&msgs), 2);
    }

    #[test]
    fn count_new_messages_with_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.last_cursor_uuid = Some("u1".to_string());

        let msgs = vec![
            make_user_msg("u1", "hello"),
            make_assistant_msg("a1", "hi"),
            make_user_msg("u2", "more"),
        ];
        assert_eq!(ext.count_new_messages(&msgs), 2); // a1, u2
    }

    #[test]
    fn should_extract_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.enabled = false;
        ext.turns_since_last.store(5, Ordering::Relaxed);
        assert!(!ext.should_extract(&[], false));
    }

    #[test]
    fn should_extract_subagent() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.turns_since_last.store(5, Ordering::Relaxed);
        assert!(!ext.should_extract(&[], true));
    }

    #[test]
    fn should_extract_throttled() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        // turns_since_last is 0, throttle_interval is 1
        assert!(!ext.should_extract(&[], false));
    }

    #[test]
    fn should_extract_passes_all_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.turns_since_last.store(1, Ordering::Relaxed);
        let msgs = vec![make_user_msg("u1", "hello")];
        assert!(ext.should_extract(&msgs, false));
    }

    #[test]
    fn should_extract_blocked_by_memory_write() {
        let tmp = tempfile::tempdir().unwrap();
        let memory_path = format!("{}/note.md", tmp.path().to_string_lossy());
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.turns_since_last.store(1, Ordering::Relaxed);

        let msgs = vec![make_assistant_tool_use("a1", "Write", &memory_path)];
        assert!(!ext.should_extract(&msgs, false));
    }

    #[test]
    fn overlap_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        assert!(ext.try_acquire());
        assert!(!ext.try_acquire()); // second acquire fails
        ext.release();
        assert!(ext.try_acquire()); // works again after release
    }

    #[test]
    fn build_extraction_prompt_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        assert!(ext.build_extraction_prompt(&[]).is_none());
    }

    #[test]
    fn build_extraction_prompt_with_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = MemoryExtractor::new(tmp.path().to_path_buf());
        let msgs = vec![
            make_user_msg("u1", "I prefer Chinese"),
            make_assistant_msg("a1", "Got it"),
        ];
        let prompt = ext.build_extraction_prompt(&msgs).unwrap();
        assert!(prompt.contains("I prefer Chinese"));
        assert!(prompt.contains("Got it"));
        assert!(prompt.contains("JSON array"));
    }

    #[test]
    fn process_results_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ext = MemoryExtractor::new(tmp.path().to_path_buf());
        let msgs = vec![make_user_msg("u1", "hello")];
        let saved = ext.process_results("[]", &msgs);
        assert_eq!(saved, 0);
        assert_eq!(ext.last_cursor_uuid.as_deref(), Some("u1"));
    }

    #[test]
    fn process_results_with_memories() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ext = MemoryExtractor::new(tmp.path().to_path_buf());
        let msgs = vec![make_user_msg("u1", "hello")];
        let response = r#"[{"fact":"User likes Rust","source":"user","category":"user"}]"#;
        let saved = ext.process_results(response, &msgs);
        assert_eq!(saved, 1);
        assert_eq!(ext.last_cursor_uuid.as_deref(), Some("u1"));
    }

    // ── Tool permissions ─────────────────────────────────────────────

    #[test]
    fn tool_permission_readonly_allowed() {
        let dir = Path::new("/tmp/memory");
        assert!(is_tool_allowed("Read", None, dir));
        assert!(is_tool_allowed("Grep", None, dir));
        assert!(is_tool_allowed("Glob", None, dir));
        assert!(is_tool_allowed("Bash", None, dir));
    }

    #[test]
    fn tool_permission_write_in_memory_dir() {
        let dir = Path::new("/tmp/memory");
        assert!(is_tool_allowed("Write", Some("/tmp/memory/note.md"), dir));
        assert!(is_tool_allowed("Edit", Some("/tmp/memory/note.md"), dir));
    }

    #[test]
    fn tool_permission_write_outside_denied() {
        let dir = Path::new("/tmp/memory");
        assert!(!is_tool_allowed("Write", Some("/tmp/other/file.md"), dir));
        assert!(!is_tool_allowed("Edit", Some("/home/user/code.rs"), dir));
    }

    #[test]
    fn tool_permission_unknown_tool_denied() {
        let dir = Path::new("/tmp/memory");
        assert!(!is_tool_allowed("Agent", None, dir));
        assert!(!is_tool_allowed("MCP", None, dir));
    }

    // ── Stop hooks integration ───────────────────────────────────────

    #[test]
    fn stop_decision_no_hooks_no_extractor() {
        let result = handle_stop_hooks(None, &[], false, None);
        assert!(matches!(result, StopDecision::Stop));
    }

    #[test]
    fn stop_decision_feedback_hook() {
        let decision = crate::hooks::HookDecision::FeedbackAndContinue {
            feedback: "please continue".to_string(),
        };
        let result = handle_stop_hooks(None, &[], false, Some(decision));
        match result {
            StopDecision::Continue { feedback } => assert_eq!(feedback, "please continue"),
            _ => panic!("Expected Continue"),
        }
    }

    #[test]
    fn stop_decision_triggers_extraction() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ext = MemoryExtractor::new(tmp.path().to_path_buf());
        ext.turns_since_last.store(1, Ordering::Relaxed);
        let msgs = vec![make_user_msg("u1", "hello")];
        let result = handle_stop_hooks(Some(&mut ext), &msgs, false, None);
        assert!(matches!(result, StopDecision::ExtractMemories));
    }

    // ── find_relevant_memories ────────────────────────────────────────

    #[test]
    fn find_relevant_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let results = find_relevant_memories("rust code", tmp.path(), 5);
        assert!(results.is_empty());
    }

    #[test]
    fn find_relevant_keyword_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("rust_pref.md"),
            "---\nname: Rust Preference\ndescription: User prefers Rust over Python\ntype: user\n---\nContent",
        ).unwrap();
        std::fs::write(
            tmp.path().join("python_note.md"),
            "---\nname: Python Note\ndescription: Python is used for scripts\ntype: project\n---\nContent",
        ).unwrap();

        let results = find_relevant_memories("rust coding", tmp.path(), 5);
        assert!(!results.is_empty());
        // Rust should be first (matches "rust")
        assert!(results[0].name.as_deref().unwrap().contains("Rust"));
    }

    #[test]
    fn find_relevant_max_results() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(
                tmp.path().join(format!("note_{}.md", i)),
                format!("---\nname: Test Note {}\ndescription: test description\ntype: user\n---\nContent", i),
            ).unwrap();
        }

        let results = find_relevant_memories("test note", tmp.path(), 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn find_relevant_empty_query() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("note.md"), "---\nname: X\n---\nY").unwrap();
        let results = find_relevant_memories("", tmp.path(), 5);
        assert!(results.is_empty());
    }

    // ── Build notification ───────────────────────────────────────────

    #[test]
    fn build_notification_format() {
        let msg = MemoryExtractor::build_notification(2, &["a.md".to_string(), "b.md".to_string()]);
        assert_eq!(msg, "2 memories saved: a.md, b.md");
    }

    #[test]
    fn build_notification_empty() {
        let msg = MemoryExtractor::build_notification(3, &[]);
        assert_eq!(msg, "3 memories saved");
    }
}
