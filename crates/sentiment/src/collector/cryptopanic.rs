//! CryptoPanic collector — JSON API, auth-optional.
//!
//! Direct port of `~/santiment/src/collector/rss.py::fetch_cryptopanic`.
//! The free tier returns up to 30 posts per call; we cap at 30
//! to match and rely on the cycle polling cadence for freshness.

use crate::collector::Collector;
use crate::types::Article;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CryptoPanicCollector {
    url: String,
    http: reqwest::Client,
}

impl CryptoPanicCollector {
    /// `url` is the full JSON endpoint, e.g.
    /// `https://cryptopanic.com/api/v1/posts/?auth_token=XXX&public=true`.
    /// No auth = rate-limited free tier; still useful for MVP.
    pub fn new(url: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            url: url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
        })
    }
}

#[async_trait]
impl Collector for CryptoPanicCollector {
    fn name(&self) -> &'static str {
        "cryptopanic"
    }

    async fn collect(&self) -> Vec<Article> {
        let resp = match self.http.get(&self.url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "cryptopanic non-2xx");
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!(error = %e, "cryptopanic fetch failed");
                return Vec::new();
            }
        };
        let body: Payload = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "cryptopanic json parse failed");
                return Vec::new();
            }
        };

        let now = Utc::now();
        body.results
            .into_iter()
            .take(30)
            .filter_map(|p| {
                let url = p.url?;
                Some(Article {
                    url,
                    title: p.title.unwrap_or_default(),
                    summary: String::new(),
                    source: format!(
                        "CryptoPanic / {}",
                        p.source
                            .and_then(|s| s.title)
                            .unwrap_or_else(|| "unknown".into())
                    ),
                    published_at: p.published_at.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|d| d.with_timezone(&Utc))
                    }),
                    collected_at: now,
                })
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    results: Vec<Post>,
}

#[derive(Deserialize)]
struct Post {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Deserialize)]
struct Source {
    #[serde(default)]
    title: Option<String>,
}
