//! Memory system — mirrors claude-code's `~/.claude/projects/<key>/memory/` file-based memory.
//!
//! # Design (aligned with original TypeScript)
//!
//! Memory files are plain `.md` files living under project-isolated directories:
//!   - `~/.claude/projects/<sanitized-git-root>/memory/`  (project-isolated)
//!   - `<project>/.claude/memory/`                        (project-scoped, in-repo)
//!   - `~/.claude/memory/`                                (legacy user-global, backward compat)
//!
//! The project key is derived from the canonical git root path, sanitized via
//! `git_util::sanitize_path_key()` (replaces non-alphanumeric → `-`).
//!
//! Each file **may** start with a YAML frontmatter block (between `---` markers)
//! containing:
//!   - `type:` one of `user | feedback | project | reference`
//!   - `description:` short one-liner shown in the manifest
//!
//! ## Injection strategy
//!
//! `load_memories_for_prompt()` returns a formatted block that is prepended to
//! the system prompt (same approach as CLAUDE.md injection).  For context
//! efficiency we include:
//!   1. A compact manifest (one line per file) so Claude knows what's available.
//!   2. The full content of each file (up to `MAX_MEMORY_BYTES` per file,
//!      `MAX_TOTAL_BYTES` total).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_MEMORY_FILES: usize = 200;
const MAX_MEMORY_BYTES_PER_FILE: usize = 10_000;
const MAX_TOTAL_MEMORY_BYTES: usize = 100_000;

// ── Memory types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

// ── Memory header (frontmatter metadata) ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryHeader {
    pub filename: String,
    pub file_path: PathBuf,
    pub mtime: SystemTime,
    pub description: Option<String>,
    pub memory_type: Option<MemoryType>,
}

// ── Memory entry (header + content) ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub header: MemoryHeader,
    /// Body text after the frontmatter (possibly truncated).
    pub content: String,
    pub truncated: bool,
}

// ── Frontmatter parsing ──────────────────────────────────────────────────────

/// Extract YAML frontmatter from `---\n...\n---` at the start of a file.
/// Returns `(frontmatter_lines, body)`.
fn parse_frontmatter(text: &str) -> (Vec<String>, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return (Vec::new(), text);
    };
    // Accept `---\n` or `---\r\n`
    let rest = rest.trim_start_matches('\n').trim_start_matches('\r');
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let fm = &rest[..end];
    let body_start = end + 4; // skip `\n---`
    let body = if body_start <= rest.len() {
        rest[body_start..].trim_start_matches('\n').trim_start_matches('\r')
    } else {
        ""
    };
    let lines: Vec<String> = fm.lines().map(|l| l.to_string()).collect();
    (lines, body)
}

/// Parse a simple YAML key: value line.
fn parse_yaml_kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(':')?;
    Some((k.trim(), v.trim()))
}

fn parse_header_from_frontmatter(lines: &[String]) -> (Option<MemoryType>, Option<String>) {
    let mut mem_type = None;
    let mut description = None;
    for line in lines {
        if let Some((k, v)) = parse_yaml_kv(line) {
            match k {
                "type" => mem_type = MemoryType::from_str(v),
                "description" => description = Some(v.to_string()),
                _ => {}
            }
        }
    }
    (mem_type, description)
}

// ── Directory scanning ───────────────────────────────────────────────────────

/// Scan a directory for `*.md` files (excluding `MEMORY.md` index files).
/// Returns headers sorted newest-first, capped at `MAX_MEMORY_FILES`.
pub fn scan_memory_dir(dir: &Path) -> Vec<MemoryHeader> {
    let mut headers = Vec::new();

    let walk = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|x| x == "md")
                && e.file_name() != "MEMORY.md"
        });

    for entry in walk {
        let path = entry.path().to_path_buf();
        let filename = path
            .strip_prefix(dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.file_name().unwrap_or_default().to_string_lossy().to_string());

        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        // Read first 30 lines for frontmatter only
        let preview = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!("Skipped unreadable memory file {}: {}", path.display(), e);
                continue;
            }
        };
        let first_30: String = preview.lines().take(30).collect::<Vec<_>>().join("\n");
        let (fm_lines, _) = parse_frontmatter(&first_30);
        let (mem_type, description) = parse_header_from_frontmatter(&fm_lines);

        headers.push(MemoryHeader {
            filename,
            file_path: path,
            mtime,
            description,
            memory_type: mem_type,
        });
    }

    headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    headers.truncate(MAX_MEMORY_FILES);
    headers
}

