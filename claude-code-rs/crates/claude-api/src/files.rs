//! Files API client — upload/download session files via Anthropic Files API.
//!
//! Aligned with TS `services/api/filesApi.ts`:
//! - Bearer token (OAuth) authentication
//! - Multipart form-data upload with UUID boundary
//! - Exponential backoff retry (500ms base, 3 attempts max)
//! - Non-retryable: 401/403/404/413
//! - Path traversal protection
//! - Concurrent batch operations (default 5 workers)

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{debug, warn};
use uuid::Uuid;

// ── Constants ────────────────────────────────────────────────────────────────

const FILES_API_BETA_HEADER: &str = "files-api-2025-04-14,oauth-2025-04-20";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 500;
const MAX_FILE_SIZE_BYTES: u64 = 500 * 1024 * 1024; // 500 MB
const DEFAULT_CONCURRENCY: usize = 5;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const LIST_TIMEOUT: Duration = Duration::from_secs(60);

// ── Types ────────────────────────────────────────────────────────────────────

/// Configuration for the Files API client.
#[derive(Debug, Clone)]
pub struct FilesApiConfig {
    /// OAuth token for Bearer authentication.
    pub oauth_token: String,
    /// API base URL (default: https://api.anthropic.com).
    pub base_url: String,
    /// Session ID for workspace directory isolation.
    pub session_id: String,
}

impl FilesApiConfig {
    pub fn new(oauth_token: String, session_id: String) -> Self {
        Self {
            oauth_token,
            base_url: "https://api.anthropic.com".to_string(),
            session_id,
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

/// A file attachment spec (from CLI args: `file_id:relative/path`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSpec {
    pub file_id: String,
    pub relative_path: String,
}

/// Result of a download operation.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub file_id: String,
    pub path: String,
    pub success: bool,
    pub error: Option<String>,
    pub bytes_written: Option<usize>,
}

/// Result of an upload operation.
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub path: String,
    pub success: bool,
    pub file_id: Option<String>,
    pub size: Option<u64>,
    pub error: Option<String>,
}

/// File metadata from the list endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct FileMetadata {
    pub id: String,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
}

/// API list response.
#[derive(Debug, Deserialize)]
struct ListFilesResponse {
    data: Vec<FileMetadata>,
    #[serde(default)]
    has_more: bool,
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

/// Build common headers for Files API requests.
fn build_headers(config: &FilesApiConfig) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.oauth_token))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(
        "anthropic-version",
        HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    headers.insert(
        "anthropic-beta",
        HeaderValue::from_static(FILES_API_BETA_HEADER),
    );
    headers
}

/// Retry with exponential backoff. Non-retryable status codes bail immediately.
async fn retry_with_backoff<T, F, Fut>(
    operation: &str,
    mut attempt_fn: F,
) -> anyhow::Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = RetryResult<T>>,
{
    let mut last_error = String::new();
    for attempt in 1..=MAX_RETRIES {
        match attempt_fn(attempt).await {
            RetryResult::Done(val) => return Ok(val),
            RetryResult::Retry(err) => {
                last_error = err.unwrap_or_else(|| "unknown error".to_string());
                if attempt < MAX_RETRIES {
                    let delay = BASE_DELAY_MS * (1u64 << (attempt - 1));
                    debug!("[files-api] {} retry {}/{} in {}ms: {}", operation, attempt, MAX_RETRIES, delay, last_error);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
            RetryResult::Fatal(err) => {
                return Err(anyhow::anyhow!("[files-api] {}: {}", operation, err));
            }
        }
    }
    Err(anyhow::anyhow!("[files-api] {} failed after {} attempts: {}", operation, MAX_RETRIES, last_error))
}

/// Retry control flow.
enum RetryResult<T> {
    Done(T),
    Retry(Option<String>),
    Fatal(String),
}

/// Check if an HTTP status is non-retryable.
fn is_non_retryable(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 413)
}

// ── Download ─────────────────────────────────────────────────────────────────

/// Download a single file's content by file ID.
pub async fn download_file(
    file_id: &str,
    config: &FilesApiConfig,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{}/v1/files/{}/content", config.base_url, file_id);
    let client = reqwest::Client::new();

    retry_with_backoff("download", |_attempt| {
        let url = url.clone();
        let headers = build_headers(config);
        let client = client.clone();
        async move {
            let resp = match client
                .get(&url)
                .headers(headers)
                .timeout(DOWNLOAD_TIMEOUT)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return RetryResult::Retry(Some(e.to_string())),
            };

            let status = resp.status().as_u16();
            if status == 200 {
                match resp.bytes().await {
                    Ok(bytes) => RetryResult::Done(bytes.to_vec()),
                    Err(e) => RetryResult::Retry(Some(e.to_string())),
                }
            } else if is_non_retryable(status) {
                let body = resp.text().await.unwrap_or_default();
                RetryResult::Fatal(format!("HTTP {}: {}", status, body))
            } else {
                let body = resp.text().await.unwrap_or_default();
                RetryResult::Retry(Some(format!("HTTP {}: {}", status, body)))
            }
        }
    })
    .await
}

