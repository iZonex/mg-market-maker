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

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Opaque on purpose — the inner mutex holds a BufWriter that
        // is neither Debug nor cheap to format, and the only field
        // the reader cares about is the file path that owns us.
        f.debug_struct("AuditLog").finish_non_exhaustive()
    }
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
    /// Last event's SHA-256 (hex). Seeded from the last line of
    /// the existing file at construction so a restart picks up
    /// the chain without breaking it. Reset on daily rotation —
    /// the archived file retains the closed chain.
    last_hash: Option<String>,
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
    /// Epic 36.3 — SHA-256 of the previous event's serialised
    /// body (hex). Forms a tamper-evident hash chain across the
    /// whole log: insertion, deletion, or modification of any
    /// event breaks the chain at that point and every subsequent
    /// record. `None` on the first event after rotation / init.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
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

    /// 22W-1 — protections stack tripped a per-pair guard
    /// (StoplossGuard / CooldownPeriod / MaxDrawdownPause /
    /// LowProfitPairs). `detail` carries the guard name + reason
    /// string so the operator can tell which rule fired.
    ProtectionsLocked,
    /// 22W-1 — all protections guards cleared for the pair
    /// (lock expired or recovery trigger). Paired with a prior
    /// `ProtectionsLocked` event.
    ProtectionsCleared,

    /// 22W-5 — XEMM executor rejected a hedge because adverse
    /// slippage on the hedge venue's top-of-book exceeded the
    /// configured `max_slippage_bps`. The primary-leg fill
    /// already landed; the operator now owns a one-sided
    /// inventory and must decide (retry, cancel maker quote,
    /// manual unwind).
    XemmHedgeRejected,

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

    /// Epic F stage-3 — listing sniper placed a real entry
    /// IOC after the quarantine window expired. `detail`
    /// carries `venue=…, symbol=…, qty=…, price=…,
    /// notional=…` so post-mortems can reconstruct the
    /// decision without walking the order-manager fill log.
    ListingEntered,

    /// Epic F stage-3 — listing sniper REJECTED an entry
    /// candidate. `detail` carries the rejection reason:
    /// `quarantine`, `max_active`, `status`, `zero_qty`,
    /// `no_book`, `place_err(...)`. Observer-mode runs
    /// (`enter_on_discovery = false`) emit this with
    /// `reason=disabled` exactly once per symbol so the
    /// audit trail records the fact that the sniper saw
    /// the listing but did not act.
    ListingEntryRejected,

    /// Epic B stage-2 — background pair-screener scan
    /// result. `detail` carries `y=SYM, x=SYM, coint=bool,
    /// adf=<stat>, crit=<stat>, beta=<val>, n=<samples>` so
    /// ops can browse the audit trail and pick candidate
    /// cointegrated pairs for a stat-arb driver without
    /// re-running the test.
    CointegrationScreened,

    /// Epic A stage-2 #1 — inline dispatch tick actually
    /// placed orders against the bundle. `detail` carries
    /// target side + total target qty + total dispatched qty
    /// plus per-leg `(venue, dispatched_qty, error?)`.
    /// Emitted once per successful dispatch tick including
    /// partial successes; errors are flagged in-line so
    /// post-mortems correlate a failed leg to its audit row
    /// without re-walking the log.
    RouteDispatched,

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

    // Epic H — Visual strategy graph (node-editor composer).
    /// A strategy graph was deployed (validated + swapped into the
    /// engine). `detail` carries `graph={name} hash={sha256}` so
    /// compliance exports can join on the hash to reconstruct the
    /// graph that was live at any point in time.
    StrategyGraphDeployed,
    /// An operator loaded a previous version of a graph from the
    /// per-hash history store and redeployed it. `detail` carries
    /// `graph={name} from_hash={sha256} to_hash={sha256}` so the
    /// backwards-transition is as visible as forward deploys.
    StrategyGraphRolledBack,
    /// A deploy attempt was refused because the graph contained a
    /// restricted (pentest-only) node kind and the runtime was not
    /// started with `MM_ALLOW_RESTRICTED=yes-pentest-mode`. `detail` carries the
    /// offending kind(s) so the regulator can confirm the gate
    /// actually fired rather than being silently suppressed.
    StrategyGraphDeployRejected,
    /// A sink node (`Out.Flatten`, `Out.KillEscalate`, `Out.SpreadMult`,
    /// `Out.SizeMult`) fired on a tick of a live graph. `detail`
    /// carries `action=... hash={sha256}` so the regulator can trace
    /// a kill-switch escalation back to the exact authored graph.
    /// Only emitted for high-consequence sinks (flatten, kill); the
    /// multipliers fire every tick and would spam the log.
    StrategyGraphSinkFired,

    // Epic R — surveillance & manipulation detectors.
    /// A [`surveillance`](crate::surveillance) detector crossed its
    /// alert threshold (`score ≥ 0.8` by default). `detail` carries
    /// `pattern={spoofing|layering|quote_stuffing|...} score={0..1}
    /// cancel_ratio={...} lifetime_ms={...}` so a reviewer can
    /// recompute the decision from the audit row alone. Dedupes on
    /// the caller side: the engine emits at most one alert per
    /// pattern per `cooldown_secs` to keep the log readable during
    /// sustained hot windows.
    SurveillanceAlert,

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

    // Epic 38 — Dashboard auth surveillance.
    /// Dashboard login succeeded. `detail` carries `user_id=…,
    /// role=…, ip=…`. Written for every accepted `/api/auth/login`
    /// so credential-stuffing attempts leave a trail even if the
    /// attacker eventually guesses a valid key.
    LoginSucceeded,
    /// Dashboard login failed — API key unknown. `detail` carries
    /// the source IP plus a short prefix of the supplied key (for
    /// correlation, never the full key) and the failure reason.
    LoginFailed,
    /// Dashboard logout requested. `detail` carries `user_id=…,
    /// ip=…`. Tokens are stateless so the server-side effect is
    /// advisory; the event exists to mark the operator's intent
    /// for post-incident review.
    LogoutSucceeded,

    // Epic 40.3 — perp funding accrual.
    /// A funding-settlement instant booked a `realised_delta`
    /// into `PnlAttribution::funding_pnl_realised`. `detail`
    /// carries `rate=…, mark=…, inventory=…, delta=…,
    /// next_funding_time=…`. Emitted **only at settlement**
    /// (not on the continuous MTM tick) so the audit log
    /// records one row per period even at HL's 1-hour
    /// cadence. MiCA reporting consumes this alongside
    /// `OrderFilled` to reconstruct PnL.
    FundingAccrued,
}

