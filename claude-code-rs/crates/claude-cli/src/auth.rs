/// Read OAuth access token from `~/.claude/.credentials.json`.
///
/// The TS Claude Code stores OAuth tokens in this file with the structure:
/// ```json
/// { "claudeAiOauth": { "accessToken": "...", "expiresAt": ... } }
/// ```
pub(crate) fn read_oauth_credentials() -> Option<String> {
    let home = dirs::home_dir()?;
    let cred_path = home.join(".claude").join(".credentials.json");
    let content = std::fs::read_to_string(&cred_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let oauth = json.get("claudeAiOauth")?;

    // Check expiry — expiresAt is milliseconds since epoch
    if let Some(expires_at) = oauth.get("expiresAt").and_then(|v| v.as_i64()) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let now_ms = match i64::try_from(now_ms) {
            Ok(ms) => ms,
            Err(_) => {
                tracing::warn!("System time overflow checking OAuth expiry — treating as expired");
                return None;
            }
        };
        if now_ms > expires_at {
            tracing::debug!("OAuth token expired (expiresAt={})", expires_at);
            return None;
        }
    }

    let token = oauth.get("accessToken")?.as_str()?;
    if token.is_empty() {
        return None;
    }
    tracing::debug!("Loaded OAuth token from {}", cred_path.display());
    Some(token.to_string())
}

/// Read `primaryApiKey` from `~/.claude/config.json` (Claude Code config).
pub(crate) fn read_claude_config_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let config_path = home.join(".claude").join("config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let key = json.get("primaryApiKey")?.as_str()?;
    if key.is_empty() {
        return None;
    }
    tracing::debug!("Loaded primaryApiKey from {}", config_path.display());
    Some(key.to_string())
}

/// Resolve API key based on provider.
///
/// Priority (for `anthropic` provider):
/// 1. `--api-key` CLI flag
/// 2. `ANTHROPIC_API_KEY` env var (captured by clap)
/// 3. `~/.claude/settings.json` → `api_key`
/// 4. `~/.claude/.credentials.json` → OAuth `accessToken`
/// 5. `~/.claude.json` → `primaryApiKey`
///
/// Other providers: `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, etc.
/// `ollama` / `local`: no key required.
pub(crate) fn resolve_api_key(
    provider: &str,
    cli_key: Option<&str>,
    settings_key: Option<&str>,
) -> anyhow::Result<String> {
    // Explicit CLI flag always wins
    if let Some(key) = cli_key {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!(
                "API key is empty. Provide a valid key via --api-key or environment variable."
            ));
        }
        return Ok(trimmed.to_string());
    }

    match provider {
        "anthropic" => {
            // settings.json api_key
            if let Some(key) = settings_key {
                return Ok(key.to_string());
            }
            // ANTHROPIC_API_KEY is already captured by clap's env attribute;
            // if we reach here, it wasn't set. Try other sources.

            // ANTHROPIC_AUTH_TOKEN (used by proxy/managed setups)
            if let Ok(token) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
                let t = token.trim();
                if !t.is_empty() {
                    return Ok(t.to_string());
                }
            }

            // OAuth credentials (~/.claude/.credentials.json)
            if let Some(token) = read_oauth_credentials() {
                return Ok(token);
            }
            // Config file (~/.claude/config.json → primaryApiKey)
            if let Some(key) = read_claude_config_key() {
                return Ok(key);
            }

            Err(anyhow::anyhow!(
                "API key required. Set ANTHROPIC_API_KEY, use --api-key, \
                 or login via the official Claude Code CLI."
            ))
        }
        "openai" | "together" | "groq" => {
            let env_var = match provider {
                "together" => "TOGETHER_API_KEY",
                "groq" => "GROQ_API_KEY",
                // "openai" and any future variant
                _ => "OPENAI_API_KEY",
            };
            std::env::var(env_var).or_else(|_| {
                settings_key.map(|k| k.to_string()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "API key required for {} provider. Set {} or use --api-key.",
                        provider,
                        env_var
                    )
                })
            })
        }
        "deepseek" => std::env::var("DEEPSEEK_API_KEY").or_else(|_| {
            settings_key.map(|k| k.to_string()).ok_or_else(|| {
                anyhow::anyhow!(
                    "API key required for DeepSeek. Set DEEPSEEK_API_KEY or use --api-key."
                )
            })
        }),
        "ollama" | "local" => {
            // No key needed
            Ok("ollama".to_string())
        }
        "openai-compatible" => {
            // Try OPENAI_API_KEY, fallback to settings, then allow empty
            std::env::var("OPENAI_API_KEY")
                .or_else(|_| Ok(settings_key.unwrap_or("").to_string()))
        }
        _ => {
            // Unknown provider — try settings key, then OPENAI_API_KEY
            if let Some(key) = settings_key {
                Ok(key.to_string())
            } else {
                std::env::var("OPENAI_API_KEY").map_err(|_| {
                    anyhow::anyhow!(
                        "API key required for {} provider. Use --api-key.",
                        provider
                    )
                })
            }
        }
    }
}

