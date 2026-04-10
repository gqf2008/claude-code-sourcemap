//! LSP server configuration.
//!
//! Servers can be configured in `~/.claude/settings.json` or `.claude/settings.json`:
//! ```json
//! {
//!   "lsp": {
//!     "servers": {
//!       "rust": {
//!         "command": "rust-analyzer",
//!         "args": [],
//!         "extensions": [".rs"]
//!       },
//!       "typescript": {
//!         "command": "typescript-language-server",
//!         "args": ["--stdio"],
//!         "extensions": [".ts", ".tsx", ".js", ".jsx"]
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Or via environment variables:
//!   `CLAUDE_LSP_RS=rust-analyzer`
//!   `CLAUDE_LSP_TS=typescript-language-server --stdio`
//!   `CLAUDE_LSP_PY=pyright-langserver --stdio`

use std::collections::HashMap;
use std::path::Path;

/// Configuration for a single LSP server.
#[derive(Debug, Clone)]
pub struct LspServerConfig {
    /// The executable to run (e.g., "rust-analyzer").
    pub command: String,
    /// Arguments to pass (e.g., ["--stdio"]).
    pub args: Vec<String>,
    /// File extensions handled by this server (e.g., [".rs"]).
    pub extensions: Vec<String>,
    /// Language ID for LSP textDocument/didOpen (e.g., "rust").
    pub language_id: String,
}

/// Map of server-name → config.
pub type LspServerMap = HashMap<String, LspServerConfig>;

/// Load LSP server configs from settings.json and environment variables.
pub fn load_lsp_configs(cwd: &Path) -> LspServerMap {
    let mut servers = LspServerMap::new();

    // Load from settings files
    load_from_settings(cwd, &mut servers);

    // Override/extend with environment variables
    load_from_env(&mut servers);

    servers
}

fn load_from_settings(cwd: &Path, servers: &mut LspServerMap) {
    let settings_paths = [
        dirs::home_dir().map(|h| h.join(".claude").join("settings.json")),
        Some(cwd.join(".claude").join("settings.json")),
    ];

    for path_opt in &settings_paths {
        let Some(path) = path_opt else { continue };
        let Ok(content) = std::fs::read_to_string(path) else { continue };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else { continue };

        let Some(lsp_section) = val.get("lsp") else { continue };
        let Some(server_map) = lsp_section.get("servers").and_then(|s| s.as_object()) else { continue };

        for (name, cfg) in server_map {
            let Some(command) = cfg.get("command").and_then(|c| c.as_str()) else { continue };
            let args: Vec<String> = cfg
                .get("args")
                .and_then(|a| a.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let extensions: Vec<String> = cfg
                .get("extensions")
                .and_then(|e| e.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let language_id = cfg
                .get("languageId")
                .and_then(|l| l.as_str())
                .unwrap_or(name.as_str())
                .to_string();

            servers.insert(name.clone(), LspServerConfig {
                command: command.to_string(),
                args,
                extensions,
                language_id,
            });
        }
    }
}

fn load_from_env(servers: &mut LspServerMap) {
    // CLAUDE_LSP_RS=rust-analyzer → rust server for .rs files
    let env_map: &[(&str, &[&str], &str)] = &[
        ("CLAUDE_LSP_RS", &[".rs"], "rust"),
        ("CLAUDE_LSP_TS", &[".ts", ".tsx", ".js", ".jsx"], "typescript"),
        ("CLAUDE_LSP_PY", &[".py"], "python"),
        ("CLAUDE_LSP_GO", &[".go"], "go"),
        ("CLAUDE_LSP_CPP", &[".cpp", ".cc", ".c", ".h", ".hpp"], "cpp"),
        ("CLAUDE_LSP_CS", &[".cs"], "csharp"),
        ("CLAUDE_LSP_JAVA", &[".java"], "java"),
    ];

    for (env_var, exts, lang) in env_map {
        let Ok(val) = std::env::var(env_var) else { continue };
        let mut parts = val.split_whitespace();
        let Some(command) = parts.next() else { continue };
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();

        let name = lang.to_string();
        servers.entry(name.clone()).or_insert(LspServerConfig {
            command: command.to_string(),
            args,
            extensions: exts.iter().map(|s| s.to_string()).collect(),
            language_id: lang.to_string(),
        });
    }
}

/// Find the server config for a given file path (by extension).
pub fn find_server_for_file<'a>(servers: &'a LspServerMap, file_path: &Path) -> Option<(&'a str, &'a LspServerConfig)> {
    let ext = file_path.extension().and_then(|e| e.to_str())?;
    let dot_ext = format!(".{}", ext);

    servers.iter()
        .find(|(_, cfg)| cfg.extensions.iter().any(|e| e == &dot_ext))
        .map(|(name, cfg)| (name.as_str(), cfg))
}

/// Infer language_id from file extension (fallback when no server config).
pub fn language_id_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts") | Some("tsx") => "typescript",
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => "javascript",
        Some("py") => "python",
        Some("go") => "go",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") => "cpp",
        Some("cs") => "csharp",
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("php") => "php",
        Some("swift") => "swift",
        Some("kt") => "kotlin",
        Some("sh") | Some("bash") => "shellscript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml") | Some("yml") => "yaml",
        Some("md") => "markdown",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_find_server_for_file() {
        let mut servers = LspServerMap::new();
        servers.insert("rust".to_string(), LspServerConfig {
            command: "rust-analyzer".into(),
            args: vec![],
            extensions: vec![".rs".into()],
            language_id: "rust".into(),
        });
        servers.insert("typescript".to_string(), LspServerConfig {
            command: "tsserver".into(),
            args: vec!["--stdio".into()],
            extensions: vec![".ts".into(), ".tsx".into()],
            language_id: "typescript".into(),
        });

        assert!(find_server_for_file(&servers, &PathBuf::from("main.rs")).is_some());
        assert!(find_server_for_file(&servers, &PathBuf::from("app.ts")).is_some());
        assert!(find_server_for_file(&servers, &PathBuf::from("go.go")).is_none());
    }

    #[test]
    fn test_language_id_for_path() {
        assert_eq!(language_id_for_path(&PathBuf::from("main.rs")), "rust");
        assert_eq!(language_id_for_path(&PathBuf::from("app.ts")), "typescript");
        assert_eq!(language_id_for_path(&PathBuf::from("main.py")), "python");
        assert_eq!(language_id_for_path(&PathBuf::from("unknown.xyz")), "plaintext");
    }
}
