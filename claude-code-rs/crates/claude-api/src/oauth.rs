//! OAuth authentication stubs — placeholder for future implementation.
//!
//! Claude Code supports OAuth for enterprise SSO and third-party service
//! authentication.  This module provides trait definitions and configuration
//! types.  Full OAuth flow (PKCE, token refresh, device flow) is deferred.

use serde::{Deserialize, Serialize};

/// OAuth provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_url: String,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

/// Stored OAuth token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl OAuthToken {
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now >= expires_at
        } else {
            false
        }
    }
}

/// OAuth authentication flow (placeholder).
///
/// Future implementation will support:
/// - Authorization Code flow with PKCE
/// - Device Authorization Grant flow
/// - Token refresh
/// - Secure token storage in OS keychain
#[allow(dead_code)]
pub struct OAuthFlow {
    config: OAuthConfig,
}

#[allow(dead_code)]
impl OAuthFlow {
    pub fn new(config: OAuthConfig) -> Self {
        Self { config }
    }

    /// Start the authorization flow (stub).
    pub async fn authorize(&self) -> anyhow::Result<OAuthToken> {
        anyhow::bail!("OAuth flow not yet implemented. Use API key authentication.")
    }

    /// Refresh an expired token (stub).
    pub async fn refresh(&self, _token: &OAuthToken) -> anyhow::Result<OAuthToken> {
        anyhow::bail!("OAuth token refresh not yet implemented.")
    }
}
