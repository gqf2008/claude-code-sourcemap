//! API backend trait — abstraction over Anthropic, Bedrock, Vertex, and Foundry.
//!
//! Each backend knows how to:
//! - Construct the correct base URL and authentication headers
//! - Map canonical model IDs to provider-specific format
//! - Send messages (streaming and non-streaming)
//!
//! The [`AnthropicClient`](crate::client::AnthropicClient) accepts any backend
//! via [`with_backend`](crate::client::AnthropicClient::with_backend).

use std::pin::Pin;

use anyhow::Result;
use futures::Stream;
use reqwest::header::HeaderMap;

use crate::types::{MessagesRequest, MessagesResponse, StreamEvent};

// ── Trait ────────────────────────────────────────────────────────────────────

/// A backend that can send messages to a Claude-compatible API.
///
/// Implementors handle provider-specific concerns: base URL, auth headers,
/// model ID mapping, and any custom request transformations.
#[async_trait::async_trait]
pub trait ApiBackend: Send + Sync {
    /// Human-readable provider name (e.g. "firstParty", "bedrock", "vertex").
    fn provider_name(&self) -> &str;

    /// Base URL for the messages endpoint (e.g. `https://api.anthropic.com`).
    fn base_url(&self) -> &str;

    /// Build provider-specific HTTP headers (auth, version, beta flags).
    fn headers(&self) -> Result<HeaderMap>;

    /// Map a canonical model ID to the provider-specific format.
    ///
    /// For first-party, this is identity. For Bedrock, it adds the ARN prefix.
    /// For Vertex, it uses `@` separator.
    fn map_model_id(&self, canonical: &str) -> String;

