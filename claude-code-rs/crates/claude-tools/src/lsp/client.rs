//! LSP client — spawns a language server and communicates via JSON-RPC over stdio.
//!
//! Uses sequential request-response with Content-Length framing.
//! Designed for single-turn tool use (not a persistent connection between tool calls).

use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::time::timeout;
use tracing::debug;

use super::config::LspServerConfig;
use super::transport;

/// Timeout for LSP requests (15 seconds).
const LSP_TIMEOUT: Duration = Duration::from_secs(15);

/// A connected LSP server client.
pub struct LspClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: AtomicI64,
    /// Files opened in this session.
    opened_files: std::collections::HashSet<String>,
}

impl LspClient {
    /// Start an LSP server and initialize it.
    pub fn start(config: &LspServerConfig, root_path: &Path) -> Result<Self> {
        let mut cmd = std::process::Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()) // suppress server logs
            .current_dir(root_path);

        debug!("Starting LSP server: {} {:?}", config.command, config.args);

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn LSP server '{}'. Make sure it is installed and in PATH.", config.command)
        })?;

        let stdin = child.stdin.take().context("LSP server stdin unavailable")?;
        let stdout = child.stdout.take().context("LSP server stdout unavailable")?;
        let reader = BufReader::new(stdout);
        let root_uri = path_to_uri(root_path);

        let mut client = Self {
            child,
            stdin,
            reader,
            next_id: AtomicI64::new(1),
            opened_files: std::collections::HashSet::new(),
        };

        // Send initialize
        client.initialize(&root_uri)?;

        Ok(client)
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        transport::write_message(&mut self.stdin, &msg)?;
        transport::read_response(&mut self.reader, id)
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        transport::write_message(&mut self.stdin, &msg)?;
        Ok(())
    }

    fn initialize(&mut self, root_uri: &str) -> Result<()> {
        let id = self.next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "linkSupport": false },
                        "references": {},
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": false },
                    },
                    "workspace": {
                        "symbol": {}
                    }
                },
                "workspaceFolders": [{"uri": root_uri, "name": "workspace"}]
            }
        });
        transport::write_message(&mut self.stdin, &msg)?;
        transport::read_response(&mut self.reader, id)?;

        // Send initialized notification
        self.send_notification("initialized", json!({}))?;
        debug!("LSP server initialized successfully");
        Ok(())
    }

    /// Open a file in the language server (sends textDocument/didOpen).
    pub fn open_file(&mut self, file_path: &Path, language_id: &str) -> Result<()> {
        let uri = path_to_uri(file_path);
        if self.opened_files.contains(&uri) {
            return Ok(()); // Already open
        }

        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Cannot read file: {}", file_path.display()))?;

        self.send_notification("textDocument/didOpen", json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": content
            }
        }))?;

        self.opened_files.insert(uri);
        Ok(())
    }

    /// Go-to-definition: returns a list of (file_path, line, character) locations.
    pub fn go_to_definition(&mut self, file_path: &Path, line: u32, character: u32) -> Result<Vec<LspLocation>> {
        let uri = path_to_uri(file_path);
        let result = self.send_request("textDocument/definition", json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }))?;
        parse_locations(result)
    }

    /// Find references: returns list of locations where the symbol is used.
    pub fn find_references(&mut self, file_path: &Path, line: u32, character: u32) -> Result<Vec<LspLocation>> {
        let uri = path_to_uri(file_path);
        let result = self.send_request("textDocument/references", json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        }))?;
        parse_locations(result)
    }

    /// Hover: returns hover text for the symbol at the position.
    pub fn hover(&mut self, file_path: &Path, line: u32, character: u32) -> Result<Option<String>> {
        let uri = path_to_uri(file_path);
        let result = self.send_request("textDocument/hover", json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }))?;

        if result.is_null() {
            return Ok(None);
        }

        // Extract text from hover result
        let text = if let Some(contents) = result.get("contents") {
            match contents {
                Value::String(s) => s.clone(),
                Value::Object(obj) => {
                    obj.get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                }
                Value::Array(arr) => {
                    arr.iter()
                        .filter_map(|item| {
                            if item.is_string() {
                                item.as_str().map(|s| s.to_string())
                            } else {
                                item.get("value").and_then(|v| v.as_str()).map(|s| s.to_string())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                _ => return Ok(None),
            }
        } else {
            return Ok(None);
        };

        if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
    }

    /// Document symbols: returns list of symbols in the file.
    pub fn document_symbols(&mut self, file_path: &Path) -> Result<Vec<LspSymbol>> {
        let uri = path_to_uri(file_path);
        let result = self.send_request("textDocument/documentSymbol", json!({
            "textDocument": { "uri": uri }
        }))?;
        parse_symbols(result)
    }

    /// Workspace symbols: search for symbols matching the query.
    pub fn workspace_symbols(&mut self, query: &str) -> Result<Vec<LspSymbol>> {
        let result = self.send_request("workspace/symbol", json!({
            "query": query
        }))?;
        parse_symbols(result)
    }

    /// Shutdown the server gracefully.
    pub fn shutdown(mut self) {
        let _ = self.send_request("shutdown", json!(null));
        let _ = self.send_notification("exit", json!(null));
        let _ = self.child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A location in a file.
#[derive(Debug, Clone)]
pub struct LspLocation {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A symbol (function, class, variable, etc.).
#[derive(Debug, Clone)]
pub struct LspSymbol {
    pub name: String,
    pub kind: String,
    pub file_path: Option<String>,
    pub line: u32,
    pub character: u32,
}

fn parse_locations(val: Value) -> Result<Vec<LspLocation>> {
    let items = match val {
        Value::Array(arr) => arr,
        Value::Object(_) => vec![val],
        Value::Null => return Ok(vec![]),
        _ => bail!("Unexpected location response: {:?}", val),
    };

    let mut locs = Vec::new();
    for item in items {
        // Handle LocationLink (targetUri + targetRange) or Location (uri + range)
        let uri = item.get("targetUri")
            .or_else(|| item.get("uri"))
            .and_then(|u| u.as_str())
            .unwrap_or_default();

        let range = item.get("targetRange")
            .or_else(|| item.get("range"))
            .unwrap_or(&Value::Null);

        let start = range.get("start");
        let end = range.get("end");

        locs.push(LspLocation {
            file_path: uri_to_path(uri),
            line: start.and_then(|s| s.get("line")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            character: start.and_then(|s| s.get("character")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            end_line: end.and_then(|e| e.get("line")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            end_character: end.and_then(|e| e.get("character")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        });
    }
    Ok(locs)
}

const SYMBOL_KIND_NAMES: &[&str] = &[
    "File", "Module", "Namespace", "Package", "Class", "Method", "Property", "Field",
    "Constructor", "Enum", "Interface", "Function", "Variable", "Constant", "String",
    "Number", "Boolean", "Array", "Object", "Key", "Null", "EnumMember", "Struct",
    "Event", "Operator", "TypeParameter",
];

fn symbol_kind_name(kind: u64) -> &'static str {
    if kind == 0 {
        return "Unknown";
    }
    SYMBOL_KIND_NAMES.get(kind.saturating_sub(1) as usize).copied().unwrap_or("Unknown")
}

fn parse_symbols(val: Value) -> Result<Vec<LspSymbol>> {
    let items = match val {
        Value::Array(arr) => arr,
        Value::Null => return Ok(vec![]),
        _ => bail!("Unexpected symbols response"),
    };

    let mut symbols = Vec::new();
    for item in items {
        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
        let kind_num = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
        let kind = symbol_kind_name(kind_num).to_string();

        // Document symbols have 'selectionRange', workspace symbols have 'location'
        let (file_path, line, character) = if let Some(loc) = item.get("location") {
            let uri = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
            let l = loc.get("range").and_then(|r| r.get("start"))
                .and_then(|s| s.get("line")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let c = loc.get("range").and_then(|r| r.get("start"))
                .and_then(|s| s.get("character")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            (Some(uri_to_path(&uri)), l, c)
        } else if let Some(sel) = item.get("selectionRange") {
            let l = sel.get("start").and_then(|s| s.get("line")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let c = sel.get("start").and_then(|s| s.get("character")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            (None, l, c)
        } else {
            (None, 0, 0)
        };

        symbols.push(LspSymbol { name, kind, file_path, line, character });
    }
    Ok(symbols)
}

/// Convert a filesystem path to a file:// URI.
pub fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/"))
            .join(path)
    };

    // Canonicalize if possible
    let abs = abs.canonicalize().unwrap_or(abs);

    #[cfg(windows)]
    {
        // Windows: file:///C:/path/to/file (forward slashes)
        let s = abs.to_string_lossy().replace('\\', "/");
        format!("file:///{}", s.trim_start_matches('/'))
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", abs.display())
    }
}

/// Convert a file:// URI back to a filesystem path string.
fn uri_to_path(uri: &str) -> String {
    if let Some(path) = uri.strip_prefix("file://") {
        #[cfg(windows)]
        {
            // Remove extra leading slash for Windows: file:///C:/... → C:/...
            let path = path.trim_start_matches('/');
            path.replace('/', "\\")
        }
        #[cfg(not(windows))]
        {
            path.to_string()
        }
    } else {
        uri.to_string()
    }
}

/// Spawn an LSP client in a blocking thread with a timeout.
pub async fn start_lsp_client_async(config: LspServerConfig, root_path: std::path::PathBuf) -> Result<LspClient> {
    timeout(LSP_TIMEOUT, tokio::task::spawn_blocking(move || {
        LspClient::start(&config, &root_path)
    }))
    .await
    .context("LSP server initialization timed out")?
    .context("LSP server thread failed")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_to_uri_absolute() {
        #[cfg(not(windows))]
        {
            let uri = path_to_uri(Path::new("/home/user/project/main.rs"));
            assert!(uri.starts_with("file://"));
            assert!(uri.contains("main.rs"));
        }
    }

    #[test]
    fn test_uri_to_path_roundtrip() {
        #[cfg(not(windows))]
        {
            let path = Path::new("/home/user/project/main.rs");
            let uri = path_to_uri(path);
            let back = uri_to_path(&uri);
            assert!(back.contains("main.rs"));
        }
    }

    #[test]
    fn test_parse_locations_null() {
        let locs = parse_locations(Value::Null).unwrap();
        assert!(locs.is_empty());
    }

    #[test]
    fn test_parse_locations_array() {
        let val = serde_json::json!([{
            "uri": "file:///home/user/main.rs",
            "range": {
                "start": {"line": 10, "character": 4},
                "end": {"line": 10, "character": 10}
            }
        }]);
        let locs = parse_locations(val).unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 10);
        assert_eq!(locs[0].character, 4);
    }

    #[test]
    fn test_parse_symbols_empty() {
        let syms = parse_symbols(Value::Null).unwrap();
        assert!(syms.is_empty());
    }

    #[test]
    fn test_symbol_kind_names() {
        assert_eq!(symbol_kind_name(1), "File");
        assert_eq!(symbol_kind_name(5), "Class");
        assert_eq!(symbol_kind_name(12), "Function");
        assert_eq!(symbol_kind_name(0), "Unknown");
        assert_eq!(symbol_kind_name(999), "Unknown");
    }
}
