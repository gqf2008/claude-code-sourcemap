//! In-process MCP server for Computer Use tools.
//!
//! Implements `tools/list` and `tools/call` directly without spawning
//! a subprocess. Registered as a built-in MCP server by the agent engine.

use claude_mcp::types::{McpContent, McpToolDef, McpToolResult};
use serde_json::{json, Value};

use crate::input::{self, MouseButton, ScrollDirection};
use crate::screenshot;
use crate::session_lock::SessionLock;

/// Server name used when registering with `McpManager`.
pub const SERVER_NAME: &str = "computer-use";

/// In-process Computer Use MCP server.
pub struct ComputerUseMcpServer {
    /// Session lock to prevent concurrent desktop control.
    _lock: SessionLock,
    /// Cached platform info (collected once at creation).
    platform: PlatformInfo,
}

/// Detected platform capabilities.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub display_server: String,
    pub has_display: bool,
    pub hostname: String,
}

#[allow(clippy::unused_self)]
impl ComputerUseMcpServer {
    /// Create a new Computer Use server, acquiring the session lock.
    pub fn new() -> anyhow::Result<Self> {
        let lock = SessionLock::acquire()?;
        let platform = detect_platform();
        Ok(Self { _lock: lock, platform })
    }

    /// Get the detected platform info.
    pub fn platform_info(&self) -> &PlatformInfo {
        &self.platform
    }

