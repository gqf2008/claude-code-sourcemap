//! Session persistence — save/restore conversation sessions to disk.
//!
//! Two storage formats:
//!
//! 1. **JSON snapshot** (`{id}.json`) — full session state, atomic write.
//!    Good for small sessions, used by `save_session()` / `load_session()`.
//!
//! 2. **JSONL transcript** (`{id}.jsonl`) — append-only, one entry per line.
//!    Used for incremental recording during live sessions.
//!    Aligned with TS `sessionStorage.ts` `appendEntry()`.
//!
//! A lightweight manifest (`index.json`) caches session metadata.

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::message::Message;

// ── Data types ───────────────────────────────────────────────────────────────

/// Per-model usage entry for session persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub api_calls: u32,
    pub cost_usd: f64,
}

/// A persisted session snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Unique session identifier.
    pub id: String,
    /// Display title (first user message, truncated).
    pub title: String,
    /// Model used.
    pub model: String,
    /// Working directory at session start.
    pub cwd: String,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last saved.
    pub updated_at: DateTime<Utc>,
    /// Total turns completed.
    pub turn_count: u32,
    /// Token usage.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Per-model usage breakdown.
    #[serde(default)]
    pub model_usage: HashMap<String, SessionModelUsage>,
    /// Total cost in USD.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Full conversation history.
    pub messages: Vec<Message>,
    /// Git branch at time of session (for resume picker).
    #[serde(default)]
    pub git_branch: Option<String>,
    /// User-set session name.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// Auto-generated title from AI.
    #[serde(default)]
    pub ai_title: Option<String>,
    /// Conversation summary (at leaf).
    #[serde(default)]
    pub summary: Option<String>,
    /// Last user prompt (truncated, for resume picker).
    #[serde(default)]
    pub last_prompt: Option<String>,
}

/// Lightweight session metadata for listing (without messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub model: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub turn_count: u32,
    pub message_count: usize,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub last_prompt: Option<String>,
}

/// Manifest file for fast session listing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionManifest {
    sessions: Vec<SessionMeta>,
}

fn manifest_path() -> PathBuf {
    sessions_dir().join("index.json")
}

fn load_manifest() -> SessionManifest {
    let path = manifest_path();
    if !path.exists() {
        return SessionManifest::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
            tracing::warn!("Corrupt session manifest, using default: {}", e);
            SessionManifest::default()
        }),
        Err(e) => {
            tracing::warn!("Failed to read session manifest: {}", e);
            SessionManifest::default()
        }
    }
}

fn save_manifest(manifest: &SessionManifest) {
    let path = manifest_path();
    match serde_json::to_string_pretty(manifest) {
        Ok(json) => {
            if let Err(e) = atomic_write(&path, json.as_bytes()) {
                tracing::warn!("Failed to save session manifest: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to serialize session manifest: {}", e);
        }
    }
}

/// Write data to a file atomically: write to a `.tmp` sibling, then rename.
///
/// On most filesystems `rename` is atomic, so readers never see a
/// partially-written file. If the process crashes before rename, only the
/// `.tmp` file is left (harmless).
fn atomic_write(target: &Path, data: &[u8]) -> anyhow::Result<()> {
    // Create temp file in the same directory as target to ensure rename
    // stays on the same filesystem (required for atomic rename).
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{}.tmp", file_name));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}

fn update_manifest_entry(meta: &SessionMeta) {
    let mut manifest = load_manifest();
    if let Some(existing) = manifest.sessions.iter_mut().find(|s| s.id == meta.id) {
        *existing = meta.clone();
    } else {
        manifest.sessions.push(meta.clone());
    }
    save_manifest(&manifest);
}

fn remove_manifest_entry(id: &str) {
    let mut manifest = load_manifest();
    manifest.sessions.retain(|s| s.id != id);
    save_manifest(&manifest);
}

// ── Paths ────────────────────────────────────────────────────────────────────

/// Return the sessions directory: `~/.claude/sessions/`
pub fn sessions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("sessions")
}

/// Path for a specific session file.
#[cfg(not(test))]
fn session_path(id: &str) -> anyhow::Result<PathBuf> {
    session_path_inner(id)
}

#[cfg(test)]
pub(crate) fn session_path(id: &str) -> anyhow::Result<PathBuf> {
    session_path_inner(id)
}

fn session_path_inner(id: &str) -> anyhow::Result<PathBuf> {
    // Validate session ID to prevent path traversal
    if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("Invalid session ID: must be alphanumeric, dash, or underscore");
    }
    Ok(sessions_dir().join(format!("{}.json", id)))
}

