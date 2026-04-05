use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// SkillTool — invoke a loaded skill (markdown prompt) by name.
///
/// Skills are `.md` files in `.claude/skills/` that expand into prompts with
/// optional `allowedTools` and `model` metadata.  This tool lets the model
/// invoke a skill programmatically rather than the user typing `/skillname`.
pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str { "Skill" }

    fn description(&self) -> &str {
        "Execute a skill (a reusable prompt template loaded from .claude/skills/). \
         Skills expand into prompts that guide a sub-task. Use /skills to list available ones."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "The skill name (e.g. \"commit\", \"review-pr\", \"pdf\"). Use /skills to list available ones."
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments passed to the skill prompt (replaces $ARGUMENTS in the template)."
                }
            },
            "required": ["skill"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let skill_name = input["skill"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'skill' parameter"))?
            .trim_start_matches('/');

        let args = input["args"].as_str().unwrap_or("");

        // Load skills from cwd
        let skills = claude_core::skills::load_skills(&context.cwd);

        let skill = skills.iter().find(|s| s.name == skill_name);
        let skill = match skill {
            Some(s) => s,
            None => {
                let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                return Ok(ToolResult::error(format!(
                    "Skill '{}' not found. Available: {:?}",
                    skill_name, available
                )));
            }
        };

        // Expand the skill content with arguments
        let mut content = skill.system_prompt.clone();
        if !args.is_empty() {
            content = content.replace("$ARGUMENTS", args);
        }

        // Extract metadata from frontmatter-style comments if present
        // (SkillEntry already parses frontmatter, but check inline comments too)
        let mut extra_allowed_tools: Option<Vec<String>> = None;
        let skill_model = skill.model.clone();

        for line in content.lines() {
            let trimmed = line.trim().to_string();
            if let Some(rest) = trimmed.strip_prefix("<!-- allowedTools:") {
                if let Some(tools_str) = rest.strip_suffix("-->") {
                    extra_allowed_tools = Some(
                        tools_str.split(',')
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect()
                    );
                }
            }
        }

        // Merge skill's allowed_tools with any inline overrides
        let allowed_tools = if !skill.allowed_tools.is_empty() {
            Some(skill.allowed_tools.clone())
        } else {
            extra_allowed_tools
        };

        let mut result = json!({
            "success": true,
            "commandName": skill_name,
            "status": "inline",
            "expandedPrompt": content.trim(),
        });

        if let Some(tools) = allowed_tools {
            result["allowedTools"] = json!(tools);
        }
        if let Some(m) = skill_model {
            result["model"] = json!(m);
        }

        Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
    }
}
