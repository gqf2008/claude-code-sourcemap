//! Tests for config module.

use super::*;

#[test]
fn settings_default_is_empty() {
    let s = Settings::default();
    assert!(s.api_key.is_none());
    assert!(s.model.is_none());
    assert!(s.permission_rules.is_empty());
}

#[test]
fn settings_serde_roundtrip() {
    let s = Settings {
        model: Some("claude-sonnet-4-20250514".into()),
        language: Some("Chinese".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    let loaded: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("claude-sonnet-4-20250514"));
    assert_eq!(loaded.language.as_deref(), Some("Chinese"));
}

#[test]
fn merge_overlay_wins() {
    let base = Settings {
        model: Some("base-model".into()),
        language: Some("English".into()),
        ..Default::default()
    };
    let overlay = Settings {
        model: Some("overlay-model".into()),
        ..Default::default()
    };
    let merged = merge_settings(base, &overlay);
    assert_eq!(merged.model.as_deref(), Some("overlay-model"));
    assert_eq!(merged.language.as_deref(), Some("English"));
}

#[test]
fn merge_tools_combine() {
    let base = Settings {
        allowed_tools: vec!["FileRead".into()],
        ..Default::default()
    };
    let overlay = Settings {
        allowed_tools: vec!["Bash".into()],
        ..Default::default()
    };
    let merged = merge_settings(base, &overlay);
    assert_eq!(merged.allowed_tools, vec!["FileRead", "Bash"]);
}

#[test]
fn merge_rules_append() {
    let base = Settings {
        permission_rules: vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: None,
            behavior: crate::permissions::PermissionBehavior::Ask,
        }],
        ..Default::default()
    };
    let overlay = Settings {
        permission_rules: vec![PermissionRule {
            tool_name: "FileWrite".into(),
            pattern: None,
            behavior: crate::permissions::PermissionBehavior::Allow,
        }],
        ..Default::default()
    };
    let merged = merge_settings(base, &overlay);
    assert_eq!(merged.permission_rules.len(), 2);
}

#[test]
fn settings_save_and_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let s = Settings {
        model: Some("test-model".into()),
        language: Some("中文".into()),
        ..Default::default()
    };
    let path = s.save_to(SettingsSource::Project, dir.path()).unwrap();
    assert!(path.exists());

    let loaded = load_settings_file(&path).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("test-model"));
    assert_eq!(loaded.language.as_deref(), Some("中文"));
}

#[test]
fn load_merged_multi_layer() {
    let dir = tempfile::tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    // Project settings
    let proj = Settings { model: Some("proj-model".into()), ..Default::default() };
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&proj).unwrap(),
    ).unwrap();

    // Local override
    let local = Settings { language: Some("Japanese".into()), ..Default::default() };
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string(&local).unwrap(),
    ).unwrap();

    let loaded = Settings::load_merged(dir.path());
    assert_eq!(loaded.settings.model.as_deref(), Some("proj-model"));
    assert_eq!(loaded.settings.language.as_deref(), Some("Japanese"));
    assert!(loaded.sources.contains(&SettingsSource::Project));
    assert!(loaded.sources.contains(&SettingsSource::Local));
}

#[test]
fn update_field_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = Settings::update_field(SettingsSource::Project, dir.path(), |s| {
        s.model = Some("new-model".into());
    }).unwrap();

    let loaded = load_settings_file(&path).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("new-model"));
}

#[test]
fn add_permission_rule_dedup() {
    let dir = tempfile::tempdir().unwrap();
    let rule = PermissionRule {
        tool_name: "Bash".into(),
        pattern: Some("npm*".into()),
        behavior: crate::permissions::PermissionBehavior::Allow,
    };

    Settings::add_permission_rule(rule.clone(), SettingsSource::Project, dir.path()).unwrap();
    Settings::add_permission_rule(rule.clone(), SettingsSource::Project, dir.path()).unwrap();

    let path = project_settings_path(dir.path());
    let loaded = load_settings_file(&path).unwrap();
    assert_eq!(loaded.permission_rules.len(), 1); // no duplicate
}