// ── Save ─────────────────────────────────────────────────────────────────────

/// Save a session snapshot to disk and update the manifest index.
///
/// Uses atomic write (temp file → rename) to prevent corruption if the
/// process crashes mid-write.
pub fn save_session(session: &SessionSnapshot) -> anyhow::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_path(&session.id)?;

    // Serialize first — fail early if JSON serialization fails
    let json = serde_json::to_string_pretty(session)?;

    // Atomic write: temp file → rename
    atomic_write(&path, json.as_bytes())?;

    // Update manifest index
    let meta = SessionMeta {
        id: session.id.clone(),
        title: session.title.clone(),
        model: session.model.clone(),
        cwd: session.cwd.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
        turn_count: session.turn_count,
        message_count: session.messages.len(),
        total_cost_usd: session.total_cost_usd,
        git_branch: session.git_branch.clone(),
        custom_title: session.custom_title.clone(),
        summary: session.summary.clone(),
        last_prompt: session.last_prompt.clone(),
    };
    update_manifest_entry(&meta);
    Ok(())
}

// ── Load ─────────────────────────────────────────────────────────────────────

/// Load a session by ID.
pub fn load_session(id: &str) -> anyhow::Result<SessionSnapshot> {
    let path = session_path(id)?;
    if !path.exists() {
        anyhow::bail!("Session not found: {}", id);
    }
    let json = std::fs::read_to_string(&path)?;
    let session: SessionSnapshot = serde_json::from_str(&json)?;
    Ok(session)
}

// ── List ─────────────────────────────────────────────────────────────────────

/// List all saved sessions (metadata only, sorted by updated_at desc).
/// Uses the manifest index for fast listing; falls back to scanning files.
pub fn list_sessions() -> Vec<SessionMeta> {
    // Try manifest first
    let manifest = load_manifest();
    if !manifest.sessions.is_empty() {
        let mut sessions = manifest.sessions;
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        return sessions;
    }

    // Fallback: scan all session files
    let dir = sessions_dir();
    if !dir.exists() {
        return Vec::new();
    }

    let mut sessions: Vec<SessionMeta> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            // Skip the manifest file itself
            if path.file_name().and_then(|n| n.to_str()) == Some("index.json") {
                return None;
            }
            read_session_meta(&path)
        })
        .collect();

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    // Rebuild manifest from scanned sessions
    if !sessions.is_empty() {
        let manifest = SessionManifest { sessions: sessions.clone() };
        save_manifest(&manifest);
    }

    sessions
}

/// Read only metadata from a session file (deserialise but drop messages).
fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    let json = std::fs::read_to_string(path).ok()?;
    let snap: SessionSnapshot = serde_json::from_str(&json).ok()?;
    Some(SessionMeta {
        message_count: snap.messages.len(),
        id: snap.id,
        title: snap.title,
        model: snap.model,
        cwd: snap.cwd,
        created_at: snap.created_at,
        updated_at: snap.updated_at,
        turn_count: snap.turn_count,
        total_cost_usd: snap.total_cost_usd,
        git_branch: snap.git_branch,
        custom_title: snap.custom_title,
        summary: snap.summary,
        last_prompt: snap.last_prompt,
    })
}

// ── Delete ───────────────────────────────────────────────────────────────────

/// Delete a saved session and remove it from the manifest.
pub fn delete_session(id: &str) -> anyhow::Result<()> {
    let path = session_path(id)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    // Also remove JSONL transcript if present
    let jsonl = transcript_path(id)?;
    if jsonl.exists() {
        let _ = std::fs::remove_file(&jsonl);
    }
    remove_manifest_entry(id);
    Ok(())
}