/// Build a safe download path, rejecting path traversal.
pub fn build_download_path(
    base_path: &Path,
    session_id: &str,
    relative_path: &str,
) -> Option<PathBuf> {
    let normalized = Path::new(relative_path);

    // Reject paths that traverse above workspace
    for component in normalized.components() {
        if let std::path::Component::ParentDir = component {
            warn!("Invalid file path: {}. Path must not traverse above workspace", relative_path);
            return None;
        }
    }

    let uploads_base = base_path.join(session_id).join("uploads");

    // Strip redundant prefixes
    let clean = relative_path
        .trim_start_matches('/')
        .trim_start_matches("uploads/");

    Some(uploads_base.join(clean))
}

/// Download a file and save it to disk.
pub async fn download_and_save_file(
    attachment: &FileSpec,
    config: &FilesApiConfig,
    base_path: &Path,
) -> DownloadResult {
    let full_path = match build_download_path(base_path, &config.session_id, &attachment.relative_path) {
        Some(p) => p,
        None => {
            return DownloadResult {
                file_id: attachment.file_id.clone(),
                path: attachment.relative_path.clone(),
                success: false,
                error: Some("Path traversal rejected".to_string()),
                bytes_written: None,
            };
        }
    };

    let content = match download_file(&attachment.file_id, config).await {
        Ok(c) => c,
        Err(e) => {
            return DownloadResult {
                file_id: attachment.file_id.clone(),
                path: full_path.to_string_lossy().to_string(),
                success: false,
                error: Some(e.to_string()),
                bytes_written: None,
            };
        }
    };

    // Ensure parent directory exists
    if let Some(parent) = full_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return DownloadResult {
                file_id: attachment.file_id.clone(),
                path: full_path.to_string_lossy().to_string(),
                success: false,
                error: Some(format!("Failed to create directory: {}", e)),
                bytes_written: None,
            };
        }
    }

    let bytes_written = content.len();
    if let Err(e) = tokio::fs::write(&full_path, &content).await {
        return DownloadResult {
            file_id: attachment.file_id.clone(),
            path: full_path.to_string_lossy().to_string(),
            success: false,
            error: Some(format!("Failed to write file: {}", e)),
            bytes_written: None,
        };
    }

    debug!("[files-api] Saved {} to {:?} ({} bytes)", attachment.file_id, full_path, bytes_written);

    DownloadResult {
        file_id: attachment.file_id.clone(),
        path: full_path.to_string_lossy().to_string(),
        success: true,
        error: None,
        bytes_written: Some(bytes_written),
    }
}

