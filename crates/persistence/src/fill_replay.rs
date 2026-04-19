//! Fill replay from audit log (Epic 7 item 7.4).
//!
//! Parses `OrderFilled` events from the JSONL audit trail and
//! reconstructs inventory + PnL state. Used to validate
//! checkpoint accuracy after a crash.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;
use tracing::{info, warn};

use crate::checkpoint::SymbolCheckpoint;

/// Result of replaying fills from an audit log.
#[derive(Debug, Clone)]
pub struct FillReplayResult {
    /// Reconstructed net inventory.
    pub inventory: Decimal,
    /// Reconstructed realized PnL (simplified spread capture).
    pub realized_pnl: Decimal,
    /// Total fills replayed.
    pub fill_count: u64,
    /// Total volume.
    pub total_volume: Decimal,
    /// Timestamp of last fill in the log.
    pub last_fill_at: Option<DateTime<Utc>>,
}

/// Minimal audit event shape for fill replay (only fields we need).
#[derive(Debug, Deserialize)]
struct AuditEventSlim {
    event_type: String,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    price: Option<Decimal>,
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    timestamp: Option<DateTime<Utc>>,
}

/// Replay fills from an audit JSONL file.
///
/// Reads `data/audit/{symbol}.jsonl` line by line, filters for
/// `order_filled` events, and reconstructs inventory from the
/// fill sequence. Returns `None` if the file doesn't exist or
/// contains no fills.
pub fn replay_fills_from_audit(audit_path: &Path) -> Option<FillReplayResult> {
    let content = std::fs::read_to_string(audit_path).ok()?;
    let mut inventory = Decimal::ZERO;
    let mut realized_pnl = Decimal::ZERO;
    let mut total_volume = Decimal::ZERO;
    let mut fill_count = 0u64;
    let mut last_fill_at = None;
    let mut last_mid = Decimal::ZERO;

    for line in content.lines() {
        let Ok(event) = serde_json::from_str::<AuditEventSlim>(line) else {
            continue;
        };
        if event.event_type != "order_filled" {
            continue;
        }
        let Some(side) = &event.side else { continue };
        let Some(price) = event.price else { continue };
        let Some(qty) = event.qty else { continue };

        let fill_value = price * qty;
        total_volume += fill_value;
        fill_count += 1;
        last_fill_at = event.timestamp;

        // Simplified spread capture vs last known mid.
        if last_mid > Decimal::ZERO {
            let capture = match side.as_str() {
                "Buy" => (last_mid - price) * qty,
                "Sell" => (price - last_mid) * qty,
                _ => Decimal::ZERO,
            };
            realized_pnl += capture;
        }

        match side.as_str() {
            "Buy" => inventory += qty,
            "Sell" => inventory -= qty,
            _ => {}
        }

        // Update mid estimate from fill price.
        last_mid = price;
    }

    if fill_count == 0 {
        return None;
    }

    info!(
        fills = fill_count,
        inventory = %inventory,
        pnl = %realized_pnl,
        "replayed fills from audit log"
    );

    Some(FillReplayResult {
        inventory,
        realized_pnl,
        fill_count,
        total_volume,
        last_fill_at,
    })
}

