//! TeamStatus tool — lists team members, their status, and aggregated results.

use serde_json::{json, Value};

use clawed_core::tool::{Tool, ToolContext, ToolResult};

use crate::helpers;

/// Tool for querying team status and aggregated results.
pub struct TeamStatusTool;

#[async_trait::async_trait]
impl Tool for TeamStatusTool {
    fn name(&self) -> &str {
        "TeamStatus"
    }

    fn description(&self) -> &str {
        "Get the status of a team: list all members with their roles, \
         activity status, and metadata. Useful for the coordinator to \
         monitor team progress."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Name of the team to query."
                }
            },
            "required": ["team_name"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let team_name = input["team_name"]
            .as_str()
            .unwrap_or("")
            .trim();

        if team_name.is_empty() {
            return Ok(ToolResult::error("team_name must be a non-empty string"));
        }

        let team = match helpers::read_team_file(team_name) {
            Ok(t) => t,
            Err(_) => {
                return Ok(ToolResult::error(format!(
                    "Team '{}' not found or config is invalid",
                    team_name
                )));
            }
        };

        let mut output = String::new();
        output.push_str(&format!("# Team: {}\n", team.name));
        if let Some(ref desc) = team.description {
            output.push_str(&format!("Description: {}\n", desc));
        }
        output.push_str(&format!("Lead: {}\n", team.lead_agent_id));
        output.push_str(&format!("Members: {}\n\n", team.members.len()));

        // Member table
        let active_count = team.members.iter().filter(|m| m.is_active).count();
        let inactive_count = team.members.len() - active_count;
        output.push_str(&format!(
            "Active: {} | Inactive: {}\n\n",
            active_count, inactive_count
        ));

        for member in &team.members {
            let status = if member.is_active { "🟢 active" } else { "⚪ idle" };
            let role = member.agent_type.as_deref().unwrap_or("general");
            let model = member.model.as_deref().unwrap_or("default");
            let color = member.color.as_deref().unwrap_or("-");

            output.push_str(&format!(
                "- {} ({}) [{}] status={} model={} color={}\n",
                member.name, member.agent_id, role, status, model, color
            ));
        }

        // Team allowed paths
        if !team.team_allowed_paths.is_empty() {
            output.push_str("\nShared Allowed Paths:\n");
            for tap in &team.team_allowed_paths {
                output.push_str(&format!(
                    "  {} → {} (by {} at {})\n",
                    tap.path, tap.tool_name, tap.added_by, tap.added_at
                ));
            }
        }

        Ok(ToolResult::text(output))
    }
}

/// Format an aggregated summary of team results from completed agents.
///
/// Used by the coordinator to present a unified view of all finished work.
pub fn format_team_summary(
    team_name: &str,
    results: &[(String, String, u64, u32)], // (agent_name, result, tokens, tool_uses)
) -> String {
    let mut output = String::new();
    output.push_str(&format!("## Team '{}' — Aggregated Results\n\n", team_name));

    let total_tokens: u64 = results.iter().map(|(_, _, t, _)| t).sum();
    let total_tools: u32 = results.iter().map(|(_, _, _, u)| u).sum();
    output.push_str(&format!(
        "**{} agents completed** | Total tokens: {} | Total tool uses: {}\n\n",
        results.len(),
        total_tokens,
        total_tools
    ));

    for (name, result, tokens, tools) in results {
        let preview = if result.len() > 300 {
            let truncated: String = result.chars().take(300).collect();
            format!("{}...", truncated)
        } else {
            result.clone()
        };
        output.push_str(&format!(
            "### {} (tokens: {}, tools: {})\n{}\n\n",
            name, tokens, tools, preview
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use clawed_core::permissions::PermissionMode;
    use std::path::PathBuf;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("."),
            abort_signal: clawed_core::tool::AbortSignal::default(),
            permission_mode: PermissionMode::BypassAll,
            messages: vec![],
        }
    }

    fn result_text(result: &ToolResult) -> String {
        result.content.iter().filter_map(|c| {
            if let clawed_core::message::ToolResultContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        }).collect::<Vec<_>>().join("")
    }

    #[test]
    fn tool_metadata() {
        let tool = TeamStatusTool;
        assert_eq!(tool.name(), "TeamStatus");
        assert!(tool.is_read_only());
    }

    #[tokio::test]
    async fn status_nonexistent_team() {
        let tool = TeamStatusTool;
        let result = tool
            .call(json!({"team_name": "nonexistent-xyz-42"}), &test_context())
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn status_existing_team() {
        let name = format!("test-status-{}", uuid::Uuid::new_v4().as_simple());
        let team = TeamFile {
            name: name.clone(),
            description: Some("Test team for status".into()),
            created_at: 0,
            lead_agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
            lead_session_id: None,
            members: vec![
                TeamMember {
                    agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
                    name: TEAM_LEAD_NAME.into(),
                    agent_type: Some("coordinator".into()),
                    model: None, prompt: None, color: None,
                    joined_at: 0, cwd: ".".into(), session_id: None,
                    is_active: true, backend_type: None,
                },
                TeamMember {
                    agent_id: format_agent_id("researcher", &name),
                    name: "researcher".into(),
                    agent_type: Some("researcher".into()),
                    model: Some("claude-sonnet-4-20250514".into()),
                    prompt: None, color: Some("cyan".into()),
                    joined_at: 0, cwd: ".".into(), session_id: None,
                    is_active: true, backend_type: None,
                },
            ],
            team_allowed_paths: vec![],
        };
        helpers::write_team_file(&name, &team).unwrap();

        let tool = TeamStatusTool;
        let result = tool.call(json!({"team_name": &name}), &test_context()).await.unwrap();
        let text = result_text(&result);
        assert!(text.contains("# Team:"));
        assert!(text.contains("researcher"));
        assert!(text.contains("active"));
        assert!(text.contains("Members: 2"));

        let _ = helpers::cleanup_team_directories(&name);
    }

    #[test]
    fn format_team_summary_basic() {
        let results = vec![
            ("researcher".into(), "Found 3 bugs".into(), 500u64, 5u32),
            ("fixer".into(), "Fixed all issues".into(), 800, 12),
        ];
        let summary = format_team_summary("alpha", &results);
        assert!(summary.contains("alpha"));
        assert!(summary.contains("2 agents completed"));
        assert!(summary.contains("Total tokens: 1300"));
        assert!(summary.contains("researcher"));
        assert!(summary.contains("fixer"));
    }

    #[test]
    fn format_team_summary_truncates_long_results() {
        let long_result = "x".repeat(500);
        let results = vec![("agent".into(), long_result, 100, 1)];
        let summary = format_team_summary("t", &results);
        assert!(summary.contains("..."));
        assert!(summary.len() < 600);
    }
}
