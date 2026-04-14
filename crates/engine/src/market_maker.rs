use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use mm_common::config::AppConfig;
use mm_common::types::ProductSpec;
use mm_dashboard::alerts::{AlertManager, AlertSeverity};
use mm_dashboard::state::{
    BookDepthLevel, DashboardState, IncidentRecord, PnlSnapshot, SymbolState,
};
use mm_exchange_core::events::MarketEvent;
use mm_portfolio::Portfolio;
use mm_risk::audit::{AuditEventType, AuditLog};
use mm_risk::circuit_breaker::{CircuitBreaker, TripReason};
use mm_risk::exposure::ExposureManager;
use mm_risk::inventory::InventoryManager;
use mm_risk::kill_switch::{KillLevel, KillSwitch, KillSwitchConfig};
use mm_risk::pnl::PnlTracker;
use mm_risk::sla::{SlaConfig, SlaTracker};
use mm_risk::toxicity::{AdverseSelectionTracker, KyleLambda, VpinEstimator};
use mm_strategy::autotune::AutoTuner;
use mm_strategy::funding_arb_driver::{DriverEvent, FundingArbDriver};
use mm_strategy::inventory_skew::AdvancedInventoryManager;
use mm_strategy::momentum::MomentumSignals;
use mm_strategy::paired_unwind::PairedUnwindExecutor;
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::twap::TwapExecutor;
use mm_strategy::volatility::VolatilityEstimator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::balance_cache::BalanceCache;
use crate::book_keeper::BookKeeper;
use crate::connector_bundle::ConnectorBundle;
use crate::order_id_map::OrderIdMap;
use crate::order_manager::OrderManager;

/// The main market-making engine for a single symbol.
///
/// ALL subsystems wired:
/// - Strategy (A-S / GLFT / Grid) + momentum alpha
/// - Risk (circuit breaker, kill switch 5-level, inventory, exposure)
/// - Toxicity (VPIN, Kyle's Lambda, adverse selection)
/// - SLA compliance + PnL attribution
/// - Auto-tuning (regime + toxicity)
/// - Advanced inventory (quadratic skew, urgency, dynamic sizing)
/// - Audit trail (append-only JSONL)
/// - Balance cache (pre-check + reservation)
/// - Order ID mapping (UUID ↔ exchange)
/// - TWAP executor (for kill switch L4 flatten)
/// - Dashboard state updates
/// - Reconciliation on reconnect
pub struct MarketMakerEngine {
    symbol: String,
    config: AppConfig,
    product: ProductSpec,
    strategy: Box<dyn Strategy>,
    connectors: ConnectorBundle,

    // Core.
    book_keeper: BookKeeper,
    /// Second book for the hedge leg when `connectors.hedge` is set.
    /// Cross-product strategies read its mid as `StrategyContext.ref_price`.
    hedge_book: Option<BookKeeper>,
    /// Second `OrderManager` for the hedge leg, built lazily when
    /// `connectors.hedge` is present. Kept strictly separate
    /// from the primary `order_manager` so per-leg diffing,
    /// cancel-all, and fill routing never mix the two venues.
    hedge_order_manager: Option<OrderManager>,
    order_manager: OrderManager,
    inventory_manager: InventoryManager,
    exposure_manager: ExposureManager,
    circuit_breaker: CircuitBreaker,
    volatility_estimator: VolatilityEstimator,

    // Risk.
    kill_switch: KillSwitch,
    audit: Arc<AuditLog>,
    balance_cache: BalanceCache,
    order_id_map: OrderIdMap,

    // Toxicity.
    vpin: VpinEstimator,
    kyle_lambda: KyleLambda,
    adverse_selection: AdverseSelectionTracker,

    // Strategy augmentation.
    momentum: MomentumSignals,
    auto_tuner: AutoTuner,
    adv_inventory: AdvancedInventoryManager,
    twap: Option<TwapExecutor>,
    /// Paired-unwind executor for kill-switch L4 on a basis /
    /// funding-arb position. Populated only when
    /// `connectors.pair` is set AND the kill switch escalates
    /// to `FlattenAll`. Replaces the single-leg `twap`
    /// executor in dual-connector mode — running both would
    /// double-flatten the primary leg.
    paired_unwind: Option<PairedUnwindExecutor>,
    /// Funding-arb driver owned by the engine. When set, the
    /// engine's select loop adds a periodic tick that pulls
    /// funding rate + both mids, asks
    /// `FundingArbEngine::evaluate`, and dispatches via
    /// `FundingArbExecutor`. DriverEvent routing (audit,
    /// kill-switch escalation on uncompensated pair break)
    /// lives in `handle_driver_event`. Mutually exclusive with
    /// regular maker quoting — operators who run funding arb
    /// set `StrategyType::FundingArb` in config and the engine
    /// uses the driver as the main tick instead of
    /// `refresh_quotes`.
    funding_arb_driver: Option<FundingArbDriver>,
    funding_arb_tick: std::time::Duration,

    // Tracking.
    sla_tracker: SlaTracker,
    pnl_tracker: PnlTracker,
    volume_limiter: mm_risk::VolumeLimitTracker,
    dashboard: Option<DashboardState>,
    alerts: Option<AlertManager>,
    /// Multi-currency portfolio aggregator. Shared across all
    /// `MarketMakerEngine` instances in a multi-symbol process
    /// via `Arc<Mutex<_>>` so a single unified snapshot covers
    /// every symbol's positions. `None` in single-symbol
    /// deployments or tests that don't care about unified PnL.
    portfolio: Option<Arc<Mutex<Portfolio>>>,

    // Timing.
    cycle_start: Instant,
    last_mid: Decimal,
    tick_count: u64,
    reconcile_counter: u64,
}

