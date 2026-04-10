//! Terminal UI components using `cliclack`.
//!
//! Provides interactive dialogs for:
//! - Permission confirmation (tool execution approval)
//! - Model selection (from known aliases)
//! - Initialization wizard (API key + defaults)
//! - Generic confirm / select helpers
//!
//! These are intended for structured multi-step interactions.
//! The main REPL loop uses `crossterm` for line editing with paste support,
//! and streaming output also uses `crossterm` directly.

#![allow(dead_code)]

use std::io;

/// Result of a permission confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionChoice {
    /// Allow this one time.
    AllowOnce,
    /// Allow and remember for this session.
    AllowSession,
    /// Allow and add a permanent rule.
    AllowAlways,
    /// Deny this request.
    Deny,
}

/// Show a permission confirmation dialog for a tool invocation.
///
/// Displays the tool name, description, and risk level, then asks
/// the user to allow or deny.
pub fn permission_confirm(
    tool_name: &str,
    description: &str,
    risk_level: &str,
) -> io::Result<PermissionChoice> {
    cliclack::intro(format!("🔒 Permission required: {}", tool_name))?;

    cliclack::note(
        format!("Risk: {}", risk_level),
        description,
    )?;

    let choice = cliclack::select("Allow this action?")
        .item("once", "Allow once", "This invocation only")
        .item("session", "Allow for session", "Remember until exit")
        .item("always", "Allow always", "Add permanent rule")
        .item("deny", "Deny", "Block this action")
        .interact()?;

    let result = match choice {
        "once" => PermissionChoice::AllowOnce,
        "session" => PermissionChoice::AllowSession,
        "always" => PermissionChoice::AllowAlways,
        _ => PermissionChoice::Deny,
    };

    cliclack::outro(match &result {
        PermissionChoice::AllowOnce => "✓ Allowed (once)",
        PermissionChoice::AllowSession => "✓ Allowed (session)",
        PermissionChoice::AllowAlways => "✓ Allowed (always)",
        PermissionChoice::Deny => "✗ Denied",
    })?;

    Ok(result)
}

/// Model selection entry for the select dialog.
struct ModelOption {
    id: &'static str,
    label: &'static str,
    hint: &'static str,
}

/// Known model options for the selection dialog.
const MODEL_OPTIONS: &[ModelOption] = &[
    ModelOption {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        hint: "Fast, balanced (default)",
    },
    ModelOption {
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6",
        hint: "Most capable, slower",
    },
    ModelOption {
        id: "claude-sonnet-4-5",
        label: "Claude Sonnet 4.5",
        hint: "Extended thinking",
    },
    ModelOption {
        id: "claude-opus-4-5",
        label: "Claude Opus 4.5",
        hint: "Highest reasoning",
    },
    ModelOption {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        hint: "Fastest, cheapest",
    },
];

/// Show a model selection dialog.
///
/// Returns the selected model ID string.
pub fn model_select(current: &str) -> io::Result<String> {
    let mut select = cliclack::select(format!("Select model (current: {})", current));

    for opt in MODEL_OPTIONS {
        let hint = if opt.id == current {
            format!("{} ← current", opt.hint)
        } else {
            opt.hint.to_string()
        };
        select = select.item(opt.id.to_string(), opt.label, hint);
    }

    // Allow custom model ID entry
    select = select.item("__custom__".to_string(), "Custom model ID", "Enter manually");

    let chosen: String = select.interact()?;

    if chosen == "__custom__" {
        let custom: String = cliclack::input("Enter model ID:")
            .placeholder("claude-sonnet-4-20250514")
            .interact()?;
        Ok(custom)
    } else {
        Ok(chosen)
    }
}

/// Simple yes/no confirmation.
pub fn confirm(message: &str) -> io::Result<bool> {
    cliclack::confirm(message).interact()
}

/// Show a multi-step initialization wizard.
///
/// Collects API key and default model, returns `(api_key, model)`.
pub fn init_wizard(default_model: &str) -> io::Result<(String, String)> {
    cliclack::intro("🚀 Claude Code Setup")?;

    let api_key: String = cliclack::input("Anthropic API key:")
        .placeholder("sk-ant-...")
        .validate(|input: &String| {
            if input.trim().is_empty() {
                Err("API key is required".to_string())
            } else {
                Ok(())
            }
        })
        .interact()?;

    let model: String = cliclack::select("Default model:")
        .item(
            "claude-sonnet-4-6".to_string(),
            "Claude Sonnet 4.6",
            "Recommended — fast & capable",
        )
        .item(
            "claude-opus-4-6".to_string(),
            "Claude Opus 4.6",
            "Most capable, higher cost",
        )
        .item(
            "claude-haiku-4-5".to_string(),
            "Claude Haiku 4.5",
            "Fastest, lowest cost",
        )
        .initial_value(default_model.to_string())
        .interact()?;

    let permission_mode: String = cliclack::select("Permission mode:")
        .item(
            "default".to_string(),
            "Default",
            "Ask before file writes and commands",
        )
        .item(
            "accept-edits".to_string(),
            "Accept edits",
            "Auto-allow file writes, ask for commands",
        )
        .item(
            "bypass-all".to_string(),
            "Bypass all",
            "Auto-allow everything (risky!)",
        )
        .initial_value("default".to_string())
        .interact()?;

    let _permission_mode = permission_mode; // stored but not returned yet

    cliclack::outro(format!(
        "✓ Setup complete! Using {} with key {}...{}",
        model,
        &api_key[..6.min(api_key.len())],
        &api_key[api_key.len().saturating_sub(4)..]
    ))?;

    Ok((api_key, model))
}

/// Show a spinner while an async operation runs.
///
/// Returns the spinner handle. Call `.stop()` on it when done.
pub fn spinner(message: &str) -> io::Result<cliclack::ProgressBar> {
    let s = cliclack::spinner();
    s.start(message);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_choice_variants() {
        assert_eq!(PermissionChoice::AllowOnce, PermissionChoice::AllowOnce);
        assert_ne!(PermissionChoice::AllowOnce, PermissionChoice::Deny);
        assert_ne!(PermissionChoice::AllowSession, PermissionChoice::AllowAlways);
    }

    #[test]
    fn model_options_list() {
        assert!(MODEL_OPTIONS.len() >= 3);
        assert_eq!(MODEL_OPTIONS[0].id, "claude-sonnet-4-6");
    }

    // Note: Interactive cliclack functions can't be unit tested without a TTY.
    // They are tested manually or via integration tests with a PTY mock.
}
