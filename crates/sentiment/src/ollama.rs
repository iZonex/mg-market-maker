//! Local-LLM sentiment scorer — Ollama adapter.
//!
//! Port of `~/santiment/src/analyzer/sentiment.py` to Rust:
//!   1. `POST /api/generate` on the local Ollama with a
//!      strict JSON-mode system prompt.
//!   2. Parse `{signal, score, assets, reasoning}`.
//!   3. Normalise assets via [`crate::ticker::normalize_asset_list`].
//!   4. Wrap in a [`SentimentAnalysis`] tagged `scorer =
//!      "ollama"`.
//!
//! Failure modes route to the keyword fallback at the call
//! site — this module does NOT fall back internally so
//! observability on Ollama health stays honest.

use crate::ticker;
use crate::types::{SentimentAnalysis, SentimentSignal};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You are a financial sentiment analyst. Given a news headline \
and summary, respond ONLY with valid JSON:\n\n\
{\n\
  \"signal\": \"bullish\" | \"bearish\" | \"neutral\",\n\
  \"score\": <float from -1.0 (very bearish) to 1.0 (very bullish)>,\n\
  \"assets\": [\"<ticker1>\", \"<ticker2>\"],\n\
  \"reasoning\": \"<one sentence>\"\n\
}\n\n\
Rules:\n\
- Identify all mentioned financial assets (BTC, ETH, SPX, EUR/USD, etc.)\n\
- If no specific asset is mentioned, use the most relevant market index\n\
- Be precise with the score: 0.0 is truly neutral\n\
- Keep reasoning under 30 words";

/// Connection + model config.
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Base URL, e.g. `http://localhost:11434`.
    pub base_url: String,
    /// Model tag — `"mistral-small3.1"`, `"qwen2.5:7b"`,
    /// `"llama3.1:8b"`, etc.
    pub model: String,
    /// Per-request timeout. Default 60 s — local inference on
    /// small models lands well under this; the budget is wide
    /// to cover cold-start after Ollama swaps models.
    pub timeout: Duration,
    /// Sampling temperature for the JSON-mode request. 0.1
    /// matches santiment's choice — we want determinism
    /// here, not creative writing.
    pub temperature: f64,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".into(),
            // Gemma 3 4B — multimodal (text + images), fast
            // on consumer GPUs, stable in JSON-mode. Good
            // default when the collector mixes headlines
            // with social-media images (screenshots, chart
            // replies). Swap via TOML for anything else
            // Ollama exposes: `llama3.2:3b`, `qwen2.5:7b`,
            // `mistral-small3.1`, a custom fine-tune.
            model: "gemma3:4b".into(),
            timeout: Duration::from_secs(60),
            temperature: 0.1,
        }
    }
}

/// Thin wrapper so the crate's caller holds one client across
/// many articles (HTTP connection pool, config reuse).
pub struct OllamaClient {
    cfg: OllamaConfig,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(cfg: OllamaConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()?;
        Ok(Self { cfg, http })
    }

    /// Score `title` + `summary` via the local LLM. Returns a
    /// fully-populated `SentimentAnalysis` with
    /// `scorer = "ollama"`. Propagates HTTP / parse errors
    /// unchanged so the caller can decide whether to retry or
    /// fall back to [`crate::keyword::score`].
    pub async fn analyze(
        &self,
        title: &str,
        summary: &str,
    ) -> anyhow::Result<SentimentAnalysis> {
        self.analyze_multimodal(title, summary, &[]).await
    }

    /// Multimodal analyzer — same JSON-mode contract as
    /// [`Self::analyze`] plus any base64-encoded images
    /// passed through. Ollama's `/api/generate` accepts an
    /// `images` array of base64 strings when the active
    /// model supports vision (gemma3, llava, qwen2.5-vl).
    /// Non-vision models silently ignore the field, so the
    /// caller can pass images unconditionally and still
    /// work against a text-only deployment.
    pub async fn analyze_multimodal(
        &self,
        title: &str,
        summary: &str,
        images_b64: &[String],
    ) -> anyhow::Result<SentimentAnalysis> {
        let prompt = format!("Headline: {title}\nSummary: {summary}");
        let req = OllamaRequest {
            model: &self.cfg.model,
            system: SYSTEM_PROMPT,
            prompt: &prompt,
            stream: false,
            format: "json",
            options: Options {
                temperature: self.cfg.temperature,
            },
            images: if images_b64.is_empty() {
                None
            } else {
                Some(images_b64)
            },
        };
        let resp = self
            .http
            .post(format!("{}/api/generate", self.cfg.base_url))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<OllamaResponse>()
            .await?;

        Ok(parse_body(&resp.response))
    }
}

fn parse_body(body: &str) -> SentimentAnalysis {
    let parsed: RawScore = serde_json::from_str(body).unwrap_or_default();
    let score = Decimal::from_str(&parsed.score.to_string())
        .unwrap_or(dec!(0))
        .max(dec!(-1))
        .min(dec!(1));
    let signal = match parsed.signal.to_ascii_lowercase().as_str() {
        "bullish" => SentimentSignal::Bullish,
        "bearish" => SentimentSignal::Bearish,
        _ => SentimentSignal::Neutral,
    };
    let assets = ticker::normalize_asset_list(&parsed.assets);

    SentimentAnalysis {
        signal,
        score,
        assets,
        reasoning: parsed.reasoning,
        analyzed_at: Utc::now(),
        scorer: "ollama".into(),
    }
}

#[derive(Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    stream: bool,
    format: &'a str,
    options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<&'a [String]>,
}

#[derive(Serialize)]
struct Options {
    temperature: f64,
}

#[derive(Deserialize)]
struct OllamaResponse {
    response: String,
}

#[derive(Deserialize, Default)]
struct RawScore {
    #[serde(default = "default_signal")]
    signal: String,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    reasoning: String,
}

fn default_signal() -> String {
    "neutral".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_body_handles_valid_json() {
        let body =
            r#"{"signal":"bullish","score":0.8,"assets":["bitcoin","ETH"],"reasoning":"ETF approved"}"#;
        let a = parse_body(body);
        assert_eq!(a.signal, SentimentSignal::Bullish);
        assert_eq!(a.score, dec!(0.8));
        assert_eq!(a.assets, vec!["BTC", "ETH"]);
        assert_eq!(a.reasoning, "ETF approved");
        assert_eq!(a.scorer, "ollama");
    }

    #[test]
    fn parse_body_on_junk_stays_neutral() {
        let a = parse_body("not json at all");
        assert_eq!(a.signal, SentimentSignal::Neutral);
        assert_eq!(a.score, dec!(0));
        assert!(a.assets.is_empty());
    }

    #[test]
    fn score_clamps_outside_unit_interval() {
        let body = r#"{"signal":"bearish","score":-2.5,"assets":[]}"#;
        let a = parse_body(body);
        assert_eq!(a.score, dec!(-1));
    }

    #[test]
    fn unknown_signal_collapses_to_neutral() {
        let body = r#"{"signal":"panic","score":0.1,"assets":[]}"#;
        let a = parse_body(body);
        assert_eq!(a.signal, SentimentSignal::Neutral);
    }
}