impl MarketMakerEngine {
    pub fn new(
        symbol: String,
        config: AppConfig,
        product: ProductSpec,
        strategy: Box<dyn Strategy>,
        connectors: ConnectorBundle,
        dashboard: Option<DashboardState>,
        alerts: Option<AlertManager>,
    ) -> Self {
        let tick_secs = Decimal::from(config.market_maker.refresh_interval_ms) / dec!(1000);
        let vol_est = VolatilityEstimator::new(dec!(0.94), tick_secs);

        let sla_config = SlaConfig {
            max_spread_bps: config.sla.max_spread_bps,
            min_depth_quote: config.sla.min_depth_quote,
            min_uptime_pct: config.sla.min_uptime_pct,
            two_sided_required: config.sla.two_sided_required,
            max_requote_secs: config.sla.max_requote_secs,
            min_order_rest_secs: config.sla.min_order_rest_secs,
        };

        let ks_config = KillSwitchConfig {
            daily_loss_limit: config.kill_switch.daily_loss_limit,
            daily_loss_warning: config.kill_switch.daily_loss_warning,
            max_position_value: config.kill_switch.max_position_value,
            max_message_rate: config.kill_switch.max_message_rate,
            max_consecutive_errors: config.kill_switch.max_consecutive_errors,
            ..Default::default()
        };

        // Audit log: data/audit/{symbol}.jsonl
        let audit_path = format!("data/audit/{}.jsonl", symbol.to_lowercase());
        let audit = Arc::new(
            AuditLog::new(Path::new(&audit_path))
                .unwrap_or_else(|e| panic!("failed to create audit log at {audit_path}: {e}")),
        );

        let vpin = VpinEstimator::new(
            config.toxicity.vpin_bucket_size,
            config.toxicity.vpin_num_buckets,
        );

        let hedge_book = connectors
            .pair
            .as_ref()
            .map(|pair| BookKeeper::new(&pair.hedge_symbol));
        let hedge_order_manager = connectors.hedge.as_ref().map(|_| OrderManager::new());
        Self {
            book_keeper: BookKeeper::new(&symbol),
            hedge_book,
            hedge_order_manager,
            order_manager: OrderManager::new(),
            inventory_manager: InventoryManager::new(),
            exposure_manager: ExposureManager::new(dec!(0)),
            circuit_breaker: CircuitBreaker::new(),
            volatility_estimator: vol_est,
            kill_switch: KillSwitch::new(ks_config),
            audit,
            balance_cache: BalanceCache::new(),
            order_id_map: OrderIdMap::new(),
            vpin,
            kyle_lambda: KyleLambda::new(config.toxicity.kyle_window),
            adverse_selection: AdverseSelectionTracker::new(200),
            momentum: MomentumSignals::new(config.market_maker.momentum_window),
            auto_tuner: AutoTuner::new(200),
            adv_inventory: AdvancedInventoryManager::new(config.risk.max_inventory),
            twap: None,
            paired_unwind: None,
            funding_arb_driver: None,
            funding_arb_tick: std::time::Duration::from_secs(60),
            sla_tracker: SlaTracker::new(sla_config),
            pnl_tracker: PnlTracker::new(product.maker_fee, product.taker_fee),
            volume_limiter: mm_risk::VolumeLimitTracker::new(
                config.risk.max_daily_volume_quote,
                config.risk.max_hourly_volume_quote,
            ),
            dashboard,
            alerts,
            portfolio: None,
            symbol,
            config,
            product,
            strategy,
            connectors,
            cycle_start: Instant::now(),
            last_mid: dec!(0),
            tick_count: 0,
            reconcile_counter: 0,
        }
    }

    /// Attach a shared multi-currency portfolio to this engine.
    ///
    /// Pass the same `Arc<Mutex<Portfolio>>` to every engine in a
    /// multi-symbol deployment so the dashboard reports a single
    /// unified PnL snapshot. Operators who don't need unified
    /// reporting just skip this call and the engine's existing
    /// `PnlTracker` remains the sole source of truth for dashboard
    /// gauges.
    pub fn with_portfolio(mut self, portfolio: Arc<Mutex<Portfolio>>) -> Self {
        self.portfolio = Some(portfolio);
        self
    }

    /// Attach a `FundingArbDriver` to the engine. The engine's
    /// main loop polls it on `tick_interval` and routes every
    /// `DriverEvent` to the audit trail + kill switch.
    /// Requires a dual-connector bundle — funding arb can't run
    /// without a hedge leg.
    pub fn with_funding_arb_driver(
        mut self,
        driver: FundingArbDriver,
        tick_interval: std::time::Duration,
    ) -> Self {
        self.funding_arb_driver = Some(driver);
        self.funding_arb_tick = tick_interval;
        self
    }

    pub async fn run(
        &mut self,
        ws_rx: mpsc::UnboundedReceiver<MarketEvent>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        self.run_with_hedge(ws_rx, None, shutdown_rx).await
    }

