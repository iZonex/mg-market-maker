//! Cheap keyword-based sentiment fallback.
//!
//! When Ollama is unreachable we must NOT stop ingesting —
//! the ticker counter still needs articles so rate /
//! acceleration stay live. A keyword scorer gives the engine
//! a degraded (but non-zero) signal.
//!
//! Methodology: keyword-count over a curated bullish /
//! bearish word list. Score is normalised to `[-1.0, +1.0]`
//! via `(pos - neg) / (pos + neg + ε)`. No stemming, no
//! phrase detection — we deliberately keep this dumb so it
//! has no regression surface. The real sentiment path is
//! Ollama; this is a safety net.

use crate::types::{SentimentAnalysis, SentimentSignal};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const BULLISH: &[&str] = &[
    "surge",
    "surges",
    "rally",
    "rallies",
    "rallied",
    "bullish",
    "breakout",
    "pump",
    "moon",
    "ath",
    "record high",
    "all-time high",
    "upgrade",
    "partnership",
    "adoption",
    "approved",
    "approval",
    "launch",
    "launches",
    "gain",
    "gains",
    "beats",
    "outperform",
    "outperforms",
    "soar",
    "soars",
    "breakthrough",
    "positive",
    "buy",
    "accumulate",
    "bull",
];

const BEARISH: &[&str] = &[
    "crash",
    "crashes",
    "dump",
    "plunge",
    "plunges",
    "plunged",
    "bearish",
    "sec",
    "fine",
    "fines",
    "lawsuit",
    "sued",
    "hack",
    "hacked",
    "exploit",
    "liquidation",
    "liquidations",
    "liquidated",
    "rejection",
    "rejected",
    "downgrade",
    "ban",
    "banned",
    "drop",
    "drops",
    "fall",
    "falls",
    "fell",
    "slump",
    "slumps",
    "decline",
    "declines",
    "fraud",
    "scam",
    "negative",
    "sell",
    "dump",
    "bear",
];

/// Score `text` (typically `article.body()`) by keyword match
/// count. Returns a `SentimentAnalysis` with the `keyword`
/// scorer tag. `assets` is left empty — the caller must
/// detect assets through the ticker-aware path
/// ([`crate::ticker::normalize_asset_list`] on a pre-
/// extracted list from the article).
pub fn score(text: &str) -> SentimentAnalysis {
    let lower = text.to_lowercase();
    let mut pos: u32 = 0;
    let mut neg: u32 = 0;
    for kw in BULLISH {
        if lower.contains(kw) {
            pos += 1;
        }
    }
    for kw in BEARISH {
        if lower.contains(kw) {
            neg += 1;
        }
    }
    let score = if pos + neg == 0 {
        dec!(0)
    } else {
        // (pos - neg) / (pos + neg), clamped to [-1, 1].
        let num = Decimal::from(pos as i64 - neg as i64);
        let denom = Decimal::from((pos + neg) as i64);
        num / denom
    };

    let signal = if score > dec!(0.15) {
        SentimentSignal::Bullish
    } else if score < dec!(-0.15) {
        SentimentSignal::Bearish
    } else {
        SentimentSignal::Neutral
    };

    SentimentAnalysis {
        signal,
        score,
        assets: Vec::new(),
        reasoning: String::new(),
        analyzed_at: Utc::now(),
        scorer: "keyword".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullish_text_scores_positive() {
        let a = score("Bitcoin rally — ETF approval sparks record high");
        assert_eq!(a.signal, SentimentSignal::Bullish);
        assert!(a.score > dec!(0));
        assert_eq!(a.scorer, "keyword");
    }

    #[test]
    fn bearish_text_scores_negative() {
        let a = score("Exchange hacked in major exploit, liquidations cascade");
        assert_eq!(a.signal, SentimentSignal::Bearish);
        assert!(a.score < dec!(0));
    }

    #[test]
    fn empty_text_is_neutral() {
        let a = score("");
        assert_eq!(a.signal, SentimentSignal::Neutral);
        assert_eq!(a.score, dec!(0));
    }

    #[test]
    fn balanced_mix_lands_near_zero() {
        let a = score("Token rally then crash, both bullish and bearish commentary");
        // Score stays close to 0 — the classifier shouldn't
        // confidently pick a side when the evidence is
        // balanced.
        assert!(a.score.abs() < dec!(0.5));
    }

    #[test]
    fn score_clamped_to_unit_interval() {
        let a = score("rally surge bullish breakout moon pump gain");
        assert!(a.score <= dec!(1));
        assert!(a.score >= dec!(-1));
    }
}