// ── JSONL Transcript ─────────────────────────────────────────────────────────

/// A single entry in a JSONL transcript file.
///
/// Aligned with TS `types/logs.ts` — each variant maps to a `type` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TranscriptEntry {
    /// User message.
    #[serde(rename = "user")]
    User {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_uuid: Option<String>,
        message: Message,
        timestamp: DateTime<Utc>,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_branch: Option<String>,
    },
    /// Assistant response.
    #[serde(rename = "assistant")]
    Assistant {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_uuid: Option<String>,
        message: Message,
        timestamp: DateTime<Utc>,
        session_id: String,
    },
    /// System event (compaction, hook output, etc.).
    #[serde(rename = "system")]
    System {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_uuid: Option<String>,
        subtype: String,
        message: String,
        timestamp: DateTime<Utc>,
        session_id: String,
    },
    /// User-set session title.
    #[serde(rename = "custom-title")]
    CustomTitle {
        session_id: String,
        custom_title: String,
    },
    /// Auto-generated session title.
    #[serde(rename = "ai-title")]
    AiTitle {
        session_id: String,
        ai_title: String,
    },
    /// Conversation summary at a leaf node.
    #[serde(rename = "summary")]
    Summary {
        leaf_uuid: String,
        summary: String,
    },
    /// Last user prompt (for resume picker).
    #[serde(rename = "last-prompt")]
    LastPrompt {
        session_id: String,
        last_prompt: String,
    },
    /// Turn duration checkpoint (for consistency checks).
    #[serde(rename = "turn-duration")]
    TurnDuration {
        session_id: String,
        turn_index: u32,
        duration_ms: u64,
        message_count: usize,
        timestamp: DateTime<Utc>,
    },
}

/// Path for a JSONL transcript file.
fn transcript_path(id: &str) -> anyhow::Result<PathBuf> {
    if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("Invalid session ID for transcript");
    }
    Ok(sessions_dir().join(format!("{}.jsonl", id)))
}

/// Append a single entry to the JSONL transcript file.
///
/// Thread-safe: each call opens → appends → closes the file.
/// Creates the sessions directory and file if they don't exist.
pub fn append_transcript_entry(id: &str, entry: &TranscriptEntry) -> anyhow::Result<()> {
    use std::io::Write;

    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = transcript_path(id)?;

    let mut line = serde_json::to_string(entry)?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Load all entries from a JSONL transcript file.
///
/// Skips malformed lines (logs warning). Returns entries in file order.
pub fn load_transcript(id: &str) -> anyhow::Result<Vec<TranscriptEntry>> {
    let path = transcript_path(id)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)?;
    let mut entries = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                tracing::warn!("Skipping malformed transcript line {} in {}: {}", i + 1, id, e);
            }
        }
    }
    Ok(entries)
}