/// Find all memory directories to scan (in priority order):
///   1. `~/.claude/projects/<sanitized-git-root>/memory/`  (project-isolated)
///   2. `<cwd>/.claude/memory/`                            (in-repo project-scoped)
///   3. `~/.claude/memory/`                                (legacy user-global)
pub fn memory_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Project-isolated: ~/.claude/projects/<key>/memory/
    if let Some(project_dir) = project_isolated_memory_dir(cwd) {
        if project_dir.exists() {
            dirs.push(project_dir);
        }
    }

    // 2. In-repo: <cwd>/.claude/memory/
    let project = cwd.join(".claude").join("memory");
    if project.exists() && !dirs.contains(&project) {
        dirs.push(project);
    }

    // 3. Legacy global: ~/.claude/memory/
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join("memory");
        if p.exists() && !dirs.contains(&p) {
            dirs.push(p);
        }
    }

    dirs
}

/// Compute the project-isolated memory directory path.
///
/// Pattern: `~/.claude/projects/<sanitized-git-root>/memory/`
/// Falls back to `<sanitized-cwd>` if not inside a git repo.
fn project_isolated_memory_dir(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let base = crate::git_util::find_git_root(cwd)
        .unwrap_or_else(|| cwd.to_path_buf());
    let key = crate::git_util::sanitize_path_key(&base.to_string_lossy());
    Some(home.join(".claude").join("projects").join(key).join("memory"))
}

/// Returns the primary memory directory path for behavioral prompt injection.
///
/// Prefers the project-isolated directory (`~/.claude/projects/<key>/memory/`),
/// falling back to in-repo `.claude/memory/`, then legacy global.
/// Creates the directory if it doesn't exist yet (so the model can write immediately).
pub fn primary_memory_dir(cwd: &Path) -> Option<PathBuf> {
    // 1. Project-isolated (preferred)
    if let Some(isolated) = project_isolated_memory_dir(cwd) {
        if !isolated.exists() {
            if let Err(e) = std::fs::create_dir_all(&isolated) {
                warn!("Failed to create memory dir {:?}: {}", isolated, e);
            }
        }
        if isolated.exists() {
            return Some(isolated);
        }
    }

    // 2. In-repo project dir
    let project = cwd.join(".claude").join("memory");
    if !project.exists() {
        if let Err(e) = std::fs::create_dir_all(&project) {
            warn!("Failed to create memory dir {:?}: {}", project, e);
        }
    }
    if project.exists() {
        return Some(project);
    }

    // 3. Legacy global
    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".claude").join("memory");
        if user_dir.exists() {
            return Some(user_dir);
        }
    }
    None
}

// ── Reading memory content ───────────────────────────────────────────────────

/// Read the body of a memory file (after frontmatter), truncated to limit.
fn read_memory_body(path: &Path) -> (String, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to read memory file {:?}: {}", path, e);
            return (String::new(), false);
        }
    };
    let (_, body) = parse_frontmatter(&text);
    if body.len() > MAX_MEMORY_BYTES_PER_FILE {
        // Find a valid UTF-8 char boundary at or before the limit
        let mut end = MAX_MEMORY_BYTES_PER_FILE;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        (body[..end].to_string(), true)
    } else {
        (body.to_string(), false)
    }
}

// ── Human-readable age ───────────────────────────────────────────────────────

