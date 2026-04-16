//! Transfer log persistence (Epic 4 item 4.7).
//!
//! Append-only JSONL log of all cross-venue transfers (withdrawals
//! and internal transfers). Supports date-range queries for the
//! dashboard transfer history endpoint.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// A recorded transfer event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub venue: String,
    pub direction: TransferDirection,
    pub asset: String,
    pub qty: Decimal,
    #[serde(default)]
    pub from_wallet: Option<String>,
    #[serde(default)]
    pub to_wallet: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    pub status: TransferStatus,
    #[serde(default)]
    pub venue_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Withdraw,
    InternalTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Pending,
    Confirmed,
    Failed,
}

/// Append a transfer record to the JSONL log.
pub fn append(path: &Path, record: &TransferRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let json = serde_json::to_string(record).map_err(std::io::Error::other)?;
    writeln!(file, "{}", json)
}

/// Read transfer records within a date range.
pub fn read_range(
    path: &Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<TransferRecord> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<TransferRecord>(line).ok())
        .filter(|r| r.timestamp >= from && r.timestamp <= to)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_record() -> TransferRecord {
        TransferRecord {
            id: "tx-001".into(),
            timestamp: Utc::now(),
            venue: "binance".into(),
            direction: TransferDirection::InternalTransfer,
            asset: "USDT".into(),
            qty: dec!(1000),
            from_wallet: Some("SPOT".into()),
            to_wallet: Some("FUTURES".into()),
            address: None,
            network: None,
            status: TransferStatus::Confirmed,
            venue_id: Some("12345".into()),
        }
    }

    #[test]
    fn serde_roundtrip() {
        let r = sample_record();
        let json = serde_json::to_string(&r).unwrap();
        let parsed: TransferRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "tx-001");
        assert_eq!(parsed.direction, TransferDirection::InternalTransfer);
    }

    #[test]
    fn append_and_read() {
        let p = std::env::temp_dir().join(format!(
            "mm_test_transfer_{}.jsonl",
            std::process::id()
        ));
        let r = sample_record();
        append(&p, &r).unwrap();

        let records = read_range(
            &p,
            Utc::now() - chrono::Duration::hours(1),
            Utc::now() + chrono::Duration::hours(1),
        );
        assert_eq!(records.len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_range_filters_by_date() {
        let p = std::env::temp_dir().join(format!(
            "mm_test_transfer_range_{}.jsonl",
            std::process::id()
        ));
        let r = sample_record();
        append(&p, &r).unwrap();

        // Query a range that excludes the record.
        let records = read_range(
            &p,
            Utc::now() + chrono::Duration::hours(1),
            Utc::now() + chrono::Duration::hours(2),
        );
        assert!(records.is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
