//! X / Twitter collector — recent-search via API v2.
//!
//! Authentication: app-only bearer token (`$TWITTER_BEARER`).
//! We hit `GET /2/tweets/search/recent?query=...` which returns
//! up to 100 tweets per call with `created_at` + `author_id` +
//! `id`. No elevated access required; the free tier limits are
//! enforced by X, not here.
//!
//! Ideology: we treat tweets as articles identical to RSS items
//! so the downstream mention counter / sentiment scorer doesn't
//! care where text came from. A tweet's "url" is constructed
//! from `https://x.com/i/status/{id}` — stable + dedup-friendly.
//!
//! Media: v2 `expansions=attachments.media_keys` returns media
//! URLs alongside the text. When a tweet has an attached image
//! we capture its URL on `Article.summary` (separated by `\n\n`
//! `[img] https://...`). The multimodal analyzer picks that up
//! and passes base64-encoded bytes to Ollama. Fetching the
//! image bytes themselves is the orchestrator's job — this
//! module only enumerates.

use crate::collector::Collector;
use crate::types::Article;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct TwitterCollector {
    queries: Vec<String>,
    bearer: String,
    http: reqwest::Client,
    max_results: u32,
}

impl TwitterCollector {
    /// `queries` is a list of Twitter search strings, e.g.
    /// `"bitcoin -is:retweet lang:en"`. Each runs once per
    /// `collect` cycle. `max_results` is capped by X at 100
    /// per request on the free tier.
    pub fn new(queries: Vec<String>, bearer: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            queries,
            bearer: bearer.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()?,
            max_results: 50,
        })
    }

    pub fn with_max_results(mut self, n: u32) -> Self {
        // X API caps this at 100 on the recent-search endpoint.
        self.max_results = n.clamp(10, 100);
        self
    }

    async fn fetch_one(&self, query: &str) -> Vec<Article> {
        let url = "https://api.twitter.com/2/tweets/search/recent";
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.bearer)
            .query(&[
                ("query", query),
                ("max_results", &self.max_results.to_string()),
                (
                    "tweet.fields",
                    "created_at,author_id,lang,public_metrics,attachments",
                ),
                ("expansions", "attachments.media_keys"),
                ("media.fields", "url,type,preview_image_url"),
            ])
            .send()
            .await;
        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "twitter non-2xx");
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!(error = %e, "twitter fetch failed");
                return Vec::new();
            }
        };
        let body: Payload = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "twitter json parse failed");
                return Vec::new();
            }
        };

        // Build media-key → url index.
        let media_map: HashMap<String, String> = body
            .includes
            .and_then(|i| i.media)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| {
                let key = m.media_key?;
                let url = m.url.or(m.preview_image_url)?;
                Some((key, url))
            })
            .collect();

        let now = Utc::now();
        let src = format!("X / {query}");
        body.data
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                let img_urls: Vec<String> = t
                    .attachments
                    .as_ref()
                    .and_then(|a| a.media_keys.as_ref())
                    .map(|keys| {
                        keys.iter()
                            .filter_map(|k| media_map.get(k).cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                let summary = if img_urls.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n\n{}",
                        img_urls
                            .iter()
                            .map(|u| format!("[img] {u}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                };
                Article {
                    url: format!("https://x.com/i/status/{}", t.id),
                    title: t.text.unwrap_or_default(),
                    summary,
                    source: src.clone(),
                    published_at: t.created_at.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|d| d.with_timezone(&Utc))
                    }),
                    collected_at: now,
                }
            })
            .collect()
    }
}

#[async_trait]
impl Collector for TwitterCollector {
    fn name(&self) -> &'static str {
        "twitter"
    }

    async fn collect(&self) -> Vec<Article> {
        let mut out = Vec::new();
        for q in &self.queries {
            out.extend(self.fetch_one(q).await);
        }
        out
    }
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    data: Option<Vec<Tweet>>,
    #[serde(default)]
    includes: Option<Includes>,
}

#[derive(Deserialize)]
struct Tweet {
    id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    attachments: Option<Attachments>,
}

#[derive(Deserialize)]
struct Attachments {
    #[serde(default)]
    media_keys: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Includes {
    #[serde(default)]
    media: Option<Vec<Media>>,
}

#[derive(Deserialize)]
struct Media {
    #[serde(default)]
    media_key: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    preview_image_url: Option<String>,
}
