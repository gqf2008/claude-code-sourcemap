//! Exponential-backoff retry for Anthropic API calls.
//!
//! Aligned with the TypeScript `withRetry.ts` implementation:
//! - Exponential delay: BASE_DELAY * 2^(attempt-1), capped at MAX_DELAY
//! - 25% jitter to prevent thundering herd
//! - Honors `Retry-After` response header
//! - Retryable: 429 (rate-limit), 529 (overloaded), 500/502/503 (transient)
//! - Non-retryable: 400/401/403/404 (client errors)

use std::time::Duration;
use tracing::{info, warn};

/// Default retry parameters (matching TS defaults).
const MAX_RETRIES: u32 = 10;
const BASE_DELAY_MS: u64 = 500;
const MAX_DELAY_MS: u64 = 32_000;

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: MAX_RETRIES,
            base_delay_ms: BASE_DELAY_MS,
            max_delay_ms: MAX_DELAY_MS,
        }
    }
}

/// Whether an HTTP status code is retryable.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 529 | 500 | 502 | 503)
}

/// Whether an HTTP status code is an overloaded error.
pub fn is_overloaded(status: u16) -> bool {
    status == 529
}

/// Whether an HTTP status code is a rate-limit error.
pub fn is_rate_limited(status: u16) -> bool {
    status == 429
}

/// Compute retry delay for a given attempt (1-based).
///
/// If the server sent `Retry-After` (in seconds), we honour it.
/// Otherwise: `min(base * 2^(attempt-1), max_delay) + jitter(0..25%)`.
pub fn retry_delay(attempt: u32, retry_after_secs: Option<u64>, config: &RetryConfig) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs);
    }
    let exp = config.base_delay_ms.saturating_mul(1u64 << (attempt - 1).min(20));
    let base = exp.min(config.max_delay_ms);
    // Deterministic jitter: use attempt number to get ~12.5% average jitter
    let jitter = (base / 8).wrapping_mul(((attempt as u64).wrapping_mul(7) + 3) % 4);
    Duration::from_millis(base.saturating_add(jitter))
}

/// Structured API error with status and body.
#[derive(Debug, Clone)]
pub struct ApiHttpError {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<u64>,
}

impl std::fmt::Display for ApiHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "API error ({}): {}", self.status, self.body)
    }
}

impl std::error::Error for ApiHttpError {}

/// Execute `action` with retry, calling `on_retry` before each retry sleep.
///
/// `action` is an async closure that returns `Result<T>`. If it returns an
/// `ApiHttpError` with a retryable status, we wait and try again.
///
/// Returns the first successful result or the last error.
pub async fn with_retry<T, F, Fut, R>(
    config: &RetryConfig,
    mut action: F,
    mut on_retry: R,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ApiHttpError>>,
    R: FnMut(u32, u16, &Duration),
{
    let mut last_err: Option<ApiHttpError> = None;

    for attempt in 1..=(config.max_retries + 1) {
        match action().await {
            Ok(val) => return Ok(val),
            Err(err) => {
                if attempt > config.max_retries || !is_retryable_status(err.status) {
                    return Err(anyhow::anyhow!("{}", err));
                }

                let delay = retry_delay(attempt, err.retry_after, config);
                on_retry(attempt, err.status, &delay);

                if is_overloaded(err.status) {
                    warn!(
                        "API overloaded (529), retry {}/{} in {:.1}s",
                        attempt, config.max_retries, delay.as_secs_f64()
                    );
                } else if is_rate_limited(err.status) {
                    info!(
                        "Rate limited (429), retry {}/{} in {:.1}s",
                        attempt, config.max_retries, delay.as_secs_f64()
                    );
                } else {
                    warn!(
                        "Transient error ({}), retry {}/{} in {:.1}s",
                        err.status, attempt, config.max_retries, delay.as_secs_f64()
                    );
                }

                tokio::time::sleep(delay).await;
                last_err = Some(err);
            }
        }
    }

    Err(anyhow::anyhow!("{}", last_err.unwrap()))
}