    /// List available tools.
    pub fn list_tools(&self) -> Vec<McpToolDef> {
        vec![
            McpToolDef {
                name: "screenshot".into(),
                description: Some("Capture a screenshot of the screen. Returns a base64-encoded PNG image.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "region": {
                            "type": "object",
                            "description": "Optional: capture a specific region",
                            "properties": {
                                "x": { "type": "integer" },
                                "y": { "type": "integer" },
                                "width": { "type": "integer" },
                                "height": { "type": "integer" }
                            },
                            "required": ["x", "y", "width", "height"]
                        },
                        "exclude_terminal": {
                            "type": "boolean",
                            "description": "If true, minimize the terminal window before capturing and restore after. Reduces self-referential screenshots.",
                            "default": false
                        }
                    }
                })),
                annotations: None,
            },
            McpToolDef {
                name: "click".into(),
                description: Some("Click at screen coordinates.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer", "description": "X coordinate" },
                        "y": { "type": "integer", "description": "Y coordinate" },
                        "button": {
                            "type": "string",
                            "enum": ["left", "right", "middle"],
                            "description": "Mouse button (default: left)"
                        }
                    },
                    "required": ["x", "y"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "double_click".into(),
                description: Some("Double-click at screen coordinates.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer", "description": "X coordinate" },
                        "y": { "type": "integer", "description": "Y coordinate" },
                        "button": {
                            "type": "string",
                            "enum": ["left", "right", "middle"],
                            "description": "Mouse button (default: left)"
                        }
                    },
                    "required": ["x", "y"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "type_text".into(),
                description: Some("Type a text string using the keyboard.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Text to type" }
                    },
                    "required": ["text"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "key".into(),
                description: Some(
                    "Press a key or key combination. Examples: 'enter', 'ctrl+c', 'alt+tab', 'shift+a'.".into()
                ),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "combo": {
                            "type": "string",
                            "description": "Key combination (e.g., 'ctrl+c', 'enter', 'f5')"
                        }
                    },
                    "required": ["combo"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "scroll".into(),
                description: Some("Scroll at screen coordinates.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer", "description": "X coordinate" },
                        "y": { "type": "integer", "description": "Y coordinate" },
                        "direction": {
                            "type": "string",
                            "enum": ["up", "down", "left", "right"],
                            "description": "Scroll direction"
                        },
                        "amount": {
                            "type": "integer",
                            "description": "Scroll amount in lines (default: 3)"
                        }
                    },
                    "required": ["x", "y", "direction"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "mouse_move".into(),
                description: Some("Move the mouse cursor to screen coordinates.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer", "description": "X coordinate" },
                        "y": { "type": "integer", "description": "Y coordinate" }
                    },
                    "required": ["x", "y"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "cursor_position".into(),
                description: Some("Get the current mouse cursor position.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {}
                })),
                annotations: None,
            },
            McpToolDef {
                name: "clipboard_read".into(),
                description: Some("Read the current clipboard text content.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {}
                })),
                annotations: None,
            },
            McpToolDef {
                name: "clipboard_write".into(),
                description: Some("Write text to the system clipboard.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Text to copy to clipboard" }
                    },
                    "required": ["text"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "platform_info".into(),
                description: Some("Get OS, architecture, display server, and capability info.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {}
                })),
                annotations: None,
            },
        ]
    }

    /// Call a tool by name with the given input.
    pub fn call_tool(&self, name: &str, input: Value) -> McpToolResult {
        match name {
            "screenshot" => self.handle_screenshot(input),
            "click" => self.handle_click(input),
            "double_click" => self.handle_double_click(input),
            "type_text" => self.handle_type_text(input),
            "key" => self.handle_key(input),
            "scroll" => self.handle_scroll(input),
            "mouse_move" => self.handle_mouse_move(input),
            "cursor_position" => self.handle_cursor_position(),
            "clipboard_read" => self.handle_clipboard_read(),
            "clipboard_write" => self.handle_clipboard_write(input),
            "platform_info" => self.handle_platform_info(),
            _ => err_result(format!("Unknown tool: {name}")),
        }
    }

    fn handle_screenshot(&self, input: Value) -> McpToolResult {
        let exclude_terminal = input.get("exclude_terminal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Minimize terminal before capture if requested
        // Best-effort: minimize terminal before capture.
        // Uses a fixed delay which may be insufficient on slow systems.
        if exclude_terminal {
            minimize_terminal_window();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let result = if let Some(region) = input.get("region") {
            let x = region["x"].as_i64().unwrap_or(0) as i32;
            let y = region["y"].as_i64().unwrap_or(0) as i32;
            let w = region["width"].as_u64().unwrap_or(100) as u32;
            let h = region["height"].as_u64().unwrap_or(100) as u32;
            screenshot::capture_region(x, y, w, h)
        } else {
            screenshot::capture_screen()
        };

        if exclude_terminal {
            restore_terminal_window();
        }

        match result {
            Ok(ss) => McpToolResult {
                content: vec![
                    McpContent {
                        content_type: "image".into(),
                        text: None,
                        data: Some(ss.base64_png),
                        mime_type: Some("image/png".into()),
                    },
                    McpContent {
                        content_type: "text".into(),
                        text: Some(format!("Screenshot: {}x{}", ss.width, ss.height)),
                        data: None,
                        mime_type: None,
                    },
                ],
                is_error: false,
            },
            Err(e) => McpToolResult {
                content: vec![McpContent {
                    content_type: "text".into(),
                    text: Some(format!("Screenshot failed: {e}")),
                    data: None,
                    mime_type: None,
                }],
                is_error: true,
            },
        }
    }

    fn handle_click(&self, input: Value) -> McpToolResult {
        let x = input["x"].as_i64().unwrap_or(0) as i32;
        let y = input["y"].as_i64().unwrap_or(0) as i32;
        let button = parse_button(input.get("button"));

        match input::click(x, y, button) {
            Ok(()) => ok_result(format!("Clicked {button:?} at ({x}, {y})")),
            Err(e) => err_result(format!("Click failed: {e}")),
        }
    }

    fn handle_double_click(&self, input: Value) -> McpToolResult {
        let x = input["x"].as_i64().unwrap_or(0) as i32;
        let y = input["y"].as_i64().unwrap_or(0) as i32;
        let button = parse_button(input.get("button"));

        match input::double_click(x, y, button) {
            Ok(()) => ok_result(format!("Double-clicked {button:?} at ({x}, {y})")),
            Err(e) => err_result(format!("Double-click failed: {e}")),
        }
    }

    fn handle_type_text(&self, input: Value) -> McpToolResult {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return err_result("Missing 'text' parameter".into()),
        };

        match input::type_text(text) {
            Ok(()) => ok_result(format!("Typed {} characters", text.len())),
            Err(e) => err_result(format!("Type failed: {e}")),
        }
    }

    fn handle_key(&self, input: Value) -> McpToolResult {
        let combo = match input["combo"].as_str() {
            Some(c) => c,
            None => return err_result("Missing 'combo' parameter".into()),
        };

        match input::key_press(combo) {
            Ok(()) => ok_result(format!("Pressed: {combo}")),
            Err(e) => err_result(format!("Key press failed: {e}")),
        }
    }

    fn handle_scroll(&self, input: Value) -> McpToolResult {
        let x = input["x"].as_i64().unwrap_or(0) as i32;
        let y = input["y"].as_i64().unwrap_or(0) as i32;
        let amount = input["amount"].as_i64().unwrap_or(3) as i32;
        let direction = match input["direction"].as_str() {
            Some("up") => ScrollDirection::Up,
            Some("down") => ScrollDirection::Down,
            Some("left") => ScrollDirection::Left,
            Some("right") => ScrollDirection::Right,
            _ => return err_result("Invalid 'direction'. Use: up, down, left, right".into()),
        };

        match input::scroll(x, y, direction, amount) {
            Ok(()) => ok_result(format!("Scrolled {direction:?} {amount} at ({x}, {y})")),
            Err(e) => err_result(format!("Scroll failed: {e}")),
        }
    }

    fn handle_mouse_move(&self, input: Value) -> McpToolResult {
        let x = input["x"].as_i64().unwrap_or(0) as i32;
        let y = input["y"].as_i64().unwrap_or(0) as i32;

        match input::mouse_move(x, y) {
            Ok(()) => ok_result(format!("Moved to ({x}, {y})")),
            Err(e) => err_result(format!("Mouse move failed: {e}")),
        }
    }

    fn handle_cursor_position(&self) -> McpToolResult {
        match input::cursor_position() {
            Ok((x, y)) => ok_result(format!("Cursor at ({x}, {y})")),
            Err(e) => err_result(format!("Failed to get cursor position: {e}")),
        }
    }

    fn handle_clipboard_read(&self) -> McpToolResult {
        match clipboard_read() {
            Ok(text) => {
                if text.is_empty() {
                    ok_result("Clipboard is empty".into())
                } else {
                    ok_result(text)
                }
            }
            Err(e) => err_result(format!("Clipboard read failed: {e}")),
        }
    }

    fn handle_clipboard_write(&self, input: Value) -> McpToolResult {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return err_result("Missing 'text' parameter".into()),
        };
        match clipboard_write(text) {
            Ok(()) => ok_result(format!("Copied {} chars to clipboard", text.len())),
            Err(e) => err_result(format!("Clipboard write failed: {e}")),
        }
    }

    fn handle_platform_info(&self) -> McpToolResult {
        let info = &self.platform;
        let text = format!(
            "OS: {} {}\nArch: {}\nDisplay: {}\nHas display: {}\nHostname: {}",
            info.os, info.os_version, info.arch, info.display_server,
            info.has_display, info.hostname,
        );
        ok_result(text)
    }
}