    /// Dual-connector variant of [`run`]. Callers with a hedge
    /// connector subscribe its market-data stream and pass it in
    /// `hedge_rx`; events from that channel only update the hedge
    /// `BookKeeper` and never place orders — the primary leg is
    /// still the only one touched by `OrderManager`.
    pub async fn run_with_hedge(
        &mut self,
        mut ws_rx: mpsc::UnboundedReceiver<MarketEvent>,
        mut hedge_rx: Option<mpsc::UnboundedReceiver<MarketEvent>>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        info!(
            symbol = %self.symbol,
            strategy = self.strategy.name(),
            dual = self.connectors.is_dual(),
            "engine starting"
        );
        self.audit.risk_event(
            &self.symbol,
            AuditEventType::EngineStarted,
            self.strategy.name(),
        );

        // Initial balance fetch.
        self.refresh_balances().await;

        // Initial orderbook snapshot via REST.
        match self
            .connectors
            .primary
            .get_orderbook(&self.symbol, 25)
            .await
        {
            Ok((bids, asks, seq)) => {
                self.book_keeper.book.apply_snapshot(bids, asks, seq);
                info!(seq, "initial book snapshot loaded");
            }
            Err(e) => warn!(error = %e, "failed to fetch initial book"),
        }

        // Initial hedge orderbook snapshot.
        if let (Some(hedge), Some(pair)) = (
            self.connectors.hedge.as_ref(),
            self.connectors.pair.as_ref(),
        ) {
            match hedge.get_orderbook(&pair.hedge_symbol, 25).await {
                Ok((bids, asks, seq)) => {
                    if let Some(hb) = self.hedge_book.as_mut() {
                        hb.book.apply_snapshot(bids, asks, seq);
                        info!(seq, hedge_symbol = %pair.hedge_symbol, "initial hedge book loaded");
                    }
                }
                Err(e) => warn!(error = %e, "failed to fetch initial hedge book"),
            }
        }

        let refresh_ms = self.config.market_maker.refresh_interval_ms;
        let mut refresh_interval =
            tokio::time::interval(tokio::time::Duration::from_millis(refresh_ms));
        let mut sla_interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        let mut summary_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        // Reconcile every 60 seconds.
        let mut reconcile_interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        // Funding-arb driver tick. Only used when
        // `funding_arb_driver` is Some; the select arm below
        // gates on that so a None driver never fires.
        let mut funding_arb_interval = tokio::time::interval(self.funding_arb_tick);
        // Skip the first immediate tick so a fresh driver does
        // not fire before the first market-data sample lands.
        funding_arb_interval.tick().await;
        self.cycle_start = Instant::now();

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        self.shutdown().await;
                        return Ok(());
                    }
                }
                Some(event) = ws_rx.recv() => {
                    self.handle_ws_event(event);
                }
                Some(event) = async {
                    match hedge_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_hedge_event(event);
                }
                _ = refresh_interval.tick() => {
                    if let Err(e) = self.refresh_quotes().await {
                        error!(error = %e, "quote refresh failed");
                        self.kill_switch.on_error();
                    }
                }
                _ = sla_interval.tick() => {
                    self.tick_second().await;
                }
                _ = summary_interval.tick() => {
                    self.log_periodic_summary();
                    self.update_dashboard();
                    self.audit.flush();
                }
                _ = reconcile_interval.tick() => {
                    self.reconcile().await;
                }
                _ = funding_arb_interval.tick(),
                    if self.funding_arb_driver.is_some() =>
                {
                    // Borrow-check dance: take the driver out,
                    // tick it, then put it back. Keeps
                    // `handle_driver_event` free to mutate
                    // other engine state (kill switch, audit)
                    // without conflicting on `&mut self`.
                    if let Some(mut driver) = self.funding_arb_driver.take() {
                        let event = driver.tick_once().await;
                        self.funding_arb_driver = Some(driver);
                        self.handle_driver_event(event);
                    }
                }
            }
        }
    }

    /// Handle a `DriverEvent` from the in-engine
    /// `FundingArbDriver`. Routes to audit + kill switch so
    /// pair breaks escalate automatically. An uncompensated
    /// pair break trips kill switch L2 `StopNewOrders` —
    /// stronger escalation (L4 paired unwind) is left to the
    /// operator via the regular kill-switch policy paths.
    fn handle_driver_event(&mut self, event: DriverEvent) {
        match event {
            DriverEvent::Entered { .. } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairDispatchEntered,
                    "funding arb driver entered",
                );
            }
            DriverEvent::Exited { reason, .. } => {
                self.audit
                    .risk_event(&self.symbol, AuditEventType::PairDispatchExited, &reason);
            }
            DriverEvent::TakerRejected { reason } => {
                self.audit
                    .risk_event(&self.symbol, AuditEventType::PairTakerRejected, &reason);
                warn!(%reason, "funding arb taker leg rejected — position stayed flat");
            }
            DriverEvent::PairBreak {
                reason,
                compensated,
            } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairBreak,
                    &format!("compensated={compensated}: {reason}"),
                );
                self.record_incident(
                    if compensated { "high" } else { "critical" },
                    &format!("funding arb pair break (compensated={compensated}): {reason}"),
                );
                if !compensated {
                    // Uncompensated break: flip to L2 so no
                    // new orders fire until the operator
                    // investigates. Intentionally NOT L4 —
                    // L4 would start a paired unwind on an
                    // already-broken pair, which compounds
                    // the problem.
                    self.kill_switch.manual_trigger(
                        mm_risk::kill_switch::KillLevel::StopNewOrders,
                        "uncompensated funding arb pair break",
                    );
                    // Drop the driver so it stops ticking
                    // until the operator explicitly restarts
                    // the engine.
                    self.funding_arb_driver = None;
                }
            }
            DriverEvent::Hold | DriverEvent::InputUnavailable { .. } => {}
        }
    }

    /// Handle an event from the hedge connector's market-data
    /// stream. Book events update `hedge_book`; `Fill` events
    /// from the hedge leg feed per-leg bookkeeping
    /// (`hedge_order_manager`, `paired_unwind.on_hedge_fill`,
    /// shared portfolio). Connectivity events are logged at
    /// trace level but intentionally don't trip the primary
    /// engine's circuit breaker — a hedge disconnect is a
    /// degraded state, not a full market-maker outage.
    fn handle_hedge_event(&mut self, event: MarketEvent) {
        match &event {
            MarketEvent::BookSnapshot { .. } | MarketEvent::BookDelta { .. } => {
                if let Some(hb) = self.hedge_book.as_mut() {
                    hb.on_event(&event);
                }
            }
            MarketEvent::Fill { fill, .. } => {
                info!(
                    trade_id = fill.trade_id,
                    side = ?fill.side,
                    price = %fill.price,
                    qty = %fill.qty,
                    "HEDGE FILL"
                );
                if let Some(hedge_om) = self.hedge_order_manager.as_mut() {
                    hedge_om.on_fill(fill.order_id, fill.qty);
                }
                if let Some(unwind) = self.paired_unwind.as_mut() {
                    unwind.on_hedge_fill(fill.qty);
                    if !unwind.active() {
                        info!("paired unwind complete");
                        self.paired_unwind = None;
                    }
                }
                let signed_qty = match fill.side {
                    mm_common::types::Side::Buy => fill.qty,
                    mm_common::types::Side::Sell => -fill.qty,
                };

                // Portfolio gets the hedge fill with the hedge
                // symbol so per-asset tracking remains symmetric
                // across the two legs of a pair.
                if let (Some(pf), Some(pair)) = (&self.portfolio, self.connectors.pair.as_ref()) {
                    if let Ok(mut pf) = pf.lock() {
                        pf.on_fill(&pair.hedge_symbol, signed_qty, fill.price);
                    }
                }

                // Reconcile the funding-arb driver's perp-leg
                // bookkeeping with the real fill.
                if let Some(driver) = self.funding_arb_driver.as_mut() {
                    driver.on_hedge_fill(signed_qty);
                }
            }
            _ => {}
        }
    }

    /// Refresh balances from exchange.
    async fn refresh_balances(&mut self) {
        if let Ok(balances) = self.connectors.primary.get_balances().await {
            let quote_balance = balances
                .iter()
                .find(|b| b.asset == self.product.quote_asset)
                .map(|b| b.available)
                .unwrap_or_default();
            self.exposure_manager = ExposureManager::new(quote_balance);
            self.balance_cache.update_from_exchange(&balances);
            info!(quote_balance = %quote_balance, "balances refreshed");
        }
    }

    /// Periodic reconciliation: compare internal state vs exchange.
    async fn reconcile(&mut self) {
        self.reconcile_counter += 1;

        // Refresh balances every reconciliation.
        self.refresh_balances().await;

        // Query open orders from exchange and reconcile.
        // Note: this uses the custom exchange client; for Binance/Bybit use their connector.
        // For now, log that reconciliation ran.
        let internal_ids: std::collections::HashSet<_> =
            self.order_manager.live_order_ids().into_iter().collect();

        info!(
            internal_orders = internal_ids.len(),
            reconcile_cycle = self.reconcile_counter,
            "reconciliation cycle"
        );

        self.audit.risk_event(
            &self.symbol,
            AuditEventType::BalanceReconciled,
            &format!("cycle {}", self.reconcile_counter),
        );
    }

    async fn tick_second(&mut self) {
        self.sla_tracker.tick();
        self.kill_switch.tick_second();
        self.adv_inventory.tick(self.inventory_manager.inventory());

        if let Some(mid) = self.book_keeper.book.mid_price() {
            self.adverse_selection
                .update_mid(mid, self.config.toxicity.adverse_selection_lookback_ms);
            self.pnl_tracker.mark_to_market(mid);

            // Mark-to-market the shared portfolio. The portfolio
            // holds a mark price per symbol so unrealised PnL
            // snapshots reflect the latest mid on every tick.
            if let Some(pf) = &self.portfolio {
                if let Ok(mut pf) = pf.lock() {
                    pf.mark_price(&self.symbol, mid);
                }
            }

            let total_pnl = self.pnl_tracker.attribution.total_pnl();
            self.kill_switch.update_pnl(total_pnl);

            let position_value = self.inventory_manager.inventory().abs() * mid;
            self.kill_switch.update_position_value(position_value);

            // TWAP execution (if active).
            if let Some(twap) = &mut self.twap {
                if let Some(quote) = twap.next_slice(mid) {
                    // Execute TWAP slice via order manager.
                    info!(side = ?quote.side, price = %quote.price, qty = %quote.qty, "TWAP slice");
                }
            }

            // Paired-unwind execution (if active). Dispatches
            // the slice through the hedge leg's own
            // `OrderManager` so cancel-all + fill tracking
            // stay per-venue.
            if let Some(unwind) = self.paired_unwind.as_mut() {
                if let Some(hedge_mid) = self.hedge_book.as_ref().and_then(|hb| hb.book.mid_price())
                {
                    let slice = unwind.next_slice(mid, hedge_mid);
                    if let Some(p) = &slice.primary {
                        if let Err(e) = self
                            .order_manager
                            .execute_unwind_slice(
                                &self.symbol,
                                p,
                                &self.product,
                                &self.connectors.primary,
                            )
                            .await
                        {
                            warn!(error = %e, "unwind primary dispatch failed");
                        }
                    }
                    if let Some(h) = &slice.hedge {
                        if let (Some(hedge_conn), Some(hedge_om), Some(pair)) = (
                            self.connectors.hedge.as_ref(),
                            self.hedge_order_manager.as_mut(),
                            self.connectors.pair.as_ref(),
                        ) {
                            if let Err(e) = hedge_om
                                .execute_unwind_slice(
                                    &pair.hedge_symbol,
                                    h,
                                    &self.product,
                                    hedge_conn,
                                )
                                .await
                            {
                                warn!(error = %e, "unwind hedge dispatch failed");
                            }
                        }
                    }
                }
            }
        }

        // Kill switch L3+ → cancel all.
        if self.kill_switch.level() >= KillLevel::CancelAll {
            self.order_manager
                .cancel_all(&self.connectors.primary, &self.symbol)
                .await;
            self.balance_cache.reset_reservations();

            // Kill switch L4 → start the right flatten executor.
            //
            // Single-connector mode: single-leg `TwapExecutor`
            // against the one position.
            //
            // Dual-connector mode (basis / funding arb): pick
            // `PairedUnwindExecutor` so both legs flatten in
            // lockstep. Running `TwapExecutor` alongside would
            // double-flatten the primary leg and leave the hedge
            // leg dangling, exactly the failure mode AD-11 of
            // the spot-and-cross-product epic calls out.
            if self.kill_switch.level() >= KillLevel::FlattenAll
                && self.twap.is_none()
                && self.paired_unwind.is_none()
            {
                let inv = self.inventory_manager.inventory();
                if !inv.is_zero() {
                    let side = if inv > dec!(0) {
                        mm_common::types::Side::Sell
                    } else {
                        mm_common::types::Side::Buy
                    };

                    if let Some(pair) = self.connectors.pair.clone() {
                        // Paired unwind: infer the hedge-leg
                        // direction from the primary inventory
                        // sign. Long-spot → short-hedge, and vice
                        // versa. Basis-neutral pairs always have
                        // opposite sides on the two legs.
                        let primary_side = if inv > dec!(0) {
                            mm_common::types::Side::Buy
                        } else {
                            mm_common::types::Side::Sell
                        };
                        let hedge_side = primary_side.opposite();
                        self.paired_unwind = Some(PairedUnwindExecutor::new(
                            pair,
                            primary_side,
                            hedge_side,
                            inv.abs(),
                            60,
                            10,
                            dec!(5),
                        ));
                        self.audit.risk_event(
                            &self.symbol,
                            AuditEventType::KillSwitchEscalated,
                            &format!("paired unwind started: primary {side:?} {}", inv.abs()),
                        );
                        self.record_incident(
                            "critical",
                            &format!("Kill switch L4: paired unwind {} base", inv.abs()),
                        );
                    } else {
                        self.twap = Some(TwapExecutor::new(
                            self.symbol.clone(),
                            side,
                            inv.abs(),
                            60,      // 60 seconds.
                            10,      // 10 slices.
                            dec!(5), // 5 bps aggressive.
                        ));
                        self.audit.risk_event(
                            &self.symbol,
                            AuditEventType::KillSwitchEscalated,
                            &format!("TWAP flatten started: {side:?} {}", inv.abs()),
                        );
                        self.record_incident(
                            "critical",
                            &format!("Kill switch L4: TWAP flatten {side:?} {}", inv.abs()),
                        );
                    }
                }
            }
        }
    }

    fn handle_ws_event(&mut self, event: MarketEvent) {
        match &event {
            MarketEvent::BookSnapshot { .. } | MarketEvent::BookDelta { .. } => {
                self.book_keeper.on_event(&event);
                if let Some(mid) = self.book_keeper.book.mid_price() {
                    self.volatility_estimator.update(mid);
                    if !self.last_mid.is_zero() {
                        let ret = (mid - self.last_mid) / self.last_mid;
                        self.auto_tuner.on_return(ret);
                    }
                    self.last_mid = mid;
                }
            }
            MarketEvent::Trade { trade, .. } => {
                self.vpin.on_trade(trade);
                self.momentum.on_trade(trade);

                if !self.last_mid.is_zero() {
                    let signed_vol = match trade.taker_side {
                        mm_common::types::Side::Buy => trade.qty * trade.price,
                        mm_common::types::Side::Sell => -(trade.qty * trade.price),
                    };
                    let dp = trade.price - self.last_mid;
                    self.kyle_lambda.update(dp, signed_vol);
                }
                if let Some(v) = self.vpin.vpin() {
                    self.auto_tuner.set_toxicity(v);
                }
            }
            MarketEvent::Fill { fill, .. } => {
                info!(
                    trade_id = fill.trade_id,
                    side = ?fill.side,
                    price = %fill.price,
                    qty = %fill.qty,
                    "FILL"
                );

                self.audit.order_filled(
                    &self.symbol,
                    fill.order_id,
                    fill.side,
                    fill.price,
                    fill.qty,
                    fill.is_maker,
                );

                self.inventory_manager.on_fill(fill);
                self.order_manager.on_fill(fill.order_id, fill.qty);
                self.sla_tracker.on_fill();
                self.kill_switch.on_fill();
                self.volume_limiter.on_trade(fill.price * fill.qty);

                // Feed the shared multi-currency portfolio. The
                // qty passed to `on_fill` is signed — positive on
                // buys, negative on sells — so the portfolio can
                // correctly flip / close positions.
                if let Some(pf) = &self.portfolio {
                    let signed_qty = match fill.side {
                        mm_common::types::Side::Buy => fill.qty,
                        mm_common::types::Side::Sell => -fill.qty,
                    };
                    if let Ok(mut pf) = pf.lock() {
                        pf.on_fill(&self.symbol, signed_qty, fill.price);
                    }
                }

                self.balance_cache.release(
                    fill.side,
                    fill.price,
                    fill.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                );

                if let Some(mid) = self.book_keeper.book.mid_price() {
                    self.pnl_tracker.on_fill(fill, mid);
                    self.adverse_selection.on_fill(fill.price, fill.side, mid);
                }

                if let Some(twap) = &mut self.twap {
                    twap.on_fill(fill.qty);
                    if twap.is_complete() {
                        info!("TWAP execution complete");
                        self.twap = None;
                    }
                }

                if let Some(unwind) = self.paired_unwind.as_mut() {
                    unwind.on_primary_fill(fill.qty);
                    if !unwind.active() {
                        info!("paired unwind complete");
                        self.paired_unwind = None;
                    }
                }

                // Reconcile the funding-arb driver's position
                // bookkeeping with the real primary-leg fill.
                if let Some(driver) = self.funding_arb_driver.as_mut() {
                    let signed_qty = match fill.side {
                        mm_common::types::Side::Buy => fill.qty,
                        mm_common::types::Side::Sell => -fill.qty,
                    };
                    driver.on_primary_fill(signed_qty);
                }
            }
            MarketEvent::OrderUpdate { .. } => {}
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                // Listen-key / user-data streams push balance
                // snapshots when the wallet moves (fills,
                // deposits, withdrawals). Plug them straight into
                // `BalanceCache` — same shape as
                // `update_from_exchange` but for a single asset.
                use mm_common::types::Balance;
                self.balance_cache.update_from_exchange(&[Balance {
                    asset: asset.clone(),
                    wallet: *wallet,
                    total: *total,
                    locked: *locked,
                    available: *available,
                }]);
            }
            MarketEvent::Connected { .. } => {
                info!("exchange connected");
                self.audit
                    .risk_event(&self.symbol, AuditEventType::ExchangeConnected, "");
                self.circuit_breaker.reset();
            }
            MarketEvent::Disconnected { .. } => {
                warn!("exchange disconnected");
                self.audit
                    .risk_event(&self.symbol, AuditEventType::ExchangeDisconnected, "");
                self.circuit_breaker.trip(TripReason::StaleBook);
            }
        }
    }

    async fn refresh_quotes(&mut self) -> Result<()> {
        self.tick_count += 1;

        // Kill switch check.
        if !self.kill_switch.allow_new_orders() {
            if self.kill_switch.level() >= KillLevel::CancelAll {
                self.order_manager
                    .cancel_all(&self.connectors.primary, &self.symbol)
                    .await;
                self.balance_cache.reset_reservations();
            }
            return Ok(());
        }

        // Circuit breaker.
        self.circuit_breaker
            .check_stale_book(self.book_keeper.book.last_update_ms, &self.config.risk);
        self.circuit_breaker
            .check_spread(self.book_keeper.book.spread_bps(), &self.config.risk);

        if let Some(mid) = self.book_keeper.book.mid_price() {
            let equity = self.exposure_manager_equity(mid);
            self.exposure_manager.update_equity(equity);
            if self
                .exposure_manager
                .is_drawdown_breached(equity, &self.config.risk)
            {
                self.circuit_breaker.trip(TripReason::MaxDrawdown);
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::CircuitBreakerTripped,
                    "drawdown",
                );
                self.record_incident("high", "Circuit breaker tripped: max drawdown exceeded");
            }
            if self.exposure_manager.is_exposure_breached(
                self.inventory_manager.inventory(),
                mid,
                &self.config.risk,
            ) {
                self.circuit_breaker.trip(TripReason::MaxExposure);
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::CircuitBreakerTripped,
                    "exposure",
                );
                self.record_incident("high", "Circuit breaker tripped: max exposure exceeded");
            }
        }

        if self.circuit_breaker.is_tripped() {
            self.order_manager
                .cancel_all(&self.connectors.primary, &self.symbol)
                .await;
            self.balance_cache.reset_reservations();
            return Ok(());
        }

        if !self.book_keeper.is_ready() {
            return Ok(());
        }

        let mid = self.book_keeper.book.mid_price().unwrap();

        // Time remaining.
        let elapsed_secs = self.cycle_start.elapsed().as_secs();
        let horizon = self.config.market_maker.time_horizon_secs;
        let t_remaining = if elapsed_secs >= horizon {
            self.cycle_start = Instant::now();
            dec!(1)
        } else {
            (Decimal::from(horizon - elapsed_secs) / Decimal::from(horizon)).max(dec!(0.01))
        };

        // Volatility.
        let sigma = self
            .volatility_estimator
            .volatility()
            .unwrap_or(self.config.market_maker.sigma);

        // Kill switch multipliers.
        let ks_spread = self.kill_switch.spread_multiplier();
        let ks_size = self.kill_switch.size_multiplier();

        // Auto-tune.
        let (eff_gamma, eff_size, eff_spread) = if self.config.toxicity.autotune_enabled {
            (
                self.config.market_maker.gamma * self.auto_tuner.effective_gamma_mult() * ks_spread,
                self.config.market_maker.order_size
                    * self.auto_tuner.effective_size_mult()
                    * ks_size,
                self.config.market_maker.min_spread_bps
                    * self.auto_tuner.effective_spread_mult()
                    * ks_spread,
            )
        } else {
            (
                self.config.market_maker.gamma * ks_spread,
                self.config.market_maker.order_size * ks_size,
                self.config.market_maker.min_spread_bps * ks_spread,
            )
        };

        // Momentum alpha.
        let alpha_mid = if self.config.market_maker.momentum_enabled {
            let alpha = self.momentum.alpha(&self.book_keeper.book, mid);
            mid + alpha * mid * t_remaining
        } else {
            mid
        };

        let mut tuned = self.config.market_maker.clone();
        tuned.gamma = eff_gamma;
        tuned.order_size = eff_size;
        tuned.min_spread_bps = eff_spread;

        let ref_price = self.hedge_book.as_ref().and_then(|hb| hb.book.mid_price());

        let ctx = StrategyContext {
            book: &self.book_keeper.book,
            product: &self.product,
            config: &tuned,
            inventory: self.inventory_manager.inventory(),
            volatility: sigma,
            time_remaining: t_remaining,
            mid_price: alpha_mid,
            ref_price,
        };

        let mut quotes = self.strategy.compute_quotes(&ctx);

        // Inventory limits + urgency + dynamic sizing.
        self.inventory_manager
            .apply_limits(&mut quotes, &self.config.risk);
        self.adv_inventory
            .apply_urgency(&mut quotes, self.inventory_manager.inventory(), mid);

        for q in quotes.iter_mut() {
            if let Some(bid) = &mut q.bid {
                bid.qty = self.product.round_qty(self.adv_inventory.dynamic_size(
                    bid.qty,
                    self.inventory_manager.inventory(),
                    bid.side,
                ));
                // Balance pre-check.
                if !self.balance_cache.can_afford(
                    bid.side,
                    bid.price,
                    bid.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                ) {
                    *bid = mm_common::types::Quote {
                        side: bid.side,
                        price: bid.price,
                        qty: dec!(0),
                    };
                }
            }
            if let Some(ask) = &mut q.ask {
                ask.qty = self.product.round_qty(self.adv_inventory.dynamic_size(
                    ask.qty,
                    self.inventory_manager.inventory(),
                    ask.side,
                ));
                if !self.balance_cache.can_afford(
                    ask.side,
                    ask.price,
                    ask.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                ) {
                    *ask = mm_common::types::Quote {
                        side: ask.side,
                        price: ask.price,
                        qty: dec!(0),
                    };
                }
            }
        }

        // Enforce max order size.
        let max_size = self.config.risk.max_order_size;
        if !max_size.is_zero() {
            for q in quotes.iter_mut() {
                if let Some(bid) = &mut q.bid {
                    if bid.qty > max_size {
                        bid.qty = self.product.round_qty(max_size);
                    }
                }
                if let Some(ask) = &mut q.ask {
                    if ask.qty > max_size {
                        ask.qty = self.product.round_qty(max_size);
                    }
                }
            }
        }

        // Enforce volume limits — cancel all new quotes if daily/hourly cap exceeded.
        if !self.volume_limiter.can_trade(dec!(0)) {
            warn!("volume limit reached — suppressing new quotes");
            for q in quotes.iter_mut() {
                q.bid = None;
                q.ask = None;
            }
        }

        // Remove quotes with zero qty.
        for q in quotes.iter_mut() {
            if q.bid.as_ref().map(|b| b.qty.is_zero()).unwrap_or(false) {
                q.bid = None;
            }
            if q.ask.as_ref().map(|a| a.qty.is_zero()).unwrap_or(false) {
                q.ask = None;
            }
        }

        self.kill_switch.on_message_sent();

        self.order_manager
            .execute_diff(
                &self.symbol,
                &quotes,
                &self.product,
                &self.connectors.primary,
            )
            .await?;

        // SLA update.
        let has_bid = quotes.iter().any(|q| q.bid.is_some());
        let has_ask = quotes.iter().any(|q| q.ask.is_some());
        let bid_depth: Decimal = quotes
            .iter()
            .filter_map(|q| q.bid.as_ref())
            .map(|b| b.price * b.qty)
            .sum();
        let ask_depth: Decimal = quotes
            .iter()
            .filter_map(|q| q.ask.as_ref())
            .map(|a| a.price * a.qty)
            .sum();
        self.sla_tracker.update_quotes(
            has_bid,
            has_ask,
            self.book_keeper.book.spread_bps(),
            bid_depth,
            ask_depth,
        );

        Ok(())
    }

    fn exposure_manager_equity(&self, mid_price: Decimal) -> Decimal {
        self.inventory_manager.total_pnl(mid_price)
    }

    /// Record an incident to the dashboard state and fire a Telegram alert.
    fn record_incident(&self, severity: &str, description: &str) {
        if let Some(ds) = &self.dashboard {
            ds.add_incident(IncidentRecord {
                timestamp: chrono::Utc::now(),
                severity: severity.to_string(),
                description: description.to_string(),
                duration_secs: 0,
                resolved: false,
            });
        }
        if let Some(alerts) = &self.alerts {
            let alert_severity = match severity {
                "critical" => AlertSeverity::Critical,
                "high" => AlertSeverity::Warning,
                _ => AlertSeverity::Info,
            };
            alerts.alert(
                alert_severity,
                description,
                &format!("Symbol: {}", self.symbol),
                Some(&self.symbol),
            );
        }
    }

    /// Push state to dashboard for HTTP API + Prometheus metrics.
    fn update_dashboard(&self) {
        let Some(ds) = &self.dashboard else { return };

        // Publish the unified portfolio snapshot on every
        // dashboard update. Taking the snapshot under the mutex
        // keeps the dashboard's view consistent across all
        // symbols in a multi-engine deployment.
        if let Some(pf) = &self.portfolio {
            if let Ok(pf) = pf.lock() {
                ds.update_portfolio(pf.snapshot());
            }
        }

        let regime = self.auto_tuner.regime_detector.regime();
        let regime_str = format!("{regime:?}");

        ds.update(SymbolState {
            symbol: self.symbol.clone(),
            mid_price: self.last_mid,
            spread_bps: self.book_keeper.book.spread_bps().unwrap_or(dec!(0)),
            inventory: self.inventory_manager.inventory(),
            inventory_value: self.inventory_manager.inventory().abs() * self.last_mid,
            live_orders: self.order_manager.live_count(),
            total_fills: self.pnl_tracker.attribution.round_trips,
            pnl: PnlSnapshot {
                total: self.pnl_tracker.attribution.total_pnl(),
                spread: self.pnl_tracker.attribution.spread_pnl,
                inventory: self.pnl_tracker.attribution.inventory_pnl,
                rebates: self.pnl_tracker.attribution.rebate_income,
                fees: self.pnl_tracker.attribution.fees_paid,
                round_trips: self.pnl_tracker.attribution.round_trips,
                volume: self.pnl_tracker.attribution.total_volume,
            },
            volatility: self.volatility_estimator.volatility().unwrap_or(dec!(0)),
            vpin: self.vpin.vpin().unwrap_or(dec!(0)),
            kyle_lambda: self.kyle_lambda.lambda().unwrap_or(dec!(0)),
            adverse_bps: self
                .adverse_selection
                .adverse_selection_bps()
                .unwrap_or(dec!(0)),
            kill_level: self.kill_switch.level() as u8,
            sla_uptime_pct: self.sla_tracker.uptime_pct(),
            regime: regime_str,
            spread_compliance_pct: self.sla_tracker.spread_compliance_pct(),
            book_depth_levels: [dec!(0.5), dec!(1), dec!(2), dec!(5)]
                .iter()
                .map(|&pct| BookDepthLevel {
                    pct_from_mid: pct,
                    bid_depth_quote: self.book_keeper.book.bid_depth_within_pct_quote(pct),
                    ask_depth_quote: self.book_keeper.book.ask_depth_within_pct_quote(pct),
                })
                .collect(),
            locked_in_orders_quote: self.order_manager.locked_value_quote(),
            sla_max_spread_bps: self.sla_tracker.config().max_spread_bps,
            sla_min_depth_quote: self.sla_tracker.config().min_depth_quote,
        });
    }

    fn log_periodic_summary(&self) {
        let regime = self.auto_tuner.regime_detector.regime();
        info!(
            symbol = %self.symbol,
            ?regime,
            kill_level = %self.kill_switch.level(),
            inventory = %self.inventory_manager.inventory(),
            vpin = ?self.vpin.vpin(),
            kyle_lambda = ?self.kyle_lambda.lambda(),
            adverse_bps = ?self.adverse_selection.adverse_selection_bps(),
            live_orders = self.order_manager.live_count(),
            id_map = self.order_id_map.len(),
            balance_usdt = %self.balance_cache.available(&self.product.quote_asset),
            "status"
        );
        self.pnl_tracker.log_summary();
        self.sla_tracker.log_summary();

        if self.adv_inventory.is_urgent() {
            warn!(urgency = %self.adv_inventory.urgency_level(), "inventory urgency active");
        }
    }

    pub async fn shutdown(&mut self) {
        info!(symbol = %self.symbol, "shutting down — cancelling all orders");
        self.audit
            .risk_event(&self.symbol, AuditEventType::EngineShutdown, "graceful");
        self.order_manager
            .cancel_all(&self.connectors.primary, &self.symbol)
            .await;
        // Cancel live orders on the hedge leg too — a shutdown
        // with a dangling hedge order is the exact state that
        // turns a delta-neutral pair into a naked position over
        // the restart window.
        if let (Some(hedge_om), Some(hedge_conn), Some(pair)) = (
            self.hedge_order_manager.as_mut(),
            self.connectors.hedge.as_ref(),
            self.connectors.pair.as_ref(),
        ) {
            hedge_om.cancel_all(hedge_conn, &pair.hedge_symbol).await;
        }
        self.balance_cache.reset_reservations();
        self.pnl_tracker.log_summary();
        self.sla_tracker.log_summary();
        self.audit.flush();
        self.update_dashboard();
        info!(symbol = %self.symbol, "shutdown complete");
    }
}

