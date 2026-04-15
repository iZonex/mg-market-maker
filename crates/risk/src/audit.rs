use chrono::{DateTime, Utc};
use mm_common::types::{OrderId, Price, Qty, Side};
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use tracing::error;

/// Structured audit trail — append-only log of all market maker actions.
///
/// MiCA requirements:
/// - 5-year retention (7 on request)
/// - Chronological, searchable
/// - Microsecond timestamps
/// - Full order lifecycle (place → fill/cancel)
///
/// Format: JSONL (one JSON object per line, append-only).
pub struct AuditLog {
    writer: Mutex<std::io::BufWriter<std::fs::File>>,
    sequence: std::sync::atomic::AtomicU64,
}

/// An audit event — every action the MM takes.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<OrderId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<Qty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    // Order lifecycle.
    OrderPlaced,
    OrderCancelled,
    OrderAmended,
    OrderFilled,
    OrderRejected,

    // Risk events.
    CircuitBreakerTripped,
    KillSwitchEscalated,
    KillSwitchReset,
    InventoryLimitHit,

    // Connectivity.
    ExchangeConnected,
    ExchangeDisconnected,
    ExchangeReconnected,

    // System.
    EngineStarted,
    EngineShutdown,
    ConfigLoaded,
    CheckpointSaved,
    BalanceReconciled,

    // SLA.
    SlaViolation,

    // Strategy.
    StrategyQuoteRefresh,
    RegimeChange,

    // Compliance / surveillance.
    /// Periodic Order-to-Trade Ratio snapshot, emitted every
    /// aggregation window. `detail` carries the numeric ratio
    /// so regulators can reconstruct the time series from the
    /// audit trail alone. MiCA compliance signal.
    OrderToTradeRatioSnapshot,

    /// Reconciliation detected a drift between the tracked
    /// `InventoryManager.inventory()` and the wallet balance
    /// delta since engine start. `detail` carries the signed
    /// drift, baseline wallet, current wallet, and whether the
    /// tracker was force-corrected. Fires on every reconcile
    /// cycle where the drift exceeds the configured
    /// tolerance; reset on manual intervention.
    InventoryDriftDetected,

    // Cross-product pair dispatch (funding arb / basis trade).
    /// Atomic pair dispatch succeeded — both legs placed.
    PairDispatchEntered,
    /// Atomic pair dispatch exited cleanly — both legs reversed.
    PairDispatchExited,
    /// Taker leg rejected before any position committed. Safe,
    /// no exposure.
    PairTakerRejected,
    /// Maker leg rejected after taker leg filled. The executor
    /// fires a compensating market reversal; `detail` says
    /// whether the compensation itself succeeded. An
    /// uncompensated break is the cue to escalate the kill
    /// switch to L2 StopNewOrders.
    PairBreak,

    /// Cross-venue basis crossed below the entry threshold —
    /// `detail` carries the signed basis bps and the venue
    /// pair. Emitted by the engine the first refresh tick
    /// after the threshold flips. P1.4 stage-1.
    CrossVenueBasisEntered,
    /// Cross-venue basis crossed above the exit threshold —
    /// `detail` carries the signed basis bps and the venue
    /// pair. Emitted exactly once per round-trip so the audit
    /// trail records both sides of every cross-venue position.
    CrossVenueBasisExited,

    // Pair lifecycle (P2.3 stage-1).
    /// Lifecycle manager observed the symbol for the first
    /// time on its periodic refresh. `detail` carries the
    /// snapshot's tick/lot/min_notional.
    PairLifecycleListed,
    /// Venue removed the symbol entirely (or `get_product_spec`
    /// returned "symbol not found"). Engine cancels every order
    /// and refuses to ever requote until restart.
    PairLifecycleDelisted,
    /// `trading_status` flipped from Trading to Halted /
    /// Break / PreTrading. Engine cancels all + paused.
    PairLifecycleHalted,
    /// `trading_status` flipped back to Trading. Engine
    /// clears the paused flag.
    PairLifecycleResumed,
    /// Tick or lot size changed without a status flip.
    /// Engine updates `self.product` in place and re-rounds
    /// the next quote refresh.
    PairLifecycleTickLotChanged,
    /// `min_notional` changed without a status or tick/lot
    /// flip. Surfaced separately so the audit trail records
    /// exactly what moved.
    PairLifecycleMinNotionalChanged,

    // Epic C — Portfolio-level risk view.
    /// Hedge optimizer produced a non-empty basket. `detail`
    /// carries the basket summary (symbols + signed qty).
    /// Emitted on every refresh tick where the optimizer
    /// recommends a non-trivial hedge — operators can
    /// reconstruct the hedge path from the audit trail alone.
    HedgeBasketRecommended,
    /// Per-strategy VaR guard dropped the throttle below 1.0
    /// for a strategy class. `detail` carries the strategy
    /// class + the new throttle multiplier. Emitted only on
    /// throttle **transitions** so the audit log isn't
    /// spammed on every refresh tick while the throttle is
    /// stable.
    VarGuardThrottleApplied,

    // Epic A — Cross-venue Smart Order Router.
    /// Smart Order Router produced a non-empty route
    /// decision. `detail` carries the target side + qty +
    /// the per-venue legs with their effective cost. Fires
    /// inside [`MarketMakerEngine::recommend_route`] so
    /// every advisory routing call leaves a breadcrumb in
    /// the audit trail, even before stage-2 inline
    /// dispatch lands.
    RouteDecisionEmitted,
}

