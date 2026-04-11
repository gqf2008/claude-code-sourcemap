//! Mouse and keyboard input simulation using `enigo`.
//!
//! Provides cross-platform desktop automation for:
//! - Mouse: click, double-click, move, scroll
//! - Keyboard: type text, press/release keys, key combinations

use enigo::{
    Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Mouse button type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl From<MouseButton> for enigo::Button {
    fn from(btn: MouseButton) -> Self {
        match btn {
            MouseButton::Left => enigo::Button::Left,
            MouseButton::Right => enigo::Button::Right,
            MouseButton::Middle => enigo::Button::Middle,
        }
    }
}

/// Scroll direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Create an Enigo instance with default settings.
fn create_enigo() -> anyhow::Result<Enigo> {
    Enigo::new(&Settings::default())
        .map_err(|e| anyhow::anyhow!("Failed to initialize input controller: {e}"))
}

// ── Mouse operations ──

/// Click at the specified coordinates.
pub fn click(x: i32, y: i32, button: MouseButton) -> anyhow::Result<()> {
    debug!(x, y, ?button, "click");
    let mut enigo = create_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| anyhow::anyhow!("move_mouse failed: {e}"))?;
    enigo.button(button.into(), Direction::Click)
        .map_err(|e| anyhow::anyhow!("click failed: {e}"))?;
    Ok(())
}

/// Double-click at the specified coordinates.
pub fn double_click(x: i32, y: i32, button: MouseButton) -> anyhow::Result<()> {
    debug!(x, y, ?button, "double_click");
    let mut enigo = create_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| anyhow::anyhow!("move_mouse failed: {e}"))?;
    enigo.button(button.into(), Direction::Click)
        .map_err(|e| anyhow::anyhow!("click 1 failed: {e}"))?;
    enigo.button(button.into(), Direction::Click)
        .map_err(|e| anyhow::anyhow!("click 2 failed: {e}"))?;
    Ok(())
}

/// Move the mouse to the specified coordinates.
pub fn mouse_move(x: i32, y: i32) -> anyhow::Result<()> {
    debug!(x, y, "mouse_move");
    let mut enigo = create_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| anyhow::anyhow!("move_mouse failed: {e}"))?;
    Ok(())
}

/// Scroll at the specified coordinates.
pub fn scroll(x: i32, y: i32, direction: ScrollDirection, amount: i32) -> anyhow::Result<()> {
    debug!(x, y, ?direction, amount, "scroll");
    let mut enigo = create_enigo()?;
    enigo.move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| anyhow::anyhow!("move_mouse failed: {e}"))?;

    match direction {
        ScrollDirection::Up => enigo.scroll(amount, enigo::Axis::Vertical),
        ScrollDirection::Down => enigo.scroll(-amount, enigo::Axis::Vertical),
        ScrollDirection::Left => enigo.scroll(-amount, enigo::Axis::Horizontal),
        ScrollDirection::Right => enigo.scroll(amount, enigo::Axis::Horizontal),
    }
    .map_err(|e| anyhow::anyhow!("scroll failed: {e}"))?;
    Ok(())
}

/// Get the current cursor position.
pub fn cursor_position() -> anyhow::Result<(i32, i32)> {
    let enigo = create_enigo()?;
    let (x, y) = enigo.location()
        .map_err(|e| anyhow::anyhow!("cursor_position failed: {e}"))?;
    Ok((x, y))
}

// ── Keyboard operations ──

/// Type a text string.
pub fn type_text(text: &str) -> anyhow::Result<()> {
    debug!(len = text.len(), "type_text");
    let mut enigo = create_enigo()?;
    enigo.text(text)
        .map_err(|e| anyhow::anyhow!("type_text failed: {e}"))?;
    Ok(())
}

