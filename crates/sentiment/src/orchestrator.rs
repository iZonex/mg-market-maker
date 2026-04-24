//! G1 — background orchestrator.
//!
//! One task per process. Owns the collectors, the Ollama
//! client, the keyword fallback, the mention counter, and the
//! output callback. One `tick()` call:
//!
//!   1. Fan out `collect()` across every configured source.
//!   2. Dedup by URL against an in-memory seen-set.
//!   3. Score each new article — Ollama first, keyword on
//!      failure.
//!   4. Fold into the `MentionCounter` per detected asset.
//!   5. For each monitored asset, call `snapshot_for` and
//!      hand the resulting `SentimentTick` to `emit`.
//!
//! The orchestrator is intentionally synchronous per-cycle —
//! parallel calls to Ollama would hit the local GPU queue
//! and stall faster than serial in our measurements. When
//! volume grows we'll batch prompts instead of fanning out.

use crate::bot_filter::{BotFilter, BotFilterConfig};
use crate::collector::Collector;
use crate::counter::MentionCounter;
use crate::keyword;
use crate::ollama::OllamaClient;
use crate::persistence::{ArticleRecord, ArticleWriter};
use crate::ticker::normalize_ticker;
use crate::types::{Article, SentimentAnalysis, SentimentTick};
use chrono::Utc;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Output sink for freshly-computed ticks. One call per asset
/// per cycle. Wrapped in `Arc` so the spawn closure can hold
/// a clone cheaply.
pub type TickSink = Arc<dyn Fn(SentimentTick) + Send + Sync + 'static>;

/// Observability hook fired once per analysed article.
/// Orchestrator doesn't know about Prometheus; it invokes
/// this callback with `(scorer_tag, asset_count)` so the
/// caller (server) can bump its own counters. `Arc<dyn Fn>`
/// matches the sink pattern.
pub type AnalyzeHook = Arc<dyn Fn(&str) + Send + Sync + 'static>;

pub struct Orchestrator {
    pub collectors: Vec<Box<dyn Collector>>,
    pub ollama: Option<OllamaClient>,
    pub counter: MentionCounter,
    pub seen_urls: HashSet<String>,
    pub monitored_assets: Vec<String>,
    pub sink: TickSink,
    /// Optional per-article hook for Prometheus counters.
    /// Called with the scorer tag (`"ollama"` / `"keyword"`)
    /// on every analysed article.
    pub analyze_hook: Option<AnalyzeHook>,
    /// Near-duplicate suppressor. Filters retweet storms +
    /// RSS syndication cascades so the mention rate reacts
    /// to genuinely new content, not repost waves. See
    /// [`crate::bot_filter`] for the algorithm.
    pub bot_filter: BotFilter,
    /// Optional JSONL writer — every analysed article lands
    /// as one record in the target file. Enabled via
    /// `[sentiment.persist_path]` in config. `None` keeps the
    /// pipeline entirely in-memory for trials / backtests.
    pub article_writer: Option<Arc<ArticleWriter>>,
    /// Upper bound on the seen-URL cache. Above this we evict
    /// the oldest entries in insertion order. Keeps memory
    /// bounded on long-running processes.
    pub seen_cap: usize,
    pub seen_order: std::collections::VecDeque<String>,
}

impl Orchestrator {
    pub fn new(
        collectors: Vec<Box<dyn Collector>>,
        ollama: Option<OllamaClient>,
        monitored_assets: Vec<String>,
        sink: TickSink,
    ) -> Self {
        Self {
            collectors,
            ollama,
            counter: MentionCounter::new(),
            seen_urls: HashSet::new(),
            monitored_assets,
            sink,
            analyze_hook: None,
            bot_filter: BotFilter::new(BotFilterConfig::default()),
            article_writer: None,
            seen_cap: 50_000,
            seen_order: std::collections::VecDeque::new(),
        }
    }

    /// Override the bot-filter config. Useful in tests +
    /// when operators tune the burst thresholds per venue.
    pub fn with_bot_filter_config(mut self, cfg: BotFilterConfig) -> Self {
        self.bot_filter = BotFilter::new(cfg);
        self
    }

    /// Attach a JSONL article writer. Every analysed article
    /// lands as one line. Combined with the shipper (which
    /// uploads tailed chunks on its cadence) this closes the
    /// article-level audit loop for compliance.
    pub fn with_article_writer(mut self, writer: Arc<ArticleWriter>) -> Self {
        self.article_writer = Some(writer);
        self
    }

    /// Attach a Prometheus hook (or similar telemetry
    /// callback). Fired once per analysed article with the
    /// scorer tag — `"ollama"` when the LLM returned
    /// cleanly, `"keyword"` on fallback.
    pub fn with_analyze_hook(mut self, hook: AnalyzeHook) -> Self {
        self.analyze_hook = Some(hook);
        self
    }