fn parse_button(value: Option<&Value>) -> MouseButton {
    match value.and_then(Value::as_str) {
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn ok_result(text: String) -> McpToolResult {
    McpToolResult {
        content: vec![McpContent {
            content_type: "text".into(),
            text: Some(text),
            data: None,
            mime_type: None,
        }],
        is_error: false,
    }
}

fn err_result(text: String) -> McpToolResult {
    McpToolResult {
        content: vec![McpContent {
            content_type: "text".into(),
            text: Some(text),
            data: None,
            mime_type: None,
        }],
        is_error: true,
    }
}

// ── Clipboard ────────────────────────────────────────────────────────────────

/// Read text from the system clipboard.
fn clipboard_read() -> anyhow::Result<String> {
    // Use platform-specific commands for clipboard access (no extra dependency needed)
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Clipboard"])
            .output()?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
        } else {
            anyhow::bail!("powershell Get-Clipboard failed");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("pbpaste").output()?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            anyhow::bail!("pbpaste failed");
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Try xclip first, then xsel, then wl-paste (Wayland)
        for cmd in &[
            &["xclip", "-selection", "clipboard", "-o"][..],
            &["xsel", "--clipboard", "--output"][..],
            &["wl-paste"][..],
        ] {
            if let Ok(out) = std::process::Command::new(cmd[0]).args(&cmd[1..]).output() {
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).to_string());
                }
            }
        }
        anyhow::bail!("No clipboard tool found (install xclip, xsel, or wl-paste)")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("Clipboard not supported on this OS")
    }
}

/// Write text to the system clipboard.
fn clipboard_write(text: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Set-Clipboard -Value $input"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("powershell Set-Clipboard failed");
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("pbcopy failed");
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        // Try xclip, then xsel, then wl-copy
        for (cmd, args) in &[
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("wl-copy", vec![]),
        ] {
            if let Ok(mut child) = std::process::Command::new(cmd)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if let Ok(status) = child.wait() {
                    if status.success() {
                        return Ok(());
                    }
                }
            }
        }
        anyhow::bail!("No clipboard tool found (install xclip, xsel, or wl-copy)")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
        anyhow::bail!("Clipboard not supported on this OS")
    }
}

// ── Platform Detection ───────────────────────────────────────────────────────

/// Detect OS, architecture, display server, and capabilities.
fn detect_platform() -> PlatformInfo {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();

    let os_version = detect_os_version();
    let display_server = detect_display_server();
    let has_display = detect_has_display();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into());

    PlatformInfo { os, os_version, arch, display_server, has_display, hostname }
}

