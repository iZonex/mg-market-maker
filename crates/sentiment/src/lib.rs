//! Native social/sentiment pipeline (Epic G, stage 0).
//!
//! Three things live here:
//!
//! 1. **Types** ([`types`]) — `Article`, `SentimentAnalysis`,
//!    `SentimentTick`. The tick is the crate's output
//!    surface; risk engines consume state, not raw articles.
//!
//! 2. **Analyzers** ([`ollama`], [`keyword`]) — local LLM
//!    via Ollama for quality, keyword fallback for resilience.
//!    No-one else in the workspace imports HTTP clients for
//!    this; the LLM path lives here.
//!
//! 3. **Window aggregator** ([`counter`]) — `MentionCounter`
//!    keeps 1-hour per-asset deques and derives
//!    `mentions_rate`, `mentions_acceleration`,
//!    `sentiment_delta` in O(1) per record.
//!
//! G1 (next sprint) adds **collectors** (RSS, CryptoPanic,
//! Twitter) and storage. G2 adds a `SocialRiskEngine` in
//! `mm-risk` that turns ticks into spread multipliers /
//! inventory skew / kill-switch triggers.

pub mod bot_filter;
pub mod collector;
pub mod counter;
pub mod keyword;
pub mod ollama;
pub mod orchestrator;
pub mod persistence;
pub mod ticker;
pub mod types;

pub use counter::MentionCounter;
pub use ollama::{OllamaClient, OllamaConfig};
pub use ticker::{normalize_asset_list, normalize_ticker};
pub use types::{Article, SentimentAnalysis, SentimentSignal, SentimentTick};
