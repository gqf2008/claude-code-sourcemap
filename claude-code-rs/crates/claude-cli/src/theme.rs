//! Terminal theme system — 6 themes matching the TS original.
//!
//! Each theme defines semantic color slots. Colors are represented as ANSI
//! escape sequences (256-color or RGB where supported). The active theme is
//! loaded once at startup from `settings.json` and can be switched at runtime
//! via the `/theme` REPL command.

use std::sync::OnceLock;
use std::sync::RwLock;

// ── Color type ──────────────────────────────────────────────────────────

/// An ANSI color expressed as an escape sequence fragment.
/// e.g. `"38;2;215;119;87"` for RGB(215,119,87) foreground.
#[derive(Debug, Clone)]
pub struct AnsiColor {
    /// Foreground escape (e.g. `"\x1b[38;2;215;119;87m"`)
    pub fg: &'static str,
    /// Background escape (e.g. `"\x1b[48;2;215;119;87m"`)
    pub bg: &'static str,
}

impl AnsiColor {
    const fn new(fg: &'static str, bg: &'static str) -> Self {
        Self { fg, bg }
    }

    /// Shorthand for 256-color foreground only (bg = "").
    const fn fg(fg: &'static str) -> Self {
        Self { fg, bg: "" }
    }
}

// ── Theme definition ────────────────────────────────────────────────────

/// Semantic color slots for terminal rendering.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: ThemeName,

    // Brand
    pub claude: AnsiColor,

    // UI elements
    pub permission: AnsiColor,
    pub plan_mode: AnsiColor,
    pub bash_border: AnsiColor,
    pub prompt_border: AnsiColor,

    // Text
    pub text: AnsiColor,
    pub inverse_text: AnsiColor,
    pub dim: AnsiColor,
    pub subtle: AnsiColor,

    // Semantic
    pub success: AnsiColor,
    pub error: AnsiColor,
    pub warning: AnsiColor,

    // Diff
    pub diff_added: AnsiColor,
    pub diff_removed: AnsiColor,

    // Code
    pub code_bg: AnsiColor,
    pub code_border: AnsiColor,

    // Agent sub-colors
    pub agent_red: AnsiColor,
    pub agent_blue: AnsiColor,
    pub agent_green: AnsiColor,
    pub agent_yellow: AnsiColor,
    pub agent_magenta: AnsiColor,

    // Spinner
    pub spinner: AnsiColor,

    // Status line
    pub status_dim: AnsiColor,
    pub status_warn: AnsiColor,
    pub status_crit: AnsiColor,

    // Reset sequence (always the same)
    pub reset: &'static str,
}

// ── Theme names ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    Dark,
    Light,
    DarkDaltonized,
    LightDaltonized,
    DarkAnsi,
    LightAnsi,
}

impl ThemeName {
    pub const ALL: &'static [ThemeName] = &[
        ThemeName::Dark,
        ThemeName::Light,
        ThemeName::DarkDaltonized,
        ThemeName::LightDaltonized,
        ThemeName::DarkAnsi,
        ThemeName::LightAnsi,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            ThemeName::Dark => "Dark",
            ThemeName::Light => "Light",
            ThemeName::DarkDaltonized => "Dark (Daltonized)",
            ThemeName::LightDaltonized => "Light (Daltonized)",
            ThemeName::DarkAnsi => "Dark (ANSI)",
            ThemeName::LightAnsi => "Light (ANSI)",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Dark => "dark",
            ThemeName::Light => "light",
            ThemeName::DarkDaltonized => "dark-daltonized",
            ThemeName::LightDaltonized => "light-daltonized",
            ThemeName::DarkAnsi => "dark-ansi",
            ThemeName::LightAnsi => "light-ansi",
        }
    }
}

