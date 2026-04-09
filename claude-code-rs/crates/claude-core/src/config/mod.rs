//! Configuration loading, merging, and persistence.
//!
//! Settings are loaded from up to 4 layers (user → project → local → CLI)
//! and merged with later layers taking priority.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};
use crate::permissions::PermissionRule;

mod hooks;
pub use hooks::{HookCommandDef, HookRule, HooksConfig};
use hooks::merge_hooks;

#[cfg(test)]
mod tests;

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

/// Merged configuration produced from up to 4 layers (user → project → local → CLI).
///
/// Settings are loaded from JSON files at well-known paths and merged with
/// later layers taking priority.  See [`load_settings`] for the merge order
/// and [`SettingsSource`] for the layer definitions.
///
/// # Layer paths
/// | Layer   | Path                                     |
/// |---------|------------------------------------------|
/// | User    | `~/.claude/settings.json`                |
/// | Project | `$CWD/.claude/settings.json`             |
/// | Local   | `$CWD/.claude/settings.local.json`       |
/// | CLI     | command-line flags / env vars (runtime)   |
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(clippy::unsafe_derive_deserialize)] // apply_env() uses unsafe set_var; safe at single-threaded init
pub struct Settings {
    /// Anthropic API key (usually set via `ANTHROPIC_API_KEY` env var).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Model identifier (e.g. `"claude-sonnet-4-20250514"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Permission mode: `"default"`, `"allowlist"`, or `"deny"`.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Tools the model is allowed to call (empty = all).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tools explicitly denied to the model.
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Completely replace the built-in system prompt with this text.
    #[serde(default)]
    pub custom_system_prompt: Option<String>,
    /// Text appended to the system prompt after all built-in sections.
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    /// Path-based permission rules (allow/deny) for file and command tools.
    #[serde(default)]
    pub permission_rules: Vec<PermissionRule>,
    /// Lifecycle hook configuration.
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Language preference (e.g. `"中文"`, `"English"`).
    #[serde(default)]
    pub language: Option<String>,
    /// Output style name (e.g. `"concise"`, `"verbose"`).
    #[serde(default)]
    pub output_style: Option<String>,
    /// Environment variables to inject (from settings.json `env` field).
    /// The TS Claude Code applies these before auth resolution.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Auto-approve mode configuration (from settings.json `autoMode`).
    #[serde(default, rename = "autoMode")]
    pub auto_mode: Option<crate::permissions::AutoModeConfig>,
}

// ── File paths ──────────────────────────────────────────────────────────────

