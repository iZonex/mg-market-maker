use chrono::{DateTime, NaiveDate, Utc};
use mm_common::types::{OrderId, Price, Qty, Side};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{error, info, warn};

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
    state: Mutex<AuditLogInner>,
    sequence: std::sync::atomic::AtomicU64,
}

struct AuditLogInner {
    writer: std::io::BufWriter<std::fs::File>,
    /// Base path (e.g. `data/audit.jsonl`). Rolled files live next
    /// to it as `audit-YYYY-MM-DD.jsonl`. Keeping the active file
    /// at a stable name lets external tools (log shippers, MiCA
    /// exporters) tail without following renames.
    base_path: PathBuf,
    /// UTC date of the current file. We compare against "today"
    /// on every write; the comparison is a single integer check,
    /// not a syscall, so the hot path overhead is negligible.
    current_date: NaiveDate,
}

/// An audit event — every action the MM takes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub symbol: String,
    /// Owning client ID (Epic 1). `None` in legacy single-client
    /// mode — existing JSONL files parse cleanly because the
    /// field is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// WebSocket book stream had a sequence gap and the engine
    /// re-anchored by pulling a fresh REST snapshot. `detail`
    /// carries the sequence the resync landed on so compliance
    /// can correlate with venue-side incident tickets.
    BookResync,

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

    // Epic B — Cointegrated pairs stat-arb driver.
    /// `StatArbDriver` opened a position. `detail` carries
    /// direction + per-leg qty + entry z-score + spread.
    /// Stage-1: advisory only — the driver does not dispatch
    /// leg orders, so there are no fills on the engine side.
    /// The event records the intent so operators can replay
    /// and sign off before stage-2 inline dispatch lands.
    StatArbEntered,
    /// `StatArbDriver` closed a position. `detail` carries
    /// exit z-score + spread + synthetic realised PnL
    /// estimate. Same advisory-only caveat as
    /// [`AuditEventType::StatArbEntered`].
    StatArbExited,

    // Epic D — Signal wave 2 (OFI / learned MP / BVC / Cartea AS).
    /// Periodic snapshot of the Cont-Kukanov-Stoikov OFI
    /// EWMA produced by `MomentumSignals`. Fires once per
    /// 30 s summary interval (NOT per L1 event — that would
    /// flood the audit log). `detail` carries the latest
    /// EWMA value plus the smoothed depth-normalised OFI.
    OfiFeatureSnapshot,
    /// Cartea adverse-selection spread widening crossed a
    /// state boundary (no-effect → widening or back).
    /// Fires only on transitions so the audit log is not
    /// spammed every refresh tick. `detail` carries the
    /// `ρ` value and the new spread multiplier direction.
    AsSpreadWidened,

    // Epic F — Defensive layer (lead-lag + news retreat).
    /// Lead-lag guard's soft-widen multiplier crossed
    /// `> 1.0` (a sharp leader-side move was detected).
    /// Fires only on the `1.0 → > 1.0` transition so the
    /// audit log is not spammed every leader-mid update.
    /// `detail` carries the |z-score| and the new multiplier.
    LeadLagTriggered,
    /// News retreat state machine entered or escalated to
    /// a higher-priority class on a fresh headline. `detail`
    /// carries the matched class plus the headline text.
    /// Critical-class transitions also escalate the engine's
    /// kill switch to L2 (`StopNewOrders`).
    NewsRetreatActivated,
    /// News retreat state machine reverted to `Normal` after
    /// the cooldown expired with no fresh same-class headline.
    /// `detail` carries the previous class.
    NewsRetreatExpired,

    // Epic F — Listing sniper (stage-3 engine integration).
    /// Listing sniper discovered a new symbol on a venue.
    /// `detail` carries venue + symbol + tick/lot/min_notional.
    ListingDiscovered,
    /// Listing sniper detected a previously-known symbol was
    /// removed from a venue. `detail` carries venue + symbol.
    ListingRemoved,

    // Epic 2 — Token Lending lifecycle.
    /// New loan agreement created. `detail` carries symbol +
    /// total_qty + counterparty + start/end dates.
    LoanCreated,
    /// Loan agreement terms updated. `detail` carries the
    /// changed field(s).
    LoanUpdated,
    /// Loan return installment is approaching its due date.
    /// `detail` carries symbol + due_date + qty.
    LoanReturnScheduled,
    /// Loan return installment completed. `detail` carries
    /// symbol + installment_idx + qty + completion date.
    LoanReturnCompleted,
    /// Loan utilization approaching limit. `detail` carries
    /// symbol + utilization_pct + threshold.
    LoanUtilizationAlert,
    /// Loan status changed (Active → PartiallyReturned →
    /// Returned / Defaulted). `detail` carries old → new status.
    LoanStatusChanged,

    // Epic 4 — Cross-venue execution.
    /// Withdraw requested. `detail` carries asset + qty + address + network + venue.
    WithdrawRequested,
    /// Withdraw confirmed by venue. `detail` carries venue withdraw ID.
    WithdrawCompleted,
    /// Internal transfer requested. `detail` carries asset + qty + from_wallet + to_wallet + venue.
    InternalTransferRequested,
    /// Internal transfer confirmed. `detail` carries venue transfer ID.
    InternalTransferCompleted,
    /// Auto-rebalancer executed a transfer. `detail` carries from_venue + to_venue + asset + qty.
    AutoRebalanceExecuted,

    // Epic 5 — Compliance reporting.
    /// Scheduled report generated. `detail` carries report type + period.
    ReportGenerated,
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
            state: Mutex::new(AuditLogInner {
                writer: std::io::BufWriter::new(file),
                base_path: path.to_path_buf(),
                current_date: Utc::now().date_naive(),
            }),
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

        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };

        let Ok(mut state) = self.state.lock() else {
            error!("audit log mutex poisoned");
            return;
        };

        // Daily rotation check. Compare today's UTC date against
        // the file's open-date marker — on first write after
        // midnight we rename the current file to an ISO-stamped
        // archive and open a fresh one at the base path. Five-year
        // MiCA retention + 500 MB/day audit volume makes a single
        // unbounded JSONL impractical for log shippers.
        let today = event.timestamp.date_naive();
        if today != state.current_date {
            if let Err(e) = Self::rotate_locked(&mut state, today) {
                warn!(error = %e, "audit log rotation failed — continuing on old file");
            }
        }

        if writeln!(state.writer, "{json}").is_err() {
            error!("failed to write audit log");
        }
    }

    /// Rotate the active audit file: flush + close current,
    /// rename it to `audit-YYYY-MM-DD.jsonl` (using the PREVIOUS
    /// date so the archive name matches the events it contains),
    /// then open a fresh file at the base path. Runs under the
    /// state mutex — callers MUST hold the lock.
    fn rotate_locked(state: &mut AuditLogInner, today: NaiveDate) -> anyhow::Result<()> {
        let _ = state.writer.flush();
        let archived = audit_archive_name(&state.base_path, state.current_date);
        if state.base_path.exists() {
            // A rotation failure is never fatal — worst case we
            // keep writing to the old file until next midnight.
            if let Err(e) = std::fs::rename(&state.base_path, &archived) {
                warn!(
                    archive = %archived.display(),
                    error = %e,
                    "audit rename failed — keeping active file"
                );
                return Err(e.into());
            }
            info!(archive = %archived.display(), "audit log rotated");
            // Post-rotation archival: gzip the file + optionally
            // spawn the configured upload command. Runs on a
            // detached std::thread so the compression + upload
            // does not stall the next audit write.
            let path_for_archive = archived.clone();
            std::thread::spawn(move || {
                archive_rotated_file(&path_for_archive);
            });
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&state.base_path)?;
        state.writer = std::io::BufWriter::new(file);
        state.current_date = today;
        Ok(())
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
            client_id: None,
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
            client_id: None,
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
            client_id: None,
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
            client_id: None,
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
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail.to_string()),
        });
    }

    /// Flush buffer to disk.
    pub fn flush(&self) {
        if let Ok(mut state) = self.state.lock() {
            let _ = state.writer.flush();
        }
    }
}