impl std::fmt::Display for ThemeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl std::str::FromStr for ThemeName {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "dark" => Ok(ThemeName::Dark),
            "light" => Ok(ThemeName::Light),
            "dark-daltonized" => Ok(ThemeName::DarkDaltonized),
            "light-daltonized" => Ok(ThemeName::LightDaltonized),
            "dark-ansi" => Ok(ThemeName::DarkAnsi),
            "light-ansi" => Ok(ThemeName::LightAnsi),
            _ => Err(format!("unknown theme: {s}")),
        }
    }
}

// ── Theme setting (includes "auto") ─────────────────────────────────────

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeSetting {
    #[default]
    Auto,
    Dark,
    Light,
    DarkDaltonized,
    LightDaltonized,
    DarkAnsi,
    LightAnsi,
}

impl ThemeSetting {
    pub fn resolve(self) -> ThemeName {
        match self {
            ThemeSetting::Auto => detect_dark_mode(),
            ThemeSetting::Dark => ThemeName::Dark,
            ThemeSetting::Light => ThemeName::Light,
            ThemeSetting::DarkDaltonized => ThemeName::DarkDaltonized,
            ThemeSetting::LightDaltonized => ThemeName::LightDaltonized,
            ThemeSetting::DarkAnsi => ThemeName::DarkAnsi,
            ThemeSetting::LightAnsi => ThemeName::LightAnsi,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ThemeSetting::Auto => "Auto",
            ThemeSetting::Dark => "Dark",
            ThemeSetting::Light => "Light",
            ThemeSetting::DarkDaltonized => "Dark (Daltonized)",
            ThemeSetting::LightDaltonized => "Light (Daltonized)",
            ThemeSetting::DarkAnsi => "Dark (ANSI)",
            ThemeSetting::LightAnsi => "Light (ANSI)",
        }
    }

    pub const ALL: &'static [ThemeSetting] = &[
        ThemeSetting::Auto,
        ThemeSetting::Dark,
        ThemeSetting::Light,
        ThemeSetting::DarkDaltonized,
        ThemeSetting::LightDaltonized,
        ThemeSetting::DarkAnsi,
        ThemeSetting::LightAnsi,
    ];
}

impl std::fmt::Display for ThemeSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

// ── Dark mode detection ─────────────────────────────────────────────────

fn detect_dark_mode() -> ThemeName {
    // Check COLORFGBG env (format: "fg;bg" — bg >= 8 usually means dark)
    if let Ok(val) = std::env::var("COLORFGBG") {
        if let Some(bg) = val.rsplit(';').next().and_then(|s| s.parse::<u8>().ok()) {
            return if bg < 8 { ThemeName::Dark } else { ThemeName::Light };
        }
    }
    // Windows Terminal / modern terminals default dark
    ThemeName::Dark
}

// ── Theme definitions ───────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";

