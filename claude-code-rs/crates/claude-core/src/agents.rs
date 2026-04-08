//! Agent definitions — custom agent profiles loaded from `.claude/agents/*.md`.
//!
//! An agent definition is a Markdown document with YAML frontmatter that
//! configures a specialized sub-agent persona:
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: "Use when reviewing code for quality and security issues"
//! tools: [FileRead, Grep, GlobTool]
//! model: inherit
//! effort: high
//! memory: project
//! color: blue
//! ---
//! You are an expert code reviewer...
//! ```
//!
//! Agent definitions are loaded from `.claude/agents/` directories following
//! the same discovery pattern as skills (cwd → parent → ... → $HOME).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tracing::debug;

// ── Types ────────────────────────────────────────────────────────────────────

/// Where an agent definition was loaded from (priority: higher wins).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentSource {
    /// Built-in hardcoded agents (General, Explore, Plan, etc.)
    BuiltIn,
    /// User-global `~/.claude/agents/`
    User,
    /// Project-level `.claude/agents/` (version controlled)
    Project,
    /// Local override `.claude/agents/` (gitignored)
    Local,
    /// Provided by a plugin
    Plugin,
}

impl fmt::Display for AgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn => write!(f, "built-in"),
            Self::User => write!(f, "user (~/.claude/agents/)"),
            Self::Project => write!(f, "project (.claude/agents/)"),
            Self::Local => write!(f, "local (.claude/agents/)"),
            Self::Plugin => write!(f, "plugin"),
        }
    }
}

/// Memory scope for agents with persistent memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMemoryScope {
    User,
    Project,
    Local,
}

impl AgentMemoryScope {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

impl fmt::Display for AgentMemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
        }
    }
}

/// A custom agent definition loaded from a `.md` file or built-in.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// Unique identifier (lowercase, hyphens; e.g. `code-reviewer`).
    pub agent_type: String,
    /// Human-readable description of when to use this agent.
    pub description: String,
    /// System prompt (Markdown body of the `.md` file).
    pub system_prompt: String,
    /// Allowed tools — empty means "all tools". `["*"]` also means all.
    pub allowed_tools: Vec<String>,
    /// Explicitly denied tools (takes priority over allowed).
    pub disallowed_tools: Vec<String>,
    /// Model override: specific model name, or `"inherit"` / empty for main model.
    pub model: Option<String>,
    /// Effort level override (e.g. "low", "medium", "high", "1"-"5").
    pub effort: Option<String>,
    /// Persistent memory scope.
    pub memory: Option<AgentMemoryScope>,
    /// Display color name (e.g. "blue", "red", "green").
    pub color: Option<String>,
    /// Permission mode override ("ask" or "auto").
    pub permission_mode: Option<String>,
    /// Maximum agentic turns before stopping.
    pub max_turns: Option<u32>,
    /// Whether this agent should default to background execution.
    pub background: bool,
    /// Skill names to preload.
    pub skills: Vec<String>,
    /// Text prepended to the first user turn.
    pub initial_prompt: Option<String>,
    /// Where this definition was loaded from.
    pub source: AgentSource,
    /// File path of the `.md` definition (None for built-in).
    pub file_path: Option<PathBuf>,
    /// Base directory the agent was loaded from.
    pub base_dir: Option<PathBuf>,
}

impl AgentDefinition {
    /// Whether the model field means "use whatever the main model is".
    pub fn inherits_model(&self) -> bool {
        match &self.model {
            None => true,
            Some(m) => m.eq_ignore_ascii_case("inherit") || m.is_empty(),
        }
    }

    /// Whether this is a built-in agent.
    pub fn is_builtin(&self) -> bool {
        self.source == AgentSource::BuiltIn
    }

