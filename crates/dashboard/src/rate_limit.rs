//! Simple in-memory token-bucket rate limiter for API endpoints.
//!
//! Protects admin endpoints from abuse. Each API key gets its
//! own bucket; unauthenticated requests share a global bucket.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::warn;

use crate::auth::TokenClaims;

/// Token bucket rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

#[derive(Debug)]
struct RateLimiterInner {
    buckets: HashMap<String, TokenBucket>,
    max_requests_per_minute: u32,
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn new(max_per_minute: u32) -> Self {
        let max = max_per_minute as f64;
        Self {
            tokens: max,
            last_refill: Instant::now(),
            max_tokens: max,
            refill_rate: max / 60.0,
        }
    }

    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl RateLimiter {
    /// Create a rate limiter with the given max requests per
    /// minute per key.
    pub fn new(max_requests_per_minute: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                buckets: HashMap::new(),
                max_requests_per_minute,
            })),
        }
    }

    /// Check if a request from `key` is allowed. Returns `true`
    /// if under the rate limit, `false` if throttled.
    pub fn check(&self, key: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let max = inner.max_requests_per_minute;
        let bucket = inner
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(max));
        bucket.try_consume()
    }

    /// Clean up stale buckets (keys not seen in > 10 minutes).
    pub fn cleanup(&self) {
        let mut inner = self.inner.lock().unwrap();
        let cutoff = Instant::now() - std::time::Duration::from_secs(600);
        inner.buckets.retain(|_, b| b.last_refill > cutoff);
    }

    /// Number of tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.inner.lock().unwrap().buckets.len()
    }
}

/// Axum middleware: throttle by authenticated user id when claims
/// are present, otherwise by source IP. Returns `429 Too Many
/// Requests` when the per-key bucket is empty. Keep buckets
/// generous for admin use (e.g., 300 req/min is fine for normal
/// operator activity but stops a runaway loop).
pub async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    // Prefer user id so a malicious IP behind a shared NAT cannot
    // starve other users, and so a legitimate user on a rotating
    // IP is still rate-limited consistently.
    let key = match req.extensions().get::<TokenClaims>() {
        Some(c) => format!("user:{}", c.user_id),
        None => format!("ip:{}", addr.ip()),
    };
    if !limiter.check(&key) {
        warn!(key = %key, path = %req.uri().path(), "rate-limited");
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_under_limit() {
        let rl = RateLimiter::new(60); // 60/min = 1/sec
        for _ in 0..10 {
            assert!(rl.check("test-key"));
        }
    }

    #[test]
    fn different_keys_independent() {
        let rl = RateLimiter::new(2);
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(rl.check("b")); // b has its own bucket
    }

    #[test]
    fn cleanup_removes_stale() {
        let rl = RateLimiter::new(60);
        rl.check("key1");
        assert_eq!(rl.tracked_keys(), 1);
        rl.cleanup();
        // key1 was just accessed, shouldn't be cleaned up yet.
        assert_eq!(rl.tracked_keys(), 1);
    }
}
