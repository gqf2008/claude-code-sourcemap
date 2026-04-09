//! Plugin manifest types and parsing.
//!
//! Implements a subset of the DXT (Developer Extension) plugin manifest
//! format used by Claude Code.  A plugin is a directory containing a
//! `.claude-plugin/plugin.json` manifest and optional component directories
//! (`commands/`, `agents/`, `skills/`, `hooks/`, `output-styles/`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Manifest ────────────────────────────────────────────────────────────────

/// Top-level plugin manifest (`plugin.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    /// Unique identifier (kebab-case, no spaces).
    pub name: String,

    /// Semantic version string (e.g. "1.2.3").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Brief user-facing description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Author information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PluginAuthor>,

    /// Homepage / documentation URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    /// Source code repository URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// SPDX license identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,

    /// Discovery tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,

    /// Dependent plugins that must be enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<String>>,

    // ── Component fields (all optional) ─────────────────────────────────

    /// Slash commands — path(s) to `.md` files or inline map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<CommandsSpec>,

    /// Agent definitions — path(s) to `.md` files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<StringOrArray>,

    /// Skill directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<StringOrArray>,

    /// MCP server configurations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<McpServersSpec>,

    /// Hook configurations — path or inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<serde_json::Value>,

    /// User configuration options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_config: Option<HashMap<String, UserConfigOption>>,

    /// Plugin-scoped settings to merge when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<HashMap<String, serde_json::Value>>,
}

/// Author metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

// ── Flexible field types ────────────────────────────────────────────────────

/// A field that accepts either a single string or an array of strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrArray {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrArray {
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            Self::Single(s) => vec![s.as_str()],
            Self::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Commands specification — single path, array of paths, or object map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandsSpec {
    Path(String),
    Paths(Vec<String>),
    Map(HashMap<String, CommandMetadata>),
}

/// Metadata for a named command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
}

/// MCP server spec — path to json, inline map, or mixed array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServersSpec {
    Path(String),
    Map(HashMap<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
}

/// User-configurable option prompted at plugin enable time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfigOption {
    #[serde(rename = "type")]
    pub config_type: UserConfigType,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub sensitive: bool,
}

/// Supported user config value types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserConfigType {
    String,
    Number,
    Boolean,
    Directory,
    File,
}

// ── LoadedPlugin ────────────────────────────────────────────────────────────

/// A fully resolved plugin after discovery and loading.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// Manifest parsed from plugin.json.
    pub manifest: PluginManifest,
    /// Root directory of the plugin.
    pub root: PathBuf,
    /// Source (marketplace name, or "local").
    pub source: String,
    /// Whether the plugin is currently enabled.
    pub enabled: bool,
}

impl LoadedPlugin {
    /// Unique identifier: `{name}@{source}`.
    pub fn id(&self) -> String {
        format!("{}@{}", self.manifest.name, self.source)
    }

    /// Path to the manifest file.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(".claude-plugin").join("plugin.json")
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Parse a plugin manifest from a JSON string.
pub fn parse_manifest(json: &str) -> Result<PluginManifest, PluginError> {
    let manifest: PluginManifest =
        serde_json::from_str(json).map_err(|e| PluginError::ManifestParse(e.to_string()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Parse a plugin manifest from a file path.
pub fn parse_manifest_file(path: &Path) -> Result<PluginManifest, PluginError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| PluginError::Io(format!("{}: {e}", path.display())))?;
    parse_manifest(&content)
}

/// Basic validation rules for a parsed manifest.
fn validate_manifest(m: &PluginManifest) -> Result<(), PluginError> {
    if m.name.is_empty() {
        return Err(PluginError::ManifestValidation(
            "Plugin name cannot be empty".into(),
        ));
    }
    if m.name.contains(' ') {
        return Err(PluginError::ManifestValidation(
            "Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")".into(),
        ));
    }
    Ok(())
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Plugin-related errors.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("Manifest parse error: {0}")]
    ManifestParse(String),
    #[error("Manifest validation error: {0}")]
    ManifestValidation(String),
    #[error("Plugin not found: {0}")]
    NotFound(String),
    #[error("Dependency unsatisfied: {0}")]
    DependencyUnsatisfied(String),
    #[error("MCP config invalid: {0}")]
    McpConfigInvalid(String),
}