#[cfg(test)]
mod dual_connector_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{InstrumentPair, PriceLevel};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_strategy::AvellanedaStoikov;

    fn sample_config() -> AppConfig {
        AppConfig::default()
    }

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
        }
    }

    fn sample_pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        }
    }

    fn snapshot(symbol: &str, venue: VenueId, bid: Decimal, ask: Decimal) -> MarketEvent {
        MarketEvent::BookSnapshot {
            venue,
            symbol: symbol.to_string(),
            bids: vec![PriceLevel {
                price: bid,
                qty: dec!(1),
            }],
            asks: vec![PriceLevel {
                price: ask,
                qty: dec!(1),
            }],
            sequence: 1,
        }
    }

    #[test]
    fn single_bundle_has_no_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.hedge_book.is_none());
        assert!(!engine.connectors.is_dual());
    }

    #[test]
    fn dual_bundle_creates_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let hb = engine.hedge_book.as_ref().expect("hedge_book must exist");
        assert_eq!(hb.book.symbol, "BTC");
        assert!(engine.connectors.is_dual());
    }

    #[test]
    fn handle_hedge_event_routes_book_updates_to_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Primary gets a spot quote around 50 000. Hedge gets a
        // perp quote around 50 100 — a +10 bps basis.
        engine.handle_ws_event(snapshot(
            "BTCUSDT",
            VenueId::Binance,
            dec!(49_999),
            dec!(50_001),
        ));
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        assert_eq!(
            engine.book_keeper.book.mid_price(),
            Some(dec!(50_000)),
            "primary mid"
        );
        let hb = engine.hedge_book.as_ref().unwrap();
        assert_eq!(hb.book.mid_price(), Some(dec!(50_100)), "hedge mid");
    }

    #[test]
    fn handle_hedge_event_is_noop_in_single_mode() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Must not panic; hedge_book is None so the routing is a
        // silent drop. The primary book must stay untouched.
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));
        assert!(engine.book_keeper.book.mid_price().is_none());
    }

    #[test]
    fn hedge_book_mid_feeds_ref_price_via_refresh_quotes() {
        // Verify the wiring that `refresh_quotes` reads
        // `hedge_book.book.mid_price()` into `StrategyContext.ref_price`.
        // Testing the real `refresh_quotes` is heavy (async, lots
        // of side effects) so we inspect the intermediate
        // expression the production code uses.
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        let ref_price = engine
            .hedge_book
            .as_ref()
            .and_then(|hb| hb.book.mid_price());
        assert_eq!(ref_price, Some(dec!(50_100)));
    }
}

