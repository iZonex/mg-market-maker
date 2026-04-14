use std::time::Duration;

use reqwest::{Response, StatusCode};
use tracing::warn;

/// Retry configuration for HTTP requests.
pub struct RetryConfig {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Initial backoff duration.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Backoff multiplier per retry.
    pub multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            multiplier: 2.0,
        }
    }
}

/// Execute an HTTP request with exponential backoff retry on 429/5xx.
pub async fn with_retry<F, Fut>(
    config: &RetryConfig,
    operation_name: &str,
    mut make_request: F,
) -> reqwest::Result<Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = reqwest::Result<Response>>,
{
    let mut backoff = config.initial_backoff;

    for attempt in 0..=config.max_retries {
        let resp = make_request().await?;

        match resp.status() {
            StatusCode::TOO_MANY_REQUESTS => {
                if attempt == config.max_retries {
                    warn!(
                        operation = operation_name,
                        attempts = attempt + 1,
                        "max retries reached on 429"
                    );
                    return Ok(resp);
                }
                // Check Retry-After header.
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);

                let wait = retry_after.unwrap_or(backoff);
                warn!(
                    operation = operation_name,
                    attempt = attempt + 1,
                    wait_ms = wait.as_millis() as u64,
                    "429 rate limited, backing off"
                );
                tokio::time::sleep(wait).await;
                backoff =
                    Duration::from_millis((backoff.as_millis() as f64 * config.multiplier) as u64)
                        .min(config.max_backoff);
            }
            status if status.is_server_error() => {
                if attempt == config.max_retries {
                    warn!(
                        operation = operation_name,
                        status = %status,
                        "max retries reached on server error"
                    );
                    return Ok(resp);
                }
                warn!(
                    operation = operation_name,
                    status = %status,
                    attempt = attempt + 1,
                    "server error, retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff =
                    Duration::from_millis((backoff.as_millis() as f64 * config.multiplier) as u64)
                        .min(config.max_backoff);
            }
            _ => {
                // Success or client error (4xx except 429) — don't retry.
                return Ok(resp);
            }
        }
    }

    // Should not reach here, but just in case.
    make_request().await
}
