//! Skills — reusable prompt templates loaded from `.claude/skills/*.md`.
//!
//! A skill file is a Markdown document with an optional YAML frontmatter block:
//!
//! ```markdown
//! ---
//! description: "Security code reviewer"
//! allowed_tools: [FileRead, Grep, Bash]
//! model: "claude-opus-4-20250514"
//! ---
//! You are an expert security reviewer.  Analyse the provided code for
//! vulnerabilities and suggest fixes.
//! ```
//!
//! Skills are loaded from (in order, duplicates ignored):
//!   1. `$CWD/.claude/skills/`
//!   2. `~/.claude/skills/`

use std::path::{Path, PathBuf};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Identifier derived from filename (lowercase, spaces → `-`).
    pub name: String,
    /// Human-readable description (from frontmatter or filename).
    pub description: String,
    /// System-prompt body (everything after the frontmatter).
    pub system_prompt: String,
    /// Tool whitelist — empty means "all tools allowed".
    pub allowed_tools: Vec<String>,
    /// Optional model override.
    pub model: Option<String>,
}

fn skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    dirs.push(cwd.join(".claude").join("skills"));
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".claude").join("skills"));
    }
    dirs
}

/// Load all skills from standard locations; project skills shadow user skills.
pub fn load_skills(cwd: &Path) -> Vec<SkillEntry> {
    load_skills_from_dirs(&skill_dirs(cwd))
}

/// Load skills from an explicit list of directories (for testing).
fn load_skills_from_dirs(dirs: &[PathBuf]) -> Vec<SkillEntry> {
    let mut skills: Vec<SkillEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                debug!("Cannot read skills dir {}: {}", dir.display(), e);
                continue;
            }
        };

        for entry in rd.flatten() {
            let path = entry.path();
            let ft = entry.file_type();

            // Format 1: skill-name/SKILL.md (directory or symlink containing SKILL.md)
            if ft.map(|t| t.is_dir() || t.is_symlink()).unwrap_or(false) {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase()
                        .replace(' ', "-");
                    if !name.is_empty() && !seen.contains(&name) {
                        if let Some(skill) = parse_skill_file(&skill_md, &name) {
                            debug!("Loaded skill '{}' from {}", name, skill_md.display());
                            seen.insert(name);
                            skills.push(skill);
                        }
                    }
                }
                continue;
            }

            // Format 2: skill-name.md (legacy flat file)
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

            if let Some(skill) = parse_skill_file(&path, &name) {
                debug!("Loaded skill '{}' from {}", name, path.display());
                seen.insert(name);
                skills.push(skill);
            }
        }
    }

    skills
}

fn parse_skill_file(path: &Path, name: &str) -> Option<SkillEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let (fm, body) = split_frontmatter(&content);

    let description = fm
        .as_deref()
        .and_then(|f| extract_string(f, "description"))
        .unwrap_or_else(|| name.replace('-', " "));

    let allowed_tools = fm
        .as_deref()
        .and_then(|f| extract_list(f, "allowed_tools"))
        .unwrap_or_default();

    let model = fm.as_deref().and_then(|f| extract_string(f, "model"));

    Some(SkillEntry {
        name: name.to_string(),
        description,
        system_prompt: body.trim().to_string(),
        allowed_tools,
        model,
    })
}

