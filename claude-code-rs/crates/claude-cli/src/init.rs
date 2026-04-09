/// Initialize a new project with CLAUDE.md and optional settings.
pub(crate) fn run_init(cwd: &std::path::Path) -> anyhow::Result<()> {
    let claude_md_path = cwd.join("CLAUDE.md");
    if claude_md_path.exists() {
        println!("CLAUDE.md already exists at {}", claude_md_path.display());
        println!("  Use `/init` in the REPL for AI-powered improvements.");
    } else {
        let content = generate_claude_md_template(cwd);
        std::fs::write(&claude_md_path, &content)?;
        println!("✓ Created {}", claude_md_path.display());
    }

    // Create .claude/ directory for skills, memory, and rules
    let claude_dir = cwd.join(".claude");
    let skills_dir = claude_dir.join("skills");
    let rules_dir = claude_dir.join("rules");
    if !skills_dir.exists() {
        std::fs::create_dir_all(&skills_dir)?;
        println!("✓ Created {}", skills_dir.display());
    }
    if !rules_dir.exists() {
        std::fs::create_dir_all(&rules_dir)?;
        println!("✓ Created {}", rules_dir.display());
    }

    // Create settings directory if it doesn't exist
    if let Some(config_dir) = dirs::config_dir() {
        let settings_dir = config_dir.join("claude");
        if !settings_dir.exists() {
            std::fs::create_dir_all(&settings_dir)?;
            println!("✓ Created config dir: {}", settings_dir.display());
        }
        let settings_path = settings_dir.join("settings.json");
        if !settings_path.exists() {
            std::fs::write(&settings_path, "{}\n")?;
            println!("✓ Created {}", settings_path.display());
        }
    }

    println!("\n🎉 Project initialized! Edit CLAUDE.md to customize Claude's behavior.");
    println!("   Run `claude` to start a conversation.");
    println!("   Run `claude` then `/init` for AI-powered CLAUDE.md generation.");
    Ok(())
}

/// Auto-detect project type and generate a tailored CLAUDE.md template.
pub(crate) fn generate_claude_md_template(cwd: &std::path::Path) -> String {
    let mut sections = Vec::new();

    // Detect project type
    let has_cargo = cwd.join("Cargo.toml").exists();
    let has_package_json = cwd.join("package.json").exists();
    let has_pyproject = cwd.join("pyproject.toml").exists();
    let has_go_mod = cwd.join("go.mod").exists();
    let has_pom = cwd.join("pom.xml").exists();
    let has_makefile = cwd.join("Makefile").exists();

    // Header
    sections.push("# CLAUDE.md\n\nThis file provides guidance to Claude Code when working with this repository.".to_string());

    // Build & Test section based on detected project type
    let mut build_cmds = Vec::new();
    if has_cargo {
        build_cmds.push("cargo build           # Build the project");
        build_cmds.push("cargo test            # Run all tests");
        build_cmds.push("cargo test -p <crate> # Test a specific crate");
        build_cmds.push("cargo clippy          # Lint");
        build_cmds.push("cargo fmt --check     # Check formatting");
    }
    if has_package_json {
        build_cmds.push("npm install           # Install dependencies");
        build_cmds.push("npm run build         # Build");
        build_cmds.push("npm test              # Run tests");
        build_cmds.push("npm run lint          # Lint");
    }
    if has_pyproject {
        build_cmds.push("pip install -e .      # Install in dev mode");
        build_cmds.push("pytest                # Run tests");
        build_cmds.push("ruff check .          # Lint");
    }
    if has_go_mod {
        build_cmds.push("go build ./...        # Build");
        build_cmds.push("go test ./...         # Test");
        build_cmds.push("go vet ./...          # Lint");
    }
    if has_pom {
        build_cmds.push("mvn compile           # Build");
        build_cmds.push("mvn test              # Test");
    }
    if has_makefile {
        build_cmds.push("make                  # Build (see Makefile for targets)");
    }

    if build_cmds.is_empty() {
        sections.push("## Build & Test\n\n```bash\n# Add your build, test, and lint commands here\n```".to_string());
    } else {
        let cmds = build_cmds.join("\n");
        sections.push(format!("## Build & Test\n\n```bash\n{}\n```", cmds));
    }

    // Code style section
    sections.push("## Code Style\n\n<!-- Add coding conventions that differ from language defaults -->".to_string());

    // Architecture section
    sections.push("## Architecture\n\n<!-- Brief description of key directories and patterns -->".to_string());

    // Important notes
    sections.push("## Important Notes\n\n<!-- Add gotchas, required env vars, or non-obvious setup steps -->".to_string());

    sections.join("\n\n")
}

/// Discover MCP server configs and generate system prompt instructions.
///
/// Scans `.mcp.json` at project and user levels, extracts server names and
/// commands, and returns `(server_name, instruction)` pairs for the system prompt.
pub(crate) fn discover_mcp_instructions(cwd: &std::path::Path) -> Vec<(String, String)> {
    let config_paths = claude_tools::mcp::discover_mcp_configs(cwd);
    let mut instructions = Vec::new();

    for path in config_paths {
        match claude_tools::mcp::load_mcp_configs(&path) {
            Ok(configs) => {
                for cfg in configs {
                    let instruction = format!(
                        "MCP server '{}': command=`{} {}`",
                        cfg.name,
                        cfg.command,
                        cfg.args.join(" "),
                    );
                    instructions.push((cfg.name, instruction));
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load MCP config {}: {}", path.display(), e);
            }
        }
    }

    instructions
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── generate_claude_md_template ──────────────────────────────────

    #[test]
    fn test_template_empty_dir() {
        let tmp = std::env::temp_dir().join("claude_test_empty_dir");
        let _ = std::fs::create_dir_all(&tmp);
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("# CLAUDE.md"));
        assert!(md.contains("## Build & Test"));
        assert!(md.contains("## Code Style"));
        assert!(md.contains("## Architecture"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_template_rust_project() {
        let tmp = std::env::temp_dir().join("claude_test_rust_proj");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("cargo build"));
        assert!(md.contains("cargo test"));
        assert!(md.contains("cargo clippy"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_template_node_project() {
        let tmp = std::env::temp_dir().join("claude_test_node_proj");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("package.json"), "{}").unwrap();
        let md = generate_claude_md_template(&tmp);
        assert!(md.contains("npm install"));
        assert!(md.contains("npm test"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_mcp_instructions_empty() {
        let tmp = std::env::temp_dir().join("claude_test_no_mcp");
        let _ = std::fs::create_dir_all(&tmp);
        let result = discover_mcp_instructions(&tmp);
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_discover_mcp_instructions_with_config() {
        let tmp = std::env::temp_dir().join("claude_test_mcp_disc");
        let _ = std::fs::create_dir_all(&tmp);
        let mcp_json = r#"{
            "mcpServers": {
                "my-server": {
                    "command": "npx",
                    "args": ["-y", "my-mcp-server"]
                }
            }
        }"#;
        std::fs::write(tmp.join(".mcp.json"), mcp_json).unwrap();
        let result = discover_mcp_instructions(&tmp);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "my-server");
        assert!(result[0].1.contains("npx"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
