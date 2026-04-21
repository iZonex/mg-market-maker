//! Real [`MarketMakerEngine`] runner for the agent.
//!
//! The PR-2c-iii-a adapter produced an `AppConfig` but stopped
//! there. This module closes the loop: given a resolved
//! credential + a connector + an `AppConfig`, it subscribes to
//! the venue's market-data stream, builds a `Box<dyn Strategy>`
//! from the configured `StrategyType`, materialises a
//! `MarketMakerEngine`, and runs its primary quoting loop.
//!
//! [`AuthorityHandle`] is the shutdown source. A background
//! task watches the handle; when authority transitions to
//! `Expired` or `Revoked`, it flips a `watch` channel that the
//! engine polls on every tick. The engine's own kill-switch
//! machinery then takes over — existing engine behaviour
//! cancels orders, unwinds inventory per its kill-level
//! configuration, and returns cleanly.
//!
//! Intentional gaps (PR-2c-iii-c):
//! - **Hedge leg** — single-connector only. When the template
//!   needs a hedge, operators get a warn log and the runner
//!   bails out. Wiring the hedge binding through the catalog
//!   requires an additional connector build + `ConnectorBundle::dual`.
//! - **DashboardState** — `None`. The agent doesn't publish to
//!   a per-deployment dashboard because the controller owns the
//!   fleet-wide view (PR-2d-lite + PR-2e).
//! - **AlertManager** — `None`. Per-deployment alerting is a
//!   PR-2f concern once the signed-envelope / reconnect story
//!   lands.

use std::sync::Arc;

