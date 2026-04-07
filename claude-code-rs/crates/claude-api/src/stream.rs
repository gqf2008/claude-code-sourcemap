use crate::types::StreamEvent;
use anyhow::Result;
use std::pin::Pin;
use std::time::Duration;

/// Default idle timeout for SSE streams (90 seconds).
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Stall warning threshold — log a warning if no data received for this long.
pub const STALL_WARNING_THRESHOLD: Duration = Duration::from_secs(30);

/// Parse a single SSE data line into a StreamEvent.
///
/// Unknown event types are silently skipped (returns `None`) to ensure
/// compatibility with Anthropic-compatible APIs that may emit extra events.
pub fn parse_sse_line(line: &str) -> Option<Result<StreamEvent>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    // SSE spec: "data:" may or may not be followed by a space.
    // Standard Anthropic API uses "data: " (with space), but some compatible
    // APIs (e.g. DashScope) use "data:" (no space).
    let data = if let Some(d) = line.strip_prefix("data: ") {
        Some(d)
    } else {
        line.strip_prefix("data:")
    };
    if let Some(data) = data {
        if data.trim() == "[DONE]" {
            return None;
        }
        match serde_json::from_str::<StreamEvent>(data) {
            Ok(event) => Some(Ok(event)),
            Err(e) => {
                // If the event has a recognizable type field but unknown variant,
                // skip it gracefully rather than killing the stream.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if v.get("type").is_some() {
                        tracing::debug!("Skipping unknown SSE event type: {}", data.chars().take(120).collect::<String>());
                        return None;
                    }
                }
                Some(Err(anyhow::anyhow!("Failed to parse SSE: {}", e)))
            }
        }
    } else {
        None
    }
}

/// Configuration for the stream idle watchdog.
#[derive(Debug, Clone)]
pub struct StreamWatchdogConfig {
    /// Maximum time to wait for any event before declaring the stream stalled.
    pub idle_timeout: Duration,
    /// Threshold for logging a stall warning (shorter than timeout).
    pub stall_warning: Duration,
}

impl Default for StreamWatchdogConfig {
    fn default() -> Self {
        Self {
            idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            stall_warning: STALL_WARNING_THRESHOLD,
        }
    }
}

impl StreamWatchdogConfig {
    /// Create from environment variable `CLAUDE_STREAM_IDLE_TIMEOUT_MS`.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(ms) = std::env::var("CLAUDE_STREAM_IDLE_TIMEOUT_MS") {
            if let Ok(ms) = ms.parse::<u64>() {
                config.idle_timeout = Duration::from_millis(ms);
            }
        }
        config
    }
}

/// Wrap an SSE event stream with an idle timeout watchdog.
///
/// If no events are received within `config.idle_timeout`, the stream yields
/// an error and terminates. A warning is logged at the `stall_warning` threshold.
///
/// This mirrors the TS behavior in `createIdleWatchdog`:
/// - 90s default idle timeout
/// - 30s stall detection warning
/// - Stream terminates on timeout (caller can fallback to non-streaming)
pub fn with_idle_watchdog(
    inner: Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>,
    config: StreamWatchdogConfig,
) -> Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>> {
    use futures::StreamExt;

    let stream = async_stream::stream! {
        tokio::pin!(inner);
        let mut stall_warned = false;

        loop {
            tokio::select! {
                item = inner.next() => {
                    match item {
                        Some(event) => {
                            stall_warned = false;
                            yield event;
                        }
                        None => break, // stream ended normally
                    }
                }
                _ = tokio::time::sleep(config.idle_timeout) => {
                    tracing::error!(
                        timeout_secs = config.idle_timeout.as_secs(),
                        "Stream idle timeout — no events received"
                    );
                    yield Err(anyhow::anyhow!(
                        "Stream idle timeout: no events for {}s",
                        config.idle_timeout.as_secs()
                    ));
                    break;
                }
                _ = tokio::time::sleep(config.stall_warning), if !stall_warned => {
                    tracing::warn!(
                        threshold_secs = config.stall_warning.as_secs(),
                        "Stream may be stalling — no events received"
                    );
                    stall_warned = true;
                    // Don't yield anything, just log and continue waiting
                }
            }
        }
    };

    Box::pin(stream)
}

