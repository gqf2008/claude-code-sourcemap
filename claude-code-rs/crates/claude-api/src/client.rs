use std::pin::Pin;
use std::sync::Arc;
use anyhow::{Context, Result};
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::info;
use crate::provider::ApiBackend;
use crate::retry::{ApiHttpError, RetryConfig, with_retry};
use crate::types::*;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

pub struct ApiClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    max_tokens: u32,
    retry_config: RetryConfig,
    /// Optional pluggable backend. When set, `messages()` / `messages_stream()`
    /// delegate to this backend instead of the inline first-party implementation.
    backend: Option<Arc<dyn ApiBackend>>,
}

impl ApiClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_model: DEFAULT_MODEL.to_string(),
            max_tokens: 16384,
            retry_config: RetryConfig::default(),
            backend: None,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// Plug in a custom API backend (Bedrock, Vertex, OpenAI, etc.).
    ///
    /// When set, `messages()` and `messages_stream()` delegate to this backend
    /// with retry wrapping. The backend handles auth, URL, and model ID mapping.
    pub fn with_backend(mut self, backend: Box<dyn ApiBackend>) -> Self {
        self.backend = Some(Arc::from(backend));
        self
    }

    /// Returns the active provider name ("firstParty", "bedrock", "vertex").
    pub fn provider_name(&self) -> &str {
        match &self.backend {
            Some(b) => b.provider_name(),
            None => "firstParty",
        }
    }

    fn headers(&self) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|_| anyhow::anyhow!("Invalid API key format"))?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        // Enable prompt caching and extended thinking
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("prompt-caching-2024-07-31"),
        );
        Ok(headers)
    }

    /// Extract `Retry-After` header value (seconds) from response headers.
    fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
        headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// Quick connectivity check: send a minimal request to verify the API key
    /// and network. Returns Ok(model_name) on success, or an error describing
    /// the problem (auth, network, etc.).
    pub async fn test_connection(&self) -> Result<String> {
        let req = MessagesRequest {
            model: self.default_model.clone(),
            max_tokens: 1,
            messages: vec![ApiMessage {
                role: "user".into(),
                content: vec![ApiContentBlock::Text {
                    text: "hi".into(),
                    cache_control: None,
                }],
            }],
            stream: false,
            ..Default::default()
        };
        let resp = self.messages(&req).await?;
        Ok(resp.model)
    }

    /// Send a non-streaming messages request (with retry).
    pub async fn messages(&self, request: &MessagesRequest) -> Result<MessagesResponse> {
        // Delegate to pluggable backend if configured
        if let Some(ref backend) = self.backend {
            return backend.send_messages(&self.http, request).await;
        }

        let url = format!("{}/v1/messages", self.base_url);
        let request = request.clone();
        let headers = self.headers()?;

        with_retry(
            &self.retry_config,
            || {
                let url = url.clone();
                let request = request.clone();
                let http = self.http.clone();
                let headers = headers.clone();
                async move {
                    let response = http
                        .post(&url)
                        .headers(headers)
                        .json(&request)
                        .send()
                        .await
                        .map_err(|e| ApiHttpError {
                            status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                            body: format!("Request failed: {}", e),
                            retry_after: None,
                            rate_limit_info: None,
                        })?;

                    if !response.status().is_success() {
                        let status = response.status().as_u16();
                        let retry_after = Self::parse_retry_after(response.headers());
                        let body = response.text().await.unwrap_or_default();
                        return Err(ApiHttpError { status, body, retry_after, rate_limit_info: None });
                    }

                    response.json::<MessagesResponse>().await.map_err(|e| ApiHttpError {
                        status: 0,
                        body: format!("Failed to parse response: {}", e),
                        retry_after: None,
                        rate_limit_info: None,
                    })
                }
            },
            |attempt, status, delay| {
                info!(
                    "Retrying API request (attempt {}, status {}, wait {:.1}s)",
                    attempt, status, delay.as_secs_f64()
                );
            },
        )
        .await
    }

    /// Send a streaming messages request (with retry on initial connection).
    pub async fn messages_stream(
        &self,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        // Delegate to pluggable backend if configured
        if let Some(ref backend) = self.backend {
            return backend.send_messages_stream(&self.http, request).await;
        }

        let url = format!("{}/v1/messages", self.base_url);
        let mut req = request.clone();
        req.stream = true;
        let headers = self.headers()?;

        // Retry only the initial connection — once streaming starts, errors
        // propagate via the stream (mid-stream retries would lose partial state).
        let response = with_retry(
            &self.retry_config,
            || {
                let url = url.clone();
                let req = req.clone();
                let http = self.http.clone();
                let headers = headers.clone();
                async move {
                    let response = http
                        .post(&url)
                        .headers(headers)
                        .json(&req)
                        .send()
                        .await
                        .map_err(|e| ApiHttpError {
                            status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                            body: format!("Request failed: {}", e),
                            retry_after: None,
                            rate_limit_info: None,
                        })?;

                    if !response.status().is_success() {
                        let status = response.status().as_u16();
                        let retry_after = Self::parse_retry_after(response.headers());
                        let body = response.text().await.unwrap_or_default();
                        return Err(ApiHttpError { status, body, retry_after, rate_limit_info: None });
                    }

                    Ok(response)
                }
            },
            |attempt, status, delay| {
                info!(
                    "Retrying stream request (attempt {}, status {}, wait {:.1}s)",
                    attempt, status, delay.as_secs_f64()
                );
            },
        )
        .await
        .context("Failed to connect streaming request")?;

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let chunk_timeout = std::time::Duration::from_secs(90);

            loop {
                match tokio::time::timeout(chunk_timeout, byte_stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();
                            if let Some(event_result) = crate::stream::parse_sse_line(&line) {
                                yield event_result;
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e));
                        return;
                    }
                    Ok(None) => {
                        // Stream ended normally
                        break;
                    }
                    Err(_) => {
                        yield Err(anyhow::anyhow!("Stream stalled: no data received for {}s", chunk_timeout.as_secs()));
                        return;
                    }
                }
            }
            if !buffer.trim().is_empty() {
                if let Some(event_result) = crate::stream::parse_sse_line(&buffer) {
                    yield event_result;
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Convenience: build a MessagesRequest with defaults
    pub fn build_request(
        &self,
        messages: Vec<ApiMessage>,
        system: Option<Vec<SystemBlock>>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> MessagesRequest {
        MessagesRequest {
            model: self.default_model.clone(),
            max_tokens: self.max_tokens,
            messages,
            system,
            tools,
            stream: false,
            stop_sequences: None,
            temperature: None,
            top_p: None,
            thinking: None,
        }
    }

    /// Send a streaming request with automatic fallback to non-streaming.
    ///
    /// First attempts `messages_stream()`. If the stream encounters an idle
    /// timeout error, automatically retries the same request via `messages()`
    /// and synthesizes a one-shot stream from the non-streaming response.
    ///
    /// This mirrors the TS `createNonStreamingFallback` behavior.
    pub async fn messages_with_stream_fallback(
        &self,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        let stream = self.messages_stream(request).await?;
        let request = request.clone();
        let client = self.clone_for_fallback();

        let wrapper = async_stream::stream! {
            use futures::StreamExt;
            tokio::pin!(stream);
            let mut had_timeout = false;

            while let Some(item) = stream.next().await {
                match &item {
                    Err(e) if crate::stream::is_idle_timeout_error(e) => {
                        tracing::warn!("Stream idle timeout — falling back to non-streaming API");
                        had_timeout = true;
                        break;
                    }
                    _ => yield item,
                }
            }

            if had_timeout {
                let mut non_stream_request = request.clone();
                non_stream_request.stream = false;

                match client.messages(&non_stream_request).await {
                    Ok(response) => {
                        // Synthesize stream events from the non-streaming response
                        for event in synthesize_stream_events(response) {
                            yield Ok(event);
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Non-streaming fallback failed: {}", e));
                    }
                }
            }
        };

        Ok(Box::pin(wrapper))
    }

    /// Create a lightweight clone for fallback requests.
    ///
    /// Preserves the backend so fallback uses the same provider.
    fn clone_for_fallback(&self) -> ApiClient {
        ApiClient {
            http: self.http.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            max_tokens: self.max_tokens,
            retry_config: self.retry_config.clone(),
            backend: self.backend.clone(),
        }
    }
}

/// Convert a non-streaming `MessagesResponse` into synthetic `StreamEvent`s.
///
/// Produces the same event sequence a streaming response would:
/// `MessageStart → ContentBlockStart → ContentBlockDelta → ContentBlockStop → MessageDelta`
fn synthesize_stream_events(response: MessagesResponse) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    let stop_reason = response.stop_reason.clone();
    let content = response.content.clone();

    // MessageStart with usage (clone before consuming)
    events.push(StreamEvent::MessageStart {
        message: response,
    });

    // Content blocks
    for (idx, block) in content.iter().enumerate() {
        match block {
            ResponseContentBlock::Text { text } => {
                events.push(StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::Text { text: String::new() },
                });
                events.push(StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: DeltaBlock::TextDelta { text: text.clone() },
                });
                events.push(StreamEvent::ContentBlockStop { index: idx });
            }
            ResponseContentBlock::ToolUse { id, name, input } => {
                events.push(StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::Value::Object(Default::default()),
                    },
                });
                let json_str = serde_json::to_string(input).unwrap_or_default();
                events.push(StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: DeltaBlock::InputJsonDelta { partial_json: json_str },
                });
                events.push(StreamEvent::ContentBlockStop { index: idx });
            }
            ResponseContentBlock::Thinking { thinking } => {
                events.push(StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::Thinking { thinking: String::new() },
                });
                events.push(StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: DeltaBlock::ThinkingDelta { thinking: thinking.clone() },
                });
                events.push(StreamEvent::ContentBlockStop { index: idx });
            }
        }
    }

    // MessageDelta with stop reason
    events.push(StreamEvent::MessageDelta {
        delta: MessageDeltaData {
            stop_reason: Some(stop_reason.unwrap_or_else(|| "end_turn".to_string())),
        },
        usage: None,
    });

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_default_constructor() {
        let c = ApiClient::new("sk-test-key");
        assert_eq!(c.api_key, "sk-test-key");
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
        assert_eq!(c.default_model, DEFAULT_MODEL);
        assert_eq!(c.max_tokens, 16384);
    }

    #[test]
    fn client_builder_chain() {
        let c = ApiClient::new("key123")
            .with_base_url("https://custom.api.com")
            .with_model("claude-haiku-4-5")
            .with_max_tokens(4096)
            .with_retry_config(RetryConfig {
                max_retries: 5,
                ..RetryConfig::default()
            });
        assert_eq!(c.base_url, "https://custom.api.com");
        assert_eq!(c.default_model, "claude-haiku-4-5");
        assert_eq!(c.max_tokens, 4096);
        assert_eq!(c.retry_config.max_retries, 5);
    }

    #[test]
    fn client_headers() {
        let c = ApiClient::new("sk-ant-test");
        let headers = c.headers().unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "sk-ant-test");
        assert_eq!(headers.get("anthropic-version").unwrap(), API_VERSION);
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert!(headers.get("anthropic-beta").is_some());
    }

    #[test]
    fn client_build_request() {
        let c = ApiClient::new("key")
            .with_model("test-model")
            .with_max_tokens(8192);

        let req = c.build_request(vec![], None, None);
        assert_eq!(req.model, "test-model");
        assert_eq!(req.max_tokens, 8192);
        assert!(!req.stream);
        assert!(req.system.is_none());
        assert!(req.tools.is_none());
        assert!(req.messages.is_empty());
    }

    #[test]
    fn client_build_request_with_system() {
        let c = ApiClient::new("key");
        let system = vec![SystemBlock {
            block_type: "text".into(),
            text: "You are helpful.".into(),
            cache_control: None,
        }];
        let req = c.build_request(vec![], Some(system), None);
        assert!(req.system.is_some());
        let sys = req.system.unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0].text, "You are helpful.");
    }

    #[test]
    fn parse_retry_after_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("30"));
        assert_eq!(ApiClient::parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(ApiClient::parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_invalid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("not-a-number"));
        assert_eq!(ApiClient::parse_retry_after(&headers), None);
    }

    // ── synthesize_stream_events ─────────────────────────────────────────

    fn make_test_response(content: Vec<ResponseContentBlock>, stop_reason: Option<String>) -> MessagesResponse {
        MessagesResponse {
            id: "msg_test".into(),
            response_type: "message".into(),
            role: "assistant".into(),
            content,
            model: "claude-sonnet-4-6".into(),
            stop_reason,
            usage: ApiUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        }
    }

    #[test]
    fn synthesize_text_response() {
        let response = make_test_response(
            vec![ResponseContentBlock::Text { text: "Hello world".into() }],
            Some("end_turn".into()),
        );
        let events = synthesize_stream_events(response);
        // MessageStart + ContentBlockStart + Delta + Stop + MessageDelta = 5
        assert_eq!(events.len(), 5);
        assert!(matches!(&events[0], StreamEvent::MessageStart { .. }));
        assert!(matches!(&events[1], StreamEvent::ContentBlockStart { index: 0, .. }));
        match &events[2] {
            StreamEvent::ContentBlockDelta { delta: DeltaBlock::TextDelta { text }, .. } => {
                assert_eq!(text, "Hello world");
            }
            _ => panic!("expected TextDelta"),
        }
        assert!(matches!(&events[3], StreamEvent::ContentBlockStop { index: 0 }));
        match &events[4] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
            }
            _ => panic!("expected MessageDelta"),
        }
    }

    #[test]
    fn synthesize_tool_use_response() {
        let response = make_test_response(
            vec![ResponseContentBlock::ToolUse {
                id: "t1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }],
            Some("tool_use".into()),
        );
        let events = synthesize_stream_events(response);
        assert_eq!(events.len(), 5);
        match &events[1] {
            StreamEvent::ContentBlockStart { content_block: ResponseContentBlock::ToolUse { id, name, .. }, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "Bash");
            }
            _ => panic!("expected ToolUse start"),
        }
        match &events[2] {
            StreamEvent::ContentBlockDelta { delta: DeltaBlock::InputJsonDelta { partial_json }, .. } => {
                let parsed: serde_json::Value = serde_json::from_str(partial_json).unwrap();
                assert_eq!(parsed["command"], "ls");
            }
            _ => panic!("expected InputJsonDelta"),
        }
    }

    #[test]
    fn synthesize_multi_block_response() {
        let response = make_test_response(
            vec![
                ResponseContentBlock::Text { text: "I'll run that command.".into() },
                ResponseContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "FileRead".into(),
                    input: serde_json::json!({"path": "/tmp/test.txt"}),
                },
            ],
            Some("tool_use".into()),
        );
        let events = synthesize_stream_events(response);
        // MessageStart + 2*(Start+Delta+Stop) + MessageDelta = 1 + 6 + 1 = 8
        assert_eq!(events.len(), 8);
    }

    #[test]
    fn synthesize_default_stop_reason() {
        let response = make_test_response(
            vec![ResponseContentBlock::Text { text: "done".into() }],
            None, // no stop_reason
        );
        let events = synthesize_stream_events(response);
        match events.last() {
            Some(StreamEvent::MessageDelta { delta, .. }) => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
            }
            _ => panic!("expected MessageDelta"),
        }
    }

    #[test]
    fn synthesize_thinking_block() {
        let response = make_test_response(
            vec![ResponseContentBlock::Thinking { thinking: "let me think...".into() }],
            Some("end_turn".into()),
        );
        let events = synthesize_stream_events(response);
        assert_eq!(events.len(), 5);
        match &events[2] {
            StreamEvent::ContentBlockDelta { delta: DeltaBlock::ThinkingDelta { thinking }, .. } => {
                assert_eq!(thinking, "let me think...");
            }
            _ => panic!("expected ThinkingDelta"),
        }
    }

    #[test]
    fn clone_for_fallback_copies_fields() {
        let c = ApiClient::new("test-key")
            .with_base_url("https://custom.api.com")
            .with_model("test-model")
            .with_max_tokens(1024);
        let fallback = c.clone_for_fallback();
        assert_eq!(fallback.api_key, "test-key");
        assert_eq!(fallback.base_url, "https://custom.api.com");
        assert_eq!(fallback.default_model, "test-model");
        assert_eq!(fallback.max_tokens, 1024);
        assert!(fallback.backend.is_none()); // no backend set → None preserved
    }

    #[test]
    fn clone_for_fallback_preserves_backend() {
        use crate::provider::MockBackend;
        let backend = Box::new(MockBackend::new());
        let c = ApiClient::new("test-key").with_backend(backend);
        let fallback = c.clone_for_fallback();
        assert!(fallback.backend.is_some());
        assert_eq!(
            fallback.backend.as_ref().unwrap().provider_name(),
            "mock"
        );
    }
}