/// Press a single key or key combination (e.g., "ctrl+c", "enter", "tab").
pub fn key_press(key_combo: &str) -> anyhow::Result<()> {
    debug!(key_combo, "key_press");
    let mut enigo = create_enigo()?;

    let parts: Vec<&str> = key_combo.split('+').map(str::trim).collect();
    let mut modifiers = Vec::new();
    let main_key = if parts.len() > 1 {
        for &part in &parts[..parts.len() - 1] {
            modifiers.push(parse_modifier(part)?);
        }
        parse_key(parts.last().unwrap())?
    } else {
        parse_key(parts[0])?
    };

    // Press modifiers
    for &m in &modifiers {
        enigo.key(m, Direction::Press)
            .map_err(|e| anyhow::anyhow!("modifier press failed: {e}"))?;
    }

    // Press and release main key
    enigo.key(main_key, Direction::Click)
        .map_err(|e| anyhow::anyhow!("key press failed: {e}"))?;

    // Release modifiers in reverse order
    for &m in modifiers.iter().rev() {
        enigo.key(m, Direction::Release)
            .map_err(|e| anyhow::anyhow!("modifier release failed: {e}"))?;
    }

    Ok(())
}

/// Parse a modifier key name.
fn parse_modifier(name: &str) -> anyhow::Result<Key> {
    match name.to_lowercase().as_str() {
        "ctrl" | "control" => Ok(Key::Control),
        "shift" => Ok(Key::Shift),
        "alt" => Ok(Key::Alt),
        "meta" | "super" | "win" | "cmd" | "command" => Ok(Key::Meta),
        other => anyhow::bail!("Unknown modifier: {other}"),
    }
}

/// Parse a key name to an enigo Key.
fn parse_key(name: &str) -> anyhow::Result<Key> {
    // Single character
    if name.len() == 1 {
        let ch = name.chars().next().unwrap();
        return Ok(Key::Unicode(ch));
    }

    match name.to_lowercase().as_str() {
        "enter" | "return" => Ok(Key::Return),
        "tab" => Ok(Key::Tab),
        "escape" | "esc" => Ok(Key::Escape),
        "backspace" => Ok(Key::Backspace),
        "delete" | "del" => Ok(Key::Delete),
        "space" => Ok(Key::Space),
        "up" | "uparrow" => Ok(Key::UpArrow),
        "down" | "downarrow" => Ok(Key::DownArrow),
        "left" | "leftarrow" => Ok(Key::LeftArrow),
        "right" | "rightarrow" => Ok(Key::RightArrow),
        "home" => Ok(Key::Home),
        "end" => Ok(Key::End),
        "pageup" => Ok(Key::PageUp),
        "pagedown" => Ok(Key::PageDown),
        "f1" => Ok(Key::F1),
        "f2" => Ok(Key::F2),
        "f3" => Ok(Key::F3),
        "f4" => Ok(Key::F4),
        "f5" => Ok(Key::F5),
        "f6" => Ok(Key::F6),
        "f7" => Ok(Key::F7),
        "f8" => Ok(Key::F8),
        "f9" => Ok(Key::F9),
        "f10" => Ok(Key::F10),
        "f11" => Ok(Key::F11),
        "f12" => Ok(Key::F12),
        "capslock" => Ok(Key::CapsLock),
        other => anyhow::bail!("Unknown key: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_single_char() {
        assert!(matches!(parse_key("a").unwrap(), Key::Unicode('a')));
        assert!(matches!(parse_key("Z").unwrap(), Key::Unicode('Z')));
        assert!(matches!(parse_key("1").unwrap(), Key::Unicode('1')));
    }

    #[test]
    fn parse_key_named() {
        assert!(matches!(parse_key("enter").unwrap(), Key::Return));
        assert!(matches!(parse_key("tab").unwrap(), Key::Tab));
        assert!(matches!(parse_key("escape").unwrap(), Key::Escape));
        assert!(matches!(parse_key("f1").unwrap(), Key::F1));
        assert!(matches!(parse_key("space").unwrap(), Key::Space));
    }

    #[test]
    fn parse_key_unknown() {
        assert!(parse_key("nonexistent").is_err());
    }

    #[test]
    fn parse_modifier_names() {
        assert!(matches!(parse_modifier("ctrl").unwrap(), Key::Control));
        assert!(matches!(parse_modifier("shift").unwrap(), Key::Shift));
        assert!(matches!(parse_modifier("alt").unwrap(), Key::Alt));
        assert!(matches!(parse_modifier("meta").unwrap(), Key::Meta));
        assert!(matches!(parse_modifier("cmd").unwrap(), Key::Meta));
    }

    #[test]
    fn mouse_button_conversion() {
        let _: enigo::Button = MouseButton::Left.into();
        let _: enigo::Button = MouseButton::Right.into();
        let _: enigo::Button = MouseButton::Middle.into();
    }
}
