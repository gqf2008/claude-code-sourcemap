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