#[test]
fn settings_summary_format() {
    let s = Settings {
        model: Some("claude-sonnet-4-20250514".into()),
        language: Some("Chinese".into()),
        api_key: Some("sk-test".into()),
        ..Default::default()
    };
    let summary = s.summary();
    assert!(summary.contains("claude-sonnet-4-20250514"));
    assert!(summary.contains("Chinese"));
    assert!(summary.contains("****")); // key is masked
    assert!(!summary.contains("sk-test")); // key not leaked
}

#[test]
fn settings_source_display() {
    assert_eq!(SettingsSource::User.to_string(), "~/.claude/settings.json");
    assert_eq!(SettingsSource::Project.to_string(), ".claude/settings.json");
    assert_eq!(SettingsSource::Local.to_string(), ".claude/settings.local.json");
}

#[test]
fn export_json_is_valid() {
    let s = Settings { model: Some("test".into()), ..Default::default() };
    let json = s.export_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["model"], "test");
}

#[test]
fn merge_hooks_overlay_replaces_base_entirely() {
    let base = Settings {
        hooks: HooksConfig {
            pre_tool_use: vec![HookRule {
                matcher: Some(".*".into()),
                hooks: vec![HookCommandDef {
                    hook_type: "command".into(),
                    command: "echo base".into(),
                    timeout_ms: None,
                }],
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let overlay = Settings {
        hooks: HooksConfig {
            stop: vec![HookRule {
                matcher: Some(".*".into()),
                hooks: vec![HookCommandDef {
                    hook_type: "command".into(),
                    command: "echo overlay".into(),
                    timeout_ms: None,
                }],
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let merged = merge_settings(base, &overlay);
    // Overlay should entirely replace base hooks
    assert!(merged.hooks.pre_tool_use.is_empty(), "base pre_tool_use should be gone");
    assert_eq!(merged.hooks.stop.len(), 1);
    assert_eq!(merged.hooks.stop[0].hooks[0].command, "echo overlay");
}

#[test]
fn merge_hooks_empty_overlay_keeps_base() {
    let base = Settings {
        hooks: HooksConfig {
            pre_tool_use: vec![HookRule {
                matcher: Some(".*".into()),
                hooks: vec![HookCommandDef {
                    hook_type: "command".into(),
                    command: "echo base".into(),
                    timeout_ms: None,
                }],
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let overlay = Settings::default();
    let merged = merge_settings(base, &overlay);
    // Empty overlay keeps base hooks intact
    assert_eq!(merged.hooks.pre_tool_use.len(), 1);
    assert_eq!(merged.hooks.pre_tool_use[0].hooks[0].command, "echo base");
}

#[test]
fn merge_three_layers_priority() {
    let user = Settings {
        model: Some("user-model".into()),
        language: Some("English".into()),
        allowed_tools: vec!["tool_a".into()],
        ..Default::default()
    };
    let project = Settings {
        model: Some("project-model".into()),
        allowed_tools: vec!["tool_b".into()],
        ..Default::default()
    };
    let local = Settings {
        model: Some("local-model".into()),
        ..Default::default()
    };
    // Merge order: user → project → local (later wins)
    let step1 = merge_settings(user, &project);
    let final_settings = merge_settings(step1, &local);
    // local model wins
    assert_eq!(final_settings.model.as_deref(), Some("local-model"));
    // Language from user is preserved (project/local don't set it)
    assert_eq!(final_settings.language.as_deref(), Some("English"));
    // Tools are union-merged from user + project
    assert!(final_settings.allowed_tools.contains(&"tool_a".to_string()));
    assert!(final_settings.allowed_tools.contains(&"tool_b".to_string()));
}

#[test]
fn merge_permission_rules_are_appended_not_deduped() {
    let base = Settings {
        permission_rules: vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: None,
            behavior: crate::permissions::PermissionBehavior::Allow,
        }],
        ..Default::default()
    };
    let overlay = Settings {
        permission_rules: vec![
            PermissionRule {
                tool_name: "Bash".into(), // duplicate
                pattern: None,
                behavior: crate::permissions::PermissionBehavior::Allow,
            },
            PermissionRule {
                tool_name: "FileWrite".into(),
                pattern: Some("src/**".into()),
                behavior: crate::permissions::PermissionBehavior::Allow,
            },
        ],
        ..Default::default()
    };
    let merged = merge_settings(base, &overlay);
    // Rules are appended (not deduplicated at merge time)
    assert_eq!(merged.permission_rules.len(), 3);
}
