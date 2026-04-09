//! Plugin loader — discover and load plugins from standard paths.

use std::path::{Path, PathBuf};
use tracing::{info, warn, debug};

use super::manifest::{PluginManifest, PluginCommand, PluginSkill};

/// A loaded and validated plugin.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// The parsed manifest.
    pub manifest: PluginManifest,
    /// Absolute path to the plugin directory.
    pub dir: PathBuf,
    /// Where the plugin was loaded from.
    pub source: PluginSource,
}

/// Where a plugin was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// Project-local: `.claude/plugins/<name>/`
    Project,
    /// User-global: `~/.claude/plugins/<name>/`
    User,
}

impl std::fmt::Display for PluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project => write!(f, "project"),
            Self::User => write!(f, "user"),
        }
    }
}

/// Discovers, loads, and manages plugins.
pub struct PluginLoader {
    plugins: Vec<LoadedPlugin>,
}

impl PluginLoader {
    /// Create a new loader and discover plugins from standard paths.
    ///
    /// Search order (later entries override earlier for same name):
    /// 1. `~/.claude/plugins/` — user-global
    /// 2. `<cwd>/.claude/plugins/` — project-local
    pub fn discover(cwd: &Path) -> Self {
        let mut plugins = Vec::new();

        // User-global plugins
        if let Some(home) = dirs::home_dir() {
            let user_dir = home.join(".claude").join("plugins");
            load_plugins_from_dir(&user_dir, PluginSource::User, &mut plugins);
        }

        // Project-local plugins
        let project_dir = cwd.join(".claude").join("plugins");
        load_plugins_from_dir(&project_dir, PluginSource::Project, &mut plugins);

        let count = plugins.len();
        if count > 0 {
            info!("Loaded {} plugin(s)", count);
        }

        Self { plugins }
    }

    /// Get all loaded and enabled plugins.
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Get a plugin by name.
    pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| p.manifest.name == name)
    }

    /// Get all commands from all enabled plugins.
    pub fn all_commands(&self) -> Vec<(&LoadedPlugin, &super::manifest::PluginCommand)> {
        self.plugins.iter()
            .filter(|p| p.manifest.enabled)
            .flat_map(|p| p.manifest.commands.iter().map(move |c| (p, c)))
            .collect()
    }

    /// Get all skills from all enabled plugins.
    pub fn all_skills(&self) -> Vec<(&LoadedPlugin, &super::manifest::PluginSkill)> {
        self.plugins.iter()
            .filter(|p| p.manifest.enabled)
            .flat_map(|p| p.manifest.skills.iter().map(move |s| (p, s)))
            .collect()
    }

    /// Get all hooks for a specific event from all enabled plugins.
    pub fn hooks_for_event(&self, event: super::manifest::HookEvent) -> Vec<(&LoadedPlugin, &super::manifest::PluginHook)> {
        self.plugins.iter()
            .filter(|p| p.manifest.enabled)
            .flat_map(|p| p.manifest.hooks.iter().map(move |h| (p, h)))
            .filter(|(_, h)| h.event == event)
            .collect()
    }

    /// Read a prompt file from a plugin directory.
    pub fn read_prompt_file(plugin: &LoadedPlugin, relative_path: &str) -> Option<String> {
        let path = plugin.dir.join(relative_path);
        match std::fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(e) => {
                warn!("Failed to read plugin prompt file {:?}: {}", path, e);
                None
            }
        }
    }

    /// Get the effective prompt for a command (file or inline).
    pub fn command_prompt(plugin: &LoadedPlugin, cmd: &super::manifest::PluginCommand) -> Option<String> {
        if let Some(ref file) = cmd.prompt_file {
            Self::read_prompt_file(plugin, file)
        } else {
            cmd.prompt.clone()
        }
    }

    /// Get all MCP server configurations from enabled plugins.
    ///
    /// Returns a vec of (server_name, config_value) pairs.
    /// The config value is expected to have at minimum a "command" field.
    pub fn all_mcp_servers(&self) -> Vec<(String, &serde_json::Value)> {
        self.plugins.iter()
            .filter(|p| p.manifest.enabled)
            .flat_map(|p| p.manifest.mcp.iter())
            .map(|(name, config)| (name.clone(), config))
            .collect()
    }

    /// Total number of loaded plugins.
    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    /// Number of enabled plugins.
    pub fn enabled_count(&self) -> usize {
        self.plugins.iter().filter(|p| p.manifest.enabled).count()
    }

    /// Install a plugin from a local directory path to the user-global plugins dir.
    ///
    /// Copies the plugin directory (or extracts if `.zip`) to `~/.claude/plugins/<name>/`.
    /// Returns the plugin name on success.
    pub fn install_from_path(source: &Path) -> anyhow::Result<String> {
        // Determine if source is a directory or zip
        if source.is_dir() {
            // Must have plugin.json or .claude-plugin/plugin.json
            let manifest_path = if source.join("plugin.json").exists() {
                source.join("plugin.json")
            } else if source.join(".claude-plugin").join("plugin.json").exists() {
                source.join(".claude-plugin").join("plugin.json")
            } else {
                anyhow::bail!("No plugin.json found in {}", source.display());
            };

            let content = std::fs::read_to_string(&manifest_path)?;
            let manifest: PluginManifest = serde_json::from_str(&content)?;
            if manifest.name.is_empty() {
                anyhow::bail!("Plugin name cannot be empty");
            }

            let dest = user_plugins_dir()?.join(&manifest.name);
            if dest.exists() {
                // Remove existing version
                std::fs::remove_dir_all(&dest)?;
            }
            copy_dir_recursive(source, &dest)?;

            info!("Installed plugin '{}' v{} to {}", manifest.name, manifest.version, dest.display());
            Ok(manifest.name)
        } else if source.extension().and_then(|e| e.to_str()) == Some("zip") {
            Self::install_from_zip(source)
        } else {
            anyhow::bail!(
                "Expected a directory or .zip file, got: {}",
                source.display()
            );
        }
    }

    /// Install a plugin from a .zip archive.
    fn install_from_zip(_zip_path: &Path) -> anyhow::Result<String> {
        anyhow::bail!(
            "Zip plugin installation is not yet supported. \
             Please extract the archive and use `/plugin install <directory>`."
        );
    }
}