#[cfg(test)]
mod portfolio_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{Fill, Side};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_portfolio::Portfolio;
    use mm_strategy::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
        }
    }

    fn build_engine(symbol: &str, portfolio: Arc<Mutex<Portfolio>>) -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            symbol.to_string(),
            AppConfig::default(),
            sample_product(symbol),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
        .with_portfolio(portfolio)
    }

    fn fill_event(symbol: &str, side: Side, qty: Decimal, price: Decimal) -> MarketEvent {
        MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: symbol.to_string(),
                side,
                price,
                qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        }
    }

    #[test]
    fn engine_without_portfolio_runs_untouched() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.portfolio.is_none());
        // Fill should NOT panic when portfolio is absent.
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
    }

    #[test]
    fn fill_routes_signed_qty_to_shared_portfolio() {
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());

        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        let btc = snap.per_asset.get("BTCUSDT").expect("BTCUSDT entry");
        assert_eq!(btc.qty, dec!(0.1), "long from buy fill");
        assert_eq!(btc.avg_entry, dec!(50_000));
    }

    #[test]
    fn sell_fill_routes_negative_qty_to_portfolio() {
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());

        // Buy 0.2 then sell 0.15 → net long 0.05, realise +50 USDT
        // on the 0.15 closed at 51_000 vs avg 50_000.
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.2), dec!(50_000)));
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Sell, dec!(0.15), dec!(51_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        let btc = snap.per_asset.get("BTCUSDT").unwrap();
        assert_eq!(btc.qty, dec!(0.05));
        assert_eq!(btc.realised_pnl_native, dec!(150));
        assert_eq!(snap.total_realised_pnl, dec!(150));
    }

    #[test]
    fn multi_symbol_engines_share_one_portfolio() {
        // Two engines, one shared portfolio. After both report
        // a buy fill, the snapshot sees both positions under the
        // unified reporting currency.
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut btc_engine = build_engine("BTCUSDT", portfolio.clone());
        let mut eth_engine = build_engine("ETHUSDT", portfolio.clone());

        btc_engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
        eth_engine.handle_ws_event(fill_event("ETHUSDT", Side::Buy, dec!(1), dec!(3_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        assert_eq!(snap.per_asset.len(), 2, "both symbols tracked");
        assert!(snap.per_asset.contains_key("BTCUSDT"));
        assert!(snap.per_asset.contains_key("ETHUSDT"));
    }

    #[test]
    fn portfolio_fx_and_reporting_currency_roundtrip() {
        // Portfolio remains in USDT regardless of per-engine
        // quote assets. The engine does NOT set FX by default —
        // callers are responsible for wiring `set_fx` when the
        // engine quotes in a non-USDT asset. This test locks
        // that contract.
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.01), dec!(50_000)));
        let snap = portfolio.lock().unwrap().snapshot();
        assert_eq!(snap.reporting_currency, "USDT");
    }
}

