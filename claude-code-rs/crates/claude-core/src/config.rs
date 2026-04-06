use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};
use crate::permissions::PermissionRule;

// ── Hook configuration types ────────────────────────────────────────────────

/// A single shell-command hook definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCommandDef {
    /// Hook type — currently only `"command"` is supported.
    #[serde(rename = "type", default = "default_hook_type")]
    pub hook_type: String,
    /// Shell command to execute (passed to `sh -c` on Unix, `cmd /C` on Windows).
    pub command: String,
    /// Optional timeout in milliseconds (default: 60 000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_hook_type() -> String {
    "command".into()
}

/// A hook rule: an optional tool-name matcher + one or more hook commands.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookRule {
    /// Optional regex / glob pattern applied to the tool name.
    /// `None` or `"*"` matches every tool.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Commands to run when this rule matches.
    #[serde(default)]
    pub hooks: Vec<HookCommandDef>,
}

/// All hook rules keyed by lifecycle event name.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookRule>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookRule>,
    #[serde(default, rename = "PostToolUseFailure")]
    pub post_tool_use_failure: Vec<HookRule>,
    #[serde(default, rename = "Stop")]
    pub stop: Vec<HookRule>,
    #[serde(default, rename = "StopFailure")]
    pub stop_failure: Vec<HookRule>,
    #[serde(default, rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookRule>,
    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookRule>,
    #[serde(default, rename = "SessionEnd")]
    pub session_end: Vec<HookRule>,
    #[serde(default, rename = "Setup")]
    pub setup: Vec<HookRule>,
    #[serde(default, rename = "PreCompact")]
    pub pre_compact: Vec<HookRule>,
    #[serde(default, rename = "PostCompact")]
    pub post_compact: Vec<HookRule>,
    #[serde(default, rename = "SubagentStart")]
    pub subagent_start: Vec<HookRule>,
    #[serde(default, rename = "SubagentStop")]
    pub subagent_stop: Vec<HookRule>,
    #[serde(default, rename = "Notification")]
    pub notification: Vec<HookRule>,
    #[serde(default, rename = "PostSampling")]
    pub post_sampling: Vec<HookRule>,
    #[serde(default, rename = "PermissionRequest")]
    pub permission_request: Vec<HookRule>,
    #[serde(default, rename = "PermissionDenied")]
    pub permission_denied: Vec<HookRule>,
    #[serde(default, rename = "InstructionsLoaded")]
    pub instructions_loaded: Vec<HookRule>,
    #[serde(default, rename = "CwdChanged")]
    pub cwd_changed: Vec<HookRule>,
    #[serde(default, rename = "FileChanged")]
    pub file_changed: Vec<HookRule>,
    #[serde(default, rename = "ConfigChange")]
    pub config_change: Vec<HookRule>,
    #[serde(default, rename = "TaskCreated")]
    pub task_created: Vec<HookRule>,
    #[serde(default, rename = "TaskCompleted")]
    pub task_completed: Vec<HookRule>,
}

// ── Settings source tracking ────────────────────────────────────────────────

/// Which file a particular setting was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsSource {
    /// `~/.claude/settings.json` — user-global preferences.
    User,
    /// `$CWD/.claude/settings.json` — shared project settings.
    Project,
    /// `$CWD/.claude/settings.local.json` — local project overrides (gitignored).
    Local,
    /// Command-line flags or environment variables.
    Cli,
    /// Programmatic default.
    Default,
}

impl std::fmt::Display for SettingsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "~/.claude/settings.json"),
            Self::Project => write!(f, ".claude/settings.json"),
            Self::Local => write!(f, ".claude/settings.local.json"),
            Self::Cli => write!(f, "CLI flags"),
            Self::Default => write!(f, "default"),
        }
    }
}

// ── Main settings struct ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub custom_system_prompt: Option<String>,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    pub permission_rules: Vec<PermissionRule>,
    /// Lifecycle hook configuration.
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Language preference (e.g. "中文", "English").
    #[serde(default)]
    pub language: Option<String>,
    /// Output style name (e.g. "concise", "verbose").
    #[serde(default)]
    pub output_style: Option<String>,
}

// ── File paths ──────────────────────────────────────────────────────────────

/// `~/.claude/settings.json`
fn user_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// `$CWD/.claude/settings.json`
fn project_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".claude").join("settings.json")
}

/// `$CWD/.claude/settings.local.json`
fn local_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".claude").join("settings.local.json")
}

