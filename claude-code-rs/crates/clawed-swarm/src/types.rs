//! Core data types for swarm team management.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// On-disk team configuration persisted as `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamFile {
    /// Human-readable team name.
    pub name: String,
    /// Optional team description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unix timestamp (ms) when the team was created.
    pub created_at: u64,
    /// Deterministic agent ID of the team lead (e.g. "team-lead@my-team").
    pub lead_agent_id: String,
    /// Session ID of the leader (for cross-process discovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    /// Team members (including the lead as the first entry).
    #[serde(default)]
    pub members: Vec<TeamMember>,
    /// Paths that teammates are allowed to edit (shared permissions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub team_allowed_paths: Vec<TeamAllowedPath>,
}

/// A single team member entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    /// Deterministic agent ID (e.g. "researcher@my-team").
    pub agent_id: String,
    /// Human-readable display name.
    pub name: String,
    /// Role / specialization (e.g. "researcher", "test-runner").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Model used by this member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The prompt given to this member when spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Display color for terminal UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Unix timestamp (ms) when this member joined.
    pub joined_at: u64,
    /// Working directory for this member.
    pub cwd: String,
    /// Session ID for cross-process messaging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Whether this member is currently active.
    #[serde(default = "default_true")]
    pub is_active: bool,
    /// Backend type: "in-process", "tmux", "iterm2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_type: Option<String>,
}

/// A path that teammates are collectively allowed to edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamAllowedPath {
    pub path: String,
    pub tool_name: String,
    pub added_by: String,
    pub added_at: u64,
}

/// Live team context held in memory by the coordinator.
#[derive(Debug, Clone, Default)]
pub struct TeamContext {
    /// Current team name (None if no team active).
    pub team_name: Option<String>,
    /// Path to the team config file.
    pub team_file_path: Option<String>,
    /// Lead agent ID.
    pub lead_agent_id: Option<String>,
    /// Map of teammate name → agent_id for quick lookup.
    pub teammates: HashMap<String, String>,
}

impl TeamContext {
    pub fn is_active(&self) -> bool {
        self.team_name.is_some()
    }

    pub fn clear(&mut self) {
        self.team_name = None;
        self.team_file_path = None;
        self.lead_agent_id = None;
        self.teammates.clear();
    }
}

fn default_true() -> bool {
    true
}

/// Team lead name constant.
pub const TEAM_LEAD_NAME: &str = "team-lead";

/// Format a deterministic agent ID: `{name}@{team_name}`.
pub fn format_agent_id(name: &str, team_name: &str) -> String {
    format!("{}@{}", sanitize_agent_name(name), sanitize_name(team_name))
}

/// Sanitize a name for use in file paths and tmux pane names.
/// Replaces non-alphanumeric chars with hyphens and lowercases.
pub fn sanitize_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push('-');
        }
    }
    // Collapse consecutive hyphens
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_hyphen = false;
    for ch in result.chars() {
        if ch == '-' {
            if !prev_hyphen {
                collapsed.push(ch);
            }
            prev_hyphen = true;
        } else {
            collapsed.push(ch);
            prev_hyphen = false;
        }
    }
    collapsed.trim_matches('-').to_string()
}

/// Sanitize agent name: replace `@` with `-`.
pub fn sanitize_agent_name(name: &str) -> String {
    name.replace('@', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_basic() {
        assert_eq!(sanitize_name("My Team!"), "my-team");
        assert_eq!(sanitize_name("test--team"), "test-team");
        assert_eq!(sanitize_name("  spaces  "), "spaces");
    }

    #[test]
    fn sanitize_name_already_clean() {
        assert_eq!(sanitize_name("alpha-team"), "alpha-team");
    }

    #[test]
    fn format_agent_id_basic() {
        assert_eq!(format_agent_id("researcher", "my-team"), "researcher@my-team");
        assert_eq!(format_agent_id(TEAM_LEAD_NAME, "Alpha Squad"), "team-lead@alpha-squad");
    }

    #[test]
    fn sanitize_agent_name_replaces_at() {
        assert_eq!(sanitize_agent_name("lead@team"), "lead-team");
    }

    #[test]
    fn team_context_lifecycle() {
        let mut ctx = TeamContext::default();
        assert!(!ctx.is_active());

        ctx.team_name = Some("alpha".into());
        ctx.lead_agent_id = Some("team-lead@alpha".into());
        assert!(ctx.is_active());

        ctx.clear();
        assert!(!ctx.is_active());
        assert!(ctx.teammates.is_empty());
    }

    #[test]
    fn team_file_roundtrip() {
        let tf = TeamFile {
            name: "test-team".into(),
            description: Some("A test team".into()),
            created_at: 1700000000000,
            lead_agent_id: "team-lead@test-team".into(),
            lead_session_id: Some("session-123".into()),
            members: vec![TeamMember {
                agent_id: "team-lead@test-team".into(),
                name: TEAM_LEAD_NAME.into(),
                agent_type: None,
                model: Some("claude-sonnet-4-20250514".into()),
                prompt: None,
                color: None,
                joined_at: 1700000000000,
                cwd: "/project".into(),
                session_id: Some("session-123".into()),
                is_active: true,
                backend_type: Some("in-process".into()),
            }],
            team_allowed_paths: vec![],
        };
        let json = serde_json::to_string_pretty(&tf).unwrap();
        let deser: TeamFile = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "test-team");
        assert_eq!(deser.members.len(), 1);
        assert_eq!(deser.lead_agent_id, "team-lead@test-team");
    }
}
