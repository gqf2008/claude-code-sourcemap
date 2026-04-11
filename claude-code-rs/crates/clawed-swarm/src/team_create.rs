//! TeamCreate tool — creates a named agent team with a lead.

use serde_json::{json, Value};
use tracing::info;

use clawed_core::tool::{Tool, ToolContext, ToolResult};

use crate::helpers;
use crate::types::*;

/// Tool for creating a named agent team in coordinator mode.
pub struct TeamCreateTool;

#[async_trait::async_trait]
impl Tool for TeamCreateTool {
    fn name(&self) -> &str {
        "TeamCreate"
    }

    fn description(&self) -> &str {
        "Create a named team of agents. The team has a designated lead \
         (the coordinator) who can spawn teammates via AgentTool. \
         Only one team per leader — delete the existing team first to create a new one."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Human-readable name for the team."
                },
                "description": {
                    "type": "string",
                    "description": "Optional description of the team's purpose."
                }
            },
            "required": ["team_name"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let team_name = input["team_name"]
            .as_str()
            .unwrap_or("")
            .trim();

        if team_name.is_empty() {
            return Ok(ToolResult::error("team_name must be a non-empty string"));
        }

        let description = input["description"].as_str().map(String::from);

        // Generate a unique team name (handles collisions)
        let final_name = helpers::generate_unique_team_name(team_name);
        let lead_agent_id = format_agent_id(TEAM_LEAD_NAME, &final_name);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let cwd = context.cwd.to_string_lossy().to_string();

        // Create the team file with the lead as the first member
        let team = TeamFile {
            name: final_name.clone(),
            description,
            created_at: now_ms,
            lead_agent_id: lead_agent_id.clone(),
            lead_session_id: None,
            members: vec![TeamMember {
                agent_id: lead_agent_id.clone(),
                name: TEAM_LEAD_NAME.to_string(),
                agent_type: Some("coordinator".to_string()),
                model: None,
                prompt: None,
                color: None,
                joined_at: now_ms,
                cwd,
                session_id: None,
                is_active: true,
                backend_type: Some("in-process".to_string()),
            }],
            team_allowed_paths: vec![],
        };

        let file_path = helpers::write_team_file(&final_name, &team)?;
        let file_path_str = file_path.to_string_lossy().to_string();

        info!(
            team_name = %final_name,
            lead_agent_id = %lead_agent_id,
            "Created team"
        );

        Ok(ToolResult::text(format!(
            "Team '{}' created successfully.\n\
             Lead agent ID: {}\n\
             Config file: {}\n\n\
             You can now spawn teammates using AgentTool with the team_name parameter.",
            final_name, lead_agent_id, file_path_str
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let tool = TeamCreateTool;
        assert_eq!(tool.name(), "TeamCreate");
        assert!(!tool.is_read_only());
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("team_name")));
    }

    #[tokio::test]
    async fn empty_name_rejected() {
        let tool = TeamCreateTool;
        let result = tool.call(json!({"team_name": ""}), &test_context()).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn create_team_success() {
        let tool = TeamCreateTool;
        let name = format!("test-create-{}", uuid::Uuid::new_v4().as_simple());
        let result = tool
            .call(json!({"team_name": &name, "description": "test team"}), &test_context())
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("created successfully"));
        assert!(text.contains("team-lead@"));

        // Cleanup
        let _ = helpers::cleanup_team_directories(&name);
    }
}
