use mm_common::types::{Balance, LiveOrder, OrderId};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

/// Balance reconciliation — compares internal state vs exchange state.
///
/// Critical for detecting:
/// - Missed fills (exchange filled but we didn't get the WS event)
/// - Orphaned orders (on exchange but not in our tracker)
/// - Phantom orders (in our tracker but not on exchange)
/// - Balance drift (internal != exchange)
#[derive(Debug, Clone)]
pub struct ReconciliationResult {
    /// Orders on exchange but not tracked internally.
    pub orphaned_orders: Vec<OrderId>,
    /// Orders tracked internally but not on exchange.
    pub phantom_orders: Vec<OrderId>,
    /// Balance mismatches: (asset, internal_available, exchange_available).
    pub balance_mismatches: Vec<(String, Decimal, Decimal)>,
    /// Was reconciliation successful (no critical mismatches)?
    pub is_clean: bool,
}

/// Reconcile internal state against exchange state.
///
/// Call this:
/// - On startup (load checkpoint + query exchange)
/// - On reconnect (after WS disconnect)
/// - Periodically (every 30-60 seconds)
pub fn reconcile_orders(
    internal_order_ids: &HashSet<OrderId>,
    exchange_orders: &[LiveOrder],
) -> ReconciliationResult {
    let exchange_ids: HashSet<OrderId> = exchange_orders.iter().map(|o| o.order_id).collect();

    let orphaned: Vec<OrderId> = exchange_ids
        .difference(internal_order_ids)
        .copied()
        .collect();
    let phantom: Vec<OrderId> = internal_order_ids
        .difference(&exchange_ids)
        .copied()
        .collect();

    if !orphaned.is_empty() {
        warn!(
            count = orphaned.len(),
            "ORPHANED ORDERS on exchange — not tracked internally"
        );
    }
    if !phantom.is_empty() {
        warn!(
            count = phantom.len(),
            "PHANTOM ORDERS in tracker — not on exchange (likely filled or cancelled)"
        );
    }

    let is_clean = orphaned.is_empty() && phantom.is_empty();
    if is_clean {
        info!("order reconciliation clean");
    }

    ReconciliationResult {
        orphaned_orders: orphaned,
        phantom_orders: phantom,
        balance_mismatches: vec![],
        is_clean,
    }
}

