use claude_core::permissions::PermissionResponse;
use claude_core::permissions::PermissionSuggestion;
use std::io;

/// Interactive terminal permission prompt using `cliclack`.
/// Returns a `PermissionResponse` with the user's choice.
pub fn prompt_user(
    tool_name: &str,
    description: &str,
    suggestions: &[PermissionSuggestion],
) -> PermissionResponse {
    // If not a terminal, fall back to simple stdin
    if !io::IsTerminal::is_terminal(&io::stdin()) {
        eprint!("   Allow? [y/N]: ");
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        return match input.trim().to_lowercase().as_str() {
            "y" | "yes" => PermissionResponse::allow_once(),
            _ => PermissionResponse::deny(),
        };
    }

    // Build option list: (value_index, label, hint)
    // 0 = allow_once, 1 = allow_always, 2..2+N = suggestions, last = deny
    let mut select = cliclack::select(format!(
        "⚠  {} wants to: {}",
        tool_name, description
    ));

    select = select.item(0_usize, "Allow once", "This invocation only");
    select = select.item(1_usize, "Allow always (this session)", "Remember until exit");

    for (i, s) in suggestions.iter().enumerate() {
        select = select.item(i + 2, &s.label, "Add permission rule");
    }

    let deny_idx = suggestions.len() + 2;
    select = select.item(deny_idx, "Deny", "Block this action");

    match select.interact() {
        Ok(idx) => {
            if idx == 0 {
                PermissionResponse::allow_once()
            } else if idx == 1 {
                PermissionResponse::allow_always()
            } else if idx == deny_idx {
                PermissionResponse::deny()
            } else {
                // Suggestion selected (idx - 2)
                let suggestion_idx = idx - 2;
                if let Some(s) = suggestions.get(suggestion_idx) {
                    PermissionResponse {
                        allowed: true,
                        persist: true,
                        feedback: None,
                        selected_suggestion: Some(suggestion_idx),
                        destination: Some(s.destination),
                    }
                } else {
                    PermissionResponse::deny()
                }
            }
        }
        Err(_) => {
            // User pressed Ctrl-C or terminal error → deny
            PermissionResponse::deny()
        }
    }
}