use mm_common::config::AppConfig;
use mm_common::types::{InstrumentPair, ProductSpec};
use mm_control::lease::LeaseState;
use mm_engine::{ConnectorBundle, MarketMakerEngine};
use mm_exchange_core::connector::ExchangeConnector;
use mm_strategy::{
    AvellanedaStoikov, BasisStrategy, CrossExchangeStrategy, GlftStrategy, GridStrategy,
    Strategy,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::watch;

use crate::AuthorityHandle;

/// Drive a single DesiredStrategy deployment through a real
/// [`MarketMakerEngine`]. Constructed inside `RealEngineFactory`,
/// spawned on its own tokio task, and joined only by registry
/// abort or engine shutdown.
///
/// When `hedge_connector` is populated the runner builds a
/// `ConnectorBundle::dual`, subscribes the hedge stream, and
/// feeds it into `run_with_hedge` as the second event channel.
/// The `AppConfig.hedge` field drives the engine's hedge-side
/// bookkeeping + the XemmExecutor / BasisStrategy wiring.
pub struct MarketMakerRunner {
    pub symbol: String,
    pub deployment_id: String,
    /// R1-TEMPLATE-3 — template name the deployment was spawned
    /// with. The runner looks it up in `mm_strategy_graph::templates`
    /// and, when a graph body exists, attaches it via
    /// `MarketMakerEngine::with_strategy_graph` before the first
    /// tick. This is what makes `rug-detector-composite`,
    /// `cost-gated-quoter`, `liquidity-burn-guard`, etc. behave
    /// the way their catalog blurbs promise rather than silently
    /// falling back to plain Avellaneda-Stoikov.
    pub template: String,
    pub config: AppConfig,
    pub connector: Arc<dyn ExchangeConnector>,
    pub hedge_connector: Option<Arc<dyn ExchangeConnector>>,
    /// SOR extras — read-only connectors the router may route
    /// through. Carried on the `ConnectorBundle::extra` slot so
    /// `VenueStateAggregator::collect` can include them in route
    /// decisions. NOT subscribed (for PR-2c-iii-d): extras are
    /// fetched on demand by the SOR aggregator. Full streaming
    /// subscription lands later when the aggregator gains a
    /// push-driven book source.
    pub extra_connectors: Vec<Arc<dyn ExchangeConnector>>,
    pub authority: Option<AuthorityHandle>,
    /// Hot-reload receiver — paired with the sender the agent
    /// registry stores in `Running.config_override_tx`. Live
    /// `PATCH .../variables` calls translate each tunable into a
    /// `ConfigOverride` variant and ship it through this channel;
    /// the engine's select loop consumes each variant on its next
    /// tick without a restart.
    pub config_override_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<mm_dashboard::state::ConfigOverride>>,
    /// Agent-local shared `DashboardState`. When set, the
    /// engine publishes operator-facing state into it (atomic
    /// bundles, funding-arb pairs, SOR decisions, ...) so the
    /// agent's FetchDetails handler can serve it as details
    /// topics. `None` = observation-only runner.
    pub dashboard: Option<mm_dashboard::state::DashboardState>,
    /// Fix #3 — tenant ownership of this deployment. Tags
    /// every `FillRecord` the engine writes into DashboardState
    /// with `client_id`, which lights up the per-client PnL
    /// aggregator + per-client circuit breaker + SLA per
    /// tenant. Sourced from `DesiredStrategy.variables["client_id"]`
    /// in the engine factory, with a fallback to the agent's
    /// `profile.client_id` injected by the controller at
    /// deploy time. Absent = shared-infra deployment, legacy
    /// untagged behaviour.
    pub client_id: Option<String>,
}

impl MarketMakerRunner {
    pub async fn run(self) -> anyhow::Result<()> {
        let MarketMakerRunner {
            symbol,
            deployment_id,
            template,
            config,
            connector,
            hedge_connector,
            extra_connectors,
            authority,
            config_override_rx,
            dashboard,
            client_id,
        } = self;

        tracing::info!(
            deployment = %deployment_id,
            symbol = %symbol,
            venue = ?connector.venue_id(),
            product = ?connector.product(),
            strategy = ?config.market_maker.strategy,
            mode = %config.mode,
            "MarketMakerRunner starting"
        );

        let product = match connector.get_product_spec(&symbol).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    symbol = %symbol,
                    error = %e,
                    "get_product_spec failed — using conservative fallback"
                );
                product_fallback(&symbol)
            }
        };

        let strategy: Box<dyn Strategy> = build_strategy(&config, &product);

        // PR-2c-iii-c — dual-bundle path. When both `config.hedge`
        // and a resolved `hedge_connector` are present, the
        // runner subscribes both streams and builds a
        // `ConnectorBundle::dual`. If the config expects a hedge
        // but the factory couldn't supply the connector (stale
        // credential, unknown exchange) we refuse to start the
        // deployment — cross-exchange strategies quietly running
        // single-sided is a known dangerous failure mode.
        // R1-CROSS-1 (2026-04-22) — refuse to start dual-venue
        // strategies on a single-venue bundle. The old `(None, None)`
        // branch built a single-venue bundle regardless of the
        // strategy type: operators would deploy `cross-exchange-basic`
        // / `basis-carry-spot-perp` / `funding-aware-quoter` /
        // `stat_arb` with only a primary credential and the engine
        // would quote primary-only while the catalog promised a
        // hedge leg. Inventory builds unchecked on the primary side
        // and the operator sees "running=True, live_orders=6" with
        // no indication anything is wrong.
        //
        // The right safety net is right here — fail the deploy at
        // runner construction before market data starts streaming.
        // `config.hedge` gets populated by `app_config::build` iff
        // a hedge credential resolved; if the strategy type
        // structurally requires a hedge and it didn't, stop now.
        if strategy_requires_hedge(&config.market_maker.strategy)
            && config.hedge.is_none()
        {
            return Err(anyhow::anyhow!(
                "strategy {:?} requires a hedge credential \
                 (AppConfig.hedge) but none was configured — \
                 refusing to start single-sided to avoid unhedged \
                 exposure. Set `variables.hedge_credential` on the \
                 deployment to bind a second venue.",
                config.market_maker.strategy
            ));
        }

        let (mut bundle, hedge_rx) = match (config.hedge.as_ref(), hedge_connector.clone()) {
            (Some(hedge_cfg), Some(hedge_conn)) => {
                let hedge_symbol = hedge_cfg.pair.hedge_symbol.clone();
                let hedge_rx = hedge_conn
                    .subscribe(std::slice::from_ref(&hedge_symbol))
                    .await
                    .map_err(|e| anyhow::anyhow!("hedge subscribe failed: {e}"))?;
                let pair: InstrumentPair = hedge_cfg.pair.clone().into();
                tracing::info!(
                    deployment = %deployment_id,
                    primary_symbol = %pair.primary_symbol,
                    hedge_venue = ?hedge_conn.venue_id(),
                    hedge_product = ?hedge_conn.product(),
                    hedge_symbol = %pair.hedge_symbol,
                    "dual-connector bundle with hedge leg"
                );
                (ConnectorBundle::dual(connector.clone(), hedge_conn, pair), Some(hedge_rx))
            }
            (Some(_), None) => {
                return Err(anyhow::anyhow!(
                    "config requests hedge leg but no hedge connector was supplied — \
                     refusing to start single-sided to avoid unhedged exposure"
                ));
            }
            (None, Some(_)) => {
                tracing::warn!(
                    deployment = %deployment_id,
                    "hedge connector supplied but config.hedge is empty — dropping hedge and running primary only"
                );
                (ConnectorBundle::single(connector.clone()), None)
            }
            (None, None) => (ConnectorBundle::single(connector.clone()), None),
        };
        // Attach SOR extras — read-only references the aggregator
        // can route through. Empty vec is a no-op on the bundle.
        if !extra_connectors.is_empty() {
            tracing::info!(
                deployment = %deployment_id,
                extras = extra_connectors.len(),
                "attaching {} SOR-extra connector(s) to the bundle",
                extra_connectors.len()
            );
            bundle = bundle.with_extra(extra_connectors);
        }

        // Subscribe to market data BEFORE constructing the engine
        // so the engine's own `run_with_hedge` takes the channel.
        // A failed subscribe here is fatal for the deployment —
        // the reconcile loop will pick the failure up on the next
        // desired-state push and can re-schedule.
        let ws_rx = connector
            .subscribe(std::slice::from_ref(&symbol))
            .await
            .map_err(|e| anyhow::anyhow!("connector.subscribe failed: {e}"))?;

        // Bridge AuthorityHandle -> watch<bool> shutdown. Spawn
        // once per runner; the watcher exits when the channel is
        // dropped so there's no leaked task after engine return.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        if let Some(mut handle) = authority {
            let dep_id = deployment_id.clone();
            tokio::spawn(async move {
                // Observe the initial value first: if we're
                // already Expired / Revoked at subscribe time
                // (controller revoked us before this task started),
                // trip shutdown immediately instead of waiting
                // for the next transition.
                if is_terminal(&handle.current()) {
                    let _ = shutdown_tx.send(true);
                    return;
                }
                while let Ok(state) = handle.changed().await {
                    if is_terminal(&state) {
                        tracing::warn!(
                            deployment = %dep_id,
                            state = ?state,
                            "authority lost — signalling engine shutdown"
                        );
                        let _ = shutdown_tx.send(true);
                        return;
                    }
                }
            });
        }

        let mut engine = MarketMakerEngine::new(
            symbol.clone(),
            config,
            product,
            strategy,
            bundle,
            dashboard, // agent-local scratchpad state; populated for distributed details topics
            None,      // alerts — PR-2f wiring
        );
        // R1-TEMPLATE-3 (2026-04-22) — attach the bundled graph
        // when the template has one. Any template under
        // `crates/strategy-graph/templates/*.json` routes through
        // the graph evaluator; a plain strategy still runs for
        // templates without a graph (or when the graph fails
        // validation / is Restricted without the env gate). A
        // rejection logs a warning but never fails the deploy:
        // the plain strategy continues to produce quotes.
        // Uses `swap_strategy_graph(&mut self)` (in-place) rather
        // than the builder-style `with_strategy_graph` so we can
        // gracefully fall back to the plain strategy on validate
        // failure without losing the constructed engine.
        match mm_strategy_graph::templates::load(&template) {
            Some(Ok(graph)) => {
                let node_count = graph.nodes.len();
                match engine.swap_strategy_graph(&graph) {
                    Ok(()) => tracing::info!(
                        deployment = %deployment_id,
                        template = %template,
                        nodes = node_count,
                        "attached strategy graph from template"
                    ),
                    Err(e) => tracing::warn!(
                        deployment = %deployment_id,
                        template = %template,
                        error = ?e,
                        "graph compile rejected — falling back to plain strategy"
                    ),
                }
            }
            Some(Err(e)) => tracing::warn!(
                deployment = %deployment_id,
                template = %template,
                error = %e,
                "graph JSON parse failed — falling back to plain strategy"
            ),
            None => tracing::debug!(
                deployment = %deployment_id,
                template = %template,
                "no bundled graph for template — plain strategy only"
            ),
        }
        if let Some(rx) = config_override_rx {
            engine = engine.with_config_overrides(rx);
        }
        if let Some(cid) = client_id {
            tracing::info!(
                deployment = %deployment_id,
                client_id = %cid,
                "tagging engine with tenant client_id"
            );
            engine = engine.with_client_id(cid);
        }

        engine
            .run_with_hedge(ws_rx, hedge_rx, shutdown_rx)
            .await
            .map_err(|e| anyhow::anyhow!("engine.run_with_hedge returned error: {e}"))?;
        tracing::info!(deployment = %deployment_id, "MarketMakerRunner exited cleanly");
        Ok(())
    }
}