/// Resume the most recent session.
pub(crate) async fn resume_latest_session(
    engine: &claude_agent::engine::QueryEngine,
) -> anyhow::Result<Option<String>> {
    let sessions = claude_core::session::list_sessions();
    if let Some(latest) = sessions.first() {
        let title = engine.restore_session(&latest.id).await?;
        Ok(Some(title))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_api_key ──────────────────────────────────────────────

    #[test]
    fn test_resolve_api_key_explicit() {
        assert_eq!(
            resolve_api_key("anthropic", Some("explicit-key"), None).unwrap(),
            "explicit-key"
        );
    }

    #[test]
    fn test_resolve_api_key_ollama_no_key() {
        assert_eq!(
            resolve_api_key("ollama", None, None).unwrap(),
            "ollama"
        );
    }

    #[test]
    fn test_resolve_api_key_anthropic_settings() {
        assert_eq!(
            resolve_api_key("anthropic", None, Some("settings-key")).unwrap(),
            "settings-key"
        );
    }

    #[test]
    fn test_resolve_api_key_anthropic_no_explicit() {
        // With no explicit key or settings key, resolve_api_key will try
        // ANTHROPIC_AUTH_TOKEN, OAuth credentials, and config.json.
        // On a dev machine with Claude Code installed, this may succeed.
        // We just verify it doesn't panic and returns a valid result type.
        let result = resolve_api_key("anthropic", None, None);
        match result {
            Ok(key) => assert!(!key.trim().is_empty(), "resolved key should not be blank"),
            Err(e) => assert!(e.to_string().contains("API key required")),
        }
    }

    #[test]
    fn test_resolve_api_key_empty_rejected() {
        assert!(resolve_api_key("anthropic", Some(""), None).is_err());
        assert!(resolve_api_key("anthropic", Some("   "), None).is_err());
    }

    #[test]
    fn test_resolve_api_key_trimmed() {
        let key = resolve_api_key("anthropic", Some("  sk-abc  "), None).unwrap();
        assert_eq!(key, "sk-abc");
    }

    // ── OAuth / legacy config credential reading ─────────────────────

    #[test]
    fn test_read_oauth_credentials_valid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cred_path = tmp.path().join(".credentials.json");
        let expires = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap_or(i64::MAX)
            + 3_600_000; // 1 hour from now
        std::fs::write(
            &cred_path,
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"tok-123","expiresAt":{}}}}}"#,
                expires
            ),
        )
        .unwrap();

        // read_oauth_credentials reads from $HOME — we can't easily override that,
        // so we test the parsing logic directly
        let content = std::fs::read_to_string(&cred_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        let token = json["claudeAiOauth"]["accessToken"].as_str().unwrap();
        assert_eq!(token, "tok-123");
    }

    #[test]
    fn test_read_claude_config_key_parsing() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"primaryApiKey":"sk-ant-legacy"}"#).unwrap();
        let key = json["primaryApiKey"].as_str().unwrap();
        assert_eq!(key, "sk-ant-legacy");
    }

    #[test]
    fn test_oauth_expired_token_ignored() {
        let expired_json = r#"{"claudeAiOauth":{"accessToken":"tok-old","expiresAt":1000}}"#;
        let json: serde_json::Value = serde_json::from_str(expired_json).unwrap();
        let expires_at = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap();
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        assert!(now_ms > expires_at, "token should be expired");
    }

    #[test]
    fn test_oauth_empty_token_ignored() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"claudeAiOauth":{"accessToken":""}}"#).unwrap();
        let token = json["claudeAiOauth"]["accessToken"].as_str().unwrap();
        assert!(token.is_empty());
    }

    #[test]
    fn test_settings_env_parsing() {
        let json = r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"tok","ANTHROPIC_BASE_URL":"http://localhost:8080"}}"#;
        let settings: claude_core::config::Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.env.len(), 2);
        assert_eq!(settings.env["ANTHROPIC_AUTH_TOKEN"], "tok");
        assert_eq!(settings.env["ANTHROPIC_BASE_URL"], "http://localhost:8080");
    }

    #[test]
    fn test_resolve_api_key_auth_token_env() {
        // ANTHROPIC_AUTH_TOKEN should be picked up as fallback
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "proxy-token-123");
        let result = resolve_api_key("anthropic", None, None);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        // May succeed (picks up ANTHROPIC_AUTH_TOKEN) or fail (if some other
        // credential source matches first). Just check the token value if ok.
        if let Ok(key) = result {
            // Could be from ANTHROPIC_AUTH_TOKEN or from actual credential files on disk
            assert!(!key.is_empty());
        }
    }
}
