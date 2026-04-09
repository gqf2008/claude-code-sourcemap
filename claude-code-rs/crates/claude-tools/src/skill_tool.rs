use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// `SkillTool` — invoke a loaded skill (markdown prompt) by name.
///
/// Skills are `.md` files in `.claude/skills/` that expand into prompts with
/// optional `allowedTools` and `model` metadata.  This tool lets the model
/// invoke a skill programmatically rather than the user typing `/skillname`.
pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str { "Skill" }

    fn description(&self) -> &'static str {
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

        // Load skills from cwd (cached — no repeated disk I/O)
        let skills = claude_core::skills::get_skills(&context.cwd);

        let skill = skills.iter().find(|s| s.name == skill_name);
        let skill = if let Some(s) = skill { s } else {
            let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            return Ok(ToolResult::error(format!(
                "Skill '{skill_name}' not found. Available: {available:?}"
            )));
        };

        // Check if model invocation is disabled
        if skill.disable_model_invocation {
            return Ok(ToolResult::error(format!(
                "Skill '{skill_name}' has disabled model invocation. It can only be invoked by the user via /{skill_name}."
            )));
        }

        // Expand the skill content with arguments (handles $ARGUMENTS, ${name}, $1, ${CLAUDE_SKILL_DIR})
        let content = claude_core::skills::substitute_arguments(skill, args);

        // Merge skill's allowed_tools with any inline overrides
        let allowed_tools = if skill.allowed_tools.is_empty() {
            // Check inline HTML comments as fallback
            let mut inline_tools = None;
            for line in content.lines() {
                let trimmed = line.trim().to_string();
                if let Some(rest) = trimmed.strip_prefix("<!-- allowedTools:") {
                    if let Some(tools_str) = rest.strip_suffix("-->") {
                        inline_tools = Some(
                            tools_str.split(',')
                                .map(|t| t.trim().to_string())
                                .filter(|t| !t.is_empty())
                                .collect()
                        );
                    }
                }
            }
            inline_tools
        } else {
            Some(skill.allowed_tools.clone())
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
        if let Some(ref m) = skill.model {
            result["model"] = json!(m);
        }
        if let Some(ref ctx) = skill.context {
            result["context"] = json!(ctx);
        }
        if let Some(ref agent) = skill.agent {
            result["agent"] = json!(agent);
        }
        if let Some(ref effort) = skill.effort {
            result["effort"] = json!(effort);
        }
        if let Some(ref hint) = skill.when_to_use {
            result["whenToUse"] = json!(hint);
        }

        Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
    }
}