impl AuditLog {
    /// Create a new audit log. Appends to existing file. Seeds
    /// the hash chain from the last line on disk so a restart
    /// picks up where the previous process stopped — readers
    /// that verify the chain see an unbroken sequence across
    /// process boundaries.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let last_hash = seed_last_hash_from_file(path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            state: Mutex::new(AuditLogInner {
                writer: std::io::BufWriter::new(file),
                base_path: path.to_path_buf(),
                current_date: Utc::now().date_naive(),
                last_hash,
            }),
            sequence: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Log an event. Chains the serialised form into a
    /// tamper-evident SHA-256 sequence (Epic 36.3) and, for
    /// regulatory-critical events, forces the OS to `fsync` the
    /// record to disk before returning so power loss cannot
    /// erase a filled order or a kill-switch escalation.
    pub fn log(&self, event: AuditEvent) {
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut event = event;
        event.seq = seq;
        event.timestamp = Utc::now();

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

        // Chain-hash: stamp the prev_hash then serialise, then
        // compute this event's hash for the next write.
        event.prev_hash = state.last_hash.clone();
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        let this_hash = sha256_hex(json.as_bytes());
        state.last_hash = Some(this_hash);

        if writeln!(state.writer, "{json}").is_err() {
            error!("failed to write audit log");
        }

        // Regulatory-critical events get fsync'd before return.
        // The rest of the write path stays buffered because the
        // tick_second() flush covers them within 30 s at worst.
        if is_critical(&event.event_type) {
            if let Err(e) = state.writer.flush() {
                error!(error = %e, "audit flush failed on critical event");
                return;
            }
            if let Err(e) = state.writer.get_ref().sync_data() {
                error!(error = %e, "audit fsync failed on critical event");
            }
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
        // Fresh file = fresh chain. The archived file retains
        // the closed chain; readers verify each day's file
        // independently.
        state.last_hash = None;
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

            prev_hash: None,
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
            prev_hash: None,
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

            prev_hash: None,
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

            prev_hash: None,
        });
    }

    /// Convenience: log a dashboard auth event. No symbol context
    /// — auth is a cross-cutting concern, so `symbol` is left
    /// empty. `detail` should carry `user_id=…, ip=…, …` as a
    /// comma-separated k=v string so the audit trail is grep-able.
    pub fn auth_event(&self, event_type: AuditEventType, detail: &str) {
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type,
            symbol: String::new(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail.to_string()),
            prev_hash: None,
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
            prev_hash: None,
        });
    }

    /// Convenience: log a strategy-graph deploy. `scope_key` is the
    /// string form the engine's `DashboardState::broadcast_config_override`
    /// uses (`"Symbol(BTCUSDT)"`, `"Global"`, …) so the regulator can
    /// tell *which* engines picked this graph up on this tick.
    pub fn strategy_graph_deployed(
        &self,
        graph_name: &str,
        hash: &str,
        scope_key: &str,
        operator: &str,
        recipients: usize,
    ) {
        let detail = format!(
            "graph={graph_name} hash={hash} scope={scope_key} operator={operator} recipients={recipients}"
        );
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::StrategyGraphDeployed,
            symbol: String::new(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
            prev_hash: None,
        });
    }

    /// Convenience: log a strategy-graph rollback (an operator
    /// loaded a previous hash from `history/` and deployed it). The
    /// deploy itself still emits [`StrategyGraphDeployed`] on the
    /// following line — this event sits alongside so the regulator
    /// sees the *intent* as well as the result.
    pub fn strategy_graph_rolled_back(
        &self,
        graph_name: &str,
        from_hash: &str,
        to_hash: &str,
        operator: &str,
    ) {
        let detail = format!(
            "graph={graph_name} from_hash={from_hash} to_hash={to_hash} operator={operator}"
        );
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::StrategyGraphRolledBack,
            symbol: String::new(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
            prev_hash: None,
        });
    }

    /// Convenience: log a refused deploy. Fires when a graph
    /// references a restricted node kind (pentest-only strategies)
    /// and the runtime was not started with the explicit opt-in.
    pub fn strategy_graph_deploy_rejected(
        &self,
        graph_name: &str,
        reason: &str,
        operator: &str,
    ) {
        let detail = format!("graph={graph_name} reason={reason} operator={operator}");
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::StrategyGraphDeployRejected,
            symbol: String::new(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
            prev_hash: None,
        });
    }

    /// Convenience: log a high-consequence sink firing on a live
    /// tick. `action` is the `Debug`-form of the `SinkAction` enum
    /// variant (`Flatten { policy }` / `KillEscalate { level, reason }`),
    /// `hash` is the active graph's content hash.
    pub fn strategy_graph_sink_fired(
        &self,
        symbol: &str,
        action: &str,
        hash: &str,
    ) {
        let detail = format!("action={action} hash={hash}");
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::StrategyGraphSinkFired,
            symbol: symbol.to_string(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
            prev_hash: None,
        });
    }

    /// Convenience: log a surveillance alert. Called by the engine
    /// when a detector score crosses threshold. `detail` carries the
    /// pattern label + score + the diagnostic sub-signals so the
    /// row stands on its own for audit review.
    pub fn surveillance_alert(
        &self,
        symbol: &str,
        pattern: &str,
        score: rust_decimal::Decimal,
        detail_extra: &str,
    ) {
        let detail = format!("pattern={pattern} score={score} {detail_extra}");
        self.log(AuditEvent {
            seq: 0,
            timestamp: Utc::now(),
            event_type: AuditEventType::SurveillanceAlert,
            symbol: symbol.to_string(),
            client_id: None,
            order_id: None,
            side: None,
            price: None,
            qty: None,
            detail: Some(detail),
            prev_hash: None,
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

// ── Epic 36.3 helpers: hash-chain + critical-event fsync ──

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn is_critical(ty: &AuditEventType) -> bool {
    matches!(
        ty,
        AuditEventType::OrderFilled
            | AuditEventType::KillSwitchEscalated
            | AuditEventType::CircuitBreakerTripped
            // Strategy-graph mutations rewrite the quoting behaviour
            // of the engine — a regulator reconstructing a given
            // minute of trades needs to know which graph was live,
            // so the deploy/rollback/reject rows fsync same as a
            // kill-switch event.
            | AuditEventType::StrategyGraphDeployed
            | AuditEventType::StrategyGraphRolledBack
            | AuditEventType::StrategyGraphDeployRejected
            | AuditEventType::StrategyGraphSinkFired
            // Surveillance alerts are regulator-visible signals the
            // MM detected itself as the suspect actor — get them on
            // disk before the next page of the log rotates.
            | AuditEventType::SurveillanceAlert
    )
}

/// Read the last non-empty line of an existing audit file and
/// return the SHA-256 of that line. Used at `AuditLog::new()` to
/// seed the hash chain across process restarts.
fn seed_last_hash_from_file(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    Some(sha256_hex(last.as_bytes()))
}

/// Sprint-5 companion — offline hash-chain verification for a
/// single audit log file. Walks the JSONL rows in order and
/// checks that every `prev_hash` equals the SHA-256 of the
/// preceding serialised line (i.e. the exact write-path invariant
/// in `AuditLog::log_event`). Insertion, deletion, reordering, or
/// field tampering on any row breaks the chain at that row +
/// every row after it, so a failure at row N pinpoints the first
/// tampered boundary.
///
/// Used by disaster-recovery startup: if the last session's log
/// fails this check, the operator is required to roll forward on
/// an earlier known-good archive instead of silently trusting the
/// suspect file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainVerifyError {
    /// A line failed to parse as an `AuditEvent`. Payload is the
    /// 1-indexed line number.
    MalformedRow(usize),
    /// `prev_hash` on row `row` didn't match the computed hash of
    /// row `row - 1`. The `expected` is what the chain says
    /// should be there; `got` is what the file carries.
    ChainBroken {
        row: usize,
        expected: Option<String>,
        got: Option<String>,
    },
}

impl std::fmt::Display for ChainVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainVerifyError::MalformedRow(n) => {
                write!(f, "row {n} failed to parse as an AuditEvent")
            }
            ChainVerifyError::ChainBroken { row, expected, got } => {
                write!(
                    f,
                    "hash chain broken at row {row}: expected {expected:?}, got {got:?}"
                )
            }
        }
    }
}

