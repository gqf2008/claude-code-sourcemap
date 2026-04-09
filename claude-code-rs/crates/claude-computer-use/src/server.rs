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
}

#[allow(clippy::unused_self)]
impl ComputerUseMcpServer {
    /// Create a new Computer Use server, acquiring the session lock.
    pub fn new() -> anyhow::Result<Self> {
        let lock = SessionLock::acquire()?;
        Ok(Self { _lock: lock })
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
            _ => err_result(format!("Unknown tool: {name}")),
        }
    }

    fn handle_screenshot(&self, input: Value) -> McpToolResult {
        let result = if let Some(region) = input.get("region") {
            let x = region["x"].as_i64().unwrap_or(0) as i32;
            let y = region["y"].as_i64().unwrap_or(0) as i32;
            let w = region["width"].as_u64().unwrap_or(100) as u32;
            let h = region["height"].as_u64().unwrap_or(100) as u32;
            screenshot::capture_region(x, y, w, h)
        } else {
            screenshot::capture_screen()
        };

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
        assert_eq!(tools.len(), 8);

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"screenshot"));
        assert!(names.contains(&"click"));
        assert!(names.contains(&"double_click"));
        assert!(names.contains(&"type_text"));
        assert!(names.contains(&"key"));
        assert!(names.contains(&"scroll"));
        assert!(names.contains(&"mouse_move"));
        assert!(names.contains(&"cursor_position"));
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
}
