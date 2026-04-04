use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassAll,
    Plan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub reason: Option<String>,
}

impl PermissionResult {
    pub fn allow() -> Self {
        Self { behavior: PermissionBehavior::Allow, reason: None }
    }
    pub fn deny(reason: String) -> Self {
        Self { behavior: PermissionBehavior::Deny, reason: Some(reason) }
    }
    pub fn ask(reason: String) -> Self {
        Self { behavior: PermissionBehavior::Ask, reason: Some(reason) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: String,
    pub pattern: Option<String>,
    pub behavior: PermissionBehavior,
}

/// Wildcard matcher supporting `*` glob (e.g. `*.rs`, `foo*bar`, `*`).
pub fn matches_wildcard(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let n = parts.len();

    // Prefix must match the start of value
    if !parts[0].is_empty() && !value.starts_with(parts[0]) {
        return false;
    }
    // Suffix must match the end of value
    if !parts[n - 1].is_empty() && !value.ends_with(parts[n - 1]) {
        return false;
    }

    let start = parts[0].len();
    let end = value.len().saturating_sub(parts[n - 1].len());
    if start > end {
        return false; // prefix and suffix overlap
    }

    // Match middle segments left-to-right
    let mut pos = start;
    for part in &parts[1..n - 1] {
        if part.is_empty() {
            continue;
        }
        match value[pos..end].find(part) {
            Some(idx) => pos += idx + part.len(),
            None => return false,
        }
    }
    true
}