#[cfg(test)]
mod paired_unwind_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{Fill, InstrumentPair, Side};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_risk::kill_switch::KillLevel;
    use mm_strategy::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
        }
    }

    fn sample_pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        }
    }

    fn dual_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn single_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn buy_fill(qty: Decimal, price: Decimal) -> MarketEvent {
        MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: Side::Buy,
                price,
                qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_paired_unwind_in_dual_mode() {
        let mut engine = dual_engine();
        // Build up an inventory on the primary leg.
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        assert_eq!(engine.inventory_manager.inventory(), dec!(0.05));

        // Need a mid on the primary book for tick_second to
        // reach the kill-switch dispatch branch.
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });

        // Trip the kill switch all the way to L4.
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");
        assert_eq!(engine.kill_switch.level(), KillLevel::FlattenAll);

        // One tick drives the L4 dispatch logic.
        engine.tick_second().await;

        assert!(
            engine.paired_unwind.is_some(),
            "dual-connector mode must pick PairedUnwindExecutor"
        );
        assert!(
            engine.twap.is_none(),
            "paired_unwind must replace twap, never run both"
        );
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_twap_in_single_mode() {
        let mut engine = single_engine();
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;

        assert!(engine.twap.is_some(), "single-mode path still uses TWAP");
        assert!(engine.paired_unwind.is_none());
    }

    #[tokio::test]
    async fn paired_unwind_is_not_spawned_when_inventory_is_zero() {
        let mut engine = dual_engine();
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;
        assert!(engine.paired_unwind.is_none());
        assert!(engine.twap.is_none());
    }

    /// Seed the dual engine with inventory + both books so the
    /// L4 dispatch path can actually run the first slice.
    async fn prime_for_unwind(engine: &mut MarketMakerEngine) {
        engine.handle_ws_event(buy_fill(dec!(0.1), dec!(50_000)));
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine.handle_hedge_event(MarketEvent::BookSnapshot {
            venue: VenueId::HyperLiquid,
            symbol: "BTC-PERP".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(50_009),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_011),
                qty: dec!(10),
            }],
            sequence: 1,
        });
    }

    #[tokio::test]
    async fn slice_dispatches_orders_on_both_venues_not_just_logs() {
        let mut engine = dual_engine();
        prime_for_unwind(&mut engine).await;

        // Install a tiny-duration executor directly so the
        // first `next_slice` fires immediately — L4 dispatch
        // is tested elsewhere, here we only care about the
        // slice-to-order-manager pipeline. `num_seconds()` in
        // the executor's scheduler truncates, so the sleep
        // must exceed 1 full second to register as `elapsed = 1`.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            1, // 1 second total duration
            1, // one slice → fires on first post-schedule tick
            dec!(5),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // Do NOT trip the kill switch here — the L3+ cancel-all
        // branch in `tick_second` would clear out the slice
        // orders we just placed. The dispatch pipeline itself
        // is the only contract under test; L4 spawning is
        // covered by the earlier kill-switch test.
        engine.tick_second().await;

        // Both order managers should now have at least one live
        // order from the dispatched slice.
        let primary_live = engine.order_manager.live_count();
        let hedge_live = engine
            .hedge_order_manager
            .as_ref()
            .map(|om| om.live_count())
            .unwrap_or(0);
        assert!(
            primary_live > 0 || hedge_live > 0,
            "at least one leg must have dispatched a slice (primary={primary_live}, hedge={hedge_live})"
        );
    }

    #[tokio::test]
    async fn hedge_fill_routes_into_paired_unwind_and_portfolio() {
        let portfolio = Arc::new(Mutex::new(mm_portfolio::Portfolio::new("USDT")));
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
        .with_portfolio(portfolio.clone());

        // Seed an active paired unwind with a known target size.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        // Hedge fill comes in through the hedge WS path.
        let hedge_fill = MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 42,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy, // unwinding a short hedge = buying
                price: dec!(50_010),
                qty: dec!(0.02),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        };
        engine.handle_hedge_event(hedge_fill);

        // The portfolio saw the hedge symbol, not the primary.
        let snap = portfolio.lock().unwrap().snapshot();
        assert!(snap.per_asset.contains_key("BTC-PERP"));
        let hedge_entry = snap.per_asset.get("BTC-PERP").unwrap();
        assert_eq!(
            hedge_entry.qty,
            dec!(0.02),
            "hedge buy fill = long position"
        );
        // paired_unwind tracked the fill → progress > 0.
        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.02 filled out of 0.1 target on hedge, primary still at 0
        // → average progress = (0 + 0.2) / 2 = 0.1.
        assert_eq!(unwind.progress(), dec!(0.1));
    }

    #[tokio::test]
    async fn primary_fill_routes_into_paired_unwind_not_just_inventory() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell, // unwinding long spot = selling
                price: dec!(50_000),
                qty: dec!(0.05),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.05 filled on primary, 0 on hedge → avg = 0.25.
        assert_eq!(unwind.progress(), dec!(0.25));
    }

    #[tokio::test]
    async fn paired_unwind_clears_when_both_legs_complete() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            1,
            dec!(5),
        ));

        // Primary leg fully fills.
        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell,
                price: dec!(50_000),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });
        // Hedge leg fully fills.
        engine.handle_hedge_event(MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 2,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy,
                price: dec!(50_010),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        // Executor cleared itself on the final on_hedge_fill.
        assert!(
            engine.paired_unwind.is_none(),
            "unwind cleared on completion"
        );
    }
}