    /// Generate the markdown content for writing to a `.md` file.
    pub fn to_markdown(&self) -> String {
        let mut fm = String::from("---\n");
        fm.push_str(&format!("name: {}\n", self.agent_type));
        fm.push_str(&format!("description: \"{}\"\n", self.description.replace('"', "\\\"")));
        if !self.allowed_tools.is_empty() {
            fm.push_str(&format!("tools: [{}]\n", self.allowed_tools.join(", ")));
        }
        if !self.disallowed_tools.is_empty() {
            fm.push_str(&format!("disallowed_tools: [{}]\n", self.disallowed_tools.join(", ")));
        }
        if let Some(ref m) = self.model {
            fm.push_str(&format!("model: {}\n", m));
        }
        if let Some(ref e) = self.effort {
            fm.push_str(&format!("effort: {}\n", e));
        }
        if let Some(ref mem) = self.memory {
            fm.push_str(&format!("memory: {}\n", mem));
        }
        if let Some(ref c) = self.color {
            fm.push_str(&format!("color: {}\n", c));
        }
        if let Some(ref pm) = self.permission_mode {
            fm.push_str(&format!("permissionMode: {}\n", pm));
        }
        if let Some(mt) = self.max_turns {
            fm.push_str(&format!("maxTurns: {}\n", mt));
        }
        if self.background {
            fm.push_str("background: true\n");
        }
        if !self.skills.is_empty() {
            fm.push_str(&format!("skills: [{}]\n", self.skills.join(", ")));
        }
        if let Some(ref ip) = self.initial_prompt {
            fm.push_str(&format!("initialPrompt: \"{}\"\n", ip.replace('"', "\\\"")));
        }
        fm.push_str("---\n\n");
        fm.push_str(&self.system_prompt);
        if !self.system_prompt.ends_with('\n') {
            fm.push('\n');
        }
        fm
    }
}

// ── Cache ────────────────────────────────────────────────────────────────────

fn cache() -> &'static Mutex<HashMap<PathBuf, Vec<AgentDefinition>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<AgentDefinition>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get agent definitions with memoization (cached by canonical `cwd`).
pub fn get_agents(cwd: &Path) -> Vec<AgentDefinition> {
    let key = cwd.to_path_buf();
    let map = cache().lock().unwrap();
    if let Some(cached) = map.get(&key) {
        return cached.clone();
    }
    drop(map);
    let agents = load_agents(cwd);
    let mut map = cache().lock().unwrap();
    map.entry(key).or_insert(agents).clone()
}

/// Invalidate the agent cache so the next [`get_agents`] call rescans disk.
pub fn clear_agent_cache() {
    cache().lock().unwrap().clear();
}

// ── Directory discovery ──────────────────────────────────────────────────────

/// Collect `.claude/agents/` directories from `cwd` up to `$HOME`.
fn agent_dirs(cwd: &Path) -> Vec<(PathBuf, AgentSource)> {
    let home = dirs::home_dir();
    let mut dirs = Vec::new();
    let mut current = Some(cwd.to_path_buf());
    let is_first = std::cell::Cell::new(true);

    while let Some(dir) = current {
        let source = if is_first.get() {
            is_first.set(false);
            AgentSource::Project
        } else if home.as_ref() == Some(&dir) {
            AgentSource::User
        } else {
            AgentSource::Project
        };
        dirs.push((dir.join(".claude").join("agents"), source));

        if home.as_ref() == Some(&dir) {
            break;
        }
        let parent = dir.parent().map(|p| p.to_path_buf());
        if parent.as_ref() == Some(&dir) || parent.is_none() {
            break;
        }
        current = parent;
    }

    // Always include user agents dir
    if let Some(ref home) = home {
        let home_agents = home.join(".claude").join("agents");
        if !dirs.iter().any(|(d, _)| d == &home_agents) {
            dirs.push((home_agents, AgentSource::User));
        }
    }
    dirs
}

/// Load all agents from standard locations (uncached).
/// Project-level agents shadow user-level agents with the same name.
pub fn load_agents(cwd: &Path) -> Vec<AgentDefinition> {
    let dirs = agent_dirs(cwd);
    let mut agents: Vec<AgentDefinition> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (dir, source) in dirs {
        if !dir.exists() {
            continue;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                debug!("Cannot read agents dir {}: {}", dir.display(), e);
                continue;
            }
        };

        for entry in rd.flatten() {
            let path = entry.path();

            // Only process .md files
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase()
                .replace(' ', "-");

            if name.is_empty() || seen.contains(&name) {
                continue;
            }

            if let Some(agent) = parse_agent_file(&path, &name, source.clone()) {
                debug!("Loaded agent '{}' from {}", name, path.display());
                seen.insert(name);
                agents.push(agent);
            }
        }
    }

    agents
}