/// Download multiple files concurrently.
pub async fn download_session_files(
    files: &[FileSpec],
    config: &FilesApiConfig,
    base_path: &Path,
    concurrency: Option<usize>,
) -> Vec<DownloadResult> {
    let concurrency = concurrency.unwrap_or(DEFAULT_CONCURRENCY);
    let semaphore = std::sync::Arc::new(Semaphore::new(concurrency));

    let mut handles = Vec::new();
    for attachment in files {
        let sem = semaphore.clone();
        let att = attachment.clone();
        let cfg = config.clone();
        let bp = base_path.to_path_buf();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            download_and_save_file(&att, &cfg, &bp).await
        }));
    }

    let mut results = Vec::with_capacity(files.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(DownloadResult {
                file_id: String::new(),
                path: String::new(),
                success: false,
                error: Some(format!("Task panicked: {}", e)),
                bytes_written: None,
            }),
        }
    }

    let success_count = results.iter().filter(|r| r.success).count();
    debug!("[files-api] Downloaded {}/{} files", success_count, files.len());

    results
}

// ── Upload ───────────────────────────────────────────────────────────────────

/// Upload a single file using multipart form-data.
pub async fn upload_file(
    file_path: &Path,
    relative_path: &str,
    config: &FilesApiConfig,
) -> UploadResult {
    // Read file
    let content = match tokio::fs::read(file_path).await {
        Ok(c) => c,
        Err(e) => {
            return UploadResult {
                path: relative_path.to_string(),
                success: false,
                file_id: None,
                size: None,
                error: Some(format!("Failed to read file: {}", e)),
            };
        }
    };

    let file_size = content.len() as u64;
    if file_size > MAX_FILE_SIZE_BYTES {
        return UploadResult {
            path: relative_path.to_string(),
            success: false,
            file_id: None,
            size: Some(file_size),
            error: Some(format!(
                "File exceeds maximum size of {} MB",
                MAX_FILE_SIZE_BYTES / (1024 * 1024)
            )),
        };
    }

    let filename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let url = format!("{}/v1/files", config.base_url);
    let client = reqwest::Client::new();

    let result: anyhow::Result<String> = retry_with_backoff("upload", |_attempt| {
        let url = url.clone();
        let headers = build_headers(config);
        let client = client.clone();
        let content = content.clone();
        let filename = filename.clone();
        async move {
            // Build multipart body manually (TS parity)
            let boundary = format!("----FormBoundary{}", Uuid::new_v4());
            let mut body = Vec::new();

            // File part
            body.extend_from_slice(format!(
                "--{}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
                boundary, filename
            ).as_bytes());
            body.extend_from_slice(&content);
            body.extend_from_slice(b"\r\n");

            // Purpose part
            body.extend_from_slice(format!(
                "--{}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nuser_data\r\n",
                boundary
            ).as_bytes());

            // Final boundary
            body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

            let mut req_headers = headers;
            req_headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&format!("multipart/form-data; boundary={}", boundary))
                    .unwrap_or_else(|_| HeaderValue::from_static("multipart/form-data")),
            );
            req_headers.insert(
                "content-length",
                HeaderValue::from_str(&body.len().to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );

            let resp = match client
                .post(&url)
                .headers(req_headers)
                .body(body)
                .timeout(UPLOAD_TIMEOUT)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return RetryResult::Retry(Some(e.to_string())),
            };

            let status = resp.status().as_u16();
            if status == 200 || status == 201 {
                match resp.json::<serde_json::Value>().await {
                    Ok(data) => {
                        if let Some(id) = data["id"].as_str() {
                            RetryResult::Done(id.to_string())
                        } else {
                            RetryResult::Retry(Some("Upload succeeded but no file ID returned".to_string()))
                        }
                    }
                    Err(e) => RetryResult::Retry(Some(format!("Failed to parse response: {}", e))),
                }
            } else if is_non_retryable(status) {
                let body = resp.text().await.unwrap_or_default();
                RetryResult::Fatal(format!("HTTP {}: {}", status, body))
            } else {
                let body = resp.text().await.unwrap_or_default();
                RetryResult::Retry(Some(format!("HTTP {}: {}", status, body)))
            }
        }
    })
    .await;

    match result {
        Ok(file_id) => {
            debug!("[files-api] Uploaded {} → {} ({} bytes)", relative_path, file_id, file_size);
            UploadResult {
                path: relative_path.to_string(),
                success: true,
                file_id: Some(file_id),
                size: Some(file_size),
                error: None,
            }
        }
        Err(e) => UploadResult {
            path: relative_path.to_string(),
            success: false,
            file_id: None,
            size: Some(file_size),
            error: Some(e.to_string()),
        },
    }
}

