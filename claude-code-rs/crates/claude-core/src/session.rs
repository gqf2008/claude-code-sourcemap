//! Session persistence — save/restore conversation sessions to disk.
//!
//! Sessions are stored as JSON files under `~/.claude/sessions/`.
//! Each file contains the full conversation state: messages, model, cwd,
//! token usage, and turn count.
//!
//! Aligned with TS `sessionStorage.ts` — simplified to JSON (not JSONL)
//! since the Rust port doesn't need streaming append or sub-agent metadata.

use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::message::Message;

// ── Data types ───────────────────────────────────────────────────────────────

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
fn session_path(id: &str) -> anyhow::Result<PathBuf> {
    // Validate session ID to prevent path traversal
    if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("Invalid session ID: must be alphanumeric, dash, or underscore");
    }
    Ok(sessions_dir().join(format!("{}.json", id)))
}

// ── Save ─────────────────────────────────────────────────────────────────────

/// Save a session snapshot to disk.
pub fn save_session(session: &SessionSnapshot) -> anyhow::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_path(&session.id)?;
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(&path, json)?;
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
pub fn list_sessions() -> Vec<SessionMeta> {
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
            read_session_meta(&path)
        })
        .collect();

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
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
    })
}

// ── Delete ───────────────────────────────────────────────────────────────────

/// Delete a saved session.
pub fn delete_session(id: &str) -> anyhow::Result<()> {
    let path = session_path(id)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
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