/// Parse a single agent definition from a `.md` file.
fn parse_agent_file(path: &Path, fallback_name: &str, source: AgentSource) -> Option<AgentDefinition> {
    let content = std::fs::read_to_string(path).ok()?;
    let (fm, body) = split_frontmatter(&content);
    let fm_str = fm.as_deref();

    let agent_type = fm_str
        .and_then(|f| extract_string(f, "name"))
        .unwrap_or_else(|| fallback_name.to_string())
        .to_lowercase()
        .replace(' ', "-");

    let description = fm_str
        .and_then(|f| extract_string(f, "description"))
        .unwrap_or_else(|| agent_type.replace('-', " "));

    let allowed_tools = fm_str
        .and_then(|f| extract_list(f, "tools"))
        .unwrap_or_default();

    let disallowed_tools = fm_str
        .and_then(|f| {
            extract_list(f, "disallowed_tools")
                .or_else(|| extract_list(f, "disallowedTools"))
        })
        .unwrap_or_default();

    let model = fm_str.and_then(|f| extract_string(f, "model"));
    let effort = fm_str.and_then(|f| extract_string(f, "effort"));
    let color = fm_str.and_then(|f| extract_string(f, "color"));
    let permission_mode = fm_str.and_then(|f| {
        extract_string(f, "permissionMode")
            .or_else(|| extract_string(f, "permission_mode"))
    });
    let max_turns = fm_str
        .and_then(|f| {
            extract_string(f, "maxTurns")
                .or_else(|| extract_string(f, "max_turns"))
        })
        .and_then(|s| s.parse::<u32>().ok());

    let background = fm_str
        .and_then(|f| extract_string(f, "background"))
        .map(|v| v == "true")
        .unwrap_or(false);

    let memory = fm_str
        .and_then(|f| extract_string(f, "memory"))
        .and_then(|s| AgentMemoryScope::from_str(&s));

    let skills = fm_str
        .and_then(|f| extract_list(f, "skills"))
        .unwrap_or_default();

    let initial_prompt = fm_str.and_then(|f| {
        extract_string(f, "initialPrompt")
            .or_else(|| extract_string(f, "initial_prompt"))
    });

    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        debug!("Skipping agent '{}': empty system prompt", agent_type);
        return None;
    }

    Some(AgentDefinition {
        agent_type,
        description,
        system_prompt,
        allowed_tools,
        disallowed_tools,
        model,
        effort,
        memory,
        color,
        permission_mode,
        max_turns,
        background,
        skills,
        initial_prompt,
        source,
        file_path: Some(path.to_path_buf()),
        base_dir: path.parent().map(|p| p.to_path_buf()),
    })
}

// ── Agent file operations ────────────────────────────────────────────────────

/// Get the directory path for storing agents at a given scope.
pub fn agent_dir_for_source(source: &AgentSource, cwd: &Path) -> Option<PathBuf> {
    match source {
        AgentSource::User => dirs::home_dir().map(|h| h.join(".claude").join("agents")),
        AgentSource::Project | AgentSource::Local => Some(cwd.join(".claude").join("agents")),
        _ => None,
    }
}

/// Write an agent definition to disk as a `.md` file.
pub fn save_agent(agent: &AgentDefinition, cwd: &Path) -> Result<PathBuf, String> {
    let dir = agent_dir_for_source(&agent.source, cwd)
        .ok_or("Cannot determine agent directory for this source")?;

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create agents directory: {}", e))?;

    let file_path = dir.join(format!("{}.md", agent.agent_type));
    let content = agent.to_markdown();

    std::fs::write(&file_path, &content)
        .map_err(|e| format!("Failed to write agent file: {}", e))?;

    // Invalidate cache
    clear_agent_cache();

    Ok(file_path)
}

/// Delete an agent definition file from disk.
pub fn delete_agent(agent: &AgentDefinition) -> Result<(), String> {
    let path = agent.file_path.as_ref()
        .ok_or("Cannot delete a built-in agent")?;

    if !path.exists() {
        return Err(format!("Agent file not found: {}", path.display()));
    }

    std::fs::remove_file(path)
        .map_err(|e| format!("Failed to delete agent file: {}", e))?;

    clear_agent_cache();
    Ok(())
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Validation result for an agent definition.
pub struct AgentValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl AgentValidation {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate an agent definition before saving.
pub fn validate_agent(agent: &AgentDefinition, existing: &[AgentDefinition]) -> AgentValidation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Name format: alphanumeric + hyphens, 3-50 chars
    if agent.agent_type.len() < 2 {
        errors.push("Agent name must be at least 2 characters".into());
    }
    if agent.agent_type.len() > 50 {
        errors.push("Agent name must be 50 characters or less".into());
    }
    if !agent.agent_type.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        errors.push("Agent name must contain only alphanumeric characters and hyphens".into());
    }
    if agent.agent_type.starts_with('-') || agent.agent_type.ends_with('-') {
        errors.push("Agent name must not start or end with a hyphen".into());
    }

    // Check for duplicate names (excluding self for edits)
    let self_path = agent.file_path.as_ref();
    for other in existing {
        if other.agent_type == agent.agent_type && other.file_path.as_ref() != self_path {
            warnings.push(format!(
                "Agent '{}' already exists from {} — this definition will shadow it",
                agent.agent_type, other.source,
            ));
        }
    }

    // Description
    if agent.description.is_empty() {
        errors.push("Description is required".into());
    }

    // System prompt
    if agent.system_prompt.len() < 10 {
        errors.push("System prompt must be at least 10 characters".into());
    }

    // Max turns
    if let Some(mt) = agent.max_turns {
        if mt == 0 {
            errors.push("maxTurns must be a positive integer".into());
        }
        if mt > 1000 {
            warnings.push("maxTurns > 1000 is unusually high".into());
        }
    }

    AgentValidation { errors, warnings }
}