impl AuditLog {
    /// Create a new audit log. Appends to existing file.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: Mutex::new(std::io::BufWriter::new(file)),
            sequence: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Log an event.
    pub fn log(&self, event: AuditEvent) {
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut event = event;
        event.seq = seq;
        event.timestamp = Utc::now();

        if let Ok(json) = serde_json::to_string(&event) {
            if let Ok(mut writer) = self.writer.lock() {
                if writeln!(writer, "{json}").is_err() {
                    error!("failed to write audit log");
                }
            }
        }
    }

    /// Convenience: log an order placement.
    pub fn order_placed(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
    ) {
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::OrderPlaced,
            symbol: symbol.to_string(),
            order_id: Some(order_id),
            side: Some(side),
            price: Some(price),
            qty: Some(qty),
            detail: None,
        });
    }

    /// Convenience: log a fill.
    pub fn order_filled(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
        is_maker: bool,
    ) {
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::OrderFilled,
            symbol: symbol.to_string(),
            order_id: Some(order_id),
            side: Some(side),
            price: Some(price),
            qty: Some(qty),
            detail: Some(if is_maker {
                "maker".to_string()
            } else {
                "taker".to_string()
            }),
        });
    }

    /// Convenience: log a cancel.
    pub fn order_cancelled(&self, symbol: &str, order_id: OrderId) {
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::OrderCancelled,
            symbol: symbol.to_string(),
            order_id: Some(order_id),
            side: None,
            price: None,
            qty: None,
            detail: None,
        });
    }

    /// Convenience: emit a periodic Order-to-Trade Ratio
    /// snapshot into the audit trail. MiCA compliance: the
    /// regulator expects the time series to be reconstructable
    /// from the persistent log.
    pub fn order_to_trade_ratio_snapshot(
        &self,
        symbol: &str,
        ratio: rust_decimal::Decimal,
        adds: u64,
        updates: u64,
        cancels: u64,
        trades: u64,
    ) {
        let detail = format!(
            "ratio={ratio} adds={adds} updates={updates} cancels={cancels} trades={trades}"
        );
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::OrderToTradeRatioSnapshot,
            symbol: symbol.to_string(),
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
        });
    }

    /// Convenience: log a risk event.
    pub fn risk_event(&self, symbol: &str, event_type: AuditEventType, detail: &str) {
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type,
            symbol: symbol.to_string(),
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail.to_string()),
        });
    }

    /// Flush buffer to disk.
    pub fn flush(&self) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.flush();
        }
    }
}
