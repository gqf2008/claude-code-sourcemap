//! Session persistence — save/restore conversation sessions to disk.
//!
//! Sessions are stored as JSON files under `~/.claude/sessions/`.
//! Each file contains the full conversation state: messages, model, cwd,
//! token usage, and turn count.
//!
//! A lightweight manifest file (`index.json`) caches session metadata to
//! avoid reading every session file when listing.
//!
//! Aligned with TS `sessionStorage.ts` — simplified to JSON (not JSONL)
//! since the Rust port doesn't need streaming append or sub-agent metadata.

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
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to save session manifest: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to serialize session manifest: {}", e);
        }
    }
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
pub fn save_session(session: &SessionSnapshot) -> anyhow::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_path(&session.id)?;
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(&path, json)?;

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
    })
}

// ── Delete ───────────────────────────────────────────────────────────────────

/// Delete a saved session and remove it from the manifest.
pub fn delete_session(id: &str) -> anyhow::Result<()> {
    let path = session_path(id)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    remove_manifest_entry(id);
    Ok(())
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
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let deser: SessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deser.id, snap.id);
        assert_eq!(deser.title, snap.title);
        assert_eq!(deser.turn_count, 3);
        assert_eq!(deser.messages.len(), 1);
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
}