fn is_terminal(state: &LeaseState) -> bool {
    matches!(state, LeaseState::Expired(_) | LeaseState::Revoked { .. })
}

/// R1-CROSS-1 — which `StrategyType`s structurally require a
/// second (hedge) venue. Extracted so the predicate is testable
/// without spinning up an engine; the runner uses it to fail the
/// deploy early when `AppConfig.hedge` is absent.
pub fn strategy_requires_hedge(s: &mm_common::config::StrategyType) -> bool {
    use mm_common::config::StrategyType::*;
    matches!(
        s,
        Basis | FundingArb | CrossVenueBasis | CrossExchange | StatArb
    )
}

#[cfg(test)]
mod hedge_predicate_tests {
    use super::*;
    use mm_common::config::StrategyType;

    #[test]
    fn strategies_needing_hedge_are_enumerated() {
        for s in [
            StrategyType::Basis,
            StrategyType::FundingArb,
            StrategyType::CrossVenueBasis,
            StrategyType::CrossExchange,
            StrategyType::StatArb,
        ] {
            assert!(
                strategy_requires_hedge(&s),
                "{s:?} must require hedge"
            );
        }
    }

    #[test]
    fn single_venue_strategies_do_not_need_hedge() {
        for s in [
            StrategyType::AvellanedaStoikov,
            StrategyType::Glft,
            StrategyType::Grid,
        ] {
            assert!(
                !strategy_requires_hedge(&s),
                "{s:?} must not require hedge"
            );
        }
    }
}