/// Build the archive path for a given day — `base_path.parent/
/// base_stem-YYYY-MM-DD.base_ext`. For `data/audit.jsonl` on
/// 2026-04-17 this yields `data/audit-2026-04-17.jsonl`.
fn audit_archive_name(base_path: &Path, day: NaiveDate) -> PathBuf {
    let parent = base_path.parent().unwrap_or(Path::new("."));
    let stem = base_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audit");
    let ext = base_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jsonl");
    parent.join(format!("{stem}-{}.{ext}", day.format("%Y-%m-%d")))
}

/// Post-rotation archival: gzip the rotated file, then
/// optionally run the configured upload command. Runs on a
/// detached OS thread so the audit-log hot path never blocks
/// on compression or a flaky S3 PUT.
///
/// Pipeline:
/// 1. Compress `archived` → `archived.gz` (flate2 default level).
/// 2. Remove the original plain-text file — we only keep the
///    `.gz` so retention storage does not carry both copies.
/// 3. If `MM_AUDIT_ARCHIVE_CMD` is set, spawn a subprocess with
///    the literal substring `{file}` replaced by the absolute
///    `.gz` path. Typical values:
///      - `aws s3 cp {file} s3://bucket/audit/`
///      - `gcloud storage cp {file} gs://bucket/audit/`
///      - `rclone copy {file} remote:audit/`
///
///    The subprocess is NOT retried — on failure the `.gz` is
///    left on the local filesystem for the next cron to pick
///    up. Compliance treats on-disk .gz as a durable copy.
///
/// Errors at every stage are logged but never panic — a failed
/// archive must not take down the engine.
fn archive_rotated_file(archived: &Path) {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    // 1. Gzip.
    let gz_path = archived.with_extension(match archived.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.gz"),
        None => "gz".into(),
    });
    let src = match std::fs::read(archived) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(file = %archived.display(), error = %e, "audit archive: read failed");
            return;
        }
    };
    let gz_file = match std::fs::File::create(&gz_path) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %gz_path.display(), error = %e, "audit archive: create .gz failed");
            return;
        }
    };
    let mut encoder = GzEncoder::new(gz_file, Compression::default());
    if let Err(e) = encoder.write_all(&src) {
        warn!(path = %gz_path.display(), error = %e, "audit archive: gzip write failed");
        return;
    }
    if let Err(e) = encoder.finish() {
        warn!(path = %gz_path.display(), error = %e, "audit archive: gzip finish failed");
        return;
    }
    // Original file removed AFTER successful compression so a
    // mid-write crash does not lose both copies.
    let _ = std::fs::remove_file(archived);
    info!(gz = %gz_path.display(), "audit archive gzipped");

    // 2. Upload via configured command.
    let Ok(cmd_tmpl) = std::env::var("MM_AUDIT_ARCHIVE_CMD") else {
        return;
    };
    if cmd_tmpl.trim().is_empty() {
        return;
    }
    let gz_str = gz_path.to_string_lossy().into_owned();
    let cmd_str = cmd_tmpl.replace("{file}", &gz_str);
    // `sh -c` so the command template can use shell features
    // (pipes, env expansion). Operators who want strict arg
    // splitting should wrap their command in `/bin/sh -c`
    // themselves and skip expansion.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd_str)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            info!(gz = %gz_path.display(), "audit archive upload ok");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                gz = %gz_path.display(),
                status = ?out.status.code(),
                stderr = %stderr,
                "audit archive upload failed — file retained on local disk"
            );
        }
        Err(e) => {
            warn!(
                gz = %gz_path.display(),
                error = %e,
                "audit archive upload spawn failed — file retained on local disk"
            );
        }
    }
}