/// Upload multiple files concurrently.
pub async fn upload_session_files(
    files: &[(PathBuf, String)],
    config: &FilesApiConfig,
    concurrency: Option<usize>,
) -> Vec<UploadResult> {
    let concurrency = concurrency.unwrap_or(DEFAULT_CONCURRENCY);
    let semaphore = std::sync::Arc::new(Semaphore::new(concurrency));

    let mut handles = Vec::new();
    for (path, relative) in files {
        let sem = semaphore.clone();
        let path = path.clone();
        let relative = relative.clone();
        let cfg = config.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            upload_file(&path, &relative, &cfg).await
        }));
    }

    let mut results = Vec::with_capacity(files.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(UploadResult {
                path: String::new(),
                success: false,
                file_id: None,
                size: None,
                error: Some(format!("Task panicked: {}", e)),
            }),
        }
    }

    results
}

// ── List ─────────────────────────────────────────────────────────────────────

/// List files created after a given timestamp (with pagination).
pub async fn list_files_created_after(
    after_created_at: &str,
    config: &FilesApiConfig,
) -> anyhow::Result<Vec<FileMetadata>> {
    let client = reqwest::Client::new();
    let headers = build_headers(config);
    let mut all_files = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let mut url = format!(
            "{}/v1/files?after_created_at={}",
            config.base_url, after_created_at
        );
        if let Some(ref cursor) = after_id {
            url.push_str(&format!("&after_id={}", cursor));
        }

        let resp: ListFilesResponse = retry_with_backoff("list", |_attempt| {
            let url = url.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let resp = match client
                    .get(&url)
                    .headers(headers)
                    .timeout(LIST_TIMEOUT)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => return RetryResult::Retry(Some(e.to_string())),
                };

                let status = resp.status().as_u16();
                if status == 200 {
                    match resp.json::<ListFilesResponse>().await {
                        Ok(data) => RetryResult::Done(data),
                        Err(e) => RetryResult::Retry(Some(format!("Parse error: {}", e))),
                    }
                } else if is_non_retryable(status) {
                    let body = resp.text().await.unwrap_or_default();
                    RetryResult::Fatal(format!("HTTP {}: {}", status, body))
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    RetryResult::Retry(Some(format!("HTTP {}: {}", status, body)))
                }
            }
        })
        .await?;

        let has_more = resp.has_more;
        after_id = resp.data.last().map(|f| f.id.clone());
        all_files.extend(resp.data);

        if !has_more || after_id.is_none() {
            break;
        }
    }

    Ok(all_files)
}

// ── Parse file specs ─────────────────────────────────────────────────────────