/// Load plugins from a directory. Each subdirectory with a `plugin.json` is a plugin.
/// Also supports DXT-format plugins (`.claude-plugin/plugin.json`).
fn load_plugins_from_dir(dir: &Path, source: PluginSource, plugins: &mut Vec<LoadedPlugin>) {
    if !dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("Cannot read plugin directory {:?}: {}", dir, e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Try simple format first: plugin.json at root
        let simple_manifest = path.join("plugin.json");
        // Then try DXT format: .claude-plugin/plugin.json
        let dxt_manifest = path.join(".claude-plugin").join("plugin.json");

        let manifest_path = if simple_manifest.exists() {
            simple_manifest
        } else if dxt_manifest.exists() {
            dxt_manifest
        } else {
            continue;
        };

        match load_single_plugin(&manifest_path, &path, source) {
            Ok(plugin) => {
                // Check for duplicate names (later source wins)
                plugins.retain(|p| p.manifest.name != plugin.manifest.name);
                info!(
                    "Loaded plugin '{}' v{} from {:?} ({})",
                    plugin.manifest.name, plugin.manifest.version, path, source
                );
                plugins.push(plugin);
            }
            Err(e) => {
                warn!("Failed to load plugin from {:?}: {}", path, e);
            }
        }
    }
}

/// Load a single plugin from its manifest file.
fn load_single_plugin(
    manifest_path: &Path,
    plugin_dir: &Path,
    source: PluginSource,
) -> anyhow::Result<LoadedPlugin> {
    let content = std::fs::read_to_string(manifest_path)?;
    let mut manifest: PluginManifest = serde_json::from_str(&content)?;

    // Validate: name must not be empty
    if manifest.name.is_empty() {
        anyhow::bail!("Plugin name cannot be empty");
    }

    // Validate: command names must not conflict with built-in commands
    let builtins = [
        "help", "clear", "model", "compact", "cost", "skills", "memory",
        "session", "diff", "status", "permissions", "config", "undo",
        "review", "doctor", "init", "commit", "pr", "bug", "search",
        "version", "login", "logout", "context", "export", "mcp",
        "commit-push-pr", "cpp", "exit", "quit",
    ];
    for cmd in &manifest.commands {
        if builtins.contains(&cmd.name.as_str()) {
            anyhow::bail!(
                "Plugin command '{}' conflicts with built-in command",
                cmd.name
            );
        }
    }

    // Auto-discover commands from commands/ directory (DXT convention)
    let cmds_dir = plugin_dir.join("commands");
    if cmds_dir.is_dir() {
        let existing_names: std::collections::HashSet<String> =
            manifest.commands.iter().map(|c| c.name.clone()).collect();
        if let Ok(entries) = std::fs::read_dir(&cmds_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !existing_names.contains(stem) && !builtins.contains(&stem) {
                            let relative = format!("commands/{}.md", stem);
                            // Extract description from first heading
                            let desc = std::fs::read_to_string(&path)
                                .ok()
                                .and_then(|c| c.lines().next()
                                    .and_then(|l| l.strip_prefix('#'))
                                    .map(|s| s.trim().to_string()))
                                .unwrap_or_default();
                            manifest.commands.push(PluginCommand {
                                name: stem.to_string(),
                                description: desc,
                                prompt_file: Some(relative),
                                prompt: None,
                                allowed_tools: Vec::new(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Auto-discover skills from skills/ directory (DXT convention)
    let skills_dir = plugin_dir.join("skills");
    if skills_dir.is_dir() {
        let existing_names: std::collections::HashSet<String> =
            manifest.skills.iter().map(|s| s.name.clone()).collect();
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_file = path.join("SKILL.md");
                    if skill_file.exists() {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if !existing_names.contains(name) {
                                let relative = format!("skills/{}/SKILL.md", name);
                                manifest.skills.push(PluginSkill {
                                    name: name.to_string(),
                                    description: String::new(),
                                    prompt_file: relative,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Validate: prompt files exist
    for cmd in &manifest.commands {
        if let Some(ref file) = cmd.prompt_file {
            let path = plugin_dir.join(file);
            if !path.exists() {
                warn!(
                    "Plugin '{}' command '{}' references missing prompt file: {:?}",
                    manifest.name, cmd.name, path
                );
            }
        }
    }
    for skill in &manifest.skills {
        let path = plugin_dir.join(&skill.prompt_file);
        if !path.exists() {
            warn!(
                "Plugin '{}' skill '{}' references missing prompt file: {:?}",
                manifest.name, skill.name, path
            );
        }
    }

    Ok(LoadedPlugin {
        manifest,
        dir: plugin_dir.to_path_buf(),
        source,
    })
}

/// Get the user-global plugins directory (`~/.claude/plugins/`).
fn user_plugins_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let dir = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    fn create_plugin_dir(base: &Path, name: &str, manifest_json: &str) -> PathBuf {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plugin.json"), manifest_json).unwrap();
        dir
    }

    #[test]
    fn test_discover_empty() {
        let tmp = TempDir::new().unwrap();
        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 0);
    }

    #[test]
    fn test_discover_project_plugin() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "test-plugin", r#"{"name": "test-plugin", "description": "A test"}"#);

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);
        assert_eq!(loader.plugins()[0].manifest.name, "test-plugin");
        assert_eq!(loader.plugins()[0].source, PluginSource::Project);
    }

    #[test]
    fn test_discover_with_commands() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let _plugin_dir = create_plugin_dir(&plugins_dir, "cmd-plugin", r#"{
            "name": "cmd-plugin",
            "commands": [
                {"name": "greet", "description": "Say hello", "prompt": "Greet the user"}
            ]
        }"#);

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);

        let cmds = loader.all_commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].1.name, "greet");

        let prompt = PluginLoader::command_prompt(cmds[0].0, cmds[0].1);
        assert_eq!(prompt.as_deref(), Some("Greet the user"));
    }

    #[test]
    fn test_discover_with_prompt_file() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = create_plugin_dir(&plugins_dir, "file-plugin", r#"{
            "name": "file-plugin",
            "commands": [
                {"name": "analyze", "description": "Analyze code", "promptFile": "prompts/analyze.md"}
            ]
        }"#);

        // Create the prompt file
        let prompts_dir = plugin_dir.join("prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("analyze.md"), "Analyze the code for quality issues.").unwrap();

        let loader = PluginLoader::discover(tmp.path());
        let cmds = loader.all_commands();
        let prompt = PluginLoader::command_prompt(cmds[0].0, cmds[0].1);
        assert_eq!(prompt.as_deref(), Some("Analyze the code for quality issues."));
    }

    #[test]
    fn test_builtin_command_conflict() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "bad-plugin", r#"{
            "name": "bad-plugin",
            "commands": [{"name": "help", "description": "Override help"}]
        }"#);

        let loader = PluginLoader::discover(tmp.path());
        // Should fail to load due to conflict
        assert_eq!(loader.count(), 0);
    }

    #[test]
    fn test_disabled_plugin() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "off-plugin", r#"{
            "name": "off-plugin",
            "enabled": false,
            "commands": [{"name": "noop", "description": "Does nothing"}]
        }"#);

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);
        assert_eq!(loader.enabled_count(), 0);
        assert!(loader.all_commands().is_empty());
    }

    #[test]
    fn test_hooks_for_event() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "hook-plugin", r#"{
            "name": "hook-plugin",
            "hooks": [
                {"event": "pre_tool", "command": "echo pre"},
                {"event": "post_tool", "command": "echo post"},
                {"event": "pre_tool", "command": "echo pre2"}
            ]
        }"#);

        let loader = PluginLoader::discover(tmp.path());
        let pre_hooks = loader.hooks_for_event(super::super::manifest::HookEvent::PreTool);
        assert_eq!(pre_hooks.len(), 2);
        let post_hooks = loader.hooks_for_event(super::super::manifest::HookEvent::PostTool);
        assert_eq!(post_hooks.len(), 1);
    }

    #[test]
    fn test_get_by_name() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "alpha", r#"{"name": "alpha"}"#);
        create_plugin_dir(&plugins_dir, "beta", r#"{"name": "beta"}"#);

        let loader = PluginLoader::discover(tmp.path());
        assert!(loader.get("alpha").is_some());
        assert!(loader.get("beta").is_some());
        assert!(loader.get("gamma").is_none());
    }

    #[test]
    fn test_plugin_source_display() {
        assert_eq!(PluginSource::Project.to_string(), "project");
        assert_eq!(PluginSource::User.to_string(), "user");
    }

    #[test]
    fn test_invalid_manifest_ignored() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let bad_dir = plugins_dir.join("bad");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join("plugin.json"), "not valid json {{{").unwrap();

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 0);
    }

    #[test]
    fn test_empty_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "nameless", r#"{"name": ""}"#);

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 0);
    }

    #[test]
    fn test_dxt_format_discovery() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = plugins_dir.join("dxt-plugin");
        let manifest_dir = plugin_dir.join(".claude-plugin");
        fs::create_dir_all(&manifest_dir).unwrap();
        fs::write(manifest_dir.join("plugin.json"), r#"{"name": "dxt-plugin", "version": "2.0.0"}"#).unwrap();

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);
        assert_eq!(loader.plugins()[0].manifest.name, "dxt-plugin");
        assert_eq!(loader.plugins()[0].manifest.version, "2.0.0");
    }

    #[test]
    fn test_auto_discover_commands_dir() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = create_plugin_dir(&plugins_dir, "auto-cmd", r#"{"name": "auto-cmd"}"#);

        // Create commands/ directory with .md files
        let cmds_dir = plugin_dir.join("commands");
        fs::create_dir_all(&cmds_dir).unwrap();
        fs::write(cmds_dir.join("deploy.md"), "# Deploy\nDeploy to production").unwrap();
        fs::write(cmds_dir.join("test.md"), "# Test\nRun tests").unwrap();

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);
        let cmds = loader.all_commands();
        assert_eq!(cmds.len(), 2);

        let names: Vec<&str> = cmds.iter().map(|(_, c)| c.name.as_str()).collect();
        assert!(names.contains(&"deploy"));
        assert!(names.contains(&"test"));

        // Verify prompt file reading works
        let deploy = cmds.iter().find(|(_, c)| c.name == "deploy").unwrap();
        let prompt = PluginLoader::command_prompt(deploy.0, deploy.1);
        assert!(prompt.unwrap().contains("Deploy to production"));
    }

    #[test]
    fn test_auto_discover_skills_dir() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = create_plugin_dir(&plugins_dir, "auto-skill", r#"{"name": "auto-skill"}"#);

        // Create skills/ directory with SKILL.md
        let skill_dir = plugin_dir.join("skills").join("code-review");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "Review code changes carefully").unwrap();

        let loader = PluginLoader::discover(tmp.path());
        let skills = loader.all_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].1.name, "code-review");
    }

    #[test]
    fn test_manifest_commands_take_precedence_over_auto_discover() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = create_plugin_dir(&plugins_dir, "precedence", r#"{
            "name": "precedence",
            "commands": [{"name": "deploy", "description": "From manifest", "prompt": "manifest deploy"}]
        }"#);

        // commands/deploy.md should NOT override manifest
        let cmds_dir = plugin_dir.join("commands");
        fs::create_dir_all(&cmds_dir).unwrap();
        fs::write(cmds_dir.join("deploy.md"), "# Deploy\nFrom directory").unwrap();

        let loader = PluginLoader::discover(tmp.path());
        let cmds = loader.all_commands();
        assert_eq!(cmds.len(), 1);
        // Manifest command wins — uses inline prompt
        let prompt = PluginLoader::command_prompt(cmds[0].0, cmds[0].1);
        assert_eq!(prompt.as_deref(), Some("manifest deploy"));
    }

    #[test]
    fn test_simple_format_preferred_over_dxt() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        let plugin_dir = plugins_dir.join("dual-format");

        // Create both formats
        fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
        fs::write(plugin_dir.join("plugin.json"), r#"{"name": "simple-wins"}"#).unwrap();
        fs::write(plugin_dir.join(".claude-plugin").join("plugin.json"), r#"{"name": "dxt-loses"}"#).unwrap();

        let loader = PluginLoader::discover(tmp.path());
        assert_eq!(loader.count(), 1);
        // Simple format (plugin.json at root) wins
        assert_eq!(loader.plugins()[0].manifest.name, "simple-wins");
    }

    #[test]
    fn test_all_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join(".claude").join("plugins");
        create_plugin_dir(&plugins_dir, "mcp-plugin", r#"{
            "name": "mcp-plugin",
            "mcp": {
                "git-mcp": {"command": "git-mcp-server", "args": ["--stdio"]},
                "db-mcp": {"command": "db-mcp-server"}
            }
        }"#);

        let loader = PluginLoader::discover(tmp.path());
        let servers = loader.all_mcp_servers();
        assert_eq!(servers.len(), 2);

        let names: Vec<&str> = servers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"git-mcp"));
        assert!(names.contains(&"db-mcp"));
    }

    #[test]
    fn test_install_from_path() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("my-plugin");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("plugin.json"), r#"{"name": "test-install", "version": "1.0.0"}"#).unwrap();

        let result = PluginLoader::install_from_path(&source);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test-install");

        // Verify it was installed
        let home = dirs::home_dir().unwrap();
        let installed = home.join(".claude").join("plugins").join("test-install").join("plugin.json");
        assert!(installed.exists());

        // Clean up
        let _ = fs::remove_dir_all(home.join(".claude").join("plugins").join("test-install"));
    }

    #[test]
    fn test_install_no_manifest() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("empty-dir");
        fs::create_dir_all(&source).unwrap();

        let result = PluginLoader::install_from_path(&source);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No plugin.json"));
    }
}
