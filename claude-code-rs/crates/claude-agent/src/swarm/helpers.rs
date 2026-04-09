//! Team file I/O helpers — read / write / discover team configs on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use super::types::{TeamFile, TeamMember, sanitize_name};

/// Get the teams root directory: `~/.claude/teams/`.
pub fn teams_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".claude").join("teams"))
}

/// Get the team directory for a specific team.
pub fn team_dir(team_name: &str) -> Result<PathBuf> {
    Ok(teams_dir()?.join(sanitize_name(team_name)))
}

/// Get the path to a team's config.json.
pub fn team_file_path(team_name: &str) -> Result<PathBuf> {
    Ok(team_dir(team_name)?.join("config.json"))
}

/// Read a team config from disk.
pub fn read_team_file(team_name: &str) -> Result<TeamFile> {
    let path = team_file_path(team_name)?;
    read_team_file_at(&path)
}

/// Read a team config from a specific path.
pub fn read_team_file_at(path: &Path) -> Result<TeamFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read team file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Invalid team config JSON: {}", path.display()))
}

/// Write a team config to disk (creates directories as needed).
pub fn write_team_file(team_name: &str, team: &TeamFile) -> Result<PathBuf> {
    let path = team_file_path(team_name)?;
    write_team_file_at(&path, team)?;
    Ok(path)
}

/// Write a team config to a specific path.
pub fn write_team_file_at(path: &Path, team: &TeamFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create team directory: {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(team)
        .context("Failed to serialize team config")?;
    std::fs::write(path, json)
        .with_context(|| format!("Failed to write team file: {}", path.display()))?;
    debug!("Wrote team config to {}", path.display());
    Ok(())
}

/// Check if a team exists on disk.
pub fn team_exists(team_name: &str) -> bool {
    team_file_path(team_name)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Add a member to a team's config file.
pub fn add_member(team_name: &str, member: TeamMember) -> Result<()> {
    let mut team = read_team_file(team_name)?;
    team.members.push(member);
    write_team_file(team_name, &team)?;
    Ok(())
}

/// Remove a member by agent_id.
pub fn remove_member(team_name: &str, agent_id: &str) -> Result<bool> {
    let mut team = read_team_file(team_name)?;
    let before = team.members.len();
    team.members.retain(|m| m.agent_id != agent_id);
    let removed = team.members.len() < before;
    if removed {
        write_team_file(team_name, &team)?;
    }
    Ok(removed)
}

/// Mark a member as inactive.
pub fn deactivate_member(team_name: &str, agent_id: &str) -> Result<()> {
    let mut team = read_team_file(team_name)?;
    if let Some(member) = team.members.iter_mut().find(|m| m.agent_id == agent_id) {
        member.is_active = false;
        write_team_file(team_name, &team)?;
    }
    Ok(())
}

/// Get active (non-lead) members of a team.
pub fn active_teammates(team: &TeamFile) -> Vec<&TeamMember> {
    team.members
        .iter()
        .filter(|m| m.agent_id != team.lead_agent_id && m.is_active)
        .collect()
}

/// Clean up team directories from disk.
pub fn cleanup_team_directories(team_name: &str) -> Result<()> {
    let dir = team_dir(team_name)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("Failed to remove team directory: {}", dir.display()))?;
        debug!("Cleaned up team directory: {}", dir.display());
    }
    Ok(())
}

/// List all teams on disk.
pub fn list_teams() -> Result<Vec<String>> {
    let dir = teams_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut teams = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let config = entry.path().join("config.json");
            if config.exists() {
                if let Some(name) = entry.file_name().to_str() {
                    teams.push(name.to_string());
                }
            }
        }
    }
    Ok(teams)
}