    fn remember(&mut self, url: &str) -> bool {
        if !self.seen_urls.insert(url.to_string()) {
            return false;
        }
        self.seen_order.push_back(url.to_string());
        while self.seen_order.len() > self.seen_cap {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen_urls.remove(&old);
            }
        }
        true
    }

    async fn score(&self, article: &Article) -> SentimentAnalysis {
        if let Some(client) = &self.ollama {
            match client.analyze(&article.title, &article.summary).await {
                Ok(a) => return a,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        url = %article.url,
                        "ollama analyze failed — falling back to keyword scorer"
                    );
                }
            }
        }
        keyword::score(&article.body())
    }

    /// One poll cycle. Returns how many new articles were
    /// analyzed (useful for logging + tests).
    pub async fn tick(&mut self) -> usize {
        // Collect from every source first, drop the
        // collectors borrow, then decide which URLs are
        // fresh. Keeps the borrow on `self.collectors`
        // disjoint from the mutation on `self.seen_urls`.
        let mut gathered: Vec<Article> = Vec::new();
        for c in self.collectors.iter() {
            gathered.extend(c.collect().await);
        }
        let mut fresh: Vec<Article> = Vec::with_capacity(gathered.len());
        for article in gathered {
            if article.url.is_empty() {
                continue;
            }
            if !self.remember(&article.url) {
                continue;
            }
            fresh.push(article);
        }
        let mut analyzed = 0usize;
        for article in fresh {
            let analysis = self.score(&article).await;
            analyzed += 1;
            if let Some(hook) = &self.analyze_hook {
                hook(&analysis.scorer);
            }
            if let Some(writer) = &self.article_writer {
                let rec = ArticleRecord {
                    article: article.clone(),
                    analysis: analysis.clone(),
                };
                if let Err(e) = writer.append(&rec) {
                    tracing::warn!(error = %e, "article writer append failed");
                }
            }
            let ts = article.published_at.unwrap_or(analysis.analyzed_at);
            // Bot filter — suppress retweet / syndication
            // cascades by normalised text hash. Articles
            // that don't pass still flow through the
            // counter for their *first* occurrence
            // (burst_cap = 1 by default) so a real signal
            // isn't lost, subsequent repeats just get
            // dropped before they move the rate.
            if !self.bot_filter.should_count(&article.title, Utc::now()) {
                tracing::debug!(
                    url = %article.url,
                    "bot filter suppressed near-duplicate"
                );
                continue;
            }
            // If the analyzer (keyword fallback, or a model
            // that returned an empty list) didn't give us
            // asset tags, run the deterministic extractor on
            // the raw text so mention counters still move.
            let assets: Vec<String> = if analysis.assets.is_empty() {
                crate::ticker::extract_tickers(&article.body(), &self.monitored_assets)
            } else {
                analysis.assets.clone()
            };
            for asset in &assets {
                self.counter.record(ts, asset, analysis.score);
            }
        }

        // Emit a tick for each monitored asset. This keeps
        // downstream broadcast deterministic (one entry per
        // asset per cycle) even when a cycle saw no articles
        // for that asset — the rate decays naturally, the
        // risk engine re-evaluates to neutral.
        let now = Utc::now();
        for raw_asset in &self.monitored_assets {
            let asset = normalize_ticker(raw_asset);
            if let Some(tick) = self.counter.snapshot_for(&asset, now) {
                (self.sink)(tick);
            }
        }

        analyzed
    }

    /// Blocking run loop — ticks on the given interval until
    /// the shutdown receiver fires.
    pub async fn run(
        mut self,
        interval: Duration,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick so the orchestrator
        // settles before hitting external endpoints.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match self.tick().await {
                        0 => tracing::debug!("sentiment tick — no new articles"),
                        n => tracing::info!(
                            analyzed = n,
                            assets = self.monitored_assets.len(),
                            "sentiment tick"
                        ),
                    }
                }
                _ = shutdown.changed() => {
                    tracing::info!("sentiment orchestrator shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::Collector;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct MockCollector {
        articles: Vec<Article>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Collector for MockCollector {
        fn name(&self) -> &'static str {
            "mock"
        }
        async fn collect(&self) -> Vec<Article> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.articles.clone()
        }
    }

    #[tokio::test]
    async fn tick_scores_new_articles_and_dedups() {
        let articles = vec![
            Article {
                url: "https://example.com/a".into(),
                title: "Bitcoin rally continues".into(),
                summary: "bullish surge".into(),
                source: "mock".into(),
                published_at: Some(Utc::now()),
                collected_at: Utc::now(),
            },
            Article {
                url: "https://example.com/b".into(),
                title: "ETH hack reported".into(),
                summary: "bearish exploit".into(),
                source: "mock".into(),
                published_at: Some(Utc::now()),
                collected_at: Utc::now(),
            },
        ];
        let mc = Box::new(MockCollector {
            articles: articles.clone(),
            calls: AtomicUsize::new(0),
        });
        let captured: Arc<Mutex<Vec<SentimentTick>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_captured = captured.clone();
        let sink: TickSink = Arc::new(move |tick| {
            sink_captured.lock().unwrap().push(tick);
        });
        let mut orch = Orchestrator::new(
            vec![mc],
            None, // no Ollama — forces keyword fallback
            vec!["BTC".into(), "ETH".into()],
            sink,
        );

        // First tick: analyzes both articles.
        let n = orch.tick().await;
        assert_eq!(n, 2);

        // Second tick: same URLs, no new analysis.
        let n2 = orch.tick().await;
        assert_eq!(n2, 0);

        // Output contains ticks for both monitored assets.
        let caught = captured.lock().unwrap();
        let assets: Vec<&str> = caught.iter().map(|t| t.asset.as_str()).collect();
        assert!(assets.contains(&"BTC"));
        assert!(assets.contains(&"ETH"));
    }
}
