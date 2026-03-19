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
}
