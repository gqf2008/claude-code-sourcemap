use claude_core::config::Settings;
use claude_core::permissions::PermissionMode;

pub fn load_settings() -> anyhow::Result<Settings> {
    Settings::load()
}

pub fn parse_permission_mode(mode: &str) -> PermissionMode {
    match mode {
        "bypass" | "bypassPermissions" => PermissionMode::BypassAll,
        "acceptEdits" | "accept-edits" => PermissionMode::AcceptEdits,
        "plan" => PermissionMode::Plan,
        _ => PermissionMode::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bypass() {
        assert_eq!(parse_permission_mode("bypass"), PermissionMode::BypassAll);
        assert_eq!(parse_permission_mode("bypassPermissions"), PermissionMode::BypassAll);
    }

    #[test]
    fn test_parse_accept_edits() {
        assert_eq!(parse_permission_mode("acceptEdits"), PermissionMode::AcceptEdits);
        assert_eq!(parse_permission_mode("accept-edits"), PermissionMode::AcceptEdits);
    }

    #[test]
    fn test_parse_plan() {
        assert_eq!(parse_permission_mode("plan"), PermissionMode::Plan);
    }

    #[test]
    fn test_parse_default_fallback() {
        assert_eq!(parse_permission_mode(""), PermissionMode::Default);
        assert_eq!(parse_permission_mode("unknown"), PermissionMode::Default);
        assert_eq!(parse_permission_mode("BYPASS"), PermissionMode::Default); // case-sensitive
    }
}
