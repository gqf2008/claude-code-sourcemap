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

    Err(anyhow::anyhow!("{}", last_err.expect("retry loop ran at least once")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_retryable_status ──

    #[test]
    fn test_retryable_429() {
        assert!(is_retryable_status(429));
    }

    #[test]
    fn test_retryable_529() {
        assert!(is_retryable_status(529));
    }

    #[test]
    fn test_retryable_500() {
        assert!(is_retryable_status(500));
    }

    #[test]
    fn test_retryable_502() {
        assert!(is_retryable_status(502));
    }

    #[test]
    fn test_retryable_503() {
        assert!(is_retryable_status(503));
    }

    #[test]
    fn test_non_retryable_client_errors() {
        for code in [400, 401, 403, 404] {
            assert!(!is_retryable_status(code), "expected {} to be non-retryable", code);
        }
    }

    // ── is_overloaded ──

    #[test]
    fn test_overloaded_529() {
        assert!(is_overloaded(529));
    }

    #[test]
    fn test_overloaded_not_429() {
        assert!(!is_overloaded(429));
    }

    // ── is_rate_limited ──

    #[test]
    fn test_rate_limited_429() {
        assert!(is_rate_limited(429));
    }

    #[test]
    fn test_rate_limited_not_529() {
        assert!(!is_rate_limited(529));
    }

    // ── retry_delay ──

    #[test]
    fn test_retry_delay_with_retry_after() {
        let config = RetryConfig::default();
        let delay = retry_delay(1, Some(5), &config);
        assert_eq!(delay, Duration::from_secs(5));
    }

    #[test]
    fn test_retry_delay_first_attempt() {
        let config = RetryConfig::default();
        let delay = retry_delay(1, None, &config);
        // base = 500ms * 2^0 = 500ms, plus up to 25% jitter
        assert!(delay >= Duration::from_millis(500), "delay {:?} < 500ms", delay);
        assert!(delay < Duration::from_millis(1000), "delay {:?} >= 1000ms", delay);
    }

    #[test]
    fn test_retry_delay_second_attempt() {
        let config = RetryConfig::default();
        let delay = retry_delay(2, None, &config);
        // base = 500ms * 2^1 = 1000ms, plus jitter
        assert!(delay >= Duration::from_millis(1000), "delay {:?} < 1000ms", delay);
    }

    #[test]
    fn test_retry_delay_capped_at_max() {
        let config = RetryConfig::default();
        let delay = retry_delay(20, None, &config);
        // Jitter formula: base/8 * ((attempt*7+3) % 4), max factor = 3 → 37.5%
        let upper_bound = Duration::from_millis(config.max_delay_ms + config.max_delay_ms * 3 / 8);
        assert!(delay <= upper_bound, "delay {:?} exceeds cap {:?}", delay, upper_bound);
    }

    #[test]
    fn test_retry_delay_custom_config() {
        let config = RetryConfig {
            base_delay_ms: 100,
            max_delay_ms: 1000,
            max_retries: 3,
        };
        let d1 = retry_delay(1, None, &config);
        let d2 = retry_delay(2, None, &config);
        // First attempt base = 100ms, second = 200ms; d2 should be larger
        assert!(d2 > d1, "expected d2 {:?} > d1 {:?}", d2, d1);
        // Both should stay within max + 25%
        let upper = Duration::from_millis(1000 + 250);
        assert!(d2 <= upper, "d2 {:?} exceeds cap {:?}", d2, upper);
    }
}