/// Whether a stream error indicates an idle timeout (and should trigger fallback).
pub fn is_idle_timeout_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("Stream idle timeout")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_empty_line() {
        assert!(parse_sse_line("").is_none());
        assert!(parse_sse_line("   ").is_none());
    }

    #[test]
    fn test_parse_sse_comment_line() {
        assert!(parse_sse_line(": this is a comment").is_none());
    }

    #[test]
    fn test_parse_sse_done() {
        assert!(parse_sse_line("data: [DONE]").is_none());
        // Also without space
        assert!(parse_sse_line("data:[DONE]").is_none());
    }

    #[test]
    fn test_parse_sse_valid_ping_event() {
        let result = parse_sse_line(r#"data: {"type":"ping"}"#);
        assert!(result.is_some());
        let event = result.unwrap().expect("should parse successfully");
        assert!(matches!(event, StreamEvent::Ping));
    }

    #[test]
    fn test_parse_sse_valid_ping_no_space() {
        // Some APIs (e.g. DashScope) send "data:" without trailing space
        let result = parse_sse_line(r#"data:{"type":"ping"}"#);
        assert!(result.is_some());
        let event = result.unwrap().expect("should parse successfully");
        assert!(matches!(event, StreamEvent::Ping));
    }

    #[test]
    fn test_parse_sse_invalid_json() {
        let result = parse_sse_line("data: {invalid");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn test_parse_sse_non_data_line() {
        assert!(parse_sse_line("event: ping").is_none());
        assert!(parse_sse_line("id: 123").is_none());
    }

    // ── Watchdog config tests ────────────────────────────────────────────

    #[test]
    fn watchdog_config_default() {
        let c = StreamWatchdogConfig::default();
        assert_eq!(c.idle_timeout, Duration::from_secs(90));
        assert_eq!(c.stall_warning, Duration::from_secs(30));
    }

    #[test]
    fn watchdog_config_custom() {
        let c = StreamWatchdogConfig {
            idle_timeout: Duration::from_secs(120),
            stall_warning: Duration::from_secs(60),
        };
        assert_eq!(c.idle_timeout.as_secs(), 120);
        assert_eq!(c.stall_warning.as_secs(), 60);
    }

    // ── Watchdog stream tests ────────────────────────────────────────────

    #[tokio::test]
    async fn watchdog_passes_through_events() {
        use futures::StreamExt;

        let events = vec![
            Ok(StreamEvent::Ping),
            Ok(StreamEvent::Ping),
        ];
        let inner: Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>> =
            Box::pin(futures::stream::iter(events));

        let config = StreamWatchdogConfig {
            idle_timeout: Duration::from_secs(10),
            stall_warning: Duration::from_secs(5),
        };
        let mut stream = with_idle_watchdog(inner, config);

        let e1 = stream.next().await.unwrap().unwrap();
        assert!(matches!(e1, StreamEvent::Ping));
        let e2 = stream.next().await.unwrap().unwrap();
        assert!(matches!(e2, StreamEvent::Ping));
        // Stream should end after events are consumed
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn watchdog_timeout_on_idle_stream() {
        use futures::StreamExt;

        // Create a stream that never produces events
        let inner: Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>> =
            Box::pin(futures::stream::pending());

        let config = StreamWatchdogConfig {
            idle_timeout: Duration::from_millis(50),
            stall_warning: Duration::from_millis(200), // longer than timeout to avoid stall warning
        };
        let mut stream = with_idle_watchdog(inner, config);

        let result = stream.next().await.unwrap();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("idle timeout"), "Got: {}", err_msg);
    }

    #[tokio::test]
    async fn watchdog_propagates_errors() {
        use futures::StreamExt;

        let events: Vec<Result<StreamEvent>> = vec![
            Err(anyhow::anyhow!("upstream error")),
        ];
        let inner: Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>> =
            Box::pin(futures::stream::iter(events));

        let config = StreamWatchdogConfig::default();
        let mut stream = with_idle_watchdog(inner, config);

        let result = stream.next().await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("upstream error"));
    }

    // ── is_idle_timeout_error ────────────────────────────────────────────

    #[test]
    fn idle_timeout_error_detected() {
        let err = anyhow::anyhow!("Stream idle timeout: no events for 90s");
        assert!(is_idle_timeout_error(&err));
    }

    #[test]
    fn non_timeout_error_not_detected() {
        let err = anyhow::anyhow!("Connection reset by peer");
        assert!(!is_idle_timeout_error(&err));
    }
}
