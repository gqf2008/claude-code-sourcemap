use std::pin::Pin;
use anyhow::{Context, Result};
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::info;
use crate::retry::{ApiHttpError, RetryConfig, with_retry};
use crate::types::*;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    max_tokens: u32,
    retry_config: RetryConfig,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_model: DEFAULT_MODEL.to_string(),
            max_tokens: 16384,
            retry_config: RetryConfig::default(),
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

    /// Send a non-streaming messages request (with retry).
    pub async fn messages(&self, request: &MessagesRequest) -> Result<MessagesResponse> {
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
                        })?;

                    if !response.status().is_success() {
                        let status = response.status().as_u16();
                        let retry_after = Self::parse_retry_after(response.headers());
                        let body = response.text().await.unwrap_or_default();
                        return Err(ApiHttpError { status, body, retry_after });
                    }

                    response.json::<MessagesResponse>().await.map_err(|e| ApiHttpError {
                        status: 0,
                        body: format!("Failed to parse response: {}", e),
                        retry_after: None,
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
                        })?;

                    if !response.status().is_success() {
                        let status = response.status().as_u16();
                        let retry_after = Self::parse_retry_after(response.headers());
                        let body = response.text().await.unwrap_or_default();
                        return Err(ApiHttpError { status, body, retry_after });
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
}