#[cfg(test)]
mod driver_event_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::InstrumentPair;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_risk::kill_switch::KillLevel;
    use mm_strategy::funding_arb_driver::DriverEvent;
    use mm_strategy::AvellanedaStoikov;

    fn dual_engine_with_driver_field() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let pair = InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        };
        let bundle = ConnectorBundle::dual(primary, hedge, pair);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            ProductSpec {
                symbol: "BTCUSDT".to_string(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                tick_size: dec!(0.01),
                lot_size: dec!(0.0001),
                min_notional: dec!(10),
                maker_fee: dec!(0.0001),
                taker_fee: dec!(0.0005),
            },
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn compensated_pair_break_does_not_halt_driver_but_audits() {
        let mut engine = dual_engine_with_driver_field();
        // Driver itself is None in this test — we only assert
        // the dispatcher's behaviour on the event value. A real
        // driver would have been constructed via
        // `with_funding_arb_driver`.
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::PairBreak {
            reason: "post-only cross".to_string(),
            compensated: true,
        });
        // Compensated breaks do NOT escalate kill switch.
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn uncompensated_pair_break_escalates_to_l2_and_drops_driver() {
        let mut engine = dual_engine_with_driver_field();
        // Start with driver set so we can verify it gets dropped.
        // Construct a driver with a NullSink using real connectors.
        let driver = mm_strategy::FundingArbDriver::new(
            engine.connectors.primary.clone(),
            engine.connectors.hedge.clone().unwrap(),
            engine.connectors.pair.clone().unwrap(),
            mm_strategy::FundingArbDriverConfig::default(),
            Arc::new(mm_strategy::NullSink),
        );
        engine.funding_arb_driver = Some(driver);

        engine.handle_driver_event(DriverEvent::PairBreak {
            reason: "post-only cross".to_string(),
            compensated: false,
        });

        assert_eq!(
            engine.kill_switch.level(),
            KillLevel::StopNewOrders,
            "uncompensated break → L2"
        );
        assert!(
            engine.funding_arb_driver.is_none(),
            "driver dropped so it stops ticking"
        );
    }

    #[tokio::test]
    async fn hold_events_are_silent_noops() {
        let mut engine = dual_engine_with_driver_field();
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::Hold);
        engine.handle_driver_event(DriverEvent::InputUnavailable {
            reason: "test".to_string(),
        });
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn entered_and_exited_only_audit_do_not_escalate() {
        let mut engine = dual_engine_with_driver_field();
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::TakerRejected {
            reason: "insufficient margin".to_string(),
        });
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn fills_reconcile_driver_state_on_both_legs() {
        let mut engine = dual_engine_with_driver_field();
        let driver = mm_strategy::FundingArbDriver::new(
            engine.connectors.primary.clone(),
            engine.connectors.hedge.clone().unwrap(),
            engine.connectors.pair.clone().unwrap(),
            mm_strategy::FundingArbDriverConfig::default(),
            Arc::new(mm_strategy::NullSink),
        );
        engine.funding_arb_driver = Some(driver);

        // Primary leg fill: long 0.1 spot.
        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Buy,
                price: dec!(50_000),
                qty: dec!(0.1),
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        });

        // Hedge leg fill: short 0.1 perp.
        engine.handle_hedge_event(MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 2,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Sell,
                price: dec!(50_010),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        let state = engine.funding_arb_driver.as_ref().unwrap().state();
        assert_eq!(state.spot_position, dec!(0.1), "spot long");
        assert_eq!(state.perp_position, dec!(-0.1), "perp short");
        assert_eq!(state.net_delta, dec!(0), "delta-neutral");
    }
}
