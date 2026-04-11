//! `ConfigTool` — read and write the user's claude settings.json.
//!
//! Mirrors claude-code's `ConfigTool`.  Supports two operations:
//!  - `get`: read a top-level key from `~/.config/claude/settings.json`
//!  - `set`: write a top-level key

use async_trait::async_trait;
use clawed_core::config::Settings;
use clawed_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &'static str { "Config" }

    fn description(&self) -> &'static str {
        "Read or write settings in the Claude configuration file (~/.config/claude/settings.json). \
         Use 'get' to read a setting value and 'set' to update it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set"],
                    "description": "Operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "Setting key (e.g. 'model', 'permission_mode')"
                },
                "value": {
                    "description": "New value (required for 'set')"
                }
            },
            "required": ["action", "key"]
        })
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'action'"))?;
        let key = input["key"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'key'"))?;

        let settings_path = Settings::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?
            .join("settings.json");

        match action {
            "get" => {
                let settings: Value = if settings_path.exists() {
                    let text = tokio::fs::read_to_string(&settings_path).await?;
                    serde_json::from_str(&text)?
                } else {
                    json!({})
                };
                let val = &settings[key];
                if val.is_null() {
                    Ok(ToolResult::text(format!("{key}: (not set)")))
                } else {
                    Ok(ToolResult::text(format!("{key}: {val}")))
                }
            }
            "set" => {
                let new_val = input.get("value").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'value' for set"))?;

                let mut settings: Value = if settings_path.exists() {
                    let text = tokio::fs::read_to_string(&settings_path).await?;
                    serde_json::from_str(&text)?
                } else {
                    json!({})
                };

                settings[key] = new_val.clone();

                if let Some(parent) = settings_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let pretty = serde_json::to_string_pretty(&settings)?;
                tokio::fs::write(&settings_path, pretty).await?;
                Ok(ToolResult::text(format!("Set {key} = {new_val}")))
            }
            other => Ok(ToolResult::error(format!("Unknown action: '{other}'. Use 'get' or 'set'."))),
        }
    }
}