// ── Discovery ───────────────────────────────────────────────────────────────

/// Standard plugin directories.
pub const PLUGIN_MANIFEST_DIR: &str = ".claude-plugin";
pub const PLUGIN_MANIFEST_FILE: &str = "plugin.json";
pub const PLUGIN_COMMANDS_DIR: &str = "commands";
pub const PLUGIN_AGENTS_DIR: &str = "agents";
pub const PLUGIN_SKILLS_DIR: &str = "skills";
pub const PLUGIN_HOOKS_DIR: &str = "hooks";

/// Default plugin cache/install directory: `~/.claude/plugins/`.
pub fn default_plugins_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("plugins")
}

/// Discover plugins in a directory.
///
/// Scans immediate subdirectories for `.claude-plugin/plugin.json`.
pub fn discover_plugins(dir: &Path) -> Vec<Result<LoadedPlugin, PluginError>> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join(PLUGIN_MANIFEST_DIR).join(PLUGIN_MANIFEST_FILE);
        if !manifest_path.exists() {
            continue;
        }

        match parse_manifest_file(&manifest_path) {
            Ok(manifest) => {
                results.push(Ok(LoadedPlugin {
                    manifest,
                    root: path,
                    source: "local".into(),
                    enabled: true,
                }));
            }
            Err(e) => results.push(Err(e)),
        }
    }

    results
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_minimal_manifest() {
        let json = r#"{ "name": "my-plugin" }"#;
        let m = parse_manifest(json).unwrap();
        assert_eq!(m.name, "my-plugin");
        assert!(m.version.is_none());
        assert!(m.description.is_none());
    }

    #[test]
    fn parse_full_manifest() {
        let json = r#"{
            "name": "test-plugin",
            "version": "1.0.0",
            "description": "A test plugin",
            "author": { "name": "Alice", "email": "a@b.com" },
            "homepage": "https://example.com",
            "license": "MIT",
            "keywords": ["test", "demo"],
            "commands": { "hello": { "source": "./commands/hello.md" } },
            "agents": ["./agents/reviewer.md"],
            "skills": "./skills/",
            "mcpServers": { "my-server": { "command": "npx", "args": ["server"] } },
            "userConfig": {
                "apiKey": {
                    "type": "string",
                    "title": "API Key",
                    "description": "Your API key",
                    "required": true,
                    "sensitive": true
                }
            }
        }"#;
        let m = parse_manifest(json).unwrap();
        assert_eq!(m.name, "test-plugin");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert_eq!(m.license.as_deref(), Some("MIT"));
        assert_eq!(m.keywords.as_ref().unwrap().len(), 2);
        assert!(matches!(m.commands, Some(CommandsSpec::Map(_))));
        assert!(matches!(m.agents, Some(StringOrArray::Multiple(_))));
        assert!(matches!(m.skills, Some(StringOrArray::Single(_))));
        assert!(matches!(m.mcp_servers, Some(McpServersSpec::Map(_))));

        let uc = m.user_config.as_ref().unwrap();
        let api_key = &uc["apiKey"];
        assert_eq!(api_key.config_type, UserConfigType::String);
        assert!(api_key.required);
        assert!(api_key.sensitive);
    }

    #[test]
    fn empty_name_rejected() {
        let json = r#"{ "name": "" }"#;
        let err = parse_manifest(json).unwrap_err();
        assert!(matches!(err, PluginError::ManifestValidation(_)));
    }

    #[test]
    fn name_with_spaces_rejected() {
        let json = r#"{ "name": "my plugin" }"#;
        let err = parse_manifest(json).unwrap_err();
        assert!(matches!(err, PluginError::ManifestValidation(_)));
    }

    #[test]
    fn invalid_json_rejected() {
        let err = parse_manifest("not json").unwrap_err();
        assert!(matches!(err, PluginError::ManifestParse(_)));
    }

    #[test]
    fn string_or_array_single() {
        let json = r#"{ "name": "x", "agents": "./agents/a.md" }"#;
        let m = parse_manifest(json).unwrap();
        let agents = m.agents.as_ref().unwrap();
        assert_eq!(agents.to_vec(), vec!["./agents/a.md"]);
    }

    #[test]
    fn string_or_array_multiple() {
        let json = r#"{ "name": "x", "agents": ["a.md", "b.md"] }"#;
        let m = parse_manifest(json).unwrap();
        let agents = m.agents.as_ref().unwrap();
        assert_eq!(agents.to_vec(), vec!["a.md", "b.md"]);
    }

    #[test]
    fn commands_as_path() {
        let json = r#"{ "name": "x", "commands": "./commands/" }"#;
        let m = parse_manifest(json).unwrap();
        assert!(matches!(m.commands, Some(CommandsSpec::Path(_))));
    }

    #[test]
    fn commands_as_map() {
        let json = r#"{ "name": "x", "commands": { "hello": { "source": "./hello.md" } } }"#;
        let m = parse_manifest(json).unwrap();
        if let Some(CommandsSpec::Map(map)) = &m.commands {
            assert!(map.contains_key("hello"));
            assert_eq!(map["hello"].source.as_deref(), Some("./hello.md"));
        } else {
            panic!("Expected CommandsSpec::Map");
        }
    }

    #[test]
    fn loaded_plugin_id() {
        let p = LoadedPlugin {
            manifest: parse_manifest(r#"{"name":"test"}"#).unwrap(),
            root: PathBuf::from("/tmp/test"),
            source: "my-marketplace".into(),
            enabled: true,
        };
        assert_eq!(p.id(), "test@my-marketplace");
    }

    #[test]
    fn discover_plugins_in_tempdir() {
        let tmp = std::env::temp_dir().join("claude_plugin_test_discover");
        let _ = fs::remove_dir_all(&tmp);

        // Create a valid plugin
        let plugin_dir = tmp.join("demo-plugin").join(PLUGIN_MANIFEST_DIR);
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join(PLUGIN_MANIFEST_FILE),
            r#"{"name": "demo-plugin", "version": "0.1.0"}"#,
        )
        .unwrap();

        // Create an invalid plugin (bad json)
        let bad_dir = tmp.join("bad-plugin").join(PLUGIN_MANIFEST_DIR);
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join(PLUGIN_MANIFEST_FILE), "not json").unwrap();

        // Create a non-plugin directory (no manifest)
        fs::create_dir_all(tmp.join("not-a-plugin")).unwrap();

        let results = discover_plugins(&tmp);
        assert_eq!(results.len(), 2); // demo + bad

        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        let err_count = results.iter().filter(|r| r.is_err()).count();
        assert_eq!(ok_count, 1);
        assert_eq!(err_count, 1);

        let plugin = results.into_iter().find_map(|r| r.ok()).unwrap();
        assert_eq!(plugin.manifest.name, "demo-plugin");
        assert_eq!(plugin.source, "local");

        // Cleanup
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn discover_plugins_empty_dir() {
        let results = discover_plugins(Path::new("/nonexistent/path/12345"));
        assert!(results.is_empty());
    }

    #[test]
    fn user_config_types_deserialize() {
        let json = r#"{
            "name": "x",
            "userConfig": {
                "path": { "type": "directory", "title": "Dir", "description": "A dir" },
                "flag": { "type": "boolean", "title": "Flag", "description": "A flag" }
            }
        }"#;
        let m = parse_manifest(json).unwrap();
        let uc = m.user_config.unwrap();
        assert_eq!(uc["path"].config_type, UserConfigType::Directory);
        assert_eq!(uc["flag"].config_type, UserConfigType::Boolean);
    }

    #[test]
    fn default_plugins_dir_under_home() {
        let dir = default_plugins_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains(".claude"));
        assert!(dir_str.contains("plugins"));
    }

    #[test]
    fn mcp_servers_as_path() {
        let json = r#"{ "name": "x", "mcpServers": "./mcp.json" }"#;
        let m = parse_manifest(json).unwrap();
        assert!(matches!(m.mcp_servers, Some(McpServersSpec::Path(_))));
    }

    #[test]
    fn mcp_servers_as_map() {
        let json = r#"{ "name": "x", "mcpServers": { "s1": {"command": "node"} } }"#;
        let m = parse_manifest(json).unwrap();
        assert!(matches!(m.mcp_servers, Some(McpServersSpec::Map(_))));
    }
}
