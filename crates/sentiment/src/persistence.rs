//! Append-only JSONL writer for analysed articles.
//!
//! One line per article: `{ article, analysis }` — the raw
//! record (url / title / summary / source / timestamps) plus
//! the analyzer output (signal / score / assets / reasoning
//! / scorer tag). This mirrors the audit-log pattern the rest
//! of the workspace follows (hash chain lives separately on
//! `risk::audit`; here we keep it simple since the data is
//! not privileged).
//!
//! Operational intent: when `[archive]` is also configured
//! the shipper uploads this file to S3 on the same cadence as
//! audit + fills. A regulator asking "what news was the
//! desk watching on 2026-04-17?" answers via one presigned
//! URL.

use crate::types::{Article, SentimentAnalysis};
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// One record persisted per analysed article. Wraps both
/// halves in a single envelope so the JSONL is grep-friendly
/// (`jq 'select(.analysis.signal=="bearish")'` etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleRecord {
    pub article: Article,
    pub analysis: SentimentAnalysis,
}

/// Thread-safe JSONL writer. Constructed once, `Arc`-shared
/// across the orchestrator; each `append` acquires the mutex,
/// writes + flushes one line, releases. Flush every write
/// matches the audit-log discipline — we trade a little
/// throughput for a strict "nothing analysed is ever lost on
/// crash" invariant.
pub struct ArticleWriter {
    path: PathBuf,
    inner: Mutex<BufWriter<std::fs::File>>,
}

impl ArticleWriter {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            inner: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn append(&self, rec: &ArticleRecord) -> anyhow::Result<()> {
        let line = serde_json::to_string(rec)?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("article writer mutex poisoned"))?;
        guard.write_all(line.as_bytes())?;
        guard.write_all(b"\n")?;
        guard.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SentimentSignal;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    #[test]
    fn round_trips_through_disk() {
        let tmp = std::env::temp_dir().join(format!("mm_articles_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let w = ArticleWriter::new(tmp.clone()).expect("open");
        let rec = ArticleRecord {
            article: Article {
                url: "https://example.com/x".into(),
                title: "T".into(),
                summary: "S".into(),
                source: "mock".into(),
                published_at: Some(Utc::now()),
                collected_at: Utc::now(),
            },
            analysis: SentimentAnalysis {
                signal: SentimentSignal::Bullish,
                score: dec!(0.4),
                assets: vec!["BTC".into()],
                reasoning: "r".into(),
                analyzed_at: Utc::now(),
                scorer: "keyword".into(),
            },
        };
        w.append(&rec).expect("append");
        w.append(&rec).expect("append again");

        let body = std::fs::read_to_string(&tmp).expect("read back");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: ArticleRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.article.url, "https://example.com/x");
        assert_eq!(parsed.analysis.signal, SentimentSignal::Bullish);
        let _ = std::fs::remove_file(&tmp);
    }
}
