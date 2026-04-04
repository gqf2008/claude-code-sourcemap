use claude_core::permissions::{PermissionBehavior, PermissionMode, PermissionResult, PermissionRule};
use claude_core::tool::Tool;
use serde_json::Value;
use std::io::{self, Write};

pub struct PermissionChecker {
    rules: Vec<PermissionRule>,
    mode: PermissionMode,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self { rules, mode }
    }

    pub async fn check(&self, tool: &dyn Tool, _input: &Value) -> PermissionResult {
        if self.mode == PermissionMode::BypassAll {
            return PermissionResult::allow();
        }
        if self.mode == PermissionMode::Plan && !tool.is_read_only() {
            return PermissionResult::deny("Plan mode: writes not allowed".into());
        }
        for rule in &self.rules {
            if rule.tool_name == tool.name() || rule.tool_name == "*" {
                match rule.behavior {
                    PermissionBehavior::Allow => return PermissionResult::allow(),
                    PermissionBehavior::Deny => {
                        return PermissionResult::deny(format!("'{}' denied by rule", tool.name()));
                    }
                    PermissionBehavior::Ask => {}
                }
            }
        }
        if tool.is_read_only() {
            return PermissionResult::allow();
        }
        PermissionResult::ask(format!("Allow {} ?", tool.name()))
    }

    /// Interactive terminal permission prompt
    pub fn prompt_user(tool_name: &str, description: &str) -> bool {
        print!("\n\x1b[33m⚠  {} wants to: {}\n   Allow? [y/N]: \x1b[0m", tool_name, description);
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