/// Reconcile balances.
pub fn reconcile_balances(
    internal: &HashMap<String, Decimal>,
    exchange: &[Balance],
    tolerance_pct: Decimal,
) -> Vec<(String, Decimal, Decimal)> {
    let mut mismatches = Vec::new();

    for eb in exchange {
        let internal_val = internal.get(&eb.asset).copied().unwrap_or(dec!(0));
        let exchange_val = eb.available;

        if exchange_val.is_zero() && internal_val.is_zero() {
            continue;
        }

        let diff = (internal_val - exchange_val).abs();
        let base = internal_val.max(exchange_val);
        let pct = if base.is_zero() {
            dec!(100)
        } else {
            diff / base * dec!(100)
        };

        if pct > tolerance_pct {
            warn!(
                asset = %eb.asset,
                internal = %internal_val,
                exchange = %exchange_val,
                diff_pct = %pct,
                "BALANCE MISMATCH"
            );
            mismatches.push((eb.asset.clone(), internal_val, exchange_val));
        }
    }

    if mismatches.is_empty() {
        info!("balance reconciliation clean");
    }

    mismatches
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::types::*;
    use uuid::Uuid;

    #[test]
    fn test_clean_reconciliation() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let internal: HashSet<OrderId> = [id1, id2].into();
        let exchange = vec![
            LiveOrder {
                order_id: id1,
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                price: dec!(50000),
                qty: dec!(0.01),
                filled_qty: dec!(0),
                status: OrderStatus::Open,
                created_at: chrono::Utc::now(),
            },
            LiveOrder {
                order_id: id2,
                symbol: "BTCUSDT".into(),
                side: Side::Sell,
                price: dec!(50100),
                qty: dec!(0.01),
                filled_qty: dec!(0),
                status: OrderStatus::Open,
                created_at: chrono::Utc::now(),
            },
        ];

        let result = reconcile_orders(&internal, &exchange);
        assert!(result.is_clean);
    }

    #[test]
    fn test_orphaned_order() {
        let id1 = Uuid::new_v4();
        let orphan = Uuid::new_v4();
        let internal: HashSet<OrderId> = [id1].into();
        let exchange = vec![
            LiveOrder {
                order_id: id1,
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                price: dec!(50000),
                qty: dec!(0.01),
                filled_qty: dec!(0),
                status: OrderStatus::Open,
                created_at: chrono::Utc::now(),
            },
            LiveOrder {
                order_id: orphan,
                symbol: "BTCUSDT".into(),
                side: Side::Sell,
                price: dec!(50100),
                qty: dec!(0.01),
                filled_qty: dec!(0),
                status: OrderStatus::Open,
                created_at: chrono::Utc::now(),
            },
        ];

        let result = reconcile_orders(&internal, &exchange);
        assert!(!result.is_clean);
        assert_eq!(result.orphaned_orders.len(), 1);
        assert_eq!(result.orphaned_orders[0], orphan);
    }

    // ── Property-based tests (Epic 23) ────────────────────────

    use proptest::prelude::*;

    fn mk_live(id: OrderId) -> LiveOrder {
        LiveOrder {
            order_id: id,
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(50_000),
            qty: dec!(0.01),
            filled_qty: dec!(0),
            status: OrderStatus::Open,
            created_at: chrono::Utc::now(),
        }
    }

    proptest! {
        /// reconcile_orders partitions order IDs exactly: every
        /// ID in `exchange ∖ internal` lands in `orphaned`, every
        /// `internal ∖ exchange` in `phantom`, and their union
        /// plus the intersection == both inputs.
        #[test]
        fn orders_partition_is_exact(
            internal_count in 0usize..15,
            exchange_count in 0usize..15,
            overlap_ratio in 0u32..100,
        ) {
            use std::collections::HashSet;
            let mut internal: HashSet<OrderId> = HashSet::new();
            let mut exchange: Vec<LiveOrder> = Vec::new();
            // Build up `internal` and `exchange` with a tunable
            // overlap — some IDs shared, some unique per side.
            for _ in 0..internal_count {
                internal.insert(Uuid::new_v4());
            }
            for id in &internal {
                if (id.as_u128() % 100) < overlap_ratio as u128 {
                    exchange.push(mk_live(*id));
                }
            }
            for _ in 0..exchange_count {
                exchange.push(mk_live(Uuid::new_v4()));
            }
            let result = reconcile_orders(&internal, &exchange);

            let exchange_ids: HashSet<OrderId> =
                exchange.iter().map(|o| o.order_id).collect();
            let expected_orphan: HashSet<OrderId> =
                exchange_ids.difference(&internal).copied().collect();
            let expected_phantom: HashSet<OrderId> =
                internal.difference(&exchange_ids).copied().collect();
            let got_orphan: HashSet<OrderId> =
                result.orphaned_orders.iter().copied().collect();
            let got_phantom: HashSet<OrderId> =
                result.phantom_orders.iter().copied().collect();

            prop_assert_eq!(got_orphan, expected_orphan);
            prop_assert_eq!(got_phantom, expected_phantom);
            prop_assert_eq!(result.is_clean,
                result.orphaned_orders.is_empty()
                && result.phantom_orders.is_empty());
        }

        /// Equal internal/exchange state always produces a clean
        /// reconciliation.
        #[test]
        fn equal_sets_are_clean(
            ids in proptest::collection::vec(0u128..1_000_000, 0..20),
        ) {
            use std::collections::HashSet;
            let uuids: Vec<OrderId> =
                ids.into_iter().map(Uuid::from_u128).collect();
            let internal: HashSet<OrderId> = uuids.iter().copied().collect();
            let exchange: Vec<LiveOrder> =
                uuids.iter().copied().map(mk_live).collect();
            let result = reconcile_orders(&internal, &exchange);
            prop_assert!(result.is_clean);
            prop_assert!(result.orphaned_orders.is_empty());
            prop_assert!(result.phantom_orders.is_empty());
        }

        /// reconcile_balances returns zero mismatches when the
        /// internal and exchange views agree exactly.
        #[test]
        fn balances_exact_match_clean(
            entries in proptest::collection::vec(
                ("[A-Z]{3,5}", 1i64..1_000_000_000),
                0..10,
            ),
        ) {
            // Dedupe by asset name so a duplicate entry from the
            // regex generator doesn't make the HashMap's last-write
            // disagree with the `exchange` Vec's first-write.
            let mut seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut internal: HashMap<String, Decimal> = HashMap::new();
            let mut exchange: Vec<Balance> = Vec::new();
            for (asset, qty_raw) in &entries {
                if !seen.insert(asset.clone()) {
                    continue;
                }
                let qty = Decimal::new(*qty_raw, 2);
                internal.insert(asset.clone(), qty);
                exchange.push(Balance {
                    asset: asset.clone(),
                    wallet: WalletType::Spot,
                    total: qty,
                    locked: dec!(0),
                    available: qty,
                });
            }
            let mismatches = reconcile_balances(&internal, &exchange, dec!(1));
            prop_assert!(mismatches.is_empty(),
                "unexpected mismatches {:?}", mismatches);
        }

        /// reconcile_balances flags drifts above tolerance.
        #[test]
        fn balances_flag_large_drift(
            base_raw in 10_000i64..1_000_000_000,
            drift_pct in 10u32..95,
        ) {
            let base = Decimal::new(base_raw, 2);
            let drift = base * Decimal::from(drift_pct) / dec!(100);
            let internal: HashMap<String, Decimal> = [("BTC".to_string(), base)].into();
            let exchange = vec![Balance {
                asset: "BTC".into(),
                wallet: WalletType::Spot,
                total: base - drift,
                locked: dec!(0),
                available: base - drift,
            }];
            // Tolerance 5 % — a drift of ≥10 % must flag.
            let mismatches = reconcile_balances(&internal, &exchange, dec!(5));
            prop_assert_eq!(mismatches.len(), 1);
            prop_assert_eq!(&mismatches[0].0, "BTC");
        }

        /// Zero-on-both-sides entries are ignored (no false positive
        /// on an empty slot).
        #[test]
        fn balances_skip_zero_zero(
            asset in "[A-Z]{3,5}",
        ) {
            let internal: HashMap<String, Decimal> =
                [(asset.clone(), dec!(0))].into();
            let exchange = vec![Balance {
                asset: asset.clone(),
                wallet: WalletType::Spot,
                total: dec!(0),
                locked: dec!(0),
                available: dec!(0),
            }];
            let mismatches = reconcile_balances(&internal, &exchange, dec!(0));
            prop_assert!(mismatches.is_empty());
        }
    }
}
