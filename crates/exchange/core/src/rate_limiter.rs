use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::warn;

/// Token-bucket rate limiter for exchange API calls.
///
/// Continuous-refill token bucket: `max_tokens` tokens regenerate
/// linearly over `window` (Binance: 1200/min = 20/s). This fixes
/// the Epic 36.5 gap where the previous implementation refilled
/// the whole bucket at window boundaries — a 2× burst across the
/// boundary could trip the venue's own limit even though our
/// limiter said "within budget."
///
/// Venue-header feedback closes the loop:
/// [`RateLimiter::record_used`] accepts the `X-MBX-USED-WEIGHT-1M`
/// value returned on every Binance response and snaps our local
/// counter to the venue's view, so drift between our accounting
/// and reality self-corrects within one request.
///
/// [`RateLimiter::pause_for`] honours `Retry-After` on 429 — the
/// caller parses the header value (seconds) and blocks the bucket
/// for that many seconds before new acquires go through.
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    /// Max tokens per window (after safety buffer).
    max_tokens: u32,
    /// Current available tokens (fractional accumulator kept in
    /// f64 so the continuous refill stays smooth).
    tokens: f64,
    /// Window duration — used to derive the per-second refill
    /// rate (`max_tokens / window_secs`).
    window: Duration,
    /// Last time the bucket was topped up.
    last_refill: Instant,
    /// Operator-configured safety multiplier on the venue's
    /// nominal limit (0.8 = use 80 %).
    buffer: f64,
    /// When `Retry-After` is honoured, no acquires may proceed
    /// until this instant regardless of token count.
    pause_until: Option<Instant>,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration, buffer: f64) -> Self {
        let buffer = if buffer.is_finite() {
            buffer.clamp(0.0, 1.0)
        } else {
            0.8
        };
        let effective_max = ((max_per_window as f64) * buffer) as u32;
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                max_tokens: effective_max,
                tokens: effective_max as f64,
                window,
                last_refill: Instant::now(),
                buffer,
                pause_until: None,
            })),
        }
    }

    pub async fn acquire(&self, weight: u32) {
        loop {
            let wait_for = {
                let mut inner = self.inner.lock().await;
                inner.refill();
                if let Some(until) = inner.pause_until {
                    if Instant::now() < until {
                        Some(until.duration_since(Instant::now()))
                    } else {
                        inner.pause_until = None;
                        if inner.tokens >= weight as f64 {
                            inner.tokens -= weight as f64;
                            return;
                        }
                        None
                    }
                } else if inner.tokens >= weight as f64 {
                    inner.tokens -= weight as f64;
                    return;
                } else {
                    None
                }
            };
            match wait_for {
                Some(d) => tokio::time::sleep(d).await,
                None => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    pub async fn try_acquire(&self, weight: u32) -> bool {
        let mut inner = self.inner.lock().await;
        inner.refill();
        if let Some(until) = inner.pause_until {
            if Instant::now() < until {
                return false;
            }
            inner.pause_until = None;
        }
        if inner.tokens >= weight as f64 {
            inner.tokens -= weight as f64;
            true
        } else {
            warn!(
                available = inner.tokens as u32,
                requested = weight,
                "rate limit would be exceeded"
            );
            false
        }
    }

    pub async fn remaining(&self) -> u32 {
        let mut inner = self.inner.lock().await;
        inner.refill();
        inner.tokens as u32
    }

    /// Snap the local token count to match the venue's own
    /// accounting. Caller passes `used` = the value of the
    /// `X-MBX-USED-WEIGHT-1M` (or equivalent) header. The bucket
    /// resets to `max_tokens - used` so our throttling tracks
    /// the venue's ground truth.
    pub async fn record_used(&self, used: u32) {
        let mut inner = self.inner.lock().await;
        // Convert venue-used → our local headroom, clamped to the
        // effective max (venue's nominal limit × our safety buffer).
        let venue_headroom = (inner.max_tokens as f64 / inner.buffer.max(0.01)) - used as f64;
        let local_headroom = venue_headroom * inner.buffer;
        inner.tokens = local_headroom.clamp(0.0, inner.max_tokens as f64);
    }

    /// Honor a `Retry-After` server hint (seconds). Blocks all
    /// acquires until that moment.
    pub async fn pause_for(&self, seconds: u64) {
        let mut inner = self.inner.lock().await;
        inner.pause_until = Some(Instant::now() + Duration::from_secs(seconds));
    }
}

impl RateLimiterInner {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        let window_secs = self.window.as_secs_f64().max(1e-6);
        let per_sec = self.max_tokens as f64 / window_secs;
        self.tokens = (self.tokens + elapsed * per_sec).min(self.max_tokens as f64);
        self.last_refill = now;
    }
}

impl Clone for RateLimiter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
