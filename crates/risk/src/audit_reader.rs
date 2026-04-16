//! Audit log reader with date-range filtering (Epic 5 item 5.5).
//!
//! Reads the JSONL audit trail and returns events matching
//! time range, event type, and symbol filters. Supports signed
//! export for MiCA compliance.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::path::Path;
use tracing::info;

use crate::audit::{AuditEvent, AuditEventType};

type HmacSha256 = Hmac<Sha256>;

/// Read audit events within a date range.
pub fn read_audit_range(
    path: &Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<AuditEvent>> {
    let content = std::fs::read_to_string(path)?;
    let events: Vec<AuditEvent> = content
        .lines()
        .filter_map(|line| serde_json::from_str::<AuditEvent>(line).ok())
        .filter(|e| e.timestamp >= from && e.timestamp <= to)
        .collect();
    info!(count = events.len(), "read audit events in range");
    Ok(events)
}

/// Read audit events with optional filters.
pub fn read_audit_filtered(
    path: &Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    event_types: Option<&[AuditEventType]>,
    symbol: Option<&str>,
) -> anyhow::Result<Vec<AuditEvent>> {
    let content = std::fs::read_to_string(path)?;
    let events: Vec<AuditEvent> = content
        .lines()
        .filter_map(|line| serde_json::from_str::<AuditEvent>(line).ok())
        .filter(|e| e.timestamp >= from && e.timestamp <= to)
        .filter(|e| {
            event_types
                .as_ref()
                .is_none_or(|types| types.contains(&e.event_type))
        })
        .filter(|e| symbol.is_none_or(|s| e.symbol == s))
        .collect();
    Ok(events)
}

/// A signed audit export for MiCA compliance.
#[derive(Debug, Clone, Serialize)]
pub struct SignedAuditExport {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub event_count: usize,
    pub events: Vec<AuditEvent>,
    /// HMAC-SHA256 signature over the serialized events body.
    pub signature: String,
}

/// Create a signed audit export.
pub fn export_signed(
    events: Vec<AuditEvent>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    secret: &str,
) -> SignedAuditExport {
    let body = serde_json::to_string(&events).unwrap_or_default();
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(body.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    SignedAuditExport {
        from,
        to,
        event_count: events.len(),
        events,
        signature,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditEventType, AuditLog};
    use rust_decimal_macros::dec;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mm_test_audit_reader_{name}_{}.jsonl",
            std::process::id()
        ))
    }

    fn write_test_events(path: &Path) {
        let audit = AuditLog::new(path).unwrap();
        audit.risk_event("BTCUSDT", AuditEventType::EngineStarted, "test");
        audit.order_placed(
            "BTCUSDT",
            uuid::Uuid::new_v4(),
            mm_common::types::Side::Buy,
            dec!(50000),
            dec!(0.1),
        );
        audit.risk_event("ETHUSDT", AuditEventType::EngineStarted, "test eth");
        audit.flush();
    }

    #[test]
    fn read_range_returns_all_recent() {
        let p = tmp_path("range");
        write_test_events(&p);

        let events = read_audit_range(
            &p,
            Utc::now() - chrono::Duration::hours(1),
            Utc::now() + chrono::Duration::hours(1),
        )
        .unwrap();
        assert_eq!(events.len(), 3);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_filtered_by_symbol() {
        let p = tmp_path("symbol");
        write_test_events(&p);

        let events = read_audit_filtered(
            &p,
            Utc::now() - chrono::Duration::hours(1),
            Utc::now() + chrono::Duration::hours(1),
            None,
            Some("ETHUSDT"),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].symbol, "ETHUSDT");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_filtered_by_event_type() {
        let p = tmp_path("event_type");
        write_test_events(&p);

        let events = read_audit_filtered(
            &p,
            Utc::now() - chrono::Duration::hours(1),
            Utc::now() + chrono::Duration::hours(1),
            Some(&[AuditEventType::OrderPlaced]),
            None,
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn export_signed_produces_valid_hmac() {
        let events = vec![];
        let export = export_signed(
            events,
            Utc::now() - chrono::Duration::hours(1),
            Utc::now(),
            "test-secret",
        );
        assert_eq!(export.event_count, 0);
        assert!(!export.signature.is_empty());
        assert_eq!(export.signature.len(), 64); // SHA256 hex = 64 chars
    }
}