    /// Send a non-streaming messages request.
    ///
    /// Default implementation uses `reqwest` with the provider's headers and URL.
    /// Override for providers that need custom request signing (e.g. AWS SigV4).
    async fn send_messages(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<MessagesResponse>;

    /// Send a streaming messages request, returning an SSE event stream.
    ///
    /// Default implementation uses `reqwest` with the provider's headers and URL.
    async fn send_messages_stream(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;
}

// ── First-party backend ──────────────────────────────────────────────────────

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Direct Anthropic API backend (api.anthropic.com).
pub struct FirstPartyBackend {
    api_key: String,
    base_url: String,
}

impl FirstPartyBackend {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[async_trait::async_trait]
impl ApiBackend for FirstPartyBackend {
    fn provider_name(&self) -> &str {
        "firstParty"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn headers(&self) -> Result<HeaderMap> {
        use reqwest::header::{HeaderValue, CONTENT_TYPE};

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|_| anyhow::anyhow!("Invalid API key format"))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("prompt-caching-2024-07-31"),
        );
        Ok(headers)
    }

    fn map_model_id(&self, canonical: &str) -> String {
        canonical.to_string()
    }

    async fn send_messages(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<MessagesResponse> {
        let url = format!("{}/v1/messages", self.base_url);
        let headers = self.headers()?;

        let response = http
            .post(&url)
            .headers(headers)
            .json(request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        response
            .json::<MessagesResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {}", e))
    }

    async fn send_messages_stream(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let url = format!("{}/v1/messages", self.base_url);
        let mut req = request.clone();
        req.stream = true;
        let headers = self.headers()?;

        let response = http
            .post(&url)
            .headers(headers)
            .json(&req)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Stream request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Stream API error {}: {}", status, body);
        }

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();
                            if let Some(event_result) = crate::stream::parse_sse_line(&line) {
                                yield event_result;
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e));
                        return;
                    }
                }
            }
            // Flush remaining buffer
            if !buffer.trim().is_empty() {
                if let Some(event_result) = crate::stream::parse_sse_line(&buffer) {
                    yield event_result;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

// ── Bedrock backend (stub) ───────────────────────────────────────────────────

/// AWS Bedrock backend — uses AWS SigV4 auth and ARN-format model IDs.
///
/// This is a structural stub: model ID mapping is complete, but actual
/// AWS credential resolution and SigV4 signing are not yet implemented.
/// The `send_messages` / `send_messages_stream` methods will return errors
/// until AWS auth is wired up.
pub struct BedrockBackend {
    base_url: String,
}

impl BedrockBackend {
    pub fn new(region: impl Into<String>) -> Self {
        let base_url = format!("https://bedrock-runtime.{}.amazonaws.com", region.into());
        Self { base_url }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[async_trait::async_trait]
impl ApiBackend for BedrockBackend {
    fn provider_name(&self) -> &str {
        "bedrock"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn headers(&self) -> Result<HeaderMap> {
        // AWS SigV4 signing would happen here — stub returns empty headers
        // Real implementation needs: aws-sigv4 crate, credential chain
        Ok(HeaderMap::new())
    }

    fn map_model_id(&self, canonical: &str) -> String {
        // Delegate to core's model_for_provider for ARN-format IDs
        claude_core::model::model_for_provider(canonical, claude_core::model::ApiProvider::Bedrock)
    }

    async fn send_messages(
        &self,
        _http: &reqwest::Client,
        _request: &MessagesRequest,
    ) -> Result<MessagesResponse> {
        anyhow::bail!(
            "Bedrock backend not yet implemented: AWS SigV4 signing required. \
             Set ANTHROPIC_API_KEY and use first-party backend instead."
        )
    }

    async fn send_messages_stream(
        &self,
        _http: &reqwest::Client,
        _request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        anyhow::bail!(
            "Bedrock streaming not yet implemented: AWS SigV4 signing required."
        )
    }
}

// ── Vertex backend (stub) ────────────────────────────────────────────────────

/// Google Vertex AI backend — uses GCP auth and `@`-separator model IDs.
///
/// Structural stub: model ID mapping is complete, but GCP credential
/// resolution is not yet implemented.
pub struct VertexBackend {
    project_id: String,
    region: String,
}

impl VertexBackend {
    pub fn new(project_id: impl Into<String>, region: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            region: region.into(),
        }
    }
}

#[async_trait::async_trait]
impl ApiBackend for VertexBackend {
    fn provider_name(&self) -> &str {
        "vertex"
    }

    fn base_url(&self) -> &str {
        "https://us-central1-aiplatform.googleapis.com"
    }

    fn headers(&self) -> Result<HeaderMap> {
        // GCP OAuth2 token would be injected here
        Ok(HeaderMap::new())
    }

    fn map_model_id(&self, canonical: &str) -> String {
        claude_core::model::model_for_provider(canonical, claude_core::model::ApiProvider::Vertex)
    }

    async fn send_messages(
        &self,
        _http: &reqwest::Client,
        _request: &MessagesRequest,
    ) -> Result<MessagesResponse> {
        anyhow::bail!(
            "Vertex backend not yet implemented: GCP auth required. \
             Project: {}, Region: {}",
            self.project_id,
            self.region
        )
    }

    async fn send_messages_stream(
        &self,
        _http: &reqwest::Client,
        _request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        anyhow::bail!("Vertex streaming not yet implemented: GCP auth required.")
    }
}

// ── Backend factory ──────────────────────────────────────────────────────────

/// Detect the API backend from environment variables (mirrors TS `getAPIProvider`).
///
/// Priority: Bedrock → Vertex → FirstParty.
/// - `CLAUDE_CODE_USE_BEDROCK=1` → Bedrock
/// - `CLAUDE_CODE_USE_VERTEX=1` → Vertex
/// - Otherwise → FirstParty (requires `ANTHROPIC_API_KEY`)
pub fn detect_backend(api_key: &str) -> Box<dyn ApiBackend> {
    let is_truthy = |var: &str| -> bool {
        std::env::var(var)
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "TRUE" | "YES"))
            .unwrap_or(false)
    };

    if is_truthy("CLAUDE_CODE_USE_BEDROCK") {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        let mut backend = BedrockBackend::new(region);
        if let Ok(url) = std::env::var("ANTHROPIC_BEDROCK_BASE_URL") {
            backend = backend.with_base_url(url);
        }
        Box::new(backend)
    } else if is_truthy("CLAUDE_CODE_USE_VERTEX") {
        let project = std::env::var("ANTHROPIC_VERTEX_PROJECT_ID")
            .unwrap_or_else(|_| "unknown-project".to_string());
        let region = std::env::var("CLOUD_ML_REGION")
            .unwrap_or_else(|_| "us-central1".to_string());
        Box::new(VertexBackend::new(project, region))
    } else {
        let mut backend = FirstPartyBackend::new(api_key);
        if let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") {
            backend = backend.with_base_url(url);
        }
        Box::new(backend)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_party_provider_name() {
        let b = FirstPartyBackend::new("sk-test");
        assert_eq!(b.provider_name(), "firstParty");
    }

    #[test]
    fn first_party_base_url_default() {
        let b = FirstPartyBackend::new("key");
        assert_eq!(b.base_url(), "https://api.anthropic.com");
    }

    #[test]
    fn first_party_base_url_custom() {
        let b = FirstPartyBackend::new("key").with_base_url("https://proxy.example.com");
        assert_eq!(b.base_url(), "https://proxy.example.com");
    }

    #[test]
    fn first_party_headers_contain_required() {
        let b = FirstPartyBackend::new("sk-ant-test123");
        let h = b.headers().unwrap();
        assert_eq!(h.get("x-api-key").unwrap(), "sk-ant-test123");
        assert_eq!(h.get("anthropic-version").unwrap(), API_VERSION);
        assert!(h.get("content-type").is_some());
        assert!(h.get("anthropic-beta").is_some());
    }

    #[test]
    fn first_party_model_id_passthrough() {
        let b = FirstPartyBackend::new("key");
        assert_eq!(b.map_model_id("claude-sonnet-4"), "claude-sonnet-4");
        assert_eq!(b.map_model_id("custom-model"), "custom-model");
    }

    #[test]
    fn bedrock_provider_name() {
        let b = BedrockBackend::new("us-east-1");
        assert_eq!(b.provider_name(), "bedrock");
    }

    #[test]
    fn bedrock_model_id_mapping() {
        let b = BedrockBackend::new("us-west-2");
        let mapped = b.map_model_id("claude-sonnet-4");
        assert!(mapped.contains("anthropic"));
        assert!(mapped.contains("v1:0") || mapped.contains("sonnet"));
    }

    #[test]
    fn bedrock_base_url_default() {
        let b = BedrockBackend::new("eu-west-1");
        assert_eq!(
            b.base_url(),
            "https://bedrock-runtime.eu-west-1.amazonaws.com"
        );
    }

    #[test]
    fn bedrock_base_url_custom() {
        let b = BedrockBackend::new("us-east-1")
            .with_base_url("https://custom-bedrock.example.com");
        assert_eq!(b.base_url(), "https://custom-bedrock.example.com");
    }

    #[test]
    fn vertex_provider_name() {
        let b = VertexBackend::new("my-project", "us-central1");
        assert_eq!(b.provider_name(), "vertex");
    }

    #[test]
    fn vertex_model_id_mapping() {
        let b = VertexBackend::new("proj", "region");
        let mapped = b.map_model_id("claude-opus-4-6");
        // Vertex format: model name (may differ from canonical)
        assert!(!mapped.is_empty());
    }

    #[test]
    fn detect_backend_defaults_to_first_party() {
        // In test environment, no CLAUDE_CODE_USE_* vars should be set
        let b = detect_backend("test-key");
        assert_eq!(b.provider_name(), "firstParty");
    }

    #[test]
    fn api_backend_is_object_safe() {
        // Verify the trait can be used as dyn
        fn _takes_backend(_b: &dyn ApiBackend) {}
        let b = FirstPartyBackend::new("key");
        _takes_backend(&b);
    }
}
