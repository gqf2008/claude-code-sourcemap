//! `/theme` command handler.

use crate::theme::{self, ThemeName, ThemeSetting};

/// Handle `/theme [name]`.
///
/// - No argument: list all themes with the active one highlighted.
/// - With argument: set the specified theme.
pub(crate) fn handle_theme_command(name: &str) {
    if name.is_empty() {
        // List themes
        let current = theme::current_theme_name();
        println!("\x1b[1mTerminal Themes\x1b[0m\n");
        for &setting in ThemeSetting::ALL {
            let display = setting.display_name();
            let marker = if let Some(tn) = setting_to_name(setting) {
                if tn == current { " \x1b[32m← active\x1b[0m" } else { "" }
            } else {
                // "Auto" — check if auto resolves to current
                let resolved = ThemeSetting::Auto.resolve();
                if resolved == current && setting == ThemeSetting::Auto {
                    " \x1b[32m← active\x1b[0m"
                } else {
                    ""
                }
            };
            println!("  {:<24}{}", display, marker);
        }
        println!("\n\x1b[2mUsage: /theme <name>\x1b[0m");
        println!("\x1b[2mNames: dark, light, dark-daltonized, light-daltonized, dark-ansi, light-ansi, auto\x1b[0m");
        return;
    }

    let lower = name.to_lowercase();
    if lower == "auto" {
        let resolved = ThemeSetting::Auto.resolve();
        theme::set_theme(resolved);
        persist_theme_setting(ThemeSetting::Auto);
        println!("Theme set to \x1b[1mAuto\x1b[0m (resolved: {})", resolved.display_name());
        return;
    }

    match lower.parse::<ThemeName>() {
        Ok(tn) => {
            theme::set_theme(tn);
            let setting = name_to_setting(tn);
            persist_theme_setting(setting);
            println!("Theme set to \x1b[1m{}\x1b[0m", tn.display_name());
        }
        Err(_) => {
            eprintln!("\x1b[31mUnknown theme: {}\x1b[0m", name);
            eprintln!("\x1b[2mAvailable: dark, light, dark-daltonized, light-daltonized, dark-ansi, light-ansi, auto\x1b[0m");
        }
    }
}

fn setting_to_name(s: ThemeSetting) -> Option<ThemeName> {
    match s {
        ThemeSetting::Auto => None,
        ThemeSetting::Dark => Some(ThemeName::Dark),
        ThemeSetting::Light => Some(ThemeName::Light),
        ThemeSetting::DarkDaltonized => Some(ThemeName::DarkDaltonized),
        ThemeSetting::LightDaltonized => Some(ThemeName::LightDaltonized),
        ThemeSetting::DarkAnsi => Some(ThemeName::DarkAnsi),
        ThemeSetting::LightAnsi => Some(ThemeName::LightAnsi),
    }
}

fn name_to_setting(n: ThemeName) -> ThemeSetting {
    match n {
        ThemeName::Dark => ThemeSetting::Dark,
        ThemeName::Light => ThemeSetting::Light,
        ThemeName::DarkDaltonized => ThemeSetting::DarkDaltonized,
        ThemeName::LightDaltonized => ThemeSetting::LightDaltonized,
        ThemeName::DarkAnsi => ThemeSetting::DarkAnsi,
        ThemeName::LightAnsi => ThemeSetting::LightAnsi,
    }
}

/// Persist the theme setting to settings.json (best-effort).
fn persist_theme_setting(setting: ThemeSetting) {
    let Some(config_dir) = claude_core::config::Settings::claude_dir() else {
        return;
    };
    let settings_path = config_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    settings["theme"] = serde_json::to_value(setting).unwrap_or_default();
    if let Ok(json) = serde_json::to_string_pretty(&settings) {
        if let Some(parent) = settings_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&settings_path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setting_to_name_roundtrip() {
        for &tn in ThemeName::ALL {
            let setting = name_to_setting(tn);
            assert_eq!(setting_to_name(setting), Some(tn));
        }
    }

    #[test]
    fn test_auto_setting_resolves_to_none() {
        assert_eq!(setting_to_name(ThemeSetting::Auto), None);
    }
}