/// Validate a checkpoint against replay results.
/// Returns a list of discrepancies. Empty = consistent.
pub fn validate_checkpoint_against_replay(
    checkpoint: &SymbolCheckpoint,
    replay: &FillReplayResult,
    tolerance: Decimal,
) -> Vec<String> {
    let mut issues = Vec::new();
    let inv_diff = (checkpoint.inventory - replay.inventory).abs();
    if inv_diff > tolerance {
        issues.push(format!(
            "inventory mismatch: checkpoint={}, replay={}, diff={}",
            checkpoint.inventory, replay.inventory, inv_diff
        ));
    }
    if checkpoint.total_fills != replay.fill_count {
        issues.push(format!(
            "fill count mismatch: checkpoint={}, replay={}",
            checkpoint.total_fills, replay.fill_count
        ));
    }
    if !issues.is_empty() {
        warn!(
            issues = issues.len(),
            "checkpoint vs replay validation found discrepancies"
        );
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::io::Write;

    fn write_audit_line(f: &mut std::fs::File, side: &str, price: &str, qty: &str) {
        let line = format!(
            r#"{{"seq":1,"timestamp":"2026-04-16T00:00:00Z","event_type":"order_filled","symbol":"BTCUSDT","side":"{side}","price":{price},"qty":{qty}}}"#
        );
        writeln!(f, "{}", line).unwrap();
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mm_test_replay_{name}_{}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn replay_empty_file_returns_none() {
        let p = tmp_path("empty");
        std::fs::write(&p, "").unwrap();
        assert!(replay_fills_from_audit(&p).is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn replay_reconstructs_inventory() {
        let p = tmp_path("inventory");
        let mut f = std::fs::File::create(&p).unwrap();
        write_audit_line(&mut f, "Buy", "50000", "0.1");
        write_audit_line(&mut f, "Buy", "50100", "0.05");
        write_audit_line(&mut f, "Sell", "50200", "0.03");
        drop(f);

        let result = replay_fills_from_audit(&p).unwrap();
        assert_eq!(result.fill_count, 3);
        // 0.1 + 0.05 - 0.03 = 0.12
        assert_eq!(result.inventory, dec!(0.12));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn replay_ignores_non_fill_events() {
        let p = tmp_path("non_fill");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(
            f,
            r#"{{"seq":1,"event_type":"engine_started","symbol":"BTCUSDT"}}"#
        )
        .unwrap();
        write_audit_line(&mut f, "Buy", "50000", "1");
        writeln!(
            f,
            r#"{{"seq":3,"event_type":"order_cancelled","symbol":"BTCUSDT"}}"#
        )
        .unwrap();
        drop(f);

        let result = replay_fills_from_audit(&p).unwrap();
        assert_eq!(result.fill_count, 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_checkpoint_clean() {
        let cp = SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0.12),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(1000),
            total_fills: 3,
            inflight_atomic_bundles: Vec::new(),
        };
        let replay = FillReplayResult {
            inventory: dec!(0.12),
            realized_pnl: dec!(0),
            fill_count: 3,
            total_volume: dec!(1000),
            last_fill_at: None,
        };
        let issues = validate_checkpoint_against_replay(&cp, &replay, dec!(0.001));
        assert!(issues.is_empty());
    }

    #[test]
    fn validate_checkpoint_catches_drift() {
        let cp = SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(1.0),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(1000),
            total_fills: 5,
            inflight_atomic_bundles: Vec::new(),
        };
        let replay = FillReplayResult {
            inventory: dec!(0.5),
            realized_pnl: dec!(0),
            fill_count: 3,
            total_volume: dec!(500),
            last_fill_at: None,
        };
        let issues = validate_checkpoint_against_replay(&cp, &replay, dec!(0.001));
        assert!(issues.len() >= 2); // inventory + fill count
    }

    // ── Property-based tests (Epic 20) ────────────────────────

    use proptest::prelude::*;

    fn write_fill_lines(f: &mut std::fs::File, fills: &[(bool, i64, i64)]) {
        for (buy, price_raw, qty_raw) in fills {
            let side = if *buy { "Buy" } else { "Sell" };
            let line = format!(
                r#"{{"seq":1,"event_type":"order_filled","symbol":"BTCUSDT","side":"{}","price":{},"qty":{}}}"#,
                side,
                Decimal::new(*price_raw, 2),
                Decimal::new(*qty_raw, 4),
            );
            writeln!(f, "{}", line).unwrap();
        }
    }

    proptest! {
        /// fill_count from replay equals the number of order_filled
        /// events written. Non-fill lines don't inflate it.
        #[test]
        fn fill_count_equals_written_fills(
            fills in proptest::collection::vec(
                (any::<bool>(), 10_000i64..1_000_000, 1i64..10_000),
                1..25,
            ),
        ) {
            let p = tmp_path(&format!("prop_count_{}", rand_suffix()));
            let mut f = std::fs::File::create(&p).unwrap();
            write_fill_lines(&mut f, &fills);
            drop(f);
            let r = replay_fills_from_audit(&p).unwrap();
            prop_assert_eq!(r.fill_count, fills.len() as u64);
            let _ = std::fs::remove_file(&p);
        }

        /// Inventory equals Σ(sign × qty). Catches any regression
        /// that flips the accumulation direction.
        #[test]
        fn inventory_matches_signed_sum(
            fills in proptest::collection::vec(
                (any::<bool>(), 10_000i64..1_000_000, 1i64..10_000),
                1..25,
            ),
        ) {
            let p = tmp_path(&format!("prop_inv_{}", rand_suffix()));
            let mut f = std::fs::File::create(&p).unwrap();
            write_fill_lines(&mut f, &fills);
            drop(f);
            let r = replay_fills_from_audit(&p).unwrap();
            let expected: Decimal = fills.iter().map(|(buy, _, q)| {
                let qty = Decimal::new(*q, 4);
                if *buy { qty } else { -qty }
            }).sum();
            prop_assert_eq!(r.inventory, expected);
            let _ = std::fs::remove_file(&p);
        }

        /// total_volume equals Σ(price × qty). Catches a
        /// regression that uses only qty or only notional.
        #[test]
        fn total_volume_matches_product_sum(
            fills in proptest::collection::vec(
                (any::<bool>(), 10_000i64..1_000_000, 1i64..10_000),
                1..20,
            ),
        ) {
            let p = tmp_path(&format!("prop_vol_{}", rand_suffix()));
            let mut f = std::fs::File::create(&p).unwrap();
            write_fill_lines(&mut f, &fills);
            drop(f);
            let r = replay_fills_from_audit(&p).unwrap();
            let expected: Decimal = fills.iter().map(|(_, pr, q)| {
                Decimal::new(*pr, 2) * Decimal::new(*q, 4)
            }).sum();
            prop_assert_eq!(r.total_volume, expected);
            let _ = std::fs::remove_file(&p);
        }

        /// validate_checkpoint_against_replay: exact match produces
        /// zero issues. Catches a regression where the validator
        /// always reports a false positive.
        #[test]
        fn validate_exact_match_has_no_issues(
            inv_raw in -1_000_000i64..1_000_000,
            fills in 0u64..10_000,
            vol_raw in 0i64..1_000_000,
        ) {
            let inv = Decimal::new(inv_raw, 4);
            let cp = SymbolCheckpoint {
                symbol: "X".into(),
                inventory: inv,
                avg_entry_price: dec!(50000),
                open_order_ids: vec![],
                realized_pnl: dec!(0),
                total_volume: Decimal::new(vol_raw, 2),
                total_fills: fills,
                inflight_atomic_bundles: Vec::new(),
            };
            let r = FillReplayResult {
                inventory: inv,
                realized_pnl: dec!(0),
                fill_count: fills,
                total_volume: Decimal::new(vol_raw, 2),
                last_fill_at: None,
            };
            let issues = validate_checkpoint_against_replay(&cp, &r, dec!(0));
            prop_assert!(issues.is_empty(), "expected clean, got {:?}", issues);
        }

        /// Fill count mismatch always surfaces an issue.
        #[test]
        fn fill_count_mismatch_flagged(
            cp_fills in 0u64..1000,
            delta in 1u64..500,
        ) {
            let cp = SymbolCheckpoint {
                symbol: "X".into(),
                inventory: dec!(0),
                avg_entry_price: dec!(0),
                open_order_ids: vec![],
                realized_pnl: dec!(0),
                total_volume: dec!(0),
                total_fills: cp_fills + delta,
                inflight_atomic_bundles: Vec::new(),
            };
            let r = FillReplayResult {
                inventory: dec!(0),
                realized_pnl: dec!(0),
                fill_count: cp_fills,
                total_volume: dec!(0),
                last_fill_at: None,
            };
            let issues = validate_checkpoint_against_replay(&cp, &r, dec!(0));
            prop_assert!(!issues.is_empty());
        }
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}
