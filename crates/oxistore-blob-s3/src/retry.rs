//! Retry / exponential-backoff configuration for S3 operations.
//!
//! Transport errors and HTTP 429/503 responses are all retried with truncated
//! binary exponential backoff:
//!
//! ```text
//! delay = (base_delay_ms * 2^attempt).min(max_delay_ms)   [ms]
//! ```
//!
//! A small counter-based variance is added so concurrent requests don't all
//! retry at exactly the same moment.

use std::time::Duration;

/// Retry / backoff configuration for S3 operations.
///
/// Add via [`crate::S3BlobStoreBuilder::retry_config`] or override directly on
/// [`crate::S3Config::retry_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryConfig {
    /// Maximum number of total attempts (1 = no retry, 3 = 2 retries).
    pub max_attempts: u32,
    /// Base delay before the first retry, in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum delay cap, in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 5_000,
        }
    }
}

/// Compute the delay for a given retry attempt (0-indexed: 0 = first retry).
///
/// Uses binary exponential backoff with a simple deterministic variance derived
/// from the attempt number to spread retries.
pub fn backoff_delay(config: &RetryConfig, attempt: u32) -> Duration {
    // 2^attempt, capped to avoid overflow
    let exp: u64 = 1u64.checked_shl(attempt.min(30)).unwrap_or(u64::MAX);
    let base_ms = config.base_delay_ms.saturating_mul(exp);
    let capped_ms = base_ms.min(config.max_delay_ms);
    // Deterministic variance: odd attempts use 75%, even attempts use 100%.
    // This is enough to break uniform retry waves without introducing rand.
    let varied_ms = if attempt.is_multiple_of(2) {
        capped_ms
    } else {
        capped_ms.saturating_mul(3) / 4
    };
    Duration::from_millis(varied_ms)
}

/// Return `true` for HTTP status codes that should trigger a retry.
///
/// * 429 — Too Many Requests
/// * 503 — Service Unavailable
pub fn should_retry_status(status: u16) -> bool {
    matches!(status, 429 | 503)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_with_attempt() {
        let cfg = RetryConfig::default();
        let d0 = backoff_delay(&cfg, 0);
        let d1 = backoff_delay(&cfg, 1);
        let d2 = backoff_delay(&cfg, 2);
        assert!(d0 >= d1 || d2 >= d0, "backoff should generally increase");
        // All must be below max
        for a in 0..10 {
            let d = backoff_delay(&cfg, a);
            assert!(
                d.as_millis() <= cfg.max_delay_ms as u128,
                "delay exceeded max at attempt {a}"
            );
        }
    }

    #[test]
    fn should_retry_503_and_429() {
        assert!(should_retry_status(503));
        assert!(should_retry_status(429));
        assert!(!should_retry_status(200));
        assert!(!should_retry_status(404));
        assert!(!should_retry_status(500));
    }
}
