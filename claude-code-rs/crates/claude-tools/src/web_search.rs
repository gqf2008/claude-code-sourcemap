//! WebSearchTool — search the web for current information.
//!
//! Aligned with TS `WebSearchTool.ts`.  Uses a configurable search backend;
//! the default implementation calls a simple HTTP search API (Brave/SearXNG/etc).
//! Falls back to a stub that returns "web search unavailable" when no backend
//! is configured.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Maximum number of search results to return.
const MAX_RESULTS: usize = 8;
/// Maximum snippet length per result.
const MAX_SNIPPET_LEN: usize = 300;

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "WebSearch" }
    fn category(&self) -> ToolCategory { ToolCategory::Web }

    fn description(&self) -> &str {
        "Search the web for real-time information. Use this when you need current data, \
         recent events, or information that may not be in your training data. Returns \
         a list of relevant results with titles, URLs, and snippets."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Be specific and concise.",
                    "minLength": 2
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: restrict results to these domains only"
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: exclude results from these domains"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query'"))?;

        if query.len() < 2 {
            return Ok(ToolResult::error("Query must be at least 2 characters"));
        }

        let allowed_domains: Vec<String> = input["allowed_domains"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let blocked_domains: Vec<String> = input["blocked_domains"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Try environment-configured search backend
        let api_key = std::env::var("SEARCH_API_KEY").ok();
        let base_url = std::env::var("SEARCH_API_URL")
            .unwrap_or_else(|_| "https://api.search.brave.com/res/v1/web/search".into());

        if api_key.is_none() {
            return Ok(ToolResult::text(format!(
                "Web search is not configured. Set SEARCH_API_KEY and optionally \
                 SEARCH_API_URL environment variables.\n\nQuery was: {}",
                query
            )));
        }

        let results = do_search(
            &base_url,
            api_key.as_deref().unwrap(),
            query,
            &allowed_domains,
            &blocked_domains,
        )
        .await;

        match results {
            Ok(formatted) => Ok(ToolResult::text(formatted)),
            Err(e) => Ok(ToolResult::error(format!("Search failed: {}", e))),
        }
    }
}

/// Perform the actual HTTP search request.
async fn do_search(
    base_url: &str,
    api_key: &str,
    query: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
) -> anyhow::Result<String> {
    // Build query with domain restrictions
    let mut search_query = query.to_string();
    for domain in allowed_domains {
        search_query.push_str(&format!(" site:{}", domain));
    }
    for domain in blocked_domains {
        search_query.push_str(&format!(" -site:{}", domain));
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(base_url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", &search_query), ("count", &MAX_RESULTS.to_string())])
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Search API returned status {}", resp.status());
    }

    let body: Value = resp.json().await?;
    format_search_results(&body, query)
}

/// Format search API response into a readable text summary.
fn format_search_results(body: &Value, query: &str) -> anyhow::Result<String> {
    let mut out = format!("Search results for: {}\n\n", query);
    let mut count = 0;

    // Handle Brave Search API format
    if let Some(results) = body["web"]["results"].as_array() {
        for result in results.iter().take(MAX_RESULTS) {
            count += 1;
            let title = result["title"].as_str().unwrap_or("(no title)");
            let url = result["url"].as_str().unwrap_or("");
            let snippet = result["description"]
                .as_str()
                .or_else(|| result["snippet"].as_str())
                .unwrap_or("");

            let snippet = if snippet.len() > MAX_SNIPPET_LEN {
                // UTF-8 safe truncation: find nearest char boundary
                let mut end = MAX_SNIPPET_LEN;
                while !snippet.is_char_boundary(end) && end > 0 { end -= 1; }
                &snippet[..end]
            } else {
                snippet
            };

            out.push_str(&format!("{}. {}\n   {}\n   {}\n\n", count, title, url, snippet));
        }
    }

    // Fallback: try generic format with "results" array
    if count == 0 {
        if let Some(results) = body["results"].as_array() {
            for result in results.iter().take(MAX_RESULTS) {
                count += 1;
                let title = result["title"].as_str().unwrap_or("(no title)");
                let url = result["url"].as_str().or_else(|| result["link"].as_str()).unwrap_or("");
                let snippet = result["snippet"]
                    .as_str()
                    .or_else(|| result["description"].as_str())
                    .unwrap_or("");

                let snippet = if snippet.len() > MAX_SNIPPET_LEN {
                    let mut end = MAX_SNIPPET_LEN;
                    while !snippet.is_char_boundary(end) && end > 0 { end -= 1; }
                    &snippet[..end]
                } else {
                    snippet
                };

                out.push_str(&format!("{}. {}\n   {}\n   {}\n\n", count, title, url, snippet));
            }
        }
    }

    if count == 0 {
        out.push_str("No results found.");
    } else {
        out.push_str(&format!("({} results)", count));
    }

    Ok(out)
}