/// Legacy XDG path: `~/.config/claude/settings.json`
fn legacy_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("claude").join("settings.json"))
}

// ── Loading ─────────────────────────────────────────────────────────────────

fn load_settings_file(path: &Path) -> Option<Settings> {
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(s) => {
                debug!("Loaded settings from {}", path.display());
                Some(s)
            }
            Err(e) => {
                warn!("Failed to parse settings at {}: {}", path.display(), e);
                None
            }
        },
        Err(e) => {
            warn!("Failed to read settings at {}: {}", path.display(), e);
            None
        }
    }
}

/// Merge `overlay` on top of `base`, with overlay values taking priority.
/// Only non-default overlay values override base.
fn merge_settings(base: Settings, overlay: &Settings) -> Settings {
    Settings {
        api_key: overlay.api_key.clone().or(base.api_key),
        model: overlay.model.clone().or(base.model),
        permission_mode: overlay.permission_mode.clone().or(base.permission_mode),
        allowed_tools: if overlay.allowed_tools.is_empty() {
            base.allowed_tools
        } else {
            let mut merged = base.allowed_tools;
            for t in &overlay.allowed_tools {
                if !merged.contains(t) {
                    merged.push(t.clone());
                }
            }
            merged
        },
        denied_tools: if overlay.denied_tools.is_empty() {
            base.denied_tools
        } else {
            let mut merged = base.denied_tools;
            for t in &overlay.denied_tools {
                if !merged.contains(t) {
                    merged.push(t.clone());
                }
            }
            merged
        },
        custom_system_prompt: overlay.custom_system_prompt.clone().or(base.custom_system_prompt),
        append_system_prompt: overlay.append_system_prompt.clone().or(base.append_system_prompt),
        permission_rules: if overlay.permission_rules.is_empty() {
            base.permission_rules
        } else {
            let mut merged = base.permission_rules;
            merged.extend(overlay.permission_rules.clone());
            merged
        },
        hooks: if has_any_hooks(&overlay.hooks) {
            overlay.hooks.clone()
        } else {
            base.hooks
        },
        language: overlay.language.clone().or(base.language),
        output_style: overlay.output_style.clone().or(base.output_style),
    }
}

fn has_any_hooks(h: &HooksConfig) -> bool {
    !h.pre_tool_use.is_empty()
        || !h.post_tool_use.is_empty()
        || !h.stop.is_empty()
        || !h.session_start.is_empty()
        || !h.session_end.is_empty()
        || !h.user_prompt_submit.is_empty()
}

// ── Loaded settings with source info ────────────────────────────────────────

/// Settings loaded from all layers, with source tracking.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    /// Merged final settings.
    pub settings: Settings,
    /// Which sources contributed to the merge.
    pub sources: Vec<SettingsSource>,
    /// Per-source settings for debugging/display.
    pub layers: Vec<(SettingsSource, Settings)>,
}