/// Rebuild a `SessionSnapshot` from a JSONL transcript.
///
/// Walks all entries to extract messages, metadata, and token usage.
pub fn rebuild_from_transcript(id: &str, model: &str) -> anyhow::Result<SessionSnapshot> {
    let entries = load_transcript(id)?;

    let mut messages = Vec::new();
    let mut custom_title = None;
    let mut ai_title = None;
    let mut summary = None;
    let mut last_prompt = None;
    let mut git_branch = None;
    let mut cwd = String::new();
    let mut created_at = Utc::now();
    let mut updated_at = Utc::now();
    let mut turn_count: u32 = 0;

    for (i, entry) in entries.iter().enumerate() {
        match entry {
            TranscriptEntry::User { message, timestamp, cwd: entry_cwd, git_branch: gb, .. } => {
                if i == 0 {
                    created_at = *timestamp;
                }
                updated_at = *timestamp;
                if let Some(c) = entry_cwd {
                    cwd = c.clone();
                }
                if git_branch.is_none() {
                    git_branch = gb.clone();
                }
                messages.push(message.clone());
            }
            TranscriptEntry::Assistant { message, timestamp, .. } => {
                updated_at = *timestamp;
                turn_count += 1;
                messages.push(message.clone());
            }
            TranscriptEntry::System { message: msg, timestamp, .. } => {
                updated_at = *timestamp;
                messages.push(Message::System(crate::message::SystemMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    message: msg.clone(),
                }));
            }
            TranscriptEntry::CustomTitle { custom_title: t, .. } => {
                custom_title = Some(t.clone());
            }
            TranscriptEntry::AiTitle { ai_title: t, .. } => {
                ai_title = Some(t.clone());
            }
            TranscriptEntry::Summary { summary: s, .. } => {
                summary = Some(s.clone());
            }
            TranscriptEntry::LastPrompt { last_prompt: p, .. } => {
                last_prompt = Some(p.clone());
            }
            TranscriptEntry::TurnDuration { .. } => {}
        }
    }

    let title = custom_title.clone()
        .or_else(|| ai_title.clone())
        .unwrap_or_else(|| title_from_messages(&messages));

    Ok(SessionSnapshot {
        id: id.to_string(),
        title,
        model: model.to_string(),
        cwd,
        created_at,
        updated_at,
        turn_count,
        input_tokens: 0,
        output_tokens: 0,
        model_usage: HashMap::new(),
        total_cost_usd: 0.0,
        messages,
        git_branch,
        custom_title,
        ai_title,
        summary,
        last_prompt,
    })
}

/// Set a custom title on a session (appends to JSONL transcript).
pub fn set_custom_title(id: &str, title: &str) -> anyhow::Result<()> {
    append_transcript_entry(id, &TranscriptEntry::CustomTitle {
        session_id: id.to_string(),
        custom_title: title.to_string(),
    })
}

/// Set a summary on a session (appends to JSONL transcript).
pub fn set_summary(id: &str, leaf_uuid: &str, summary: &str) -> anyhow::Result<()> {
    append_transcript_entry(id, &TranscriptEntry::Summary {
        leaf_uuid: leaf_uuid.to_string(),
        summary: summary.to_string(),
    })
}

// ── Prompt History ──────────────────────────────────────────────────────────

/// A single entry in the global prompt history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptHistoryEntry {
    /// Display text (first 200 chars of user prompt).
    pub display: String,
    /// Timestamp (millis since epoch).
    pub timestamp: i64,
    /// Project directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Session that this prompt belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Max entries per project in prompt history.
const MAX_HISTORY_PER_PROJECT: usize = 100;

/// Path to the global prompt history file.
fn prompt_history_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("prompt_history.jsonl")
}

/// Add a prompt to the global history.
pub fn add_to_prompt_history(entry: &PromptHistoryEntry) -> anyhow::Result<()> {
    use std::io::Write;

    let path = prompt_history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut line = serde_json::to_string(entry)?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Load prompt history, optionally filtered by project.
///
/// Returns entries in reverse chronological order (newest first).
/// Limits to MAX_HISTORY_PER_PROJECT per project.
pub fn get_prompt_history(project: Option<&str>) -> Vec<PromptHistoryEntry> {
    let path = prompt_history_path();
    if !path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut entries: Vec<PromptHistoryEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // Filter by project if specified
    if let Some(proj) = project {
        entries.retain(|e| e.project.as_deref() == Some(proj));
    }

    // Reverse to get newest first
    entries.reverse();

    // Limit
    entries.truncate(MAX_HISTORY_PER_PROJECT);
    entries
}

/// Search prompt history by keyword (case-insensitive).
pub fn search_prompt_history(query: &str) -> Vec<PromptHistoryEntry> {
    let path = prompt_history_path();
    if !path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let query_lower = query.to_lowercase();

    let mut entries: Vec<PromptHistoryEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<PromptHistoryEntry>(l).ok())
        .filter(|e| e.display.to_lowercase().contains(&query_lower))
        .collect();

    entries.reverse();
    entries.truncate(MAX_HISTORY_PER_PROJECT);
    entries
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract a title from the first user message (truncated to 60 chars).
pub fn title_from_messages(messages: &[Message]) -> String {
    for msg in messages {
        if let Message::User(u) = msg {
            for block in &u.content {
                if let crate::message::ContentBlock::Text { text } = block {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let title: String = trimmed.chars().take(60).collect();
                        if title.len() < trimmed.len() {
                            return format!("{}…", title);
                        }
                        return title;
                    }
                }
            }
        }
    }
    "Untitled session".to_string()
}