fn human_age(mtime: SystemTime) -> String {
    let elapsed = mtime.elapsed().unwrap_or_default();
    let secs = elapsed.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Load all available memories and format them as a block for injection into
/// the system prompt.  Returns `None` if no memory files are found.
pub fn load_memories_for_prompt(cwd: &Path) -> Option<String> {
    let dirs = memory_dirs(cwd);
    if dirs.is_empty() {
        return None;
    }

    let mut all_headers: Vec<MemoryHeader> = Vec::new();
    for dir in &dirs {
        all_headers.extend(scan_memory_dir(dir));
    }
    // Re-sort globally and cap
    all_headers.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all_headers.truncate(MAX_MEMORY_FILES);

    if all_headers.is_empty() {
        return None;
    }

    let mut result = String::from("<memory>\n");
    result.push_str("The following memory files provide relevant context:\n\n");

    // Manifest section
    result.push_str("## Memory Files\n");
    for h in &all_headers {
        let tag = h
            .memory_type
            .as_ref()
            .map(|t| format!("[{}] ", t.as_str()))
            .unwrap_or_default();
        let age = human_age(h.mtime);
        if let Some(ref desc) = h.description {
            result.push_str(&format!("- {}{} ({}): {}\n", tag, h.filename, age, desc));
        } else {
            result.push_str(&format!("- {}{} ({})\n", tag, h.filename, age));
        }
    }

    // Content section
    let mut total_bytes = 0usize;
    result.push_str("\n## Memory Contents\n\n");

    for h in &all_headers {
        if total_bytes >= MAX_TOTAL_MEMORY_BYTES {
            result.push_str("\n> Additional memory files were omitted (context budget exceeded).\n");
            break;
        }

        let age = human_age(h.mtime);
        let header_line = format!("### Memory (saved {}): {}\n\n", age, h.filename);
        let (body, truncated) = read_memory_body(&h.file_path);

        result.push_str(&header_line);
        result.push_str(&body);
        if truncated {
            result.push_str(&format!(
                "\n\n> This memory file was truncated (>{} bytes). Use FileRead to view the full file.\n",
                MAX_MEMORY_BYTES_PER_FILE
            ));
        }
        result.push('\n');

        total_bytes += body.len();
    }

    result.push_str("</memory>\n");
    Some(result)
}

/// List memory headers (for `/memory list` CLI command).
pub fn list_memory_files(cwd: &Path) -> Vec<MemoryHeader> {
    let dirs = memory_dirs(cwd);
    let mut all: Vec<MemoryHeader> = dirs.iter().flat_map(|d| scan_memory_dir(d)).collect();
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all
}

/// Return the primary user memory directory (creates it if missing).
pub fn ensure_user_memory_dir() -> anyhow::Result<PathBuf> {
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?
        .join(".claude")
        .join("memory");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── MemoryType::from_str ─────────────────────────────────────────────

    #[test]
    fn memory_type_from_str_valid() {
        assert_eq!(MemoryType::from_str("user"), Some(MemoryType::User));
        assert_eq!(MemoryType::from_str("feedback"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::from_str("project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::from_str("reference"), Some(MemoryType::Reference));
    }

    #[test]
    fn memory_type_from_str_invalid() {
        assert_eq!(MemoryType::from_str("unknown"), None);
        assert_eq!(MemoryType::from_str(""), None);
        assert_eq!(MemoryType::from_str("  "), None);
    }

    #[test]
    fn memory_type_from_str_case_insensitive() {
        assert_eq!(MemoryType::from_str("User"), Some(MemoryType::User));
        assert_eq!(MemoryType::from_str("FEEDBACK"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::from_str("Project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::from_str("REFERENCE"), Some(MemoryType::Reference));
    }

    // ── MemoryType::as_str roundtrip ─────────────────────────────────────

    #[test]
    fn memory_type_as_str_roundtrip() {
        for variant in [
            MemoryType::User,
            MemoryType::Feedback,
            MemoryType::Project,
            MemoryType::Reference,
        ] {
            let s = variant.as_str();
            let back = MemoryType::from_str(s).expect("roundtrip should succeed");
            assert_eq!(back, variant);
        }
    }

    // ── parse_frontmatter ────────────────────────────────────────────────

    #[test]
    fn parse_frontmatter_with_valid_fm() {
        let text = "---\ntype: user\ndescription: hello\n---\nBody content here";
        let (lines, body) = parse_frontmatter(text);
        assert_eq!(lines, vec!["type: user", "description: hello"]);
        assert_eq!(body, "Body content here");
    }

    #[test]
    fn parse_frontmatter_no_fm() {
        let text = "Just some body text\nwith multiple lines";
        let (lines, body) = parse_frontmatter(text);
        assert!(lines.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn parse_frontmatter_unclosed() {
        let text = "---\ntype: user\nno closing marker";
        let (lines, body) = parse_frontmatter(text);
        assert!(lines.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn parse_frontmatter_empty_body() {
        let text = "---\ntype: project\n---\n";
        let (lines, body) = parse_frontmatter(text);
        assert_eq!(lines, vec!["type: project"]);
        assert!(body.is_empty() || body.trim().is_empty());
    }

    // ── parse_yaml_kv ────────────────────────────────────────────────────

    #[test]
    fn parse_yaml_kv_valid() {
        assert_eq!(parse_yaml_kv("type: user"), Some(("type", "user")));
        assert_eq!(
            parse_yaml_kv("description: some text"),
            Some(("description", "some text"))
        );
        // Extra whitespace around key/value
        assert_eq!(parse_yaml_kv("  key : value  "), Some(("key", "value")));
    }

    #[test]
    fn parse_yaml_kv_no_colon() {
        assert_eq!(parse_yaml_kv("no colon here"), None);
        assert_eq!(parse_yaml_kv(""), None);
    }

    // ── parse_header_from_frontmatter ────────────────────────────────────

    #[test]
    fn parse_header_type_and_description() {
        let lines = vec![
            "type: feedback".to_string(),
            "description: My memory note".to_string(),
        ];
        let (mt, desc) = parse_header_from_frontmatter(&lines);
        assert_eq!(mt, Some(MemoryType::Feedback));
        assert_eq!(desc.as_deref(), Some("My memory note"));
    }

    #[test]
    fn parse_header_unknown_type() {
        let lines = vec!["type: banana".to_string()];
        let (mt, desc) = parse_header_from_frontmatter(&lines);
        assert_eq!(mt, None);
        assert_eq!(desc, None);
    }

    #[test]
    fn parse_header_empty_lines() {
        let (mt, desc) = parse_header_from_frontmatter(&[]);
        assert_eq!(mt, None);
        assert_eq!(desc, None);
    }

    // ── human_age ────────────────────────────────────────────────────────

    #[test]
    fn human_age_just_now() {
        let now = SystemTime::now();
        assert_eq!(human_age(now), "just now");
    }

    #[test]
    fn human_age_minutes() {
        let t = SystemTime::now() - Duration::from_secs(5 * 60);
        assert_eq!(human_age(t), "5 min ago");
    }

    #[test]
    fn human_age_hours() {
        let t = SystemTime::now() - Duration::from_secs(2 * 3600);
        assert_eq!(human_age(t), "2 hr ago");
    }

    #[test]
    fn human_age_days() {
        let t = SystemTime::now() - Duration::from_secs(3 * 86400);
        assert_eq!(human_age(t), "3 days ago");
    }

    // ── scan_memory_dir ──────────────────────────────────────────────────

    #[test]
    fn scan_memory_dir_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let headers = scan_memory_dir(tmp.path());
        assert!(headers.is_empty());
    }

    #[test]
    fn scan_memory_dir_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("note1.md"),
            "---\ntype: user\ndescription: First note\n---\nHello world",
        )
        .unwrap();
        std::fs::write(tmp.path().join("note2.md"), "No frontmatter body").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 2);

        // Find the one with frontmatter
        let with_fm = headers.iter().find(|h| h.filename == "note1.md").unwrap();
        assert_eq!(with_fm.memory_type, Some(MemoryType::User));
        assert_eq!(with_fm.description.as_deref(), Some("First note"));

        // The one without frontmatter
        let without_fm = headers.iter().find(|h| h.filename == "note2.md").unwrap();
        assert_eq!(without_fm.memory_type, None);
        assert_eq!(without_fm.description, None);
    }

    #[test]
    fn scan_memory_dir_skips_memory_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "Index file").unwrap();
        std::fs::write(tmp.path().join("real.md"), "Content").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].filename, "real.md");
    }

    #[test]
    fn scan_memory_dir_skips_non_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("note.md"), "Markdown").unwrap();
        std::fs::write(tmp.path().join("data.txt"), "Text").unwrap();
        std::fs::write(tmp.path().join("config.json"), "{}").unwrap();

        let headers = scan_memory_dir(tmp.path());
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].filename, "note.md");
    }

    // ── load_memories_for_prompt (via a fake project dir) ────────────────

    #[test]
    fn load_memories_for_prompt_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // No .claude/memory/ directory ⇒ None
        let result = load_memories_for_prompt(tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn load_memories_for_prompt_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(
            mem_dir.join("greeting.md"),
            "---\ntype: project\ndescription: A greeting\n---\nHello from memory!",
        )
        .unwrap();

        let result = load_memories_for_prompt(tmp.path());
        let text = result.expect("should return Some for non-empty memory dir");

        assert!(text.starts_with("<memory>\n"));
        assert!(text.ends_with("</memory>\n"));
        assert!(text.contains("greeting.md"));
        assert!(text.contains("[project]"));
        assert!(text.contains("A greeting"));
        assert!(text.contains("Hello from memory!"));
    }

    // ── read_memory_body (indirectly via load_memories_for_prompt) ───────

    #[test]
    fn load_memories_truncates_large_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();

        // Create a file whose body exceeds MAX_MEMORY_BYTES_PER_FILE (10_000)
        let big_body = "x".repeat(15_000);
        let content = format!("---\ntype: user\n---\n{}", big_body);
        std::fs::write(mem_dir.join("big.md"), content).unwrap();

        let result = load_memories_for_prompt(tmp.path()).unwrap();
        assert!(result.contains("truncated"));
        assert!(result.contains(">10000 bytes"));
    }

    // ── primary_memory_dir ─────────────────────────────────────────────

    #[test]
    fn primary_memory_dir_creates_some_dir() {
        // primary_memory_dir tries project-isolated first (needs home + git root),
        // then falls back to in-repo .claude/memory/
        let tmp = tempfile::tempdir().unwrap();
        let result = primary_memory_dir(tmp.path());
        // Should always return Some (it creates the directory)
        assert!(result.is_some());
        let dir = result.unwrap();
        assert!(dir.exists());
        // The path should end with "memory"
        assert_eq!(dir.file_name().unwrap().to_str().unwrap(), "memory");
    }

    #[test]
    fn primary_memory_dir_fallback_to_in_repo() {
        // When project-isolated dir can't be created (e.g., no home),
        // falls back to in-repo <cwd>/.claude/memory/
        let tmp = tempfile::tempdir().unwrap();
        let in_repo = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&in_repo).unwrap();

        let result = primary_memory_dir(tmp.path());
        assert!(result.is_some());
        // Should return some valid memory dir
        assert!(result.unwrap().exists());
    }

    // ── project_isolated_memory_dir ─────────────────────────────────────

    #[test]
    fn project_isolated_dir_contains_sanitized_key() {
        let tmp = tempfile::tempdir().unwrap();
        let result = super::project_isolated_memory_dir(tmp.path());
        if let Some(dir) = result {
            // Should contain "projects" in the path
            let path_str = dir.to_string_lossy();
            assert!(path_str.contains("projects"));
            assert!(path_str.contains("memory"));
        }
        // If home_dir() is None (rare), result is None — that's ok
    }

    // ── memory_dirs with project-isolated ─────────────────────────────────

    #[test]
    fn memory_dirs_includes_in_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let in_repo = tmp.path().join(".claude").join("memory");
        std::fs::create_dir_all(&in_repo).unwrap();

        let dirs = memory_dirs(tmp.path());
        assert!(dirs.contains(&in_repo));
    }
}
