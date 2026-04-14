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