/// Format an age string like "2 hours ago", "3 days ago".
pub fn format_age(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_seconds() < 60 {
        "just now".to_string()
    } else if duration.num_minutes() < 60 {
        let m = duration.num_minutes();
        format!("{} min{} ago", m, if m == 1 { "" } else { "s" })
    } else if duration.num_hours() < 24 {
        let h = duration.num_hours();
        format!("{} hour{} ago", h, if h == 1 { "" } else { "s" })
    } else {
        let d = duration.num_days();
        format!("{} day{} ago", d, if d == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, UserMessage, AssistantMessage, SystemMessage, Message};
    use chrono::Duration;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: "u1".to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
        })
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: "a1".to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: None,
            usage: None,
        })
    }

    fn system_msg(text: &str) -> Message {
        Message::System(SystemMessage {
            uuid: "s1".to_string(),
            message: text.to_string(),
        })
    }

    // ── title_from_messages ─────────────────────────────────────────────

    #[test]
    fn title_from_messages_normal() {
        let msgs = vec![user_msg("Hello world")];
        assert_eq!(title_from_messages(&msgs), "Hello world");
    }

    #[test]
    fn title_from_messages_long_truncated() {
        let long = "a".repeat(80);
        let msgs = vec![user_msg(&long)];
        let title = title_from_messages(&msgs);
        // 60 chars + "…"
        assert!(title.ends_with('…'));
        let without_ellipsis: String = title.chars().take(60).collect();
        assert_eq!(without_ellipsis, "a".repeat(60));
    }

    #[test]
    fn title_from_messages_exactly_60_no_truncation() {
        let exact = "b".repeat(60);
        let msgs = vec![user_msg(&exact)];
        assert_eq!(title_from_messages(&msgs), exact);
    }

    #[test]
    fn title_from_messages_empty() {
        let msgs: Vec<Message> = vec![];
        assert_eq!(title_from_messages(&msgs), "Untitled session");
    }

    #[test]
    fn title_from_messages_whitespace_only() {
        let msgs = vec![user_msg("   ")];
        assert_eq!(title_from_messages(&msgs), "Untitled session");
    }

    #[test]
    fn title_from_messages_skips_assistant() {
        let msgs = vec![
            assistant_msg("I am assistant"),
            user_msg("Actual question"),
        ];
        assert_eq!(title_from_messages(&msgs), "Actual question");
    }

    #[test]
    fn title_from_messages_skips_system() {
        let msgs = vec![
            system_msg("System prompt"),
            user_msg("User query"),
        ];
        assert_eq!(title_from_messages(&msgs), "User query");
    }

    #[test]
    fn title_from_messages_trims_whitespace() {
        let msgs = vec![user_msg("  trimmed  ")];
        assert_eq!(title_from_messages(&msgs), "trimmed");
    }

    // ── format_age ──────────────────────────────────────────────────────

    #[test]
    fn format_age_just_now() {
        let dt = Utc::now() - Duration::seconds(30);
        assert_eq!(format_age(&dt), "just now");
    }

    #[test]
    fn format_age_just_now_zero() {
        let dt = Utc::now();
        assert_eq!(format_age(&dt), "just now");
    }

    #[test]
    fn format_age_singular_min() {
        let dt = Utc::now() - Duration::minutes(1);
        assert_eq!(format_age(&dt), "1 min ago");
    }

    #[test]
    fn format_age_plural_mins() {
        let dt = Utc::now() - Duration::minutes(5);
        assert_eq!(format_age(&dt), "5 mins ago");
    }

    #[test]
    fn format_age_singular_hour() {
        let dt = Utc::now() - Duration::hours(1);
        assert_eq!(format_age(&dt), "1 hour ago");
    }

    #[test]
    fn format_age_plural_hours() {
        let dt = Utc::now() - Duration::hours(3);
        assert_eq!(format_age(&dt), "3 hours ago");
    }

    #[test]
    fn format_age_singular_day() {
        let dt = Utc::now() - Duration::days(1);
        assert_eq!(format_age(&dt), "1 day ago");
    }

    #[test]
    fn format_age_plural_days() {
        let dt = Utc::now() - Duration::days(7);
        assert_eq!(format_age(&dt), "7 days ago");
    }

    // ── session_path ────────────────────────────────────────────────────

    #[test]
    fn session_path_valid() {
        let result = session_path("abc-123_def");
        assert!(result.is_ok());
        let p = result.unwrap();
        assert!(p.to_string_lossy().ends_with("abc-123_def.json"));
    }

    #[test]
    fn session_path_invalid_traversal() {
        assert!(session_path("../foo").is_err());
    }

    #[test]
    fn session_path_invalid_special_chars() {
        assert!(session_path("hello world").is_err()); // space
        assert!(session_path("foo/bar").is_err());      // slash
        assert!(session_path("a@b").is_err());           // at sign
    }

    // ── SessionSnapshot serde roundtrip ─────────────────────────────────

    #[test]
    fn session_snapshot_serde_roundtrip() {
        let now = Utc::now();
        let snap = SessionSnapshot {
            id: "test-session".to_string(),
            title: "Hello".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            cwd: "/home/user".to_string(),
            created_at: now,
            updated_at: now,
            turn_count: 3,
            input_tokens: 100,
            output_tokens: 200,
            model_usage: HashMap::new(),
            total_cost_usd: 0.05,
            messages: vec![user_msg("Hi")],
            git_branch: Some("main".to_string()),
            custom_title: None,
            ai_title: Some("AI title".to_string()),
            summary: None,
            last_prompt: Some("Hi".to_string()),
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let deser: SessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deser.id, snap.id);
        assert_eq!(deser.title, snap.title);
        assert_eq!(deser.turn_count, 3);
        assert_eq!(deser.messages.len(), 1);
        assert_eq!(deser.git_branch.as_deref(), Some("main"));
        assert_eq!(deser.ai_title.as_deref(), Some("AI title"));
    }

    // ── SessionMeta serde ───────────────────────────────────────────────

    #[test]
    fn session_meta_serde() {
        let now = Utc::now();
        let meta = SessionMeta {
            id: "m1".to_string(),
            title: "Meta test".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            cwd: "/tmp".to_string(),
            created_at: now,
            updated_at: now,
            turn_count: 1,
            message_count: 5,
            total_cost_usd: 0.0,
            git_branch: None,
            custom_title: None,
            summary: None,
            last_prompt: None,
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let deser: SessionMeta = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deser.id, "m1");
        assert_eq!(deser.message_count, 5);
    }

    #[test]
    fn session_meta_missing_cost_uses_default() {
        // total_cost_usd has #[serde(default)], so omitting it should work
        let json = r#"{
            "id": "x",
            "title": "t",
            "model": "m",
            "cwd": "/",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "turn_count": 0,
            "message_count": 0
        }"#;
        let meta: SessionMeta = serde_json::from_str(json).expect("deserialize");
        assert_eq!(meta.total_cost_usd, 0.0);
    }

    // ── SessionModelUsage default ───────────────────────────────────────

    #[test]
    fn session_model_usage_default() {
        let usage = SessionModelUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.api_calls, 0);
        assert_eq!(usage.cost_usd, 0.0);
    }

    // ── atomic_write ────────────────────────────────────────────────────

    #[test]
    fn atomic_write_creates_file() {
        let target = std::env::temp_dir().join("claude_test_atomic_write.json");
        let _ = std::fs::remove_file(&target);

        atomic_write(&target, b"hello world").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello world");

        // No .tmp file should remain
        let tmp_path = target.parent().unwrap().join(".claude_test_atomic_write.json.tmp");
        assert!(!tmp_path.exists(), "temp file should be cleaned up");

        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let tmp = std::env::temp_dir().join("claude_test_atomic_replace.json");
        std::fs::write(&tmp, "old content").unwrap();

        atomic_write(&tmp, b"new content").unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new content");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── TranscriptEntry serde ───────────────────────────────────────────

    #[test]
    fn transcript_entry_user_serde() {
        let entry = TranscriptEntry::User {
            uuid: "u1".to_string(),
            parent_uuid: None,
            message: user_msg("Hello"),
            timestamp: Utc::now(),
            session_id: "s1".to_string(),
            cwd: Some("/tmp".to_string()),
            git_branch: Some("main".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"user\""));
        let deser: TranscriptEntry = serde_json::from_str(&json).unwrap();
        match deser {
            TranscriptEntry::User { uuid, cwd, .. } => {
                assert_eq!(uuid, "u1");
                assert_eq!(cwd.as_deref(), Some("/tmp"));
            }
            _ => panic!("Expected User variant"),
        }
    }

    #[test]
    fn transcript_entry_assistant_serde() {
        let entry = TranscriptEntry::Assistant {
            uuid: "a1".to_string(),
            parent_uuid: Some("u1".to_string()),
            message: assistant_msg("Hi"),
            timestamp: Utc::now(),
            session_id: "s1".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"assistant\""));
    }

    #[test]
    fn transcript_entry_custom_title_serde() {
        let entry = TranscriptEntry::CustomTitle {
            session_id: "s1".to_string(),
            custom_title: "My Session".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"custom-title\""));
        let deser: TranscriptEntry = serde_json::from_str(&json).unwrap();
        match deser {
            TranscriptEntry::CustomTitle { custom_title, .. } => {
                assert_eq!(custom_title, "My Session");
            }
            _ => panic!("Expected CustomTitle"),
        }
    }

    #[test]
    fn transcript_entry_summary_serde() {
        let entry = TranscriptEntry::Summary {
            leaf_uuid: "leaf1".to_string(),
            summary: "We discussed Rust".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deser: TranscriptEntry = serde_json::from_str(&json).unwrap();
        match deser {
            TranscriptEntry::Summary { summary, .. } => assert_eq!(summary, "We discussed Rust"),
            _ => panic!("Expected Summary"),
        }
    }

    #[test]
    fn transcript_entry_turn_duration_serde() {
        let entry = TranscriptEntry::TurnDuration {
            session_id: "s1".to_string(),
            turn_index: 3,
            duration_ms: 1500,
            message_count: 7,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"turn-duration\""));
    }

    // ── JSONL append / load ─────────────────────────────────────────────

    #[test]
    fn transcript_path_valid() {
        let p = transcript_path("test-session").unwrap();
        assert!(p.to_string_lossy().ends_with("test-session.jsonl"));
    }

    #[test]
    fn transcript_path_invalid() {
        assert!(transcript_path("../bad").is_err());
    }

    #[test]
    fn append_and_load_transcript() {
        let id = format!("test-transcript-{}", uuid::Uuid::new_v4().simple());
        let now = Utc::now();

        // Append two entries
        let e1 = TranscriptEntry::User {
            uuid: "u1".to_string(),
            parent_uuid: None,
            message: user_msg("Hello"),
            timestamp: now,
            session_id: id.clone(),
            cwd: Some("/tmp".to_string()),
            git_branch: None,
        };
        let e2 = TranscriptEntry::Assistant {
            uuid: "a1".to_string(),
            parent_uuid: Some("u1".to_string()),
            message: assistant_msg("Hi there"),
            timestamp: now,
            session_id: id.clone(),
        };

        append_transcript_entry(&id, &e1).unwrap();
        append_transcript_entry(&id, &e2).unwrap();

        // Load and verify
        let entries = load_transcript(&id).unwrap();
        assert_eq!(entries.len(), 2);

        // Cleanup
        let _ = std::fs::remove_file(transcript_path(&id).unwrap());
    }

    #[test]
    fn load_transcript_nonexistent() {
        let entries = load_transcript("does-not-exist-99999").unwrap();
        assert!(entries.is_empty());
    }

    // ── rebuild_from_transcript ──────────────────────────────────────────

    #[test]
    fn rebuild_from_transcript_roundtrip() {
        let id = format!("test-rebuild-{}", uuid::Uuid::new_v4().simple());
        let now = Utc::now();

        append_transcript_entry(&id, &TranscriptEntry::User {
            uuid: "u1".to_string(),
            parent_uuid: None,
            message: user_msg("Build me a thing"),
            timestamp: now,
            session_id: id.clone(),
            cwd: Some("/project".to_string()),
            git_branch: Some("feature".to_string()),
        }).unwrap();

        append_transcript_entry(&id, &TranscriptEntry::Assistant {
            uuid: "a1".to_string(),
            parent_uuid: Some("u1".to_string()),
            message: assistant_msg("Sure!"),
            timestamp: now,
            session_id: id.clone(),
        }).unwrap();

        append_transcript_entry(&id, &TranscriptEntry::CustomTitle {
            session_id: id.clone(),
            custom_title: "My Build".to_string(),
        }).unwrap();

        let snap = rebuild_from_transcript(&id, "claude-sonnet").unwrap();
        assert_eq!(snap.messages.len(), 2);
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.title, "My Build");
        assert_eq!(snap.git_branch.as_deref(), Some("feature"));
        assert_eq!(snap.cwd, "/project");

        let _ = std::fs::remove_file(transcript_path(&id).unwrap());
    }

    // ── Prompt history ──────────────────────────────────────────────────

    #[test]
    fn prompt_history_entry_serde() {
        let entry = PromptHistoryEntry {
            display: "hello world".to_string(),
            timestamp: 1704067200000,
            project: Some("/home/user/project".to_string()),
            session_id: Some("s1".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deser: PromptHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.display, "hello world");
        assert_eq!(deser.timestamp, 1704067200000);
    }

    #[test]
    fn prompt_history_entry_minimal() {
        let json = r#"{"display":"hi","timestamp":0}"#;
        let entry: PromptHistoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.display, "hi");
        assert!(entry.project.is_none());
        assert!(entry.session_id.is_none());
    }

    // ── set_custom_title / set_summary ────────────────────────────────

    #[test]
    fn set_custom_title_appends_to_transcript() {
        let id = format!("test-title-{}", uuid::Uuid::new_v4().simple());
        set_custom_title(&id, "My Title").unwrap();

        let entries = load_transcript(&id).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::CustomTitle { custom_title, .. } => {
                assert_eq!(custom_title, "My Title");
            }
            _ => panic!("Expected CustomTitle"),
        }

        let _ = std::fs::remove_file(transcript_path(&id).unwrap());
    }

    #[test]
    fn set_summary_appends_to_transcript() {
        let id = format!("test-summary-{}", uuid::Uuid::new_v4().simple());
        set_summary(&id, "leaf1", "We discussed Rust porting").unwrap();

        let entries = load_transcript(&id).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            TranscriptEntry::Summary { summary, leaf_uuid, .. } => {
                assert_eq!(summary, "We discussed Rust porting");
                assert_eq!(leaf_uuid, "leaf1");
            }
            _ => panic!("Expected Summary"),
        }

        let _ = std::fs::remove_file(transcript_path(&id).unwrap());
    }

    // ── SessionSnapshot new fields backward compat ───────────────────

    #[test]
    fn snapshot_without_new_fields_deserializes() {
        let json = r#"{
            "id": "old",
            "title": "t",
            "model": "m",
            "cwd": "/",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "turn_count": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "total_cost_usd": 0,
            "messages": []
        }"#;
        let snap: SessionSnapshot = serde_json::from_str(json).unwrap();
        assert!(snap.git_branch.is_none());
        assert!(snap.custom_title.is_none());
        assert!(snap.summary.is_none());
    }
}
