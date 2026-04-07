//! OpenAI-compatible API backend.
//!
//! Translates Anthropic Messages API format ↔ OpenAI Chat Completions format,
//! enabling use of any OpenAI-compatible endpoint (OpenAI, DeepSeek, Ollama,
//! vLLM, LiteLLM, etc.) through the existing [`ApiBackend`] trait.
//!
//! # Format Mapping
//!
//! | Anthropic | OpenAI |
//! |-----------|--------|
//! | `system` blocks (top-level) | `messages[0]` with `role: "system"` |
//! | `content: [{ type: "text" }]` | `content: "string"` or `parts` array |
//! | `tool_use` content block | `tool_calls` on assistant message |
//! | `tool_result` content block | `role: "tool"` message |
//! | `stop_reason: "end_turn"` | `finish_reason: "stop"` |
//! | `stop_reason: "tool_use"` | `finish_reason: "tool_calls"` |
//! | SSE `message_start` / `content_block_delta` | SSE `chat.completion.chunk` |

mod types;
mod translate;
#[cfg(test)]
mod tests;

use std::pin::Pin;

use anyhow::{Context, Result};
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tracing::{debug, warn};

use crate::provider::ApiBackend;
use crate::types::*;
use translate::{to_openai_request, from_openai_response, OpenAIStreamState};
use types::{ChatCompletionChunk, ChatCompletionResponse};
// ── OpenAI-Compatible Backend ────────────────────────────────────────────────

/// Backend for any OpenAI Chat Completions–compatible API.
///
/// Works with OpenAI, DeepSeek, Ollama, vLLM, LiteLLM, Together AI,
/// Groq, and any other provider implementing the Chat Completions format.
///
/// # Usage
///
/// ```no_run
/// use claude_api::openai::OpenAIBackend;
///
/// // OpenAI
/// let backend = OpenAIBackend::new("sk-...", "https://api.openai.com");
///
/// // Ollama (local)
/// let backend = OpenAIBackend::new("ollama", "http://localhost:11434");
///
/// // DeepSeek
/// let backend = OpenAIBackend::new("sk-...", "https://api.deepseek.com");
/// ```
pub struct OpenAIBackend {
    api_key: String,
    base_url: String,
    /// Provider name for display (e.g. "openai", "deepseek", "ollama").
    provider: String,
}

impl OpenAIBackend {
    /// Create a new OpenAI-compatible backend.
    ///
    /// `base_url` should be the root URL without `/v1/chat/completions`.
    /// If a URL ending in `/v1` is provided, the suffix is automatically stripped
    /// to avoid double-prefixing (e.g. `https://example.com/v1` → `https://example.com`).
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        // Strip trailing /v1 to prevent double-path: /v1/v1/chat/completions
        let trimmed = url.trim_end_matches('/');
        if let Some(prefix) = trimmed.strip_suffix("/v1") {
            url = prefix.to_string();
        }
        Self {
            api_key: api_key.into(),
            base_url: url,
            provider: "openai".into(),
        }
    }

    /// Set a custom provider name for display purposes.
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider = name.into();
        self
    }

    /// Detect provider from base URL and set appropriate name.
    pub fn auto_detect_provider(mut self) -> Self {
        let url = self.base_url.to_lowercase();
        self.provider = if url.contains("openai.com") {
            "openai".into()
        } else if url.contains("deepseek.com") {
            "deepseek".into()
        } else if url.contains("localhost") || url.contains("127.0.0.1") {
            "local".into()
        } else if url.contains("together") {
            "together".into()
        } else if url.contains("groq") {
            "groq".into()
        } else {
            "openai-compatible".into()
        };
        self
    }
}

#[async_trait::async_trait]
impl ApiBackend for OpenAIBackend {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Skip auth header for local providers (Ollama doesn't need it)
        if !self.api_key.is_empty()
            && self.api_key != "ollama"
            && self.api_key != "local"
        {
            let auth_value = format!("Bearer {}", self.api_key);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_value)
                    .map_err(|_| anyhow::anyhow!("Invalid API key format"))?,
            );
        }

        Ok(headers)
    }

    fn map_model_id(&self, canonical: &str) -> String {
        // Map Anthropic canonical model names to common defaults per provider.
        // Users should override with --model for specific provider models.
        if canonical.starts_with("claude-") {
            warn!(
                provider = %self.provider,
                model = canonical,
                "Anthropic model name passed to {} provider; override with --model",
                self.provider
            );
        }
        canonical.to_string()
    }

    async fn send_messages(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<MessagesResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.headers()?;

        let openai_req = to_openai_request(request);
        debug!(
            provider = self.provider,
            model = %openai_req.model,
            "Sending chat completion request"
        );

        let response = http
            .post(&url)
            .headers(headers)
            .json(&openai_req)
            .send()
            .await
            .context("OpenAI-compatible request failed")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {} ({}): {}", status, self.provider, body);
        }

        let openai_resp: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI-compatible response")?;

        Ok(from_openai_response(openai_resp))
    }

    async fn send_messages_stream(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.headers()?;

        let mut openai_req = to_openai_request(request);
        openai_req.stream = true;

        let model = openai_req.model.clone();
        debug!(
            provider = self.provider,
            model = %model,
            "Sending streaming chat completion request"
        );

        let response = http
            .post(&url)
            .headers(headers)
            .json(&openai_req)
            .send()
            .await
            .context("OpenAI-compatible stream request failed")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Stream API error {} ({}): {}", status, self.provider, body);
        }

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut state = OpenAIStreamState::new(&model);
            let mut got_done = false;

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if line.is_empty() || line == ":" {
                                continue;
                            }

                            // Handle SSE format: "data: {...}"
                            let data = if let Some(stripped) = line.strip_prefix("data: ") {
                                stripped
                            } else if let Some(stripped) = line.strip_prefix("data:") {
                                stripped
                            } else {
                                continue;
                            };

                            // "[DONE]" signals end of stream
                            if data.trim() == "[DONE]" {
                                got_done = true;
                                break;
                            }

                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => {
                                    let events = state.process_chunk(&chunk);
                                    for event in events {
                                        yield Ok(event);
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        provider = "openai",
                                        error = %e,
                                        line = %data,
                                        "Failed to parse streaming chunk"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e));
                        return;
                    }
                }
            }

            // If the stream ended without a proper finish_reason, synthesize closing events
            if state.message_started && !got_done {
                let closing = state.finalize();
                for event in closing {
                    yield Ok(event);
                }
            }
        };

        Ok(Box::pin(stream))
    }
}