use crate::types::StreamEvent;
use anyhow::Result;

/// Parse a single SSE data line into a StreamEvent
pub fn parse_sse_line(line: &str) -> Option<Result<StreamEvent>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return None;
        }
        Some(
            serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Failed to parse SSE: {}", e)),
        )
    } else {
        None
    }
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
    }

    #[test]
    fn test_parse_sse_valid_ping_event() {
        let result = parse_sse_line(r#"data: {"type":"ping"}"#);
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
}