// ── Frontmatter parsing (reused from skills) ────────────────────────────────

/// Split `---\n<yaml>\n---\n<body>` → `(Some(yaml), body)`.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = content.replace("\r\n", "\n");
    let s = normalized.trim_start();
    if !s.starts_with("---") {
        return (None, s.to_string());
    }
    let rest = s[3..].trim_start_matches('\n');
    if let Some(end) = rest.find("\n---") {
        let yaml = rest[..end].to_string();
        let body = rest[end + 4..].trim_start_matches('\n').to_string();
        (Some(yaml), body)
    } else {
        (None, s.to_string())
    }
}

/// Extract a scalar string from simplistic YAML (`key: value`).
fn extract_string(yaml: &str, key: &str) -> Option<String> {
    for line in yaml.lines() {
        if let Some(rest) = line.trim().strip_prefix(&format!("{}:", key)) {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Extract a list from simplistic YAML — supports inline `[A, B]` and block `- A` styles.
fn extract_list(yaml: &str, key: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = yaml.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.trim().strip_prefix(&format!("{}:", key)) {
            let rest = rest.trim();
            // Inline: [A, B, C]
            if rest.starts_with('[') {
                let inner = rest.trim_matches(|c| c == '[' || c == ']');
                let items = inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Some(items);
            }
            // Comma-separated without brackets
            if rest.contains(',') {
                let items: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !items.is_empty() {
                    return Some(items);
                }
            }
            // Block list
            if rest.is_empty() && i + 1 < lines.len() {
                let items: Vec<String> = lines[i + 1..]
                    .iter()
                    .take_while(|l| l.trim().starts_with("- "))
                    .filter_map(|l| {
                        l.trim()
                            .strip_prefix("- ")
                            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    })
                    .collect();
                if !items.is_empty() {
                    return Some(items);
                }
            }
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_split_frontmatter_basic() {
        let content = "---\nname: test\ndescription: \"A test\"\n---\nHello world";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("name: test"));
        assert_eq!(body, "Hello world");
    }

    #[test]
    fn test_split_frontmatter_no_frontmatter() {
        let content = "Just some text";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, "Just some text");
    }

    #[test]
    fn test_split_frontmatter_crlf() {
        let content = "---\r\nname: test\r\n---\r\nBody text";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("name: test"));
        assert_eq!(body, "Body text");
    }

    #[test]
    fn test_extract_string_basic() {
        let yaml = "name: my-agent\ndescription: \"Test agent\"";
        assert_eq!(extract_string(yaml, "name"), Some("my-agent".into()));
        assert_eq!(extract_string(yaml, "description"), Some("Test agent".into()));
        assert_eq!(extract_string(yaml, "missing"), None);
    }

    #[test]
    fn test_extract_list_inline() {
        let yaml = "tools: [FileRead, Grep, Bash]";
        let list = extract_list(yaml, "tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn test_extract_list_comma_separated() {
        let yaml = "tools: FileRead, Grep, Bash";
        let list = extract_list(yaml, "tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn test_extract_list_block() {
        let yaml = "tools:\n- FileRead\n- Grep\n- Bash";
        let list = extract_list(yaml, "tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn test_parse_agent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-agent.md");
        fs::write(&path, "\
---
name: test-agent
description: \"A test agent for unit testing\"
tools: [FileRead, Grep]
model: inherit
effort: high
color: blue
maxTurns: 20
background: true
---
You are a test agent. Your job is to test things thoroughly.
").unwrap();

        let agent = parse_agent_file(&path, "test-agent", AgentSource::Project).unwrap();
        assert_eq!(agent.agent_type, "test-agent");
        assert_eq!(agent.description, "A test agent for unit testing");
        assert_eq!(agent.allowed_tools, vec!["FileRead", "Grep"]);
        assert!(agent.inherits_model());
        assert_eq!(agent.effort.as_deref(), Some("high"));
        assert_eq!(agent.color.as_deref(), Some("blue"));
        assert_eq!(agent.max_turns, Some(20));
        assert!(agent.background);
        assert!(agent.system_prompt.contains("test agent"));
        assert_eq!(agent.source, AgentSource::Project);
    }

    #[test]
    fn test_parse_agent_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minimal.md");
        fs::write(&path, "You are a minimal agent with no frontmatter.").unwrap();

        let agent = parse_agent_file(&path, "minimal", AgentSource::User).unwrap();
        assert_eq!(agent.agent_type, "minimal");
        assert_eq!(agent.description, "minimal");
        assert!(agent.allowed_tools.is_empty());
        assert!(agent.model.is_none());
        assert_eq!(agent.source, AgentSource::User);
    }

    #[test]
    fn test_parse_agent_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.md");
        fs::write(&path, "---\nname: empty\n---\n").unwrap();

        let agent = parse_agent_file(&path, "empty", AgentSource::User);
        assert!(agent.is_none(), "Empty system prompt should be rejected");
    }

    #[test]
    fn test_load_agents_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".claude").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        fs::write(agents_dir.join("reviewer.md"), "\
---
name: reviewer
description: \"Code review agent\"
tools: [FileRead, Grep]
---
You are a code reviewer. Analyze code for bugs and best practices.
").unwrap();

        fs::write(agents_dir.join("tester.md"), "\
---
name: tester
description: \"Test runner agent\"
---
You are a test runner. Execute tests and report results.
").unwrap();

        // Non-md file should be ignored
        fs::write(agents_dir.join("notes.txt"), "Not an agent").unwrap();

        let agents = load_agents(dir.path());
        assert_eq!(agents.len(), 2);
        let names: Vec<&str> = agents.iter().map(|a| a.agent_type.as_str()).collect();
        assert!(names.contains(&"reviewer"));
        assert!(names.contains(&"tester"));
    }

    #[test]
    fn test_to_markdown_roundtrip() {
        let agent = AgentDefinition {
            agent_type: "roundtrip-test".into(),
            description: "Test roundtrip serialization".into(),
            system_prompt: "You are a test agent.\n\nBe thorough.".into(),
            allowed_tools: vec!["FileRead".into(), "Grep".into()],
            disallowed_tools: vec![],
            model: Some("inherit".into()),
            effort: Some("high".into()),
            memory: Some(AgentMemoryScope::Project),
            color: Some("green".into()),
            permission_mode: None,
            max_turns: Some(10),
            background: false,
            skills: vec!["review".into()],
            initial_prompt: None,
            source: AgentSource::Project,
            file_path: None,
            base_dir: None,
        };

        let md = agent.to_markdown();
        assert!(md.contains("name: roundtrip-test"));
        assert!(md.contains("description: \"Test roundtrip serialization\""));
        assert!(md.contains("tools: [FileRead, Grep]"));
        assert!(md.contains("model: inherit"));
        assert!(md.contains("effort: high"));
        assert!(md.contains("memory: project"));
        assert!(md.contains("color: green"));
        assert!(md.contains("maxTurns: 10"));
        assert!(md.contains("skills: [review]"));
        assert!(md.contains("You are a test agent."));

        // Parse it back
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.md");
        std::fs::write(&path, &md).unwrap();
        let parsed = parse_agent_file(&path, "roundtrip", AgentSource::Project).unwrap();
        assert_eq!(parsed.agent_type, "roundtrip-test");
        assert_eq!(parsed.description, "Test roundtrip serialization");
        assert_eq!(parsed.allowed_tools, vec!["FileRead", "Grep"]);
        assert_eq!(parsed.effort.as_deref(), Some("high"));
        assert_eq!(parsed.max_turns, Some(10));
    }

    #[test]
    fn test_validate_agent_valid() {
        let agent = AgentDefinition {
            agent_type: "my-agent".into(),
            description: "A good agent".into(),
            system_prompt: "You are a helpful agent that does things.".into(),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
            effort: None,
            memory: None,
            color: None,
            permission_mode: None,
            max_turns: None,
            background: false,
            skills: vec![],
            initial_prompt: None,
            source: AgentSource::Project,
            file_path: None,
            base_dir: None,
        };
        let v = validate_agent(&agent, &[]);
        assert!(v.is_valid(), "Errors: {:?}", v.errors);
    }

    #[test]
    fn test_validate_agent_bad_name() {
        let agent = AgentDefinition {
            agent_type: "-".into(),
            description: "Bad name".into(),
            system_prompt: "You are an agent that tests validation.".into(),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
            effort: None,
            memory: None,
            color: None,
            permission_mode: None,
            max_turns: None,
            background: false,
            skills: vec![],
            initial_prompt: None,
            source: AgentSource::Project,
            file_path: None,
            base_dir: None,
        };
        let v = validate_agent(&agent, &[]);
        assert!(!v.is_valid());
        assert!(v.errors.iter().any(|e| e.contains("hyphen")));
    }

    #[test]
    fn test_validate_agent_empty_prompt() {
        let agent = AgentDefinition {
            agent_type: "test".into(),
            description: "Test".into(),
            system_prompt: "short".into(),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
            effort: None,
            memory: None,
            color: None,
            permission_mode: None,
            max_turns: None,
            background: false,
            skills: vec![],
            initial_prompt: None,
            source: AgentSource::Project,
            file_path: None,
            base_dir: None,
        };
        let v = validate_agent(&agent, &[]);
        assert!(!v.is_valid());
        assert!(v.errors.iter().any(|e| e.contains("10 characters")));
    }

    #[test]
    fn test_save_and_delete_agent() {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentDefinition {
            agent_type: "save-test".into(),
            description: "Saved agent".into(),
            system_prompt: "You are a saved agent for testing file I/O.".into(),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: None,
            effort: None,
            memory: None,
            color: None,
            permission_mode: None,
            max_turns: None,
            background: false,
            skills: vec![],
            initial_prompt: None,
            source: AgentSource::Project,
            file_path: None,
            base_dir: None,
        };

        let path = save_agent(&agent, dir.path()).unwrap();
        assert!(path.exists());
        assert!(path.to_str().unwrap().contains("save-test.md"));

        // Verify content
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("name: save-test"));

        // Delete
        let agent_with_path = AgentDefinition {
            file_path: Some(path.clone()),
            ..agent
        };
        delete_agent(&agent_with_path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_agent_source_display() {
        assert_eq!(AgentSource::BuiltIn.to_string(), "built-in");
        assert_eq!(AgentSource::User.to_string(), "user (~/.claude/agents/)");
        assert_eq!(AgentSource::Project.to_string(), "project (.claude/agents/)");
    }

    #[test]
    fn test_parse_with_memory_and_disallowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.md");
        fs::write(&path, "\
---
name: full-agent
description: \"Full featured agent\"
tools: [FileRead, FileWrite]
disallowed_tools: [FileWrite]
memory: user
permissionMode: auto
skills: [review, test]
initialPrompt: \"Start by reading the README\"
---
You are a fully configured agent with all options set.
").unwrap();

        let agent = parse_agent_file(&path, "full", AgentSource::Project).unwrap();
        assert_eq!(agent.allowed_tools, vec!["FileRead", "FileWrite"]);
        assert_eq!(agent.disallowed_tools, vec!["FileWrite"]);
        assert_eq!(agent.memory, Some(AgentMemoryScope::User));
        assert_eq!(agent.permission_mode.as_deref(), Some("auto"));
        assert_eq!(agent.skills, vec!["review", "test"]);
        assert_eq!(agent.initial_prompt.as_deref(), Some("Start by reading the README"));
    }

    #[test]
    fn test_inherits_model() {
        let mut agent = AgentDefinition {
            agent_type: "t".into(), description: "t".into(), system_prompt: "t".into(),
            allowed_tools: vec![], disallowed_tools: vec![], model: None, effort: None,
            memory: None, color: None, permission_mode: None, max_turns: None,
            background: false, skills: vec![], initial_prompt: None,
            source: AgentSource::Project, file_path: None, base_dir: None,
        };
        assert!(agent.inherits_model());

        agent.model = Some("inherit".into());
        assert!(agent.inherits_model());

        agent.model = Some("INHERIT".into());
        assert!(agent.inherits_model());

        agent.model = Some("".into());
        assert!(agent.inherits_model());

        agent.model = Some("claude-opus-4-20250514".into());
        assert!(!agent.inherits_model());
    }
}