impl Settings {
    /// Legacy config dir (XDG)
    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("claude"))
    }

    /// Load settings from the legacy XDG path only (backward-compatible).
    pub fn load() -> anyhow::Result<Self> {
        // Try new path first, fall back to legacy
        if let Some(path) = user_settings_path() {
            if let Some(s) = load_settings_file(&path) {
                return Ok(s);
            }
        }
        if let Some(path) = legacy_config_path() {
            if let Some(s) = load_settings_file(&path) {
                return Ok(s);
            }
        }
        Ok(Self::default())
    }

    /// Load settings with multi-layer merging:
    ///   user (~/.claude) → project (.claude/) → local (.claude/settings.local.json)
    /// Later layers override earlier ones.
    pub fn load_merged(cwd: &Path) -> LoadedSettings {
        let mut merged = Settings::default();
        let mut sources = Vec::new();
        let mut layers = Vec::new();

        // Layer 1: User global (~/.claude/settings.json)
        if let Some(path) = user_settings_path() {
            if let Some(s) = load_settings_file(&path) {
                merged = merge_settings(merged, &s);
                sources.push(SettingsSource::User);
                layers.push((SettingsSource::User, s));
            }
        }

        // Layer 1b: Legacy XDG path (fallback if no user settings)
        if sources.is_empty() {
            if let Some(path) = legacy_config_path() {
                if let Some(s) = load_settings_file(&path) {
                    merged = merge_settings(merged, &s);
                    sources.push(SettingsSource::User);
                    layers.push((SettingsSource::User, s));
                }
            }
        }

        // Layer 2: Project shared ($CWD/.claude/settings.json)
        let proj_path = project_settings_path(cwd);
        if let Some(s) = load_settings_file(&proj_path) {
            merged = merge_settings(merged, &s);
            sources.push(SettingsSource::Project);
            layers.push((SettingsSource::Project, s));
        }

        // Layer 3: Project local ($CWD/.claude/settings.local.json)
        let local_path = local_settings_path(cwd);
        if let Some(s) = load_settings_file(&local_path) {
            merged = merge_settings(merged, &s);
            sources.push(SettingsSource::Local);
            layers.push((SettingsSource::Local, s));
        }

        if sources.is_empty() {
            sources.push(SettingsSource::Default);
        }

        LoadedSettings { settings: merged, sources, layers }
    }

    /// Save settings to a specific destination file.
    pub fn save_to(&self, destination: SettingsSource, cwd: &Path) -> anyhow::Result<PathBuf> {
        let path = match destination {
            SettingsSource::User => user_settings_path()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?,
            SettingsSource::Project => project_settings_path(cwd),
            SettingsSource::Local => local_settings_path(cwd),
            _ => anyhow::bail!("Cannot save to {:?} — not a file destination", destination),
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, &json)?;
        debug!("Saved settings to {}", path.display());
        Ok(path)
    }

    /// Save to user settings (`~/.claude/settings.json`).
    pub fn save_user(&self) -> anyhow::Result<PathBuf> {
        self.save_to(SettingsSource::User, Path::new("."))
    }

    /// Update a single field in the specified settings file.
    /// Loads existing file, applies update, writes back.
    pub fn update_field(
        destination: SettingsSource,
        cwd: &Path,
        updater: impl FnOnce(&mut Settings),
    ) -> anyhow::Result<PathBuf> {
        let path = match destination {
            SettingsSource::User => user_settings_path()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?,
            SettingsSource::Project => project_settings_path(cwd),
            SettingsSource::Local => local_settings_path(cwd),
            _ => anyhow::bail!("Cannot update {:?}", destination),
        };

        let mut settings = load_settings_file(&path).unwrap_or_default();
        updater(&mut settings);
        settings.save_to(destination, cwd)
    }

    /// Add a permission rule to the specified settings file.
    pub fn add_permission_rule(
        rule: PermissionRule,
        destination: SettingsSource,
        cwd: &Path,
    ) -> anyhow::Result<PathBuf> {
        Self::update_field(destination, cwd, |s| {
            // Avoid duplicating identical rules
            if !s.permission_rules.iter().any(|r| {
                r.tool_name == rule.tool_name
                    && r.pattern == rule.pattern
                    && r.behavior == rule.behavior
            }) {
                s.permission_rules.push(rule.clone());
            }
        })
    }

    /// Export settings as formatted JSON string (for /settings export).
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Format a human-readable summary of the current settings.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        if let Some(ref model) = self.model {
            lines.push(format!("  Model: {}", model));
        }
        if let Some(ref mode) = self.permission_mode {
            lines.push(format!("  Permission mode: {}", mode));
        }
        if let Some(ref lang) = self.language {
            lines.push(format!("  Language: {}", lang));
        }
        if let Some(ref style) = self.output_style {
            lines.push(format!("  Output style: {}", style));
        }
        if !self.allowed_tools.is_empty() {
            lines.push(format!("  Allowed tools: {}", self.allowed_tools.join(", ")));
        }
        if !self.denied_tools.is_empty() {
            lines.push(format!("  Denied tools: {}", self.denied_tools.join(", ")));
        }
        if !self.permission_rules.is_empty() {
            lines.push(format!("  Permission rules: {} defined", self.permission_rules.len()));
        }
        if self.api_key.is_some() {
            lines.push("  API key: ****".into());
        }
        if lines.is_empty() {
            "  (all defaults)".into()
        } else {
            lines.join("\n")
        }
    }
}

impl LoadedSettings {
    /// Format a summary showing which sources contributed.
    pub fn display_sources(&self) -> String {
        let mut out = String::from("Settings loaded from:\n");
        for (source, layer) in &self.layers {
            out.push_str(&format!("  {} →\n", source));
            out.push_str(&format!("{}\n", layer.summary()));
        }
        if self.layers.is_empty() {
            out.push_str("  (defaults only)\n");
        }
        out.push_str(&format!("\nMerged result:\n{}", self.settings.summary()));
        out
    }
}

#[cfg(test)]
mod tests {
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
}