impl std::error::Error for ChainVerifyError {}

/// Report from a successful chain verification — rows checked
/// and the hash of the final row so callers can decide if the
/// chain extends correctly into a newer file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerifyReport {
    pub rows_checked: usize,
    pub last_hash: Option<String>,
}

/// Walk `path` (a JSONL audit log) and verify the hash chain.
/// Empty file → `Ok(ChainVerifyReport { 0, None })`. The first
/// row's `prev_hash` is expected to be `None` (fresh chain after
/// daily rotation); subsequent rows carry the previous row's
/// SHA-256.
pub fn verify_chain(path: &Path) -> std::result::Result<ChainVerifyReport, ChainVerifyError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            return Ok(ChainVerifyReport {
                rows_checked: 0,
                last_hash: None,
            });
        }
    };
    let mut prev_hash: Option<String> = None;
    let mut rows = 0usize;
    for (idx, line) in content.lines().enumerate() {
        let row = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        let event: AuditEvent = serde_json::from_str(line)
            .map_err(|_| ChainVerifyError::MalformedRow(row))?;
        if event.prev_hash != prev_hash {
            return Err(ChainVerifyError::ChainBroken {
                row,
                expected: prev_hash,
                got: event.prev_hash,
            });
        }
        prev_hash = Some(sha256_hex(line.as_bytes()));
        rows += 1;
    }
    Ok(ChainVerifyReport {
        rows_checked: rows,
        last_hash: prev_hash,
    })
}