/// `~/.claude/settings.json`
fn user_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Get the user-level settings file path (`~/.claude/settings.json`).
pub fn settings_path() -> Option<PathBuf> {
    user_settings_path()
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
        allowed_tools: {
            let mut tools = base.allowed_tools;
            tools.extend(overlay.allowed_tools.clone());
            tools
        },
        denied_tools: {
            let mut tools = base.denied_tools;
            tools.extend(overlay.denied_tools.clone());
            tools
        },
        custom_system_prompt: overlay.custom_system_prompt.clone().or(base.custom_system_prompt),
        append_system_prompt: overlay.append_system_prompt.clone().or(base.append_system_prompt),
        permission_rules: {
            let mut rules = base.permission_rules;
            rules.extend(overlay.permission_rules.clone());
            rules
        },
        hooks: merge_hooks(base.hooks, &overlay.hooks),
        language: overlay.language.clone().or(base.language),
        output_style: overlay.output_style.clone().or(base.output_style),
        env: {
            let mut env = base.env;
            env.extend(overlay.env.clone());
            env
        },
        auto_mode: overlay.auto_mode.clone().or(base.auto_mode),
    }
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

    /// The Claude Code config directory (`~/.claude/`).
    pub fn claude_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".claude"))
    }

    /// Apply the `env` map as process environment variables.
    ///
    /// This mirrors the TS Claude Code behavior where `settings.json` `env`
    /// entries are injected before auth resolution, allowing proxy configs
    /// like `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` to take effect.
    ///
    /// Returns a list of (key, previous_value) pairs so callers can restore
    /// the original environment if needed.
    ///
    /// # Safety
    /// `std::env::set_var` is not thread-safe in Rust ≥ 1.66 (deprecated).
    /// This method should only be called during startup, before spawning
    /// any worker threads.
    pub fn apply_env(&self) -> Vec<(String, Option<String>)> {
        const SECRET_KEYWORDS: &[&str] = &[
            "KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH", "PRIVATE",
        ];
        let mut previous = Vec::with_capacity(self.env.len());
        for (key, value) in &self.env {
            if !key.is_empty() {
                let upper = key.to_uppercase();
                let is_secret = SECRET_KEYWORDS.iter().any(|kw| upper.contains(kw));
                let display_val = if is_secret { "****" } else { value.as_str() };
                debug!("Injecting env from settings: {key}={display_val}");
                let old = std::env::var(key).ok();
                previous.push((key.clone(), old));
                // SAFETY: must be called single-threaded during init
                unsafe { std::env::set_var(key, value); }
            }
        }
        previous
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
    ///
    /// **Warning:** If `self` was produced by `load_merged()`, it contains data
    /// from all layers. Prefer [`update_field`](Self::update_field) which loads
    /// only the target layer, applies an update, then saves — avoiding cross-layer
    /// contamination.
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

// ── Runtime configuration (tunable constants) ───────────────────────────────

/// Runtime-tunable parameters with sensible defaults.
///
/// Unlike `Settings` (persisted to JSON), these are in-process tunables
/// loaded from environment variables. They centralise previously hard-coded
/// constants so that operators can adjust them without recompiling.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Max tools executing concurrently in a single turn.
    /// Env: `CLAUDE_MAX_TOOL_CONCURRENCY` (default: 10).
    pub max_tool_concurrency: usize,
    /// Token count at which auto-compaction triggers.
    /// Env: `CLAUDE_COMPACT_THRESHOLD` (default: 80 000).
    pub auto_compact_threshold: u64,
    /// Buffer tokens subtracted from context window for output reservation.
    /// Env: `CLAUDE_COMPACT_BUFFER` (default: 20 000).
    pub compact_buffer_tokens: u64,
    /// Maximum file size tools will read into memory (bytes).
    /// Env: `CLAUDE_MAX_READ_BYTES` (default: 50 MB).
    pub max_read_bytes: u64,
    /// Maximum content size tools will write (bytes).
    /// Env: `CLAUDE_MAX_WRITE_BYTES` (default: 10 MB).
    pub max_write_bytes: usize,
    /// Maximum tool output size in bytes before truncation.
    /// Env: `CLAUDE_MAX_TOOL_OUTPUT` (default: 30 KB).
    pub max_tool_output_bytes: usize,
    /// Maximum tool output lines before truncation.
    /// Env: `CLAUDE_MAX_TOOL_OUTPUT_LINES` (default: 2000).
    pub max_tool_output_lines: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_tool_concurrency: 10,
            auto_compact_threshold: 80_000,
            compact_buffer_tokens: 20_000,
            max_read_bytes: 50 * 1024 * 1024,
            max_write_bytes: 10 * 1024 * 1024,
            max_tool_output_bytes: 30 * 1024,
            max_tool_output_lines: 2_000,
        }
    }
}

impl RuntimeConfig {
    /// Load from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Load from an arbitrary lookup function. Testable without mutating env.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut cfg = Self::default();
        if let Some(v) = get("CLAUDE_MAX_TOOL_CONCURRENCY") {
            if let Ok(n) = v.parse::<usize>() { cfg.max_tool_concurrency = n; }
        }
        if let Some(v) = get("CLAUDE_COMPACT_THRESHOLD") {
            if let Ok(n) = v.parse::<u64>() { cfg.auto_compact_threshold = n; }
        }
        if let Some(v) = get("CLAUDE_COMPACT_BUFFER") {
            if let Ok(n) = v.parse::<u64>() { cfg.compact_buffer_tokens = n; }
        }
        if let Some(v) = get("CLAUDE_MAX_READ_BYTES") {
            if let Ok(n) = v.parse::<u64>() { cfg.max_read_bytes = n; }
        }
        if let Some(v) = get("CLAUDE_MAX_WRITE_BYTES") {
            if let Ok(n) = v.parse::<usize>() { cfg.max_write_bytes = n; }
        }
        if let Some(v) = get("CLAUDE_MAX_TOOL_OUTPUT") {
            if let Ok(n) = v.parse::<usize>() { cfg.max_tool_output_bytes = n; }
        }
        if let Some(v) = get("CLAUDE_MAX_TOOL_OUTPUT_LINES") {
            if let Ok(n) = v.parse::<usize>() { cfg.max_tool_output_lines = n; }
        }
        cfg
    }
}
