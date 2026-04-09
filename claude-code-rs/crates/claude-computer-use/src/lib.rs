//! Computer Use — in-process MCP server for desktop automation.
//!
//! Provides screenshot capture, mouse/keyboard control, and clipboard access
//! as MCP tools. Registered as a built-in MCP server in the agent engine.
//!
//! ## Tools
//!
//! | Tool | Description |
//! |------|-------------|
//! | `screenshot` | Capture screen or window screenshot |
//! | `click` | Click at coordinates |
//! | `double_click` | Double-click at coordinates |
//! | `type_text` | Type text string |
//! | `key` | Press key combination |
//! | `scroll` | Scroll at coordinates |
//! | `mouse_move` | Move mouse to coordinates |
//! | `cursor_position` | Get current cursor position |

pub mod input;
pub mod screenshot;
pub mod server;
mod session_lock;

pub use server::ComputerUseMcpServer;
pub use session_lock::SessionLock;
