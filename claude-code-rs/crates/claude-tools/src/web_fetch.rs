use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::net::IpAddr;

/// Validate a URL for SSRF protection.
///
/// Blocks: file://, ftp://, private/link-local IPs, localhost, metadata endpoints.
fn validate_url(url: &str) -> Result<(), String> {
    // Only allow http and https schemes
    let lower = url.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(format!("Blocked URL scheme — only http:// and https:// are allowed: {url}"));
    }

    // Extract host from URL: scheme://[user@]host[:port]/path
    let after_scheme = if lower.starts_with("https://") { &url[8..] } else { &url[7..] };
    let host_port = after_scheme.split('/').next().unwrap_or("");
    // Strip user@ if present
    let host_port = host_port.rsplit('@').next().unwrap_or(host_port);
    // Strip port
    let host = if host_port.starts_with('[') {
        // IPv6: [::1]:8080
        host_port.trim_start_matches('[').split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };

    if host.is_empty() {
        return Err("Invalid URL — no host specified".to_string());
    }

    // Block localhost and common internal hostnames
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".local") || host_lower.ends_with(".internal") {
        return Err(format!("Blocked URL — cannot fetch localhost/internal hosts: {host}"));
    }

    // Block cloud metadata endpoints
    if host == "169.254.169.254" || host_lower == "metadata.google.internal" {
        return Err("Blocked URL — cloud metadata endpoint".to_string());
    }

    // Block private/link-local IP ranges
    if let Ok(ip) = host.parse::<IpAddr>() {
        let is_private = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()             // 127.0.0.0/8
                || v4.is_private()           // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()        // 169.254.0.0/16
                || v4.is_unspecified()       // 0.0.0.0
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64.0.0/10 (CGNAT)
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()             // ::1
                || v6.is_unspecified()       // ::
                || v6.segments()[0] == 0xfe80 // link-local
                || v6.segments()[0] == 0xfc00 || v6.segments()[0] == 0xfd00 // ULA
            }
        };
        if is_private {
            return Err(format!("Blocked URL — cannot fetch private/internal IPs: {ip}"));
        }
    }

    Ok(())
}

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
            let content_start = html[start..].find('>').map_or(start, |i| start + i + 1);
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
        let content_start = html[start..].find('>').map_or(start, |i| start + i + 1);
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
    fn name(&self) -> &'static str { "WebFetch" }
    fn category(&self) -> ToolCategory { ToolCategory::Web }
    fn description(&self) -> &'static str {
        "Fetch a URL and return its content. Converts HTML to readable markdown by default. \
         Set raw=true to get raw HTML. Set extract_main_content=true to extract the main \
         article/body content."
    }

    fn to_auto_classifier_input(&self, input: &Value) -> Value {
        // Only pass URL; strip headers, body, and other options
        let url = input.get("url").cloned().unwrap_or(Value::Null);
        json!({"WebFetch": {"url": url}})
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

        // SSRF protection: block private/internal networks and non-HTTP schemes
        if let Err(msg) = validate_url(url) {
            return Ok(ToolResult::error(msg));
        }

        let max_len = (input["max_length"].as_u64().unwrap_or(5000) as usize).min(20_000);
        let raw = input["raw"].as_bool().unwrap_or(false);
        let extract_main = input["extract_main_content"].as_bool().unwrap_or(false);
        let timeout_secs = input["timeout"].as_u64().unwrap_or(30);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .user_agent("Mozilla/5.0 (compatible; ClaudeCode/1.0)")
            .build()?;

        let mut req = client.get(url);

        // Custom headers (with blocklist for security-sensitive headers)
        const BLOCKED_HEADERS: &[&str] = &[
            "host", "authorization", "cookie", "set-cookie",
            "proxy-authorization", "x-forwarded-for", "x-real-ip",
            "transfer-encoding", "content-length", "connection",
        ];
        if let Some(headers) = input["headers"].as_object() {
            for (k, v) in headers {
                if BLOCKED_HEADERS.iter().any(|&b| k.eq_ignore_ascii_case(b)) {
                    tracing::warn!("Blocked security-sensitive header: {}", k);
                    continue;
                }
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
            Ok(ToolResult::error(format!("HTTP {status}: {truncated}")))
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── html_to_markdown ────────────────────────────────────────

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(html_to_markdown("hello world"), "hello world");
    }

    #[test]
    fn strips_html_tags() {
        let html = "<p>Hello</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("Hello"));
        assert!(!md.contains("<p>"));
    }

    #[test]
    fn converts_headings() {
        assert!(html_to_markdown("<h1>Title</h1>").contains("# Title"));
        assert!(html_to_markdown("<h2>Sub</h2>").contains("## Sub"));
        assert!(html_to_markdown("<h3>Sub3</h3>").contains("### Sub3"));
    }

    #[test]
    fn converts_bold_and_italic() {
        assert!(html_to_markdown("<strong>bold</strong>").contains("**bold**"));
        assert!(html_to_markdown("<b>bold</b>").contains("**bold**"));
        assert!(html_to_markdown("<em>italic</em>").contains("*italic*"));
    }

    #[test]
    fn converts_code_blocks() {
        assert!(html_to_markdown("<code>x</code>").contains("`x`"));
        let pre = html_to_markdown("<pre>code block</pre>");
        assert!(pre.contains("```"));
        assert!(pre.contains("code block"));
    }

    #[test]
    fn converts_list_items() {
        let md = html_to_markdown("<ul><li>a</li><li>b</li></ul>");
        assert!(md.contains("- a"));
        assert!(md.contains("- b"));
    }

    #[test]
    fn converts_hr() {
        assert!(html_to_markdown("<hr>").contains("---"));
        assert!(html_to_markdown("<hr/>").contains("---"));
    }

    #[test]
    fn converts_br_to_newline() {
        let md = html_to_markdown("a<br>b");
        assert!(md.contains("a\nb"));
    }

    #[test]
    fn strips_script_and_style() {
        let html = "<script>alert(1)</script>visible<style>.x{}</style>also visible";
        let md = html_to_markdown(html);
        assert!(!md.contains("alert"));
        assert!(!md.contains(".x{}"));
        assert!(md.contains("visible"));
        assert!(md.contains("also visible"));
    }

    #[test]
    fn decodes_html_entities() {
        assert_eq!(html_to_markdown("&amp;"), "&");
        assert_eq!(html_to_markdown("&lt;"), "<");
        assert_eq!(html_to_markdown("&gt;"), ">");
        assert_eq!(html_to_markdown("&quot;"), "\"");
        assert_eq!(html_to_markdown("&apos;"), "'");
        // &nbsp; decodes to space (may be trimmed by collapse_blank_lines when isolated)
        assert!(html_to_markdown("a&nbsp;b").contains("a b"));
        assert_eq!(html_to_markdown("&mdash;"), "—");
        assert_eq!(html_to_markdown("&ndash;"), "–");
    }

    #[test]
    fn preserves_unknown_entities() {
        let md = html_to_markdown("&foobar;");
        assert!(md.contains("&foobar;"));
    }

    #[test]
    fn converts_blockquote() {
        assert!(html_to_markdown("<blockquote>Quote</blockquote>").contains("> Quote"));
    }

    // ── extract_main_content ────────────────────────────────────

    #[test]
    fn extract_article_tag() {
        let html = r"<html><body><nav>nav</nav><article><p>Main text</p></article></body></html>";
        let content = extract_main_content(html);
        assert!(content.contains("Main text"));
        assert!(!content.contains("nav"));
    }

    #[test]
    fn extract_main_tag() {
        let html = r"<html><body><header>H</header><main><p>Content</p></main></body></html>";
        let content = extract_main_content(html);
        assert!(content.contains("Content"));
    }

    #[test]
    fn fallback_to_body() {
        let html = r"<html><body><p>Body text</p></body></html>";
        let content = extract_main_content(html);
        assert!(content.contains("Body text"));
    }

    #[test]
    fn fallback_entire_html() {
        let html = "Just plain text without tags";
        let content = extract_main_content(html);
        assert_eq!(content, "Just plain text without tags");
    }

    // ── validate_url (SSRF protection) ──────────────────────────

    #[test]
    fn allows_public_http_urls() {
        assert!(validate_url("https://example.com/page").is_ok());
        assert!(validate_url("http://example.com").is_ok());
        assert!(validate_url("https://8.8.8.8/dns").is_ok());
    }

    #[test]
    fn blocks_non_http_schemes() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("data:text/html,<h1>x</h1>").is_err());
    }

    #[test]
    fn blocks_localhost() {
        assert!(validate_url("http://localhost/api").is_err());
        assert!(validate_url("http://127.0.0.1/api").is_err());
        assert!(validate_url("http://127.0.0.2").is_err());
    }

    #[test]
    fn blocks_private_ips() {
        assert!(validate_url("http://10.0.0.1/api").is_err());
        assert!(validate_url("http://172.16.0.1").is_err());
        assert!(validate_url("http://192.168.1.1").is_err());
        assert!(validate_url("http://0.0.0.0").is_err());
    }

    #[test]
    fn blocks_link_local() {
        assert!(validate_url("http://169.254.169.254/metadata").is_err());
    }

    #[test]
    fn blocks_metadata_endpoints() {
        assert!(validate_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_url("http://metadata.google.internal/computeMetadata/").is_err());
    }

    #[test]
    fn blocks_internal_hostnames() {
        assert!(validate_url("http://myapp.local/api").is_err());
        assert!(validate_url("http://service.internal/rpc").is_err());
    }
}
