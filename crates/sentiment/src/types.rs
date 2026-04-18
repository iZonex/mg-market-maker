//! Core data types.
//!
//! `Article` captures the raw input (headline, summary, url,
//! source, timestamp). `SentimentAnalysis` is the model output
//! (signal, score, assets, reasoning). `SentimentTick` is the
//! window-aggregated state the risk engine consumes — this is
//! what crosses the crate boundary into `mm-risk`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A single raw news / social item. Url is the dedup key —
/// the same headline arriving from two feeds produces one
/// `Article`, not two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    pub collected_at: DateTime<Utc>,
}

impl Article {
    /// Combined text used both for keyword scoring and as
    /// prompt input to the LLM. Trimmed to keep the prompt
    /// bounded — headlines carry most of the signal anyway.
    pub fn body(&self) -> String {
        let summary = self.summary.chars().take(500).collect::<String>();
        if summary.is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, summary)
        }
    }
}

/// Classifier output from a sentiment scorer — either the
/// local LLM path or the cheap keyword fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SentimentSignal {
    Bullish,
    Bearish,
    Neutral,
}

impl SentimentSignal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bullish => "bullish",
            Self::Bearish => "bearish",
            Self::Neutral => "neutral",
        }
    }
}

/// Scorer output for a single [`Article`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentAnalysis {
    pub signal: SentimentSignal,
    /// Float score in `[-1.0, +1.0]`. `Decimal` kept to stay
    /// consistent with the rest of the workspace's
    /// never-f64-for-signal rule.
    pub score: Decimal,
    /// Normalised canonical tickers the article touches
    /// (e.g. `["BTC", "ETH"]`). Empty = no specific asset
    /// detected.
    pub assets: Vec<String>,
    /// Short reasoning string — LLM only, keyword scorer
    /// leaves this blank.
    #[serde(default)]
    pub reasoning: String,
    pub analyzed_at: DateTime<Utc>,
    /// Which scorer produced this — `"ollama"` or
    /// `"keyword"`. Exposed to telemetry so operators can
    /// tell when the LLM is down and the fallback is
    /// carrying the load.
    pub scorer: String,
}

/// Window-aggregated state the risk engine consumes. Produced
/// by [`crate::counter::MentionCounter::snapshot_for`] on each
/// analyzer cycle. This is the crate's public output surface.
///
/// Ideology note: no direction (buy / sell) — just *state* +
/// *velocity*. The risk engine on the other side decides
/// whether high sentiment + low OFI means "fade" or "retreat",
/// depending on the strategy class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentTick {
    pub asset: String,
    pub ts: DateTime<Utc>,
    /// Count of mentions in the last 5 minutes.
    pub mentions_5min: u64,
    /// Count of mentions in the last 60 minutes.
    pub mentions_1h: u64,
    /// `mentions_5min / (mentions_1h / 12)`. 1.0 = flat, 2.0 =
    /// twice the typical rate, 10.0 = news-cycle spike.
    pub mentions_rate: Decimal,
    /// `mentions_rate` — `mentions_rate_1min_ago`. Positive =
    /// accelerating, negative = fading. The risk layer should
    /// react MORE to acceleration than to level because a
    /// sustained high level is typically already priced in.
    pub mentions_acceleration: Decimal,
    /// EWMA of per-article `score` over the last 5 minutes.
    /// In `[-1.0, +1.0]`.
    pub sentiment_score_5min: Decimal,
    /// EWMA score 5 minutes earlier. `sentiment_delta =
    /// sentiment_score_5min - sentiment_score_prev` lets the
    /// risk layer detect sentiment *shifts* separately from
    /// absolute level.
    pub sentiment_score_prev: Decimal,
    pub sentiment_delta: Decimal,
}
