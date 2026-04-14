use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::warn;

/// Token-bucket rate limiter for exchange API calls.
///
/// Each exchange has different rate limits:
/// - Binance: weight-based (different calls cost different weights)
/// - Bybit: 600 req/5s
/// - OKX: 1000 req/2s (order-specific)
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    /// Max tokens (requests/weight) per window.
    max_tokens: u32,
    /// Current available tokens.
    tokens: u32,
    /// Window duration.
    window: Duration,
    /// Last refill time.
    last_refill: Instant,
    /// Safety buffer: only use this fraction of the limit.
    _buffer: f64,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `max_per_window`: maximum requests/weight per window
    /// - `window`: duration of the window
    /// - `buffer`: fraction of limit to use (0.8 = use 80% of actual limit)
    pub fn new(max_per_window: u32, window: Duration, buffer: f64) -> Self {
        let effective_max = (max_per_window as f64 * buffer) as u32;
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                max_tokens: effective_max,
                tokens: effective_max,
                window,
                last_refill: Instant::now(),
                _buffer: buffer,
            })),
        }
    }

    /// Acquire `weight` tokens. Waits if necessary.
    pub async fn acquire(&self, weight: u32) {
        loop {
            {
                let mut inner = self.inner.lock().await;
                inner.maybe_refill();
                if inner.tokens >= weight {
                    inner.tokens -= weight;
                    return;
                }
            }
            // Not enough tokens — wait for a fraction of the window.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Try to acquire without waiting. Returns false if rate limited.
    pub async fn try_acquire(&self, weight: u32) -> bool {
        let mut inner = self.inner.lock().await;
        inner.maybe_refill();
        if inner.tokens >= weight {
            inner.tokens -= weight;
            true
        } else {
            warn!(
                available = inner.tokens,
                requested = weight,
                "rate limit would be exceeded"
            );
            false
        }
    }

    /// Get remaining tokens.
    pub async fn remaining(&self) -> u32 {
        let mut inner = self.inner.lock().await;
        inner.maybe_refill();
        inner.tokens
    }
}

impl RateLimiterInner {
    fn maybe_refill(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_refill) >= self.window {
            self.tokens = self.max_tokens;
            self.last_refill = now;
        }
    }
}

impl Clone for RateLimiter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