/// Split `---\n<yaml>\n---\n<body>` → `(Some(yaml), body)`.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let s = content.trim_start();
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
            // Block list
            if rest.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_fm_valid() {
        let content = "---\ndescription: test\n---\nBody text here";
        let (fm, body) = split_frontmatter(content);
        assert_eq!(fm.unwrap(), "description: test");
        assert_eq!(body, "Body text here");
    }

    #[test]
    fn split_fm_no_frontmatter() {
        let (fm, body) = split_frontmatter("Just plain body");
        assert!(fm.is_none());
        assert_eq!(body, "Just plain body");
    }

    #[test]
    fn split_fm_unclosed() {
        let (fm, _body) = split_frontmatter("---\nkey: val\nno end marker");
        assert!(fm.is_none());
    }

    #[test]
    fn extract_string_plain() {
        assert_eq!(extract_string("description: hello world", "description"), Some("hello world".into()));
    }

    #[test]
    fn extract_string_quoted() {
        assert_eq!(extract_string("description: \"quoted value\"", "description"), Some("quoted value".into()));
    }

    #[test]
    fn extract_string_missing() {
        assert_eq!(extract_string("other: value", "description"), None);
    }

    #[test]
    fn extract_string_empty_value() {
        assert_eq!(extract_string("description:", "description"), None);
    }

    #[test]
    fn extract_list_inline() {
        let yaml = "allowed_tools: [FileRead, Grep, Bash]";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn extract_list_block_style() {
        let yaml = "allowed_tools:\n- FileRead\n- Grep\n- Bash";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep", "Bash"]);
    }

    #[test]
    fn extract_list_missing_key() {
        assert!(extract_list("other: value", "allowed_tools").is_none());
    }

    #[test]
    fn extract_list_inline_quoted() {
        let yaml = "allowed_tools: [\"FileRead\", 'Grep']";
        let list = extract_list(yaml, "allowed_tools").unwrap();
        assert_eq!(list, vec!["FileRead", "Grep"]);
    }

    #[test]
    fn parse_skill_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.md");
        std::fs::write(&path, "---\ndescription: Security reviewer\nallowed_tools: [FileRead, Grep]\nmodel: claude-opus-4-20250514\n---\nYou are an expert.").unwrap();

        let skill = parse_skill_file(&path, "reviewer").unwrap();
        assert_eq!(skill.name, "reviewer");
        assert_eq!(skill.description, "Security reviewer");
        assert_eq!(skill.allowed_tools, vec!["FileRead", "Grep"]);
        assert_eq!(skill.model.as_deref(), Some("claude-opus-4-20250514"));
        assert_eq!(skill.system_prompt, "You are an expert.");
    }

    #[test]
    fn parse_skill_no_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.md");
        std::fs::write(&path, "Just a prompt body.").unwrap();

        let skill = parse_skill_file(&path, "helper").unwrap();
        assert_eq!(skill.description, "helper");
        assert!(skill.allowed_tools.is_empty());
        assert!(skill.model.is_none());
        assert_eq!(skill.system_prompt, "Just a prompt body.");
    }

    #[test]
    fn load_skills_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        let skills = load_skills_from_dirs(&[skills_dir]);
        assert!(skills.is_empty());
    }

    #[test]
    fn load_skills_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("test.md"), "---\ndescription: Test skill\n---\nDo testing.").unwrap();
        std::fs::write(skills_dir.join("review.md"), "Review code.").unwrap();
        std::fs::write(skills_dir.join("readme.txt"), "Not a skill").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"test"));
        assert!(names.contains(&"review"));
    }

    #[test]
    fn load_skills_dedup_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("test.md"), "First").unwrap();
        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn load_skills_directory_format() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");

        // Create directory-format skill: my-skill/SKILL.md
        let skill_dir = skills_dir.join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: My directory skill\n---\nDo something.",
        )
        .unwrap();

        // Also create a references file (should be ignored)
        let refs_dir = skill_dir.join("references");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(refs_dir.join("guide.md"), "Reference content").unwrap();

        // Create a flat-file skill too
        std::fs::write(skills_dir.join("flat.md"), "Flat skill body.").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"my-skill"));
        assert!(names.contains(&"flat"));

        let dir_skill = skills.iter().find(|s| s.name == "my-skill").unwrap();
        assert_eq!(dir_skill.description, "My directory skill");
        assert_eq!(dir_skill.system_prompt, "Do something.");
    }

    #[test]
    fn load_skills_directory_without_skill_md_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".claude").join("skills");
        let empty_skill = skills_dir.join("no-skill-md");
        std::fs::create_dir_all(&empty_skill).unwrap();
        std::fs::write(empty_skill.join("readme.md"), "Not a SKILL.md").unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]);
        assert!(skills.is_empty(), "dir without SKILL.md should be ignored");
    }
}