/// Parse file spec strings (`file_id:relative/path`) into FileSpec objects.
pub fn parse_file_specs(specs: &[String]) -> Vec<FileSpec> {
    let mut result = Vec::new();

    for spec in specs {
        // Split multi-spec strings by spaces
        for part in spec.split_whitespace() {
            if part.is_empty() {
                continue;
            }

            let colon_idx = match part.find(':') {
                Some(i) => i,
                None => {
                    debug!("[files-api] Invalid file spec (no colon): {}", part);
                    continue;
                }
            };

            let file_id = &part[..colon_idx];
            let relative_path = &part[colon_idx + 1..];

            if file_id.is_empty() || relative_path.is_empty() {
                debug!("[files-api] Invalid file spec (empty id or path): {}", part);
                continue;
            }

            result.push(FileSpec {
                file_id: file_id.to_string(),
                relative_path: relative_path.to_string(),
            });
        }
    }

    result
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_file_specs ──

    #[test]
    fn parse_empty_specs() {
        let result = parse_file_specs(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_single_spec() {
        let specs = vec!["file_123:src/main.rs".to_string()];
        let result = parse_file_specs(&specs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file_id, "file_123");
        assert_eq!(result[0].relative_path, "src/main.rs");
    }

    #[test]
    fn parse_multiple_specs() {
        let specs = vec![
            "file_1:a.rs".to_string(),
            "file_2:b.rs".to_string(),
        ];
        let result = parse_file_specs(&specs);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_space_separated_specs() {
        let specs = vec!["file_1:a.rs file_2:b.rs".to_string()];
        let result = parse_file_specs(&specs);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_invalid_no_colon() {
        let specs = vec!["file_id_without_path".to_string()];
        let result = parse_file_specs(&specs);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_invalid_empty_id() {
        let specs = vec![":path/file.rs".to_string()];
        let result = parse_file_specs(&specs);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_invalid_empty_path() {
        let specs = vec!["file_id:".to_string()];
        let result = parse_file_specs(&specs);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_empty_string() {
        let specs = vec!["".to_string()];
        let result = parse_file_specs(&specs);
        assert!(result.is_empty());
    }

    // ── build_download_path ──

    #[test]
    fn build_path_simple() {
        let result = build_download_path(
            Path::new("/home"),
            "sess1",
            "file.txt",
        );
        assert!(result.is_some());
        let p = result.unwrap();
        assert!(p.to_string_lossy().contains("uploads"));
        assert!(p.to_string_lossy().contains("file.txt"));
    }

    #[test]
    fn build_path_nested() {
        let result = build_download_path(
            Path::new("/home"),
            "sess1",
            "dir/sub/file.txt",
        );
        assert!(result.is_some());
    }

    #[test]
    fn build_path_traversal_rejected() {
        let result = build_download_path(
            Path::new("/home"),
            "sess1",
            "../etc/passwd",
        );
        assert!(result.is_none());
    }

    #[test]
    fn build_path_dot_slash() {
        let result = build_download_path(
            Path::new("/home"),
            "sess1",
            "./file.txt",
        );
        assert!(result.is_some());
    }

    // ── Config ──

    #[test]
    fn config_defaults() {
        let cfg = FilesApiConfig::new("token".to_string(), "sess1".to_string());
        assert_eq!(cfg.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn config_custom_base_url() {
        let cfg = FilesApiConfig::new("token".to_string(), "sess1".to_string())
            .with_base_url("http://localhost:8080".to_string());
        assert_eq!(cfg.base_url, "http://localhost:8080");
    }

    // ── Headers ──

    #[test]
    fn headers_contain_auth() {
        let cfg = FilesApiConfig::new("my-token".to_string(), "s1".to_string());
        let headers = build_headers(&cfg);
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Bearer my-token"
        );
        assert_eq!(
            headers.get("anthropic-version").unwrap().to_str().unwrap(),
            ANTHROPIC_VERSION
        );
        assert_eq!(
            headers.get("anthropic-beta").unwrap().to_str().unwrap(),
            FILES_API_BETA_HEADER
        );
    }

    // ── FileSpec serialization ──

    #[test]
    fn file_spec_roundtrip() {
        let spec = FileSpec {
            file_id: "file_abc".to_string(),
            relative_path: "src/lib.rs".to_string(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: FileSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.file_id, "file_abc");
        assert_eq!(parsed.relative_path, "src/lib.rs");
    }
}
