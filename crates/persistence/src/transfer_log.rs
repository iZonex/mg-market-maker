//! S6.4 — cross-venue transfer log.
//!
//! Append-only JSONL sink for every operator-approved transfer
//! fired from the rebalancer advisory. Keeps a forensic trail
//! that pairs with the hash-chained audit log: the rebalancer
//! recommendation is advisory-only in memory, but the *decision
//! to execute* is recorded here so operators can answer "who
//! moved 1000 USDT from Binance to Bybit at 14:22, did it succeed,
//! and what was the venue transfer ID".
//!
//! Crash-safe: the writer flushes after every append and holds
//! no in-memory buffer past the line boundary. Reads are
//! line-delimited JSON so a partially-written tail entry (very
//! rare, since a single `write!` followed by `flush` is
//! atomic at the POSIX append level) appears as a parse error
//! on the next line, not a corrupted prior record.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Single transfer-attempt outcome. One row per operator click
/// on the Execute button, regardless of whether the venue
/// returned success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    /// UUIDv4 stamped server-side on accept. The dashboard surfaces
    /// it in the UI so operators can join the row with a venue
    /// ticket if they need to escalate.
    pub transfer_id: String,
    /// Wall-clock timestamp of acceptance.
    pub ts: DateTime<Utc>,
    /// Source venue name, lowercase (`"binance"`, `"bybit"`, …).
    pub from_venue: String,
    /// Destination venue name, lowercase.
    pub to_venue: String,
    /// Asset ticker (`"USDT"`, `"BTC"`). Upper-case preserved.
    pub asset: String,
    /// Amount the operator accepted for transfer.
    pub qty: Decimal,
    /// Optional source wallet (`"SPOT"`, `"FUNDING"`, …) for
    /// intra-venue transfers. `None` for cross-venue.
    pub from_wallet: Option<String>,
    /// Optional destination wallet, same shape as `from_wallet`.
    pub to_wallet: Option<String>,
    /// Free-text reason — usually a copy of the rebalancer's
    /// recommendation `reason` field so the row stands alone
    /// without having to cross-reference the advisory snapshot.
    pub reason: Option<String>,
    /// Who clicked Execute (auth subject). `"system"` for
    /// future auto-rebalancer, `"anonymous"` when auth is off.
    pub operator: String,
    /// Outcome. `"accepted"` = logged but not dispatched (cross-
    /// venue — needs manual handling). `"executed"` = the venue
    /// connector returned a transfer ID. `"failed"` = the venue
    /// rejected. `"rejected_kill_switch"` = refused pre-dispatch.
    pub status: TransferStatus,
    /// Venue's transfer ID when `status == Executed`.
    pub venue_tx_id: Option<String>,
    /// Error text when `status == Failed`.
    pub error: Option<String>,
}

/// Terminal outcome of an Execute click.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Intent logged, not dispatched (cross-venue in V1).
    Accepted,
    /// Venue acknowledged; `venue_tx_id` populated.
    Executed,
    /// Venue rejected; `error` carries the reason.
    Failed,
    /// Kill switch at L1+ — refused before dispatch.
    RejectedKillSwitch,
}

/// Append-only JSONL writer for `TransferRecord`s.
///
/// One file-wide mutex since the expected rate is ≤ a few rows
/// per day. No fsync on every write — the rows are forensic, not
/// transactional. A process crash between `write_all` and the OS
/// flush is acceptable; the transfer would re-appear if the
/// operator re-confirmed, and we'd rather never lose an
/// already-dispatched row than gate each click on a sync.
#[derive(Debug)]
pub struct TransferLogWriter {
    path: PathBuf,
    file: Mutex<File>,
}

impl TransferLogWriter {
    /// Open (creating if needed) the log at `path` for append.
    /// Parent directory must already exist — the caller is
    /// responsible for its creation, same contract the audit
    /// log uses.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Append one record. The `\n` terminator lets the JSONL
    /// reader split on newlines without any framing header.
    pub fn append(&self, record: &TransferRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = self.file.lock().expect("transfer log mutex poisoned");
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Filesystem path the writer is pointed at. Useful for
    /// diagnostics / the `/api/v1/system/diagnostics` endpoint.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read every record back from disk. For the dashboard panel
/// history view; not in any hot path.
pub fn read_all(path: impl AsRef<Path>) -> std::io::Result<Vec<TransferRecord>> {
    let file = File::open(path.as_ref())?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TransferRecord>(&line) {
            Ok(rec) => out.push(rec),
            Err(e) => {
                // A single bad row should not lose the rest; log
                // at the caller level and keep going. The reader
                // favours availability over strictness.
                tracing::warn!(line = %i + 1, error = %e, "transfer log parse error; skipping row");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn unique_tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mm-transfer-log-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(id: &str, status: TransferStatus) -> TransferRecord {
        TransferRecord {
            transfer_id: id.into(),
            ts: Utc::now(),
            from_venue: "binance".into(),
            to_venue: "bybit".into(),
            asset: "USDT".into(),
            qty: dec!(500),
            from_wallet: None,
            to_wallet: None,
            reason: Some("deficit on bybit".into()),
            operator: "alice".into(),
            status,
            venue_tx_id: None,
            error: None,
        }
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = unique_tmp_dir();
        let path = dir.join("transfers.jsonl");
        let w = TransferLogWriter::open(&path).unwrap();

        w.append(&sample("t-1", TransferStatus::Accepted)).unwrap();
        w.append(&sample("t-2", TransferStatus::Executed)).unwrap();
        w.append(&sample("t-3", TransferStatus::Failed)).unwrap();

        let rows = read_all(&path).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].transfer_id, "t-1");
        assert_eq!(rows[0].status, TransferStatus::Accepted);
        assert_eq!(rows[2].status, TransferStatus::Failed);
    }

    #[test]
    fn bad_row_is_skipped_not_fatal() {
        let dir = unique_tmp_dir();
        let path = dir.join("transfers.jsonl");
        {
            let w = TransferLogWriter::open(&path).unwrap();
            w.append(&sample("ok-1", TransferStatus::Accepted)).unwrap();
        }
        // Corrupt append outside the writer API.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{this is not json}\n")
            .unwrap();
        {
            let w = TransferLogWriter::open(&path).unwrap();
            w.append(&sample("ok-2", TransferStatus::Executed)).unwrap();
        }
        let rows = read_all(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].transfer_id, "ok-1");
        assert_eq!(rows[1].transfer_id, "ok-2");
    }
}