/// Generate a unique team name by appending a suffix if collision.
pub fn generate_unique_team_name(base_name: &str) -> String {
    let sanitized = sanitize_name(base_name);
    if !team_exists(&sanitized) {
        return sanitized;
    }
    // Append incrementing suffix
    for i in 2..=100 {
        let candidate = format!("{sanitized}-{i}");
        if !team_exists(&candidate) {
            return candidate;
        }
    }
    warn!("Could not generate unique team name from '{}' after 100 attempts", base_name);
    format!("{}-{}", sanitized, uuid::Uuid::new_v4().as_simple())
}

/// Available teammate colors for terminal display.
pub const TEAMMATE_COLORS: &[&str] = &[
    "cyan", "magenta", "yellow", "green", "blue", "red",
    "bright-cyan", "bright-magenta", "bright-yellow", "bright-green",
];

/// Pick a color for a new teammate (round-robin from available colors).
pub fn pick_teammate_color(existing_members: &[TeamMember]) -> &'static str {
    let used: Vec<&str> = existing_members
        .iter()
        .filter_map(|m| m.color.as_deref())
        .collect();
    TEAMMATE_COLORS
        .iter()
        .find(|c| !used.contains(c))
        .unwrap_or(&TEAMMATE_COLORS[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{TeamFile, TeamMember, TEAM_LEAD_NAME, format_agent_id};

    fn make_test_team(name: &str) -> TeamFile {
        TeamFile {
            name: name.to_string(),
            description: None,
            created_at: 1700000000000,
            lead_agent_id: format_agent_id(TEAM_LEAD_NAME, name),
            lead_session_id: None,
            members: vec![TeamMember {
                agent_id: format_agent_id(TEAM_LEAD_NAME, name),
                name: TEAM_LEAD_NAME.to_string(),
                agent_type: None,
                model: None,
                prompt: None,
                color: None,
                joined_at: 1700000000000,
                cwd: ".".into(),
                session_id: None,
                is_active: true,
                backend_type: Some("in-process".into()),
            }],
            team_allowed_paths: vec![],
        }
    }

    #[test]
    fn write_and_read_team_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-team").join("config.json");
        let team = make_test_team("test-team");

        write_team_file_at(&path, &team).unwrap();
        let loaded = read_team_file_at(&path).unwrap();
        assert_eq!(loaded.name, "test-team");
        assert_eq!(loaded.members.len(), 1);
    }

    #[test]
    fn active_teammates_excludes_lead() {
        let mut team = make_test_team("t");
        team.members.push(TeamMember {
            agent_id: "researcher@t".into(),
            name: "researcher".into(),
            agent_type: Some("researcher".into()),
            model: None,
            prompt: None,
            color: Some("cyan".into()),
            joined_at: 1700000001000,
            cwd: ".".into(),
            session_id: None,
            is_active: true,
            backend_type: None,
        });
        team.members.push(TeamMember {
            agent_id: "idle@t".into(),
            name: "idle".into(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            joined_at: 1700000002000,
            cwd: ".".into(),
            session_id: None,
            is_active: false,
            backend_type: None,
        });

        let active = active_teammates(&team);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "researcher");
    }

    #[test]
    fn pick_color_avoids_used() {
        let members = vec![
            TeamMember {
                agent_id: "a".into(), name: "a".into(), agent_type: None, model: None,
                prompt: None, color: Some("cyan".into()), joined_at: 0, cwd: ".".into(),
                session_id: None, is_active: true, backend_type: None,
            },
        ];
        let color = pick_teammate_color(&members);
        assert_ne!(color, "cyan");
        assert_eq!(color, "magenta"); // next in sequence
    }

    #[test]
    fn list_teams_empty_dir() {
        // If teams dir doesn't exist, returns empty
        let result = list_teams();
        // May or may not have teams in home dir, just ensure no error
        assert!(result.is_ok());
    }

    #[test]
    fn generate_unique_name_no_collision() {
        // Random name unlikely to exist
        let name = generate_unique_team_name("xyzzy-nonexistent-42");
        assert_eq!(name, "xyzzy-nonexistent-42");
    }
}
