use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Strip HTML tags and convert to basic markdown.
fn html_to_markdown(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut skip_content = false;
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            in_tag = true;
            tag_name.clear();
            continue;
        }
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag_lower = tag_name.to_lowercase();
                let tag_base = tag_lower.split_whitespace().next().unwrap_or("");

                // Skip script/style content
                match tag_base {
                    "script" | "style" | "noscript" => { skip_content = true; }
                    "/script" | "/style" | "/noscript" => { skip_content = false; }
                    _ => {}
                }

                if !skip_content {
                    match tag_base {
                        "br" | "br/" => result.push('\n'),
                        "p" | "/p" | "div" | "/div" | "section" | "/section" => {
                            if !result.ends_with('\n') { result.push('\n'); }
                            result.push('\n');
                        }
                        "h1" => result.push_str("\n# "),
                        "h2" => result.push_str("\n## "),
                        "h3" => result.push_str("\n### "),
                        "h4" => result.push_str("\n#### "),
                        "/h1" | "/h2" | "/h3" | "/h4" | "/h5" | "/h6" => {
                            result.push('\n');
                        }
                        "li" => result.push_str("\n- "),
                        "hr" | "hr/" => result.push_str("\n---\n"),
                        "strong" | "b" => result.push_str("**"),
                        "/strong" | "/b" => result.push_str("**"),
                        "em" | "i" => result.push('*'),
                        "/em" | "/i" => result.push('*'),
                        "code" => result.push('`'),
                        "/code" => result.push('`'),
                        "pre" => result.push_str("\n```\n"),
                        "/pre" => result.push_str("\n```\n"),
                        "blockquote" => result.push_str("\n> "),
                        _ => {}
                    }

                    // Extract href from <a> tags
                    if tag_base == "a" {
                        if let Some(href_start) = tag_lower.find("href=\"") {
                            let href_content = &tag_name[href_start + 6..];
                            if let Some(href_end) = href_content.find('"') {
                                let href = &href_content[..href_end];
                                result.push('[');
                                // We'll close with the /a tag below
                                let _ = href; // href captured for later
                            }
                        }
                    }
                }
            } else {
                tag_name.push(ch);
            }
            continue;
        }
        if skip_content { continue; }

        // Decode common HTML entities
        if ch == '&' {
            let mut entity = String::new();
            for next_ch in chars.by_ref() {
                if next_ch == ';' { break; }
                entity.push(next_ch);
                if entity.len() > 8 { break; }
            }
            match entity.as_str() {
                "amp" => result.push('&'),
                "lt" => result.push('<'),
                "gt" => result.push('>'),
                "quot" => result.push('"'),
                "apos" => result.push('\''),
                "nbsp" => result.push(' '),
                "mdash" => result.push('—'),
                "ndash" => result.push('–'),
                _ => {
                    result.push('&');
                    result.push_str(&entity);
                    result.push(';');
                }
            }
        } else {
            result.push(ch);
        }
    }

    // Collapse excessive whitespace
    claude_core::text_util::collapse_blank_lines(&result)
}

/// Try to extract the main content from an HTML page (heuristic).
fn extract_main_content(html: &str) -> String {
    // Try <article>, <main>, or <div role="main">
    let lower = html.to_lowercase();
    for tag in &["<article", "<main", "<div role=\"main\""] {
        if let Some(start) = lower.find(tag) {
            let content_start = html[start..].find('>').map(|i| start + i + 1).unwrap_or(start);
            let close_tag = match *tag {
                "<article" => "</article>",
                "<main" => "</main>",
                _ => "</div>",
            };
            if let Some(end) = lower[content_start..].find(close_tag) {
                return html_to_markdown(&html[content_start..content_start + end]);
            }
        }
    }
    // Fallback: try <body>
    if let Some(start) = lower.find("<body") {
        let content_start = html[start..].find('>').map(|i| start + i + 1).unwrap_or(start);
        if let Some(end) = lower[content_start..].find("</body>") {
            return html_to_markdown(&html[content_start..content_start + end]);
        }
    }
    // Last resort: convert entire thing
    html_to_markdown(html)
}

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "WebFetch" }
    fn category(&self) -> ToolCategory { ToolCategory::Web }
    fn description(&self) -> &str {
        "Fetch a URL and return its content. Converts HTML to readable markdown by default. \
         Set raw=true to get raw HTML. Set extract_main_content=true to extract the main \
         article/body content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to fetch" },
                "max_length": { "type": "integer", "description": "Max chars to return (default 5000, max 20000)" },
                "headers": {
                    "type": "object",
                    "description": "Custom HTTP headers",
                    "additionalProperties": { "type": "string" }
                },
                "raw": { "type": "boolean", "description": "Return raw HTML without markdown conversion" },
                "extract_main_content": { "type": "boolean", "description": "Extract main content only" },
                "timeout": { "type": "integer", "description": "Timeout in seconds (default 30)" }
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let url = input["url"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'url'"))?;
        let max_len = (input["max_length"].as_u64().unwrap_or(5000) as usize).min(20_000);
        let raw = input["raw"].as_bool().unwrap_or(false);
        let extract_main = input["extract_main_content"].as_bool().unwrap_or(false);
        let timeout_secs = input["timeout"].as_u64().unwrap_or(30);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .user_agent("Mozilla/5.0 (compatible; ClaudeCode/1.0)")
            .build()?;

        let mut req = client.get(url);

        // Custom headers
        if let Some(headers) = input["headers"].as_object() {
            for (k, v) in headers {
                if let Some(val) = v.as_str() {
                    req = req.header(k.as_str(), val);
                }
            }
        }

        let resp = req.send().await?;
        let status = resp.status();
        let content_type = resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await?;

        // Convert HTML to markdown unless raw mode or non-HTML content
        let is_html = content_type.contains("html") || body.trim_start().starts_with('<');
        let processed = if !raw && is_html {
            if extract_main {
                extract_main_content(&body)
            } else {
                html_to_markdown(&body)
            }
        } else {
            body
        };

        // Truncate
        let truncated = if processed.chars().count() > max_len {
            let s: String = processed.chars().take(max_len).collect();
            format!("{}...\n[Truncated at {}/{} chars]", s, max_len, processed.chars().count())
        } else {
            processed
        };

        if status.is_success() {
            Ok(ToolResult::text(truncated))
        } else {
            Ok(ToolResult::error(format!("HTTP {}: {}", status, truncated)))
        }
    }
}