fn dark_theme() -> Theme {
    Theme {
        name: ThemeName::Dark,
        claude: AnsiColor::fg("\x1b[38;2;215;119;87m"),        // Claude orange
        permission: AnsiColor::fg("\x1b[38;2;87;105;247m"),    // Medium blue
        plan_mode: AnsiColor::fg("\x1b[38;2;0;180;180m"),      // Teal
        bash_border: AnsiColor::fg("\x1b[38;2;100;100;100m"),  // Gray
        prompt_border: AnsiColor::fg("\x1b[38;2;87;105;247m"), // Blue
        text: AnsiColor::fg("\x1b[37m"),                        // White
        inverse_text: AnsiColor::fg("\x1b[30m"),                // Black
        dim: AnsiColor::fg("\x1b[2m"),                          // Dim
        subtle: AnsiColor::fg("\x1b[38;2;128;128;128m"),       // Gray
        success: AnsiColor::fg("\x1b[38;2;105;219;124m"),      // Green
        error: AnsiColor::fg("\x1b[38;2;255;107;107m"),        // Red
        warning: AnsiColor::fg("\x1b[38;2;255;200;87m"),       // Yellow
        diff_added: AnsiColor::new("\x1b[38;2;105;219;124m", "\x1b[48;2;30;60;30m"),
        diff_removed: AnsiColor::new("\x1b[38;2;255;168;180m", "\x1b[48;2;60;30;30m"),
        code_bg: AnsiColor::fg("\x1b[48;2;40;40;40m"),
        code_border: AnsiColor::fg("\x1b[38;2;80;80;80m"),
        agent_red: AnsiColor::fg("\x1b[38;2;255;107;107m"),
        agent_blue: AnsiColor::fg("\x1b[38;2;100;149;237m"),
        agent_green: AnsiColor::fg("\x1b[38;2;105;219;124m"),
        agent_yellow: AnsiColor::fg("\x1b[38;2;255;200;87m"),
        agent_magenta: AnsiColor::fg("\x1b[38;2;200;130;255m"),
        spinner: AnsiColor::fg("\x1b[36m"),                     // Cyan
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[31m"),
        reset: RESET,
    }
}

fn light_theme() -> Theme {
    Theme {
        name: ThemeName::Light,
        claude: AnsiColor::fg("\x1b[38;2;195;99;67m"),        // Darker orange
        permission: AnsiColor::fg("\x1b[38;2;67;85;227m"),    // Deeper blue
        plan_mode: AnsiColor::fg("\x1b[38;2;0;102;102m"),     // Muted teal
        bash_border: AnsiColor::fg("\x1b[38;2;180;180;180m"), // Light gray
        prompt_border: AnsiColor::fg("\x1b[38;2;67;85;227m"),
        text: AnsiColor::fg("\x1b[30m"),                       // Black
        inverse_text: AnsiColor::fg("\x1b[37m"),               // White
        dim: AnsiColor::fg("\x1b[2m"),
        subtle: AnsiColor::fg("\x1b[38;2;140;140;140m"),
        success: AnsiColor::fg("\x1b[38;2;44;122;57m"),       // Dark green
        error: AnsiColor::fg("\x1b[38;2;171;43;63m"),         // Dark red
        warning: AnsiColor::fg("\x1b[38;2;180;130;0m"),       // Dark yellow
        diff_added: AnsiColor::new("\x1b[38;2;44;122;57m", "\x1b[48;2;220;255;220m"),
        diff_removed: AnsiColor::new("\x1b[38;2;171;43;63m", "\x1b[48;2;255;220;220m"),
        code_bg: AnsiColor::fg("\x1b[48;2;240;240;240m"),
        code_border: AnsiColor::fg("\x1b[38;2;200;200;200m"),
        agent_red: AnsiColor::fg("\x1b[38;2;171;43;63m"),
        agent_blue: AnsiColor::fg("\x1b[38;2;67;85;227m"),
        agent_green: AnsiColor::fg("\x1b[38;2;44;122;57m"),
        agent_yellow: AnsiColor::fg("\x1b[38;2;180;130;0m"),
        agent_magenta: AnsiColor::fg("\x1b[38;2;150;80;200m"),
        spinner: AnsiColor::fg("\x1b[34m"),                    // Blue
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[31m"),
        reset: RESET,
    }
}

fn dark_daltonized_theme() -> Theme {
    // Optimized for color-blind users — avoids pure red/green
    Theme {
        name: ThemeName::DarkDaltonized,
        claude: AnsiColor::fg("\x1b[38;2;215;160;87m"),       // Orange-gold
        permission: AnsiColor::fg("\x1b[38;2;100;160;255m"),  // Sky blue
        plan_mode: AnsiColor::fg("\x1b[38;2;0;200;200m"),
        bash_border: AnsiColor::fg("\x1b[38;2;100;100;100m"),
        prompt_border: AnsiColor::fg("\x1b[38;2;100;160;255m"),
        text: AnsiColor::fg("\x1b[37m"),
        inverse_text: AnsiColor::fg("\x1b[30m"),
        dim: AnsiColor::fg("\x1b[2m"),
        subtle: AnsiColor::fg("\x1b[38;2;128;128;128m"),
        success: AnsiColor::fg("\x1b[38;2;100;180;255m"),     // Blue instead of green
        error: AnsiColor::fg("\x1b[38;2;255;170;100m"),       // Orange instead of red
        warning: AnsiColor::fg("\x1b[38;2;255;255;100m"),     // Bright yellow
        diff_added: AnsiColor::new("\x1b[38;2;100;180;255m", "\x1b[48;2;20;40;70m"),
        diff_removed: AnsiColor::new("\x1b[38;2;255;170;100m", "\x1b[48;2;70;40;20m"),
        code_bg: AnsiColor::fg("\x1b[48;2;40;40;40m"),
        code_border: AnsiColor::fg("\x1b[38;2;80;80;80m"),
        agent_red: AnsiColor::fg("\x1b[38;2;255;170;100m"),
        agent_blue: AnsiColor::fg("\x1b[38;2;100;160;255m"),
        agent_green: AnsiColor::fg("\x1b[38;2;100;180;255m"),
        agent_yellow: AnsiColor::fg("\x1b[38;2;255;255;100m"),
        agent_magenta: AnsiColor::fg("\x1b[38;2;200;150;255m"),
        spinner: AnsiColor::fg("\x1b[36m"),
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[38;2;255;170;100m"),
        reset: RESET,
    }
}

fn light_daltonized_theme() -> Theme {
    Theme {
        name: ThemeName::LightDaltonized,
        claude: AnsiColor::fg("\x1b[38;2;180;120;50m"),
        permission: AnsiColor::fg("\x1b[38;2;50;100;200m"),
        plan_mode: AnsiColor::fg("\x1b[38;2;0;120;120m"),
        bash_border: AnsiColor::fg("\x1b[38;2;180;180;180m"),
        prompt_border: AnsiColor::fg("\x1b[38;2;50;100;200m"),
        text: AnsiColor::fg("\x1b[30m"),
        inverse_text: AnsiColor::fg("\x1b[37m"),
        dim: AnsiColor::fg("\x1b[2m"),
        subtle: AnsiColor::fg("\x1b[38;2;140;140;140m"),
        success: AnsiColor::fg("\x1b[38;2;0;100;180m"),       // Blue
        error: AnsiColor::fg("\x1b[38;2;180;100;30m"),        // Orange
        warning: AnsiColor::fg("\x1b[38;2;160;140;0m"),
        diff_added: AnsiColor::new("\x1b[38;2;0;100;180m", "\x1b[48;2;220;240;255m"),
        diff_removed: AnsiColor::new("\x1b[38;2;180;100;30m", "\x1b[48;2;255;235;220m"),
        code_bg: AnsiColor::fg("\x1b[48;2;240;240;240m"),
        code_border: AnsiColor::fg("\x1b[38;2;200;200;200m"),
        agent_red: AnsiColor::fg("\x1b[38;2;180;100;30m"),
        agent_blue: AnsiColor::fg("\x1b[38;2;50;100;200m"),
        agent_green: AnsiColor::fg("\x1b[38;2;0;100;180m"),
        agent_yellow: AnsiColor::fg("\x1b[38;2;160;140;0m"),
        agent_magenta: AnsiColor::fg("\x1b[38;2;130;70;170m"),
        spinner: AnsiColor::fg("\x1b[34m"),
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[38;2;180;100;30m"),
        reset: RESET,
    }
}

fn dark_ansi_theme() -> Theme {
    // Uses only standard 16-color ANSI codes for maximum compatibility
    Theme {
        name: ThemeName::DarkAnsi,
        claude: AnsiColor::fg("\x1b[33m"),       // Yellow
        permission: AnsiColor::fg("\x1b[34m"),   // Blue
        plan_mode: AnsiColor::fg("\x1b[36m"),    // Cyan
        bash_border: AnsiColor::fg("\x1b[90m"),  // Bright black (dark gray)
        prompt_border: AnsiColor::fg("\x1b[34m"),
        text: AnsiColor::fg("\x1b[37m"),         // White
        inverse_text: AnsiColor::fg("\x1b[30m"), // Black
        dim: AnsiColor::fg("\x1b[2m"),
        subtle: AnsiColor::fg("\x1b[90m"),
        success: AnsiColor::fg("\x1b[32m"),      // Green
        error: AnsiColor::fg("\x1b[31m"),        // Red
        warning: AnsiColor::fg("\x1b[33m"),      // Yellow
        diff_added: AnsiColor::new("\x1b[32m", "\x1b[42m"),
        diff_removed: AnsiColor::new("\x1b[31m", "\x1b[41m"),
        code_bg: AnsiColor::fg("\x1b[100m"),     // Bright black bg
        code_border: AnsiColor::fg("\x1b[90m"),
        agent_red: AnsiColor::fg("\x1b[31m"),
        agent_blue: AnsiColor::fg("\x1b[34m"),
        agent_green: AnsiColor::fg("\x1b[32m"),
        agent_yellow: AnsiColor::fg("\x1b[33m"),
        agent_magenta: AnsiColor::fg("\x1b[35m"),
        spinner: AnsiColor::fg("\x1b[36m"),
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[31m"),
        reset: RESET,
    }
}

fn light_ansi_theme() -> Theme {
    Theme {
        name: ThemeName::LightAnsi,
        claude: AnsiColor::fg("\x1b[33m"),
        permission: AnsiColor::fg("\x1b[34m"),
        plan_mode: AnsiColor::fg("\x1b[36m"),
        bash_border: AnsiColor::fg("\x1b[37m"),
        prompt_border: AnsiColor::fg("\x1b[34m"),
        text: AnsiColor::fg("\x1b[30m"),
        inverse_text: AnsiColor::fg("\x1b[37m"),
        dim: AnsiColor::fg("\x1b[2m"),
        subtle: AnsiColor::fg("\x1b[37m"),
        success: AnsiColor::fg("\x1b[32m"),
        error: AnsiColor::fg("\x1b[31m"),
        warning: AnsiColor::fg("\x1b[33m"),
        diff_added: AnsiColor::new("\x1b[32m", "\x1b[42m"),
        diff_removed: AnsiColor::new("\x1b[31m", "\x1b[41m"),
        code_bg: AnsiColor::fg("\x1b[47m"),      // White bg
        code_border: AnsiColor::fg("\x1b[37m"),
        agent_red: AnsiColor::fg("\x1b[31m"),
        agent_blue: AnsiColor::fg("\x1b[34m"),
        agent_green: AnsiColor::fg("\x1b[32m"),
        agent_yellow: AnsiColor::fg("\x1b[33m"),
        agent_magenta: AnsiColor::fg("\x1b[35m"),
        spinner: AnsiColor::fg("\x1b[34m"),
        status_dim: AnsiColor::fg("\x1b[2m"),
        status_warn: AnsiColor::fg("\x1b[33m"),
        status_crit: AnsiColor::fg("\x1b[31m"),
        reset: RESET,
    }
}

/// Build a theme by name.
pub fn build_theme(name: ThemeName) -> Theme {
    match name {
        ThemeName::Dark => dark_theme(),
        ThemeName::Light => light_theme(),
        ThemeName::DarkDaltonized => dark_daltonized_theme(),
        ThemeName::LightDaltonized => light_daltonized_theme(),
        ThemeName::DarkAnsi => dark_ansi_theme(),
        ThemeName::LightAnsi => light_ansi_theme(),
    }
}

// ── Global theme accessor ───────────────────────────────────────────────

static THEME: OnceLock<RwLock<Theme>> = OnceLock::new();

fn get_lock() -> &'static RwLock<Theme> {
    THEME.get_or_init(|| RwLock::new(dark_theme()))
}

/// Initialise the global theme from a `ThemeSetting` (call once at startup).
pub fn init_theme(setting: ThemeSetting) {
    let theme = build_theme(setting.resolve());
    let lock = get_lock();
    *lock.write().unwrap() = theme;
}

/// Switch the active theme at runtime.
pub fn set_theme(name: ThemeName) {
    let lock = get_lock();
    *lock.write().unwrap() = build_theme(name);
}

/// Get the current theme name.
pub fn current_theme_name() -> ThemeName {
    let lock = get_lock();
    lock.read().unwrap().name
}

/// Read a field from the active theme. Usage:
/// ```ignore
/// let color = theme_fg(|t| t.success.fg);
/// eprintln!("{color}ok{}", theme_reset());
/// ```
pub fn theme_fg<F, R>(f: F) -> R
where
    F: FnOnce(&Theme) -> R,
{
    let lock = get_lock();
    let guard = lock.read().unwrap();
    f(&guard)
}

/// The reset sequence from the active theme (always `\x1b[0m`).
pub fn theme_reset() -> &'static str {
    RESET
}

// ── Convenience macros ──────────────────────────────────────────────────

/// Format a string with a theme color. Returns `"{color}{text}\x1b[0m"`.
pub fn themed(slot: fn(&Theme) -> &AnsiColor, text: &str) -> String {
    let lock = get_lock();
    let guard = lock.read().unwrap();
    let color = slot(&guard);
    format!("{}{}{}", color.fg, text, RESET)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_themes_build() {
        for &name in ThemeName::ALL {
            let theme = build_theme(name);
            assert_eq!(theme.name, name);
            assert!(!theme.claude.fg.is_empty());
            assert_eq!(theme.reset, "\x1b[0m");
        }
    }

    #[test]
    fn test_theme_setting_resolve() {
        assert_eq!(ThemeSetting::Dark.resolve(), ThemeName::Dark);
        assert_eq!(ThemeSetting::Light.resolve(), ThemeName::Light);
        // Auto resolves to some concrete theme
        let resolved = ThemeSetting::Auto.resolve();
        assert!(ThemeName::ALL.contains(&resolved));
    }

    #[test]
    fn test_theme_name_parse() {
        assert_eq!("dark".parse::<ThemeName>().unwrap(), ThemeName::Dark);
        assert_eq!("light-daltonized".parse::<ThemeName>().unwrap(), ThemeName::LightDaltonized);
        assert!("unknown".parse::<ThemeName>().is_err());
    }

    #[test]
    fn test_theme_setting_display() {
        assert_eq!(ThemeSetting::Auto.display_name(), "Auto");
        assert_eq!(ThemeSetting::DarkAnsi.display_name(), "Dark (ANSI)");
    }

    #[test]
    fn test_themed_helper() {
        init_theme(ThemeSetting::Dark);
        let s = themed(|t| &t.success, "ok");
        assert!(s.contains("ok"));
        assert!(s.ends_with(RESET));
    }

    #[test]
    fn test_dark_ansi_uses_basic_codes() {
        let theme = dark_ansi_theme();
        // ANSI theme should only use basic codes (\x1b[3Xm), not RGB
        assert!(!theme.success.fg.contains("38;2;"));
        assert!(!theme.error.fg.contains("38;2;"));
        assert!(!theme.warning.fg.contains("38;2;"));
    }

    #[test]
    fn test_daltonized_avoids_pure_red_green() {
        let theme = dark_daltonized_theme();
        // Success should NOT be green (should be blue-ish)
        assert_ne!(theme.success.fg, "\x1b[32m");
        // Error should NOT be red (should be orange-ish)
        assert_ne!(theme.error.fg, "\x1b[31m");
    }
}
