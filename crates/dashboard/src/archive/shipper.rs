//! Background shipper — uploads new tail chunks of local
//! artefacts to S3 on a fixed cadence.
//!
//! Byte offsets per file are persisted to
//! `data/archive_offsets.json` so the shipper picks up
//! exactly where it left off across restarts. An uploaded
//! chunk is named
//! `{prefix}/{logical}/{yyyy}/{mm}/{dd}/{hh}-{offset_hex}.jsonl`
//! so auditors can reassemble the stream by sorted-key
//! listing, and the `.hex` suffix makes duplicates obvious
//! (same offset → same content).

use crate::archive::ArchiveClient;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

/// Shipper configuration.
///
/// Not serialisable — built by the server from `AppConfig` at
/// startup. Each source file is optional so operators can keep
/// only fills (e.g. quant team archive) without also shipping
/// the full audit log.
#[derive(Clone)]
pub struct ShipperConfig {
    pub interval: Duration,
    pub audit_log: Option<PathBuf>,
    pub fill_log: Option<PathBuf>,
    pub daily_reports_dir: Option<PathBuf>,
    /// Epic G — optional path to the sentiment article JSONL
    /// produced by `mm-sentiment::persistence::ArticleWriter`.
    /// Shipped on the same delta-tailing contract as audit +
    /// fills.
    pub sentiment_log: Option<PathBuf>,
    pub offset_file: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OffsetState {
    /// Byte offsets, keyed by logical stream name.
    /// Using a string key instead of the file path so a
    /// deployment that relocates `data/` doesn't re-upload
    /// everything.
    offsets: HashMap<String, u64>,
}

impl OffsetState {
    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Spawn the shipper loop. Returns the `JoinHandle` so the
/// server can await graceful shutdown at SIGTERM.
pub fn spawn(client: ArchiveClient, cfg: ShipperConfig) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut state = OffsetState::load(&cfg.offset_file);
        let mut ticker = tokio::time::interval(cfg.interval);
        // Skip the first immediate tick so boot doesn't stampede
        // into a cold S3 client.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = run_once(&client, &cfg, &mut state).await {
                tracing::warn!(error = %e, "archive shipper tick failed");
            }
            if let Err(e) = state.save(&cfg.offset_file) {
                tracing::warn!(error = %e, "archive offsets save failed");
            }
        }
    })
}

async fn run_once(
    client: &ArchiveClient,
    cfg: &ShipperConfig,
    state: &mut OffsetState,
) -> anyhow::Result<()> {
    if let Some(p) = &cfg.audit_log {
        ship_tail(client, "audit", p, state, "application/x-ndjson").await?;
    }
    if let Some(p) = &cfg.fill_log {
        ship_tail(client, "fills", p, state, "application/x-ndjson").await?;
    }
    if let Some(p) = &cfg.sentiment_log {
        ship_tail(client, "sentiment", p, state, "application/x-ndjson").await?;
    }
    if let Some(dir) = &cfg.daily_reports_dir {
        ship_daily_snapshots(client, dir, state).await?;
    }
    Ok(())
}

/// Stream the tail of a JSONL file starting at the last
/// uploaded offset. Reads the full delta into memory — fine
/// for the shipper interval (1 h default) because audit
/// logs are kilobytes per hour, not megabytes. Use
/// multipart upload if this ever needs to scale to GB-scale
/// deltas.
async fn ship_tail(
    client: &ArchiveClient,
    stream: &str,
    path: &Path,
    state: &mut OffsetState,
    content_type: &str,
) -> anyhow::Result<()> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(_) => return Ok(()), // file doesn't exist yet — nothing to ship
    };
    let current_offset = state.offsets.get(stream).copied().unwrap_or(0);
    let end = meta.len();
    if end <= current_offset {
        return Ok(()); // nothing new
    }

    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(current_offset)).await?;
    let delta_len = (end - current_offset) as usize;
    let mut buf = Vec::with_capacity(delta_len);
    file.take(delta_len as u64).read_to_end(&mut buf).await?;

    let now = Utc::now();
    let key = format!(
        "{stream}/{y:04}/{m:02}/{d:02}/{h:02}-{off:016x}.jsonl",
        y = now.year_ce().1,
        m = now.month(),
        d = now.day(),
        h = now.hour(),
        off = current_offset,
    );
    let uploaded = match client.put_object(&key, buf, content_type).await {
        Ok(k) => k,
        Err(e) => {
            crate::metrics::ARCHIVE_UPLOAD_ERRORS_TOTAL
                .with_label_values(&[stream])
                .inc();
            return Err(e);
        }
    };
    tracing::info!(
        stream,
        key = %uploaded,
        bytes = delta_len,
        "archive shipper uploaded chunk"
    );
    crate::metrics::ARCHIVE_UPLOADS_TOTAL
        .with_label_values(&[stream])
        .inc();
    crate::metrics::ARCHIVE_UPLOAD_BYTES_TOTAL
        .with_label_values(&[stream])
        .inc_by(delta_len as u64);
    crate::metrics::ARCHIVE_LAST_SUCCESS_TS
        .with_label_values(&[stream])
        .set(chrono::Utc::now().timestamp() as f64);
    state.offsets.insert(stream.to_string(), end);
    Ok(())
}

/// Daily report snapshots are atomic JSON files — a shipped
/// file never needs a delta. We remember the set of already-
/// uploaded filenames in the same offset state (reusing the
/// hashmap: key = `daily:{filename}`, value = 1).
async fn ship_daily_snapshots(
    client: &ArchiveClient,
    dir: &Path,
    state: &mut OffsetState,
) -> anyhow::Result<()> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.ends_with(".json") {
            continue;
        }
        let key = format!("daily:{name}");
        if state.offsets.contains_key(&key) {
            continue;
        }
        let body = tokio::fs::read(&path).await?;
        let body_len = body.len() as u64;
        let s3_key = format!("reports/daily/{name}");
        match client.put_object(&s3_key, body, "application/json").await {
            Ok(_) => {
                crate::metrics::ARCHIVE_UPLOADS_TOTAL
                    .with_label_values(&["daily"])
                    .inc();
                crate::metrics::ARCHIVE_UPLOAD_BYTES_TOTAL
                    .with_label_values(&["daily"])
                    .inc_by(body_len);
                crate::metrics::ARCHIVE_LAST_SUCCESS_TS
                    .with_label_values(&["daily"])
                    .set(chrono::Utc::now().timestamp() as f64);
                state.offsets.insert(key, 1);
            }
            Err(e) => {
                crate::metrics::ARCHIVE_UPLOAD_ERRORS_TOTAL
                    .with_label_values(&["daily"])
                    .inc();
                return Err(e);
            }
        }
    }
    Ok(())
}

// Re-export the timezone-aware helpers chrono puts behind the
// `Datelike` / `Timelike` traits. The shipper only needs year /
// month / day / hour so these two are enough.
use chrono::{Datelike, Timelike};
