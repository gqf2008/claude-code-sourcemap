//! OAuth authentication — PKCE Authorization Code flow.
//!
//! Claude Code supports OAuth for enterprise SSO and third-party service
//! authentication.  This module implements the full Authorization Code flow
//! with PKCE (Proof Key for Code Exchange), local redirect server for
//! capturing the auth code, token exchange, refresh, and file-based storage.

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

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
    #[must_use] 
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

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

/// Generate a random code verifier (43-128 chars, URL-safe).
fn generate_code_verifier() -> String {
    let bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

/// Derive the S256 code challenge from a verifier.
fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

// ---------------------------------------------------------------------------
// OAuthFlow
// ---------------------------------------------------------------------------

/// OAuth Authorization Code flow with PKCE.
pub struct OAuthFlow {
    config: OAuthConfig,
}

impl OAuthFlow {
    #[must_use] 
    pub const fn new(config: OAuthConfig) -> Self {
        Self { config }
    }

    /// Build the authorization URL the user should open in a browser.
    /// Returns `(url, code_verifier)`.
    #[must_use] 
    pub fn build_auth_url(&self) -> (String, String) {
        let verifier = generate_code_verifier();
        let challenge = code_challenge(&verifier);

        let redirect = self.redirect_uri();
        let scope = self.config.scopes.join(" ");
        let state = generate_code_verifier(); // reuse random generator for state

        let url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&scope={}&state={}",
            self.config.auth_url,
            urlencoding::encode(&self.config.client_id),
            urlencoding::encode(&redirect),
            urlencoding::encode(&challenge),
            urlencoding::encode(&scope),
            urlencoding::encode(&state),
        );

        (url, verifier)
    }

    /// Exchange an authorization code for tokens.
    pub async fn exchange_code(&self, code: &str, verifier: &str) -> anyhow::Result<OAuthToken> {
        let redirect = self.redirect_uri();

        let client = reqwest::Client::new();
        let resp = client
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &self.config.client_id),
                ("code", code),
                ("redirect_uri", &redirect),
                ("code_verifier", verifier),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed: {body}");
        }

        let token_resp: TokenResponse = resp.json().await?;
        Ok(token_resp.into_token())
    }

    /// Refresh an expired token.
    pub async fn refresh(&self, token: &OAuthToken) -> anyhow::Result<OAuthToken> {
        let refresh_token = token.refresh_token.as_deref()
            .ok_or_else(|| anyhow::anyhow!("No refresh token available"))?;

        let client = reqwest::Client::new();
        let resp = client
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &self.config.client_id),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: {body}");
        }

        let token_resp: TokenResponse = resp.json().await?;
        Ok(token_resp.into_token())
    }

    /// Full interactive authorize: open browser, start local callback server,
    /// wait for redirect, exchange code. Returns the obtained token.
    pub async fn authorize(&self) -> anyhow::Result<OAuthToken> {
        let (url, verifier) = self.build_auth_url();

        // Start a local TCP listener on the redirect port
        let redirect = self.redirect_uri();
        let port = extract_port(&redirect).unwrap_or(19485);
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;

        // Open browser
        if opener::open(&url).is_err() {
            eprintln!("Please open the following URL in your browser:\n  {url}");
        }

        // Wait for the callback (timeout after 5 min)
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            wait_for_code(&listener),
        )
        .await
        .map_err(|_| anyhow::anyhow!("OAuth authorization timed out (5 min)"))??;

        self.exchange_code(&code, &verifier).await
    }

    fn redirect_uri(&self) -> String {
        self.config
            .redirect_uri
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:19485/callback".to_string())
    }
}

// ---------------------------------------------------------------------------
// Token storage (file-based)
// ---------------------------------------------------------------------------

/// Save token to `~/.claude/oauth_token.json`.
pub fn save_token(token: &OAuthToken) -> anyhow::Result<()> {
    let path = token_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(token)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Load token from `~/.claude/oauth_token.json`.
pub fn load_token() -> anyhow::Result<Option<OAuthToken>> {
    let path = token_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)?;
    let token: OAuthToken = serde_json::from_str(&json)?;
    Ok(Some(token))
}

fn token_path() -> anyhow::Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".claude").join("oauth_token.json"))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

impl TokenResponse {
    fn into_token(self) -> OAuthToken {
        let expires_at = self.expires_in.map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs
        });
        OAuthToken {
            access_token: self.access_token,
            token_type: self.token_type,
            refresh_token: self.refresh_token,
            expires_at,
            scopes: self.scope.map(|s| s.split(' ').map(String::from).collect()).unwrap_or_default(),
        }
    }
}

fn extract_port(uri: &str) -> Option<u16> {
    uri.split("://").nth(1)?
        .split('/').next()?
        .split(':').nth(1)?
        .parse().ok()
}

/// Wait for a single HTTP GET on the listener, extract the `code` query param,
/// and respond with a success page.
async fn wait_for_code(listener: &tokio::net::TcpListener) -> anyhow::Result<String> {
    let (mut stream, _) = listener.accept().await?;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse: GET /callback?code=ABC&state=XYZ HTTP/1.1
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let code = path
        .split('?')
        .nth(1)
        .and_then(|qs| {
            qs.split('&').find_map(|pair| {
                let (key, val) = pair.split_once('=')?;
                
                
                if key == "code" { Some(val.to_string()) } else { None }
            })
        })
        .ok_or_else(|| anyhow::anyhow!("No 'code' parameter in OAuth callback"))?;

    let body = "<html><body><h2>✓ Authorization successful!</h2><p>You can close this tab.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;

    Ok(code)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_verifier_length() {
        let v = generate_code_verifier();
        assert!(v.len() >= 43, "Verifier should be at least 43 chars");
    }

    #[test]
    fn test_pkce_challenge_deterministic() {
        let c1 = code_challenge("test-verifier");
        let c2 = code_challenge("test-verifier");
        assert_eq!(c1, c2);
        assert!(!c1.is_empty());
    }

    #[test]
    fn test_build_auth_url() {
        let config = OAuthConfig {
            client_id: "my-client".into(),
            auth_url: "https://auth.example.com/authorize".into(),
            token_url: "https://auth.example.com/token".into(),
            scopes: vec!["read".into(), "write".into()],
            redirect_uri: None,
        };
        let flow = OAuthFlow::new(config);
        let (url, verifier) = flow.build_auth_url();
        assert!(url.starts_with("https://auth.example.com/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!verifier.is_empty());
    }

    #[test]
    fn test_extract_port() {
        assert_eq!(extract_port("http://127.0.0.1:19485/callback"), Some(19485));
        assert_eq!(extract_port("http://localhost:8080/"), Some(8080));
        assert_eq!(extract_port("http://localhost/"), None);
    }

    #[test]
    fn test_token_expiry() {
        let token = OAuthToken {
            access_token: "test".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_at: Some(0),
            scopes: vec![],
        };
        assert!(token.is_expired());

        let future = OAuthToken {
            expires_at: Some(u64::MAX),
            ..token.clone()
        };
        assert!(!future.is_expired());
    }

    #[test]
    fn test_token_response_into_token() {
        let resp = TokenResponse {
            access_token: "abc".into(),
            token_type: "Bearer".into(),
            refresh_token: Some("refresh".into()),
            expires_in: Some(3600),
            scope: Some("read write".into()),
        };
        let token = resp.into_token();
        assert_eq!(token.access_token, "abc");
        assert!(token.expires_at.is_some());
        assert_eq!(token.scopes, vec!["read", "write"]);
    }
}