fn detect_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        // Use ver command or registry
        std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Windows".into())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "macOS".into())
    }
    #[cfg(target_os = "linux")]
    {
        // Try /etc/os-release
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                content.lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".into())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "unknown".into()
    }
}

fn detect_display_server() -> String {
    #[cfg(target_os = "windows")]
    { "win32".into() }
    #[cfg(target_os = "macos")]
    { "quartz".into() }
    #[cfg(target_os = "linux")]
    {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            "wayland".into()
        } else if std::env::var("DISPLAY").is_ok() {
            "x11".into()
        } else {
            "none (headless)".into()
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    { "unknown".into() }
}

fn detect_has_display() -> bool {
    #[cfg(target_os = "windows")]
    { true } // Windows always has a display in desktop mode
    #[cfg(target_os = "macos")]
    { true }
    #[cfg(target_os = "linux")]
    {
        std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    { false }
}

/// Minimize the current terminal/console window.
/// Best-effort: does nothing if the window cannot be found.
fn minimize_terminal_window() {
    #[cfg(target_os = "windows")]
    {
        // Use powershell to minimize the console window
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command",
                "(Get-Process -Id $PID).MainWindowHandle | ForEach-Object { \
                 Add-Type '[DllImport(\"user32.dll\")] public static extern bool ShowWindow(IntPtr h, int c);' -Name W -Namespace U; \
                 [U.W]::ShowWindow($_, 6) }"])
            .output();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to set visible of first process whose frontmost is true to false"])
            .output();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdotool")
            .args(["getactivewindow", "windowminimize"])
            .output();
    }
}

/// Restore the terminal window after screenshot capture.
fn restore_terminal_window() {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command",
                "(Get-Process -Id $PID).MainWindowHandle | ForEach-Object { \
                 Add-Type '[DllImport(\"user32.dll\")] public static extern bool ShowWindow(IntPtr h, int c);' -Name W -Namespace U; \
                 [U.W]::ShowWindow($_, 9) }"])
            .output();
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, use Cmd+Tab like behavior
        let _ = std::process::Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to set visible of first process whose frontmost is true to true"])
            .output();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdotool")
            .args(["getactivewindow", "windowactivate"])
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests only verify tool listing and error handling.
    // Actual input simulation tests require a display and are not run in CI.

    #[test]
    fn list_tools_has_expected_count() {
        // We need a session lock to create the server, skip if unable
        let server = match ComputerUseMcpServer::new() {
            Ok(s) => s,
            Err(_) => return, // Can't acquire lock in this test environment
        };
        let tools = server.list_tools();
        assert_eq!(tools.len(), 11);

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"screenshot"));
        assert!(names.contains(&"click"));
        assert!(names.contains(&"double_click"));
        assert!(names.contains(&"type_text"));
        assert!(names.contains(&"key"));
        assert!(names.contains(&"scroll"));
        assert!(names.contains(&"mouse_move"));
        assert!(names.contains(&"cursor_position"));
        assert!(names.contains(&"clipboard_read"));
        assert!(names.contains(&"clipboard_write"));
        assert!(names.contains(&"platform_info"));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let server = match ComputerUseMcpServer::new() {
            Ok(s) => s,
            Err(_) => return,
        };
        let result = server.call_tool("nonexistent", json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn parse_button_defaults() {
        assert_eq!(parse_button(None), MouseButton::Left);
        assert_eq!(parse_button(Some(&json!("left"))), MouseButton::Left);
        assert_eq!(parse_button(Some(&json!("right"))), MouseButton::Right);
        assert_eq!(parse_button(Some(&json!("middle"))), MouseButton::Middle);
    }

    #[test]
    fn detect_platform_returns_valid_info() {
        let info = detect_platform();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.display_server.is_empty());
        assert!(!info.os_version.is_empty());
        // hostname may be "unknown" in some CI environments
    }

    #[test]
    fn platform_info_tool_returns_text() {
        let server = match ComputerUseMcpServer::new() {
            Ok(s) => s,
            Err(_) => return,
        };
        let result = server.call_tool("platform_info", json!({}));
        assert!(!result.is_error);
        let text = result.content[0].text.as_deref().unwrap_or("");
        assert!(text.contains("OS:"));
        assert!(text.contains("Arch:"));
    }

    #[test]
    fn clipboard_write_missing_text_returns_error() {
        let server = match ComputerUseMcpServer::new() {
            Ok(s) => s,
            Err(_) => return,
        };
        let result = server.call_tool("clipboard_write", json!({}));
        assert!(result.is_error);
    }
}
