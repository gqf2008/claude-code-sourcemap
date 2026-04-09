use claude_core::config::Settings;
use claude_core::permissions::PermissionMode;

pub fn load_settings() -> anyhow::Result<Settings> {
    Settings::load()
}

pub fn parse_permission_mode(mode: &str) -> PermissionMode {
    match mode {
        "bypass" | "bypassPermissions" => PermissionMode::BypassAll,
        "acceptEdits" | "accept-edits" => PermissionMode::AcceptEdits,
        "auto" => PermissionMode::Auto,
        "plan" => PermissionMode::Plan,
        "default" | "" => PermissionMode::Default,
        other => {
            eprintln!(
                "\x1b[33m⚠ Unknown permission mode '{}', using 'default'. \
                 Valid: default, bypass, acceptEdits, auto, plan\x1b[0m",
                other
            );
            PermissionMode::Default
        }
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
    fn test_parse_auto() {
        assert_eq!(parse_permission_mode("auto"), PermissionMode::Auto);
    }

    #[test]
    fn test_parse_default_fallback() {
        assert_eq!(parse_permission_mode(""), PermissionMode::Default);
        assert_eq!(parse_permission_mode("default"), PermissionMode::Default);
        // Unknown values still fall back to Default (with a warning to stderr)
        assert_eq!(parse_permission_mode("unknown"), PermissionMode::Default);
        assert_eq!(parse_permission_mode("BYPASS"), PermissionMode::Default); // case-sensitive
    }
}
