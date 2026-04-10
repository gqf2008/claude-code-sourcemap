//! TeamDelete tool — deletes a team and cleans up resources.

use serde_json::{json, Value};
use tracing::info;

use claude_core::tool::{Tool, ToolContext, ToolResult};

use crate::helpers;

/// Tool for deleting an agent team in coordinator mode.
pub struct TeamDeleteTool;

#[async_trait::async_trait]
impl Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        "TeamDelete"
    }

    fn description(&self) -> &str {
        "Delete the current agent team. All team directories and config files \
         will be cleaned up. Fails if there are still active teammates — \
         stop them first via TaskStop."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Name of the team to delete."
                }
            },
            "required": ["team_name"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let team_name = input["team_name"]
            .as_str()
            .unwrap_or("")
            .trim();

        if team_name.is_empty() {
            return Ok(ToolResult::error("team_name must be a non-empty string"));
        }

        // Read team config
        let team = match helpers::read_team_file(team_name) {
            Ok(t) => t,
            Err(_) => {
                return Ok(ToolResult::error(format!(
                    "Team '{}' not found or config is invalid",
                    team_name
                )));
            }
        };

        // Check for active teammates (non-lead members that are still active)
        let active = helpers::active_teammates(&team);
        if !active.is_empty() {
            let names: Vec<&str> = active.iter().map(|m| m.name.as_str()).collect();
            return Ok(ToolResult::error(format!(
                "Cannot delete team '{}' — {} active teammate(s): {}. \
                 Stop them first with TaskStop.",
                team_name,
                active.len(),
                names.join(", ")
            )));
        }

        // Clean up team directories
        helpers::cleanup_team_directories(team_name)?;

        info!(team_name = %team_name, "Deleted team");

        Ok(ToolResult::text(format!(
            "Team '{}' deleted successfully. All team files have been cleaned up.",
            team_name
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use claude_core::permissions::PermissionMode;
    use std::path::PathBuf;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("."),
            abort_signal: claude_core::tool::AbortSignal::default(),
            permission_mode: PermissionMode::BypassAll,
            messages: vec![],
        }
    }

    fn result_text(result: &ToolResult) -> String {
        result.content.iter().filter_map(|c| {
            if let claude_core::message::ToolResultContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        }).collect::<Vec<_>>().join("")
    }

    #[test]
    fn tool_metadata() {
        let tool = TeamDeleteTool;
        assert_eq!(tool.name(), "TeamDelete");
        assert!(!tool.is_read_only());
    }

    #[tokio::test]
    async fn empty_name_rejected() {
        let tool = TeamDeleteTool;
        let result = tool.call(json!({"team_name": ""}), &test_context()).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn delete_nonexistent_team() {
        let tool = TeamDeleteTool;
        let result = tool
            .call(json!({"team_name": "nonexistent-team-xyz"}), &test_context())
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("not found"));
    }

    #[tokio::test]
    async fn delete_team_with_active_members_blocked() {
        // Create a team with an active teammate
        let name = format!("test-del-active-{}", uuid::Uuid::new_v4().as_simple());
        let team = TeamFile {
            name: name.clone(),
            description: None,
            created_at: 0,
            lead_agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
            lead_session_id: None,
            members: vec![
                TeamMember {
                    agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
                    name: TEAM_LEAD_NAME.into(),
                    agent_type: None, model: None, prompt: None, color: None,
                    joined_at: 0, cwd: ".".into(), session_id: None,
                    is_active: true, backend_type: None,
                },
                TeamMember {
                    agent_id: format_agent_id("worker", &name),
                    name: "worker".into(),
                    agent_type: None, model: None, prompt: None, color: Some("cyan".into()),
                    joined_at: 0, cwd: ".".into(), session_id: None,
                    is_active: true, backend_type: None,
                },
            ],
            team_allowed_paths: vec![],
        };
        helpers::write_team_file(&name, &team).unwrap();

        let tool = TeamDeleteTool;
        let result = tool.call(json!({"team_name": &name}), &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("active teammate"));

        // Cleanup
        let _ = helpers::cleanup_team_directories(&name);
    }

    #[tokio::test]
    async fn delete_team_success() {
        // Create a team with only the lead (no active teammates)
        let name = format!("test-del-ok-{}", uuid::Uuid::new_v4().as_simple());
        let team = TeamFile {
            name: name.clone(),
            description: None,
            created_at: 0,
            lead_agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
            lead_session_id: None,
            members: vec![TeamMember {
                agent_id: format_agent_id(TEAM_LEAD_NAME, &name),
                name: TEAM_LEAD_NAME.into(),
                agent_type: None, model: None, prompt: None, color: None,
                joined_at: 0, cwd: ".".into(), session_id: None,
                is_active: true, backend_type: None,
            }],
            team_allowed_paths: vec![],
        };
        helpers::write_team_file(&name, &team).unwrap();

        let tool = TeamDeleteTool;
        let result = tool.call(json!({"team_name": &name}), &test_context()).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("deleted successfully"));
        assert!(!helpers::team_exists(&name));
    }
}
