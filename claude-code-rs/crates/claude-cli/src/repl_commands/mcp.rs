//! `/mcp` command handler — MCP server discovery and status.

use claude_tools::mcp::server::{discover_mcp_configs, load_mcp_configs};

/// Handle `/mcp [subcommand]`.
pub(crate) fn handle_mcp_command(sub: &str, cwd: &std::path::Path) {
    let sub = sub.trim();
    match sub {
        "" | "list" | "status" => show_mcp_status(cwd),
        "help" => {
            println!("\x1b[1mMCP Server Management\x1b[0m\n");
            println!("  /mcp              Show discovered MCP servers");
            println!("  /mcp list         Same as /mcp");
            println!("  /mcp status       Same as /mcp");
            println!("\nMCP configs are loaded from:");
            println!("  1. <cwd>/.mcp.json         (project-level)");
            println!("  2. ~/.claude/.mcp.json      (user-level)");
        }
        _ => {
            // Sanitize user input to prevent terminal control character injection
            let safe_sub: String = sub.chars()
                .filter(|c| !c.is_control() || *c == ' ')
                .take(50)
                .collect();
            println!("Unknown subcommand: /mcp {}. Try /mcp help", safe_sub);
        }
    }
}

fn show_mcp_status(cwd: &std::path::Path) {
    let config_paths = discover_mcp_configs(cwd);

    if config_paths.is_empty() {
        println!("\x1b[2mNo MCP server configs found.\x1b[0m");
        println!("\x1b[2mCreate .mcp.json in your project or ~/.claude/ to configure MCP servers.\x1b[0m");
        println!("\x1b[2mExample .mcp.json:\x1b[0m");
        println!("\x1b[2m{{\x1b[0m");
        println!("\x1b[2m  \"mcpServers\": {{\x1b[0m");
        println!("\x1b[2m    \"my-server\": {{\x1b[0m");
        println!("\x1b[2m      \"command\": \"npx\",\x1b[0m");
        println!("\x1b[2m      \"args\": [\"-y\", \"@my-org/my-mcp-server\"]\x1b[0m");
        println!("\x1b[2m    }}\x1b[0m");
        println!("\x1b[2m  }}\x1b[0m");
        println!("\x1b[2m}}\x1b[0m");
        return;
    }

    println!("\x1b[1mMCP Server Configs\x1b[0m\n");

    for path in &config_paths {
        let location = if path.starts_with(cwd) {
            "project"
        } else {
            "user"
        };
        println!("\x1b[2m[{}] {}\x1b[0m", location, path.display());

        match load_mcp_configs(path) {
            Ok(configs) => {
                if configs.is_empty() {
                    println!("  (no servers defined)");
                }
                for cfg in &configs {
                    let args_str = if cfg.args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", cfg.args.join(" "))
                    };
                    println!(
                        "  \x1b[32m●\x1b[0m \x1b[1m{}\x1b[0m",
                        cfg.name,
                    );
                    println!(
                        "    command: {}{}",
                        cfg.command, args_str,
                    );
                    if !cfg.env.is_empty() {
                        let env_keys: Vec<&str> = cfg.env.keys().map(|k| k.as_str()).collect();
                        println!("    env: {}", env_keys.join(", "));
                    }
                }
            }
            Err(e) => {
                println!("  \x1b[31m✗ Failed to load: {}\x1b[0m", e);
            }
        }
    }

    println!(
        "\n\x1b[2mMCP tools are injected into the system prompt at startup.\x1b[0m"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_show_mcp_no_configs() {
        // Should not panic on a directory without .mcp.json
        let tmp = std::env::temp_dir().join("claude_test_mcp_cmd");
        std::fs::create_dir_all(&tmp).expect("failed to create temp dir");
        show_mcp_status(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_show_mcp_with_config() {
        let tmp = std::env::temp_dir().join("claude_test_mcp_cmd2");
        std::fs::create_dir_all(&tmp).expect("failed to create temp dir");
        let config = r#"{
            "mcpServers": {
                "test-server": {
                    "command": "node",
                    "args": ["server.js"],
                    "env": { "PORT": "3000" }
                }
            }
        }"#;
        std::fs::write(tmp.join(".mcp.json"), config).unwrap();
        show_mcp_status(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_handle_mcp_unknown_sanitizes_input() {
        // Control characters should be stripped from echo output
        handle_mcp_command("\x1b[31mevil\x1b[0m", &std::env::temp_dir());
    }
}