/// Fall-back [`ProductSpec`] used when the venue doesn't
/// implement `get_product_spec` (custom connector) or the REST
/// call fails. Mirrors `crates/server/src/main.rs::product_fallback`.
pub fn product_fallback(symbol: &str) -> ProductSpec {
    let (base, quote) = split_base_quote(symbol);
    ProductSpec {
        symbol: symbol.to_string(),
        base_asset: base,
        quote_asset: quote,
        tick_size: dec!(0.01),
        lot_size: dec!(0.001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.002),
        trading_status: Default::default(),
    }
}

fn split_base_quote(symbol: &str) -> (String, String) {
    for suffix in ["USDT", "USDC", "BUSD", "FDUSD", "TUSD", "DAI", "BTC", "ETH"] {
        if let Some(base) = symbol.strip_suffix(suffix) {
            return (base.to_string(), suffix.to_string());
        }
    }
    (symbol.to_string(), "USDT".to_string())
}

fn build_strategy(config: &AppConfig, product: &ProductSpec) -> Box<dyn Strategy> {
    use mm_common::config::StrategyType;
    match config.market_maker.strategy {
        StrategyType::AvellanedaStoikov => Box::new(AvellanedaStoikov),
        StrategyType::Glft => Box::new(GlftStrategy::new()),
        StrategyType::Grid => Box::new(GridStrategy),
        StrategyType::Basis | StrategyType::FundingArb | StrategyType::StatArb => {
            let shift = config.market_maker.basis_shift;
            let max_basis_bps = config
                .hedge
                .as_ref()
                .map(|h| h.pair.basis_threshold_bps)
                .unwrap_or_else(|| Decimal::from(50));
            Box::new(BasisStrategy::new(shift, max_basis_bps))
        }
        StrategyType::CrossVenueBasis => {
            let shift = config.market_maker.basis_shift;
            let max_basis_bps = config
                .hedge
                .as_ref()
                .map(|h| h.pair.basis_threshold_bps)
                .unwrap_or_else(|| Decimal::from(50));
            let stale_ms = config.market_maker.cross_venue_basis_max_staleness_ms;
            Box::new(BasisStrategy::cross_venue(shift, max_basis_bps, stale_ms))
        }
        StrategyType::CrossExchange => {
            let min_profit_bps = config.market_maker.cross_exchange_min_profit_bps;
            let mut s = CrossExchangeStrategy::new(min_profit_bps);
            s.set_fees(product.maker_fee, product.taker_fee);
            Box::new(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_splits_known_suffixes() {
        let spec = product_fallback("BTCUSDT");
        assert_eq!(spec.base_asset, "BTC");
        assert_eq!(spec.quote_asset, "USDT");

        let spec = product_fallback("ETHBTC");
        assert_eq!(spec.base_asset, "ETH");
        assert_eq!(spec.quote_asset, "BTC");
    }

    #[test]
    fn fallback_unknown_suffix_treats_whole_as_base() {
        let spec = product_fallback("WEIRDNAME");
        assert_eq!(spec.base_asset, "WEIRDNAME");
        assert_eq!(spec.quote_asset, "USDT");
    }

    #[test]
    fn is_terminal_classifies_correctly() {
        assert!(!is_terminal(&LeaseState::Unclaimed));
        let now = chrono::Utc::now();
        let lease = mm_control::lease::LeaderLease {
            lease_id: uuid::Uuid::nil(),
            agent_id: "a".into(),
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(30),
            issued_seq: mm_control::seq::Seq(1),
        };
        assert!(!is_terminal(&LeaseState::Held(lease.clone())));
        assert!(is_terminal(&LeaseState::Expired(lease.clone())));
        assert!(is_terminal(&LeaseState::Revoked {
            previous: lease,
            reason: "x".into()
        }));
    }
}