#[cfg(test)]
mod chain_verify_tests {
    use super::*;

    fn tmp_audit_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mm_audit_chain_{tag}_{}_{}.jsonl",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        p
    }

    fn write_clean_chain(p: &Path, rows: usize) {
        let log = AuditLog::new(p).expect("open audit");
        for i in 0..rows {
            log.risk_event(
                "BTCUSDT",
                AuditEventType::EngineStarted,
                &format!("row-{i}"),
            );
        }
        drop(log);
    }

    /// Clean chain verifies and reports the row count.
    #[test]
    fn clean_chain_passes() {
        let p = tmp_audit_path("clean");
        write_clean_chain(&p, 5);
        let report = verify_chain(&p).expect("clean chain");
        assert_eq!(report.rows_checked, 5);
        assert!(report.last_hash.is_some());
        let _ = std::fs::remove_file(&p);
    }

    /// Tampering a middle row breaks the chain at the tampered
    /// row itself — its `prev_hash` no longer matches the computed
    /// hash of the preceding row.
    #[test]
    fn tampered_middle_row_fails_at_row() {
        let p = tmp_audit_path("tamper");
        write_clean_chain(&p, 5);
        let content = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        // Swap row 3 for a copy of row 1 — its prev_hash (null)
        // won't match the hash of row 2, so verify_chain fires at
        // row 3.
        lines[2] = lines[0];
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let err = verify_chain(&p).expect_err("chain should break");
        match err {
            ChainVerifyError::ChainBroken { row, .. } => assert_eq!(row, 3),
            other => panic!("unexpected: {other:?}"),
        }
        let _ = std::fs::remove_file(&p);
    }

    /// Missing first-row prev_hash (operator injected a synthetic
    /// row at the top) breaks the chain at row 1.
    #[test]
    fn inserted_head_row_fails_at_first_boundary() {
        let p = tmp_audit_path("insert");
        write_clean_chain(&p, 3);
        let content = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        // Synthetic row with a non-null prev_hash at the head —
        // full AuditEvent shape so it parses cleanly.
        let synthetic = r#"{"seq":0,"timestamp":"2020-01-01T00:00:00Z","event_type":"engine_started","symbol":"BTCUSDT","prev_hash":"deadbeef"}"#;
        lines.insert(0, synthetic.to_string());
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let err = verify_chain(&p).expect_err("chain should break");
        match err {
            ChainVerifyError::ChainBroken { row, expected, got } => {
                assert_eq!(row, 1);
                assert_eq!(expected, None);
                assert_eq!(got, Some("deadbeef".into()));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let _ = std::fs::remove_file(&p);
    }

    /// Empty or missing file returns an empty report — not an
    /// error. That's the expected bootstrap case.
    #[test]
    fn missing_file_returns_empty_report() {
        let p = tmp_audit_path("missing");
        let report = verify_chain(&p).expect("missing = empty");
        assert_eq!(report.rows_checked, 0);
        assert_eq!(report.last_hash, None);
    }

    /// Malformed JSON on row N points at row N.
    #[test]
    fn malformed_row_reports_line_number() {
        let p = tmp_audit_path("malformed");
        write_clean_chain(&p, 3);
        let content = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[1] = "{ not valid json".to_string();
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let err = verify_chain(&p).expect_err("should fail");
        match err {
            ChainVerifyError::MalformedRow(n) => assert_eq!(n, 2),
            other => panic!("unexpected: {other:?}"),
        }
        let _ = std::fs::remove_file(&p);
    }
}
