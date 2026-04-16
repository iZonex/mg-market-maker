# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] — 2026-04-16

### Added

- **Roadmap v2 complete: 8 epics, 4 phases.**

#### Epic 1: Multi-Client Isolation
- `ClientConfig` with per-client symbols, SLA targets, webhook URLs, API keys.
- `DashboardState` restructured: per-client `ClientState` partitions with `symbol_to_client` reverse index.
- `client_id` on `AuditEvent`, `FillRecord`, `TokenClaims`, `ApiUser`.
- Per-client endpoints: `/api/v1/client/{id}/sla`, `/api/v1/client/{id}/sla/certificate`, `/api/v1/client/{id}/pnl`, `/api/v1/client/{id}/fills`.
- Client onboarding API: `POST /api/admin/clients`.
- Per-client webhook routing via `dispatch_webhook_for_symbol()`.

#### Epic 2: Token Lending & Loan Management
- `LoanAgreement` data model with terms, return schedule, installment lifecycle.
- JSONL persistence via `LoanStore` (append-only, last-write-wins).
- `LoanUtilizationTracker` with threshold alerts, upcoming/overdue returns.
- Loan cost amortization in `PnlAttribution.loan_cost_amortized`.
- Admin loan CRUD: `POST /api/admin/loans`, `GET /api/admin/loans`.
- Client loan endpoints: `GET /api/v1/loans`, `GET /api/v1/loans/{symbol}`.
- 6 new `AuditEventType` variants for loan lifecycle.

#### Epic 3: Cross-Symbol Portfolio Risk
- `PortfolioRiskManager` with per-factor delta limits and global delta guard.
- `PortfolioVarGuard` — parametric Gaussian VaR on portfolio PnL deltas.
- Shared `FactorCovarianceEstimator` with `merge_observation()` for auto-registering factors.
- `correlation_matrix()` for pairwise factor correlations.
- Background tokio task (30s interval): evaluate risk, broadcast `ConfigOverride::PortfolioRiskMult`.
- Dashboard: `GET /api/v1/portfolio/risk`, `GET /api/v1/portfolio/correlation`.

#### Epic 4: Cross-Venue Execution
- `withdraw()` + `internal_transfer()` for Binance and Bybit connectors.
- `binance_transfer_type()` mapping (MAIN_UMFUTURE, etc.).
- `TransferRecord` JSONL persistence via `transfer_log.rs`.
- `RebalancerCfg` with `auto_execute`, cooldown, max transfer per cycle.
- `sor_inline_enabled` config flag for SOR auto-dispatch.
- 5 new `AuditEventType` variants for transfers.

#### Epic 5: Compliance & Reporting
- `Deserialize` on `AuditEvent` + `AuditEventType` (was Serialize-only).
- `audit_reader.rs`: `read_audit_range()`, `read_audit_filtered()`, `export_signed()` with HMAC-SHA256.
- `mica_report.rs`: MiCA Article 17 report template.
- `ReportBranding` on `ClientConfig` for per-client report customization.
- `WebhookEvent::ReportReady` for scheduled report delivery.
- `GET /api/v1/audit/export?from=&to=` endpoint.

#### Epic 6: Strategy Optimization & A/B Testing
- `OptimizationState` in dashboard with admin endpoints.
- `AbSplitEngine` with time-based / symbol-based split, per-variant performance tracking.
- A/B multipliers wired into engine `refresh_quotes()`.

#### Epic 7: Disaster Recovery
- `Checkpoint::validate()` sanity checks (timestamp, inventory, prices).
- `checkpoint_restore` config flag + `with_checkpoint_restore()` engine builder.
- `fill_replay.rs`: `replay_fills_from_audit()`, `validate_checkpoint_against_replay()`.
- `HealthManager` with Normal/Degraded/Critical state machine.

#### Epic 8: Paper Trading Parity
- `PaperFillCfg` configuration for `ProbabilisticFiller` parameters.
- `demo_data.rs`: synthetic market event generator (random walk + mean-reversion, deterministic LCG).

#### Pre-Flight Toolkit
- `preflight.rs`: 8 automated pre-trade checks (venue, symbol, tick/lot, fees, balance, rate limit, config sanity). Wired into main.rs — runs before every startup.
- Stale book watchdog in engine: auto-pause quoting + cancel orders when book data stale, auto-resume on fresh data.
- Smoke test mode: `MM_MODE=smoke` validates full connector stack (subscribe, measure latency, place/cancel test order, fetch balances) in 30 seconds.
- Market data recorder: `record_market_data = true` writes BookSnapshot + Trade events to JSONL for offline backtesting.
- `CalibrationReport` with GO / NEEDS_MORE_DATA / UNPROFITABLE recommendation.
- `GET /api/v1/system/preflight` health endpoint.

#### Documentation
- 6 new guides: quickstart, writing strategies, architecture, operations, adding exchanges, configuration reference.
- README fully rewritten: badges, comparison table, collapsible features, API reference, architecture diagram, contributing guidelines, disclaimer.

### Changed

- `EventRecorder` uses append mode (survives restarts).
- `BookKeeper` tracks `last_update_at` for stale detection.
- Portfolio risk multiplier applied in engine `refresh_quotes()`.

### Stats

- 1128 → 1213 tests (+85)
- ~55K → ~62K LoC (+7K)
- 19 new files, ~40 modified files
- Zero clippy warnings

## [Unreleased]

### Added

- **Risk summary + trade flow + admin symbol list** (Apr 2026).
  - `GET /api/v1/risk/summary` — unified single-pane risk view:
    position, kill switch, spread, toxicity (VPIN/Kyle/AS),
    market resilience, OTR, PnL, Sharpe, drawdown, market
    impact. Token projects & risk committees consume this.
  - `GET /api/v1/trade-flow` — per-symbol flow analysis:
    fills, volume, spread PnL, inventory PnL, net fee income,
    round trips, volume per trip.
  - `GET /api/admin/symbols` — admin overview of all active
    symbols with state summary, config channel status, regime.
  - Total: 24 client + 13 admin endpoints.

- **System diagnostics + PnL time-series + alert rules engine**
  (Apr 2026).
  - `GET /api/v1/system/diagnostics` — version, uptime, active
    symbols, total fills/volume, config channels, webhook URLs,
    alert rule count. K8s/monitoring integration.
  - `GET /api/v1/pnl/timeseries?symbol=X` — rolling 24h PnL
    time-series (1440 entries at 1-min cadence). Engine pushes
    on every summary tick. Powers frontend charts.
  - Alert rules engine:
    `POST /api/admin/alerts` — add configurable rules (PnL
    below, spread above, inventory above, uptime below).
    `GET /api/admin/alerts` — list rules.
    `GET /api/admin/alerts/check` — evaluate all rules against
    current state, return triggered rules.
  - Total: 22 client + 12 admin endpoints.

- **Automated daily snapshots + audit API + book analytics +
  rate limiter** (Apr 2026).
  - Automated daily report snapshots: engine detects UTC date
    change on summary tick and calls
    `DashboardState::snapshot_daily_report()`. 90-day rolling
    buffer persisted in memory.
  - `GET /api/v1/audit/recent?limit=N&symbol=X&event_type=Y`
    — query the audit JSONL log with filtering by symbol and
    event type. Returns structured JSON events.
  - `GET /api/v1/book/analytics` — per-symbol order book
    analytics: depth at multiple levels, top-of-book imbalance,
    liquidity score (depth/spread), locked order value.
  - `RateLimiter` module: token-bucket rate limiting per API
    key. Configurable max requests/minute with automatic
    bucket cleanup. 3 new tests.
  - Total client API: 20 endpoints.

- **Config validation + K8s probes + historical reports**
  (Apr 2026).
  - Config validation: VaR guard EWMA lambda range check,
    listing sniper scan interval warning, toxicity param
    validation.
  - K8s readiness/liveness probes: `/ready` returns 503
    until at least one symbol has market data (mid > 0).
    `/health` always 200 (liveness).
  - Historical daily report storage: 90-day rolling buffer
    in DashboardState. `GET /api/v1/report/history` lists
    available dates. `GET /api/v1/report/history/{date}`
    retrieves a specific day's report.

- **Webhook wiring + admin management + engine integration**
  (Apr 2026).
  - Engine dispatches webhooks on: engine start, kill switch
    escalation (critical incidents), large fills (>$10k
    notional).
  - `GET /api/admin/webhooks` — list configured URLs, delivery
    stats (sent/failed counts).
  - `POST /api/admin/webhooks` — register a new webhook URL
    at runtime without restart.
  - Webhook dispatcher shared across all engines via server
    startup, registered in DashboardState.

- **Performance API + webhook notifications** (Apr 2026).
  - `GET /api/v1/performance` — per-symbol Sharpe ratio,
    Sortino ratio, max drawdown, fill rate, inventory turnover,
    spread capture, win rate, profit factor. Engine wires
    `PerformanceTracker` with periodic returns, equity updates,
    and inventory samples.
  - `WebhookDispatcher` framework: configure URLs, dispatch
    JSON payloads on key events (SLA breach, kill switch,
    large fill, daily report, engine start/stop). Non-blocking
    async delivery with 5s timeout. 3 new tests.
  - Total client API: 15 endpoints.

- **Cross-venue fund management: withdraw + transfer + rebalancer**
  (Apr 2026).
  - `ExchangeConnector::withdraw(asset, qty, address, network)`
    trait method with default `NotSupported`. Venue connectors
    override when programmatic withdrawals are available.
  - `ExchangeConnector::internal_transfer(asset, qty, from, to)`
    for intra-venue wallet transfers (spot→futures, etc).
  - `Rebalancer` module: monitors per-venue balances, recommends
    transfers from surplus to deficit venues based on configurable
    thresholds. Advisory-only in v1; auto-execution is stage-2.
    4 new tests.

- **Prometheus metrics fix + market impact gauges + client
  portal spread compliance fix** (Apr 2026).
  - Fix 3 uninitialized Prometheus metrics: `MARKET_RESILIENCE`,
    `ORDER_TO_TRADE_RATIO`, `HMA_VALUE` — were declared but
    never force-initialized in `init()`.
  - 3 new Prometheus gauges: `mm_market_impact_mean_bps`,
    `mm_market_impact_adverse_pct`, `mm_fill_slippage_avg_bps`.
    Engine pushes market impact report to Prometheus on every
    dashboard update tick.
  - Client portal: `spread_quality` endpoint now uses real
    `spread_compliance_pct` (was `sla_uptime_pct`), real SLA
    target (was hardcoded 100), and volatility-adjusted
    high-vol estimate.

- **Market impact API + fill reload + admin bulk config**
  (Apr 2026).
  - `GET /api/v1/market-impact` — per-symbol market impact
    report with mean/median/std/P50 impact bps, adverse fill
    percentage. Engine wires estimator on every fill + mid
    update.
  - Fill history reload: `load_fill_history()` restores recent
    fills from `data/fills.jsonl` on startup. Fills survive
    process restarts.
  - `POST /api/admin/config/bulk` — apply a config override to
    all symbols matching a substring pattern. Body includes
    `pattern` and `override` fields. Returns matched symbols
    and applied count.

- **Per-hour SLA breakdown + fill persistence + market impact
  estimator** (Apr 2026).
  - `GET /api/v1/sla/hourly` — 24-entry per-hour SLA breakdown
    (compliance %, two-sided %, worst spread, minutes with data
    per UTC hour). Clients verify performance during specific
    trading windows.
  - `SlaTracker::hourly_presence_summary()` aggregates the
    per-minute buckets into hourly summaries.
  - Fill persistence: `DashboardState::enable_fill_log(path)`
    writes every fill to a JSONL file for history across
    restarts. Wired in server startup to `data/fills.jsonl`.
  - `MarketImpactEstimator`: tracks how fills correlate with
    subsequent mid-price moves over a configurable tick horizon.
    `MarketImpactReport` with mean/median/std impact in bps,
    adverse fill percentage. 7 new tests.

- **SLA compliance certificate + admin pause/resume**
  (Apr 2026).
  - `GET /api/v1/sla/certificate` — structured compliance
    proof for token projects and exchange audit teams. Per-symbol
    presence %, two-sided %, spread compliance, volume, fills,
    configured SLA limits, compliance verdict, HMAC signature.
  - `POST /api/admin/symbols/{symbol}/pause` — pause quoting
    for a specific symbol without restart.
  - `POST /api/admin/symbols/{symbol}/resume` — resume quoting.
  - `ConfigOverride::PauseQuoting` / `ResumeQuoting` variants
    wired through the hot-reload channel.

- **Hot config reload via admin API** (Apr 2026). Operators
  can change MM parameters on running engines without restart:
  - `POST /api/admin/config/{symbol}` — per-symbol override
  - `POST /api/admin/config` — broadcast to all engines
  - `ConfigOverride` enum: Gamma, MinSpreadBps, OrderSize,
    MaxDistanceBps, NumLevels, MomentumEnabled,
    MarketResilienceEnabled, AmendEnabled, AmendMaxTicks,
    OtrEnabled, MaxInventory.
  - Channel-based architecture: each engine receives overrides
    via mpsc channel in the select loop, applies to its owned
    config copy. No shared mutexes, no restarts.
  - All overrides logged to audit trail.

- **Execution quality reporting + CSV export** (Apr 2026).
  - `GET /api/v1/fills/slippage` — aggregated slippage report
    with P50/P95/P99 percentiles, maker/taker split, total
    fees/rebates, avg price improvement vs mid.
  - `GET /api/v1/report/daily/csv` — CSV export of the daily
    report for auditors and client compliance teams.

- **Client API: fills endpoint + NBBO capture + SLA depth fix
  + Telegram commands** (Apr 2026). Competitive gap closure
  for token-project clients:
  - `GET /api/v1/fills/recent?symbol=X&limit=N` — per-fill
    records with NBBO snapshot (best bid/ask at fill time) and
    slippage in bps. `FillRecord` stored in `DashboardState`
    (capped at 1000, oldest evicted).
  - SLA endpoint now returns real `bid_depth` / `ask_depth`
    from book depth levels (was hardcoded zeros), plus
    `sla_max_spread_bps`, `spread_compliance_pct`,
    `presence_pct_24h`, `two_sided_pct_24h`.
  - Engine wires NBBO capture on every fill via the book
    keeper's best bid/ask at fill time.
  - Telegram: `/positions` (alias `/pos`) + `/help` commands.

- **HawkesTradeFlow feature extractor** (Apr 2026). Wraps
  `BivariateHawkes` as a microstructure feature: captures
  trade-arrival clustering that EWMA-based `TradeFlow` misses.
  `default_crypto()` constructor with tuned parameters.
  4 new tests.

- **Production hardening: order reconciliation, dynamic product
  spec, covariance wiring, batch cancel_all** (Apr 2026).
  - **Real order reconciliation**: `reconcile()` now queries
    `get_open_orders()` from the venue and diffs against the
    internal `OrderManager` state. Detects ghost orders
    (tracked locally but absent on venue → removed) and
    phantom orders (live on venue but not tracked → adopted
    so the next diff can cancel them).
  - **Dynamic product spec from venue**: `product_for_symbol()`
    now calls `connector.get_product_spec()` at engine startup
    instead of panicking on unknown symbols. Falls back to
    conservative defaults when the venue doesn't support it.
    Removes the hardcoded 3-symbol lookup table.

- **Production hardening: covariance estimator wiring + safe
  mid-price access** (Apr 2026).
  - Wire `FactorCovarianceEstimator` into engine: per-tick
    mid-price returns feed the rolling estimator, replacing the
    v1 constant-1.0 `factor_variances()` stub. Hedge optimizer
    now uses real rolling variance.
  - Safe mid-price: replace `unwrap()` on `book.mid_price()`
    with `let Some(mid) = ... else { return Ok(()) }` to
    prevent panics on empty books after flash crashes.

- **Epic F stage-2 — Cartea-Jaimungal Poisson jump intensity
  for news retreat** (Apr 2026). `NewsJumpIntensity` models
  news arrival as a self-exciting point process with per-class
  weights and exponential decay. Continuous multiplier
  `M(t) = 1 + (M_max − 1) · min(1, λ(t)/λ_sat)` replaces
  the binary cooldown tier for smoother retreat profiles.
  Composable with the existing state machine via `max()`.
  8 new tests.

- **Hawkes self-exciting point process intensity estimator**
  (Apr 2026). New `HawkesIntensity` (univariate) and
  `BivariateHawkes` (mutually-exciting buy/sell) in the
  indicators crate. O(1) per-event updates via recursive
  kernel trick. `intensity_imbalance_at(t)` exposes the
  buy/sell intensity ratio for alpha signals. Taylor-series
  exp(-x) approximation, 6 terms. 13 new tests.

- **Epic C stage-2 — off-diagonal factor covariance estimator**
  (Apr 2026). `FactorCovarianceEstimator` in the hedge
  optimizer module: rolling-window per-factor return buffers,
  `variances()` for diagonal, `covariance(a, b)` and
  `correlation(a, b)` for off-diagonal entries. Replaces the
  v1 constant-1.0 diagonal stub. 5 new tests.

- **Epic D stage-2 — Stoikov iterative fixed-point for learned
  microprice** (Apr 2026). Adds `finalize_iterative()` to
  `LearnedMicroprice` that fills sparse buckets via
  inverse-distance weighting from well-sampled neighbors
  instead of clamping to zero. Single-pass, no oscillation —
  only well-sampled anchors contribute. 3 new tests.

- **Epic B stage-2 — background pair screener** (Apr 2026).
  `PairScreener` maintains a rolling mid-price buffer per
  symbol and runs Engle-Granger cointegration tests on all
  configured pairs on demand. Rolling buffer capped at 500
  samples per symbol. 5 new tests.

- **Epic C stage-2 — full-engine stress integration tests**
  (Apr 2026). Three new tests that run ALL five canonical
  stress scenarios and validate invariants: drawdown sign,
  LUNA worst-case ordering, kill-switch trips on severe
  scenarios.

- **Epic C stage-2 — cross-beta hedge optimizer** (Apr 2026).
  Extends the diagonal-only Markowitz hedge optimizer with
  off-diagonal β support. `HedgeInstrument.cross_betas` carries
  `(factor, beta)` pairs for cross-factor exposure. When ETH-PERP
  has `cross_betas = [("BTC", 0.4)]`, hedging ETH also reduces
  BTC residual exposure by `hedge_qty * 0.4`. The optimizer
  processes instruments in order and reduces the residual vector
  as it goes. 3 new tests.

- **Epic F stage-2 — per-side asymmetric lead-lag widening**
  (Apr 2026). When the leader moves UP, the follower's bid
  (stale side) gets the full multiplier; the ask (safe side)
  gets `1 + excess/2`. Mirror for DOWN moves. New methods
  `bid_multiplier()` / `ask_multiplier()` on `LeadLagGuard`.
  4 new tests.

- **Epic A stage-2 — auto-refresh SOR venue seeds from
  fee-tier** (Apr 2026). `VenueStateAggregator::update_fees()`
  method + wiring in `refresh_fee_tiers()` pushes live
  maker/taker rates into the SOR cost model so route
  recommendations reflect the latest fee tier.

- **Epic E stage-2 — per-order batch failure outcomes**
  (Apr 2026). Replaces the all-or-nothing batch error handling
  with per-order outcome tracking:
  - `BatchPlaceOutcome` enum: `Placed`, `PlacedFallback`,
    `Failed`, `Unacknowledged` — gives the engine full
    visibility into which orders succeeded via batch vs
    per-order fallback vs failed entirely.
  - `BatchCancelOutcome` enum: `Cancelled`, `CancelledFallback`,
    `Failed`.
  - ID-count mismatch now retries unacknowledged orders
    individually instead of silently dropping them.
  - `execute_diff` logs per-order failure/fallback counts.
  - Existing batch tests updated with outcome assertions.

- **Epic B stage-2 — ADF lag selection via AIC** (Apr 2026).
  Upgrades the Engle-Granger ADF regression from zero-lag to
  AIC-selected lag order (0..12). The augmented regression
  `Δε[t] = ρ·ε[t-1] + Σ γ_j·Δε[t-j] + u[t]` is solved via
  Decimal-domain normal equations with Gauss-Jordan inverse.
  AIC = T·ln(SSR/T) + 2·(p+1) selects the best lag.
  `run_with_lag(y, x, p)` exposes fixed-lag for callers that
  want to bypass AIC. 4 new tests.

- **Epic C stage-2 — historical-simulation VaR cross-check**
  (Apr 2026). Adds non-parametric VaR alongside the Gaussian
  estimate: sorts the PnL buffer and picks the empirical
  quantile (`hist_var_95`, `hist_var_99` in `RiskMetrics`).
  No distributional assumption. 4 new tests.

- **Epic C stage-2 — CVaR + EWMA variance in VaR guard**
  (Apr 2026). Extends the parametric Gaussian VaR guard with
  two stage-2 enhancements:
  - **CVaR / Expected Shortfall**: `RiskMetrics` snapshot
    exposes `cvar_95` and `cvar_99` under the Gaussian
    assumption (`CVaR_α = μ − σ·φ(z_α)/(1−α)`). Exposed for
    dashboard / audit — does NOT feed into throttle tiers.
  - **EWMA variance**: optional exponentially-weighted variance
    estimator (`ewma_lambda` config knob) that reacts faster
    to regime changes than equally-weighted sample variance.
    The guard uses `max(sample_σ, ewma_σ)` for a conservative
    VaR estimate. 7 new tests.

- **Epic B stage-2 — MacKinnon polynomial fit for ADF critical
  values** (Apr 2026). Replaces the v1 5-entry lookup table
  with the MacKinnon (1996) response-surface polynomial
  `c(p,n) = β_∞ + β_1/n + β_2/n²`. Supports 1–6 variables
  at 1%/5%/10% significance levels. Continuous function valid
  for any n ≥ 2, eliminates interpolation and clamping. 6 new
  tests covering monotonicity, asymptotic convergence,
  multi-variable ordering, and significance level ordering.

- **Epic F stage-3 — listing sniper engine integration**
  (Apr 2026). Wires the standalone `ListingSniper` discovery
  module into the server's async runtime as a background task.
  - `ListingSniperRunner`: periodic scan loop over all venue
    connectors, routes `Discovered`/`Removed` events to the
    audit trail (`ListingDiscovered`/`ListingRemoved` event
    types) and Telegram alerts.
  - Config: `[listing_sniper]` section with `enabled`,
    `scan_interval_secs`, `alert_on_discovery` knobs.
  - Server wiring: spawned as a background `tokio::spawn` task
    alongside per-symbol engines, respects the shutdown signal.
  - 3 new tests: audit routing, connector error tolerance,
    removal detection.

- **Epic B stage-3 — Johansen multivariate cointegration test**
  (Apr 2026). Generalises the bivariate Engle-Granger test to
  N ≥ 2 assets. Implements the full Johansen (1991) procedure:
  VECM differencing, constant-concentrated moment matrices
  S00/S01/S10/S11, generalised eigenvalue problem via QR
  iteration with Wilkinson shifts, trace and max-eigenvalue
  statistics against Osterwald-Lenum 1992 critical values
  (5% significance, N = 2..6). Pure function, no external
  linear algebra deps — uses f64 internally for eigenvalue
  decomposition, Decimal results for the public API.
  - `JohansenTest::run(&[&[Decimal]]) -> Option<JohansenResult>`
  - `JohansenResult`: rank, eigenvalues, eigenvectors
    (cointegrating vectors), trace/max-eigen stats, critical
    values, effective sample size.
  - 16 tests: rank detection (bivariate + trivariate
    cointegrated pairs, independent walks), eigenvalue bounds,
    eigenvector dimensions, trace stat monotonicity,
    determinism, matrix inverse, critical value tables.

- **Epic D stage-3 — wave-2 signal observability on the
  dashboard / Prometheus** (Apr 2026). Closes the
  observability gap that the wave-2 signal rollout left:
  `momentum.ofi_ewma()`, `momentum.learned_microprice_drift()`,
  and the per-side `as_prob_bid` / `as_prob_ask` derived in
  `refresh_quotes` were all live in the strategy → engine
  pipeline but had no operator visibility. Stage-3 wires
  them through the existing dashboard publish path.
  - **4 new Prometheus gauges** in `mm_dashboard::metrics`:
    - `mm_momentum_ofi_ewma{symbol}` — the CKS L1 OFI EWMA
      from `MomentumSignals`. Defaults to 0.0 before the
      OFI tracker is attached or sees its first observation.
    - `mm_momentum_learned_mp_drift{symbol}` — the Stoikov
      2018 learned-microprice drift expressed as a fraction
      of the current mid. Defaults to 0.0 when no learned
      MP model is attached or the current `(imbalance, spread)`
      bucket is under-sampled.
    - `mm_as_prob_bid{symbol}` / `mm_as_prob_ask{symbol}` —
      per-side adverse-selection probabilities derived from
      `AdverseSelectionTracker::adverse_selection_bps_{bid,ask}`
      via `cartea_spread::as_prob_from_bps`. Both default
      to 0.5 (neutral) until the per-side tracker has ≥5
      completed fills on that side.
  - **`SymbolState`** in `mm-dashboard::state` gains 4 new
    optional fields: `as_prob_bid`, `as_prob_ask`,
    `momentum_ofi_ewma`, `momentum_learned_mp_drift`. The
    JSON API preserves `None` for under-sampled / not-attached
    states; the Prometheus gauges baseline at 0.5 / 0.0
    so Grafana sees a stable pre-warmup baseline.
  - **`MarketMakerEngine::update_dashboard`** now populates
    the new fields by reading the existing accessors:
    `adverse_selection.adverse_selection_bps_bid()` /
    `_ask()` (via `cartea_spread::as_prob_from_bps`),
    `momentum.ofi_ewma()`, and the newly-promoted
    `momentum.learned_microprice_drift()` (was private
    before stage-3).
  - **`MomentumSignals::learned_microprice_drift`** is
    promoted from private to `pub` so the engine can call
    it from the dashboard publish path without re-deriving
    the `(imbalance, spread)` lookup. No semantic change.
  - **2 new dashboard tests** in `mm-dashboard::state::tests`:
    - `state_update_accepts_new_wave2_fields` — pin the
      end-to-end flow: `state.update(SymbolState{...})`
      with populated wave-2 fields → JSON API readback
      preserves them
    - `state_update_preserves_none_in_json_api` — pin the
      `None`-flow-through invariant for under-sampled /
      not-attached states
  - **Workspace stats**: 1026 → **1028 tests** (+2),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Epic D stage-3 — Cartea AS + per-side ρ for Basis +
  CrossExchange strategies** (Apr 2026). Closes the
  coverage gap that the Avellaneda + GLFT per-side wiring
  surfaced: `BasisStrategy` and `CrossExchangeStrategy`
  ignored both the symmetric `as_prob` and the per-side
  `as_prob_bid` / `as_prob_ask` fields, leaving them out
  of the Cartea closed-form widening menu while the other
  spot quoters were on. Stage-3 brings them into parity.
  - **`BasisStrategy::compute_quotes`** — adds the same
    `match (ctx.as_prob_bid, ctx.as_prob_ask)` shape as
    Avellaneda. The level-0 `half_min` is widened
    independently per side via
    `(1 − 2·ρ_side) · σ · √(T − t)` and safety-clamped at
    the wave-1 `half_min` floor so informed flow on
    either side never produces a sub-`min_spread_bps`
    quote. Each per-level offset stacks the wave-1
    `level_step` on top of the per-side widened half.
    Symmetric fallback when per-side fields are absent;
    no-op when both per-side and symmetric `as_prob` are
    `None` (byte-identical to wave-1).
  - **`CrossExchangeStrategy::compute_quotes`** — adds
    the AS additive shift to the existing `min_ask` /
    `max_bid` profit-floor edges. Bid widens *down* and
    ask widens *up* by the per-side or symmetric ρ
    component. **Safety floor at zero** (not at the
    wave-1 floor like Basis) — informed flow on
    cross-exchange must NEVER tighten the profit floor
    because that would invite an adverse fill below the
    fee threshold. The `xexch_high_rho_does_not_narrow_profit_floor`
    test pins this invariant.
  - **7 new strategy tests** (3 Basis + 4 CrossExchange):
    None-byte-identity, symmetric-low-ρ-widens-both,
    per-side-widens-one-side-only on Basis;
    None-byte-identity, symmetric-low-ρ-widens-floor,
    high-ρ-clamps-at-zero, per-side-widens-one-side-only
    on CrossExchange.
  - **Workspace stats**: 1019 → **1026 tests** (+7),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Epic D stage-3 — per-pair learned MP models keyed by
  symbol** (Apr 2026). Closes the natural follow-up from the
  engine-side learned-MP auto-attach (`momentum_learned_microprice_path`):
  multi-symbol deployments that fit a separate
  `LearnedMicroprice` per pair offline can now wire each
  pair to its own TOML file. New `MarketMakerConfig` field
  `momentum_learned_microprice_pair_paths: HashMap<String,
  String>` keyed by symbol. Lookup order at engine
  construction time:
  1. `momentum_learned_microprice_pair_paths.get(symbol)` —
     per-pair entry takes precedence
  2. `momentum_learned_microprice_path` — system-wide
     fallback
  3. None — no learned MP signal attached
  Same load-failure semantics as the system-wide path: a
  malformed or missing file logs a warning (now including
  the engine's symbol) and continues without the signal.
  All 7 test fixtures updated with a default empty
  `HashMap::new()`. **3 new engine integration tests**:
  per-pair entry takes precedence over system-wide,
  unmatched per-pair map falls through to system-wide,
  both empty skips the load entirely.
  - **Workspace stats**: 1016 → **1019 tests** (+3),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Epic F stage-3 — multi-category Bybit `list_symbols`**
  (Apr 2026). Closes the deferral that Track 3 of the
  stage-2 parallel push documented: the Bybit V5
  `list_symbols` impl scoped to a single
  `BybitCategory` (the connector's own
  `self.category`). Stage-3 ships
  [`BybitConnector::list_symbols_all_categories`] — a
  public async helper that fans out one HTTP request per
  category (`spot`, `linear`, `inverse`) against
  `/v5/market/instruments-info`, parses each response via
  the shared `parse_bybit_instruments_list` helper, and
  returns the merged list. Operators running the listing
  sniper across all Bybit V5 categories now call this
  from any single connector instance instead of
  spinning up three connectors. Per-category failures
  surface as `Err` (partial-success aggregation is a
  stage-4 polish; the listing sniper consumer can also
  fall back to per-category calls). The trait
  `list_symbols` impl stays per-category for backward
  compat — operators with a multi-category use case
  call the new helper directly. **2 new parser-level
  tests** verifying the merge preserves all rows
  (including same-symbol rows from different categories
  with distinct tick sizes) and that an all-empty merge
  yields an empty vec.
  - **Workspace stats**: 1014 → **1016 tests** (+2),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Epic D stage-3 — engine-side OFI + learned-MP
  auto-attach** (Apr 2026). Closes the second deferral
  the Epic D stage-1 closure note tracked: "OFI and
  learned-MP consumers are wired into `MomentumSignals`
  as opt-in builder knobs but the engine's default
  `MomentumSignals::new(...).with_hma(...)` construction
  does not yet attach them — operators enable per config
  in stage-2." Stage-3 ships the config knobs and the
  engine wiring.
  - **Two new `MarketMakerConfig` knobs** in
    `mm-common::config`:
    - `momentum_ofi_enabled: bool` (default `false`) —
      when `true`, the engine attaches a fresh
      `OfiTracker` via `MomentumSignals::with_ofi()`
      and feeds every L1 book event through it via
      `on_l1_snapshot`.
    - `momentum_learned_microprice_path: Option<String>`
      (default `None`) — when `Some(path)`, the engine
      loads a finalized `LearnedMicroprice` TOML file
      via `LearnedMicroprice::from_toml(path)` (the
      offline CLI fit binary from Track 2) and attaches
      it via `with_learned_microprice(model)`. **Load
      failure logs a warning and continues without the
      signal — never panics.**
    Both knobs use `#[serde(default)]` for backward
    compat; operators who didn't add them to existing
    config files see byte-identical wave-1 behaviour.
  - **Engine `MomentumSignals` construction** in
    `MarketMakerEngine::new` now reads both knobs and
    conditionally attaches the optional signals after
    the existing `with_hma` call.
  - **Engine `handle_ws_event` L1 feed**: the book-
    event branch now calls
    `momentum.on_l1_snapshot(bid_px, bid_qty, ask_px,
    ask_qty)` after the existing `momentum.on_mid` call,
    reading the freshly-applied snapshot directly from
    `book_keeper.book`. The call is a no-op when
    `with_ofi()` was not called at construction time.
  - **3 new engine integration tests** in
    `signal_wave_2_integration`:
    - `momentum_ofi_disabled_keeps_ewma_unset` — pin
      the default-off path
    - `momentum_ofi_enabled_populates_ewma_from_book_events`
      — pin the default-on path with growing-bid-depth
      snapshots
    - `momentum_learned_microprice_missing_path_does_not_panic`
      — pin the load-failure recovery
  - **All 7 test fixtures** updated with the new field
    defaults (engine integration tests, strategy bench,
    avellaneda, glft, basis, cross_exchange, simulator).
  - **Workspace stats**: 1011 → **1014 tests** (+3),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Epic D stage-3 — per-side ρ end-to-end wiring** (Apr 2026).
  Closes the deferral that Epic D stage-2 Track 2 documented:
  per-side asymmetric `ρ_b` / `ρ_a` for the Cartea closed-form
  spread shipped only as a pure function in `cartea_spread.rs`
  in stage-2 because threading it through `StrategyContext`
  would have conflicted with Track 1's file ownership of
  `crates/engine/src/market_maker.rs`. Stage-3 wires it end
  to end now that no concurrent agents are touching the
  engine.
  - **`AdverseSelectionTracker` per-side bps**
    (`mm-risk::toxicity`). New private helper
    `adverse_selection_bps_filter(side_filter)` that takes
    an `Option<Side>`. New public methods
    `adverse_selection_bps_for_side(side)`,
    `adverse_selection_bps_bid()` (= `Buy` fills),
    `adverse_selection_bps_ask()` (= `Sell` fills). Existing
    `adverse_selection_bps()` delegates to the new helper
    with `None` — byte-identical output. 4 new tests.
  - **`StrategyContext` per-side fields** (`mm-strategy::trait`).
    Two new optional fields `as_prob_bid: Option<Decimal>`
    and `as_prob_ask: Option<Decimal>`. Existing `as_prob`
    stays as the symmetric fallback. **Per-side wins only
    when both fields are `Some`** — either being `None`
    falls back to the symmetric path inside the strategy.
    All ~10 construction sites updated with `None` defaults.
  - **`AvellanedaStoikov` per-side path**
    (`mm-strategy::avellaneda`). `compute_quotes` now computes
    `(bid_half_spread, ask_half_spread)` separately via a
    `match` on the per-side fields. Per-side adds the AS
    additive term independently to each side, each clamped
    at `min_spread/2`. Symmetric fallback preserves the
    Epic D stage-2 path byte-identically. 3 new tests.
  - **`GlftStrategy` per-side path** (`mm-strategy::glft`).
    Mirrors the Avellaneda shape. Level-offset spreading
    uses the average half-spread to preserve wave-1
    level-stacking semantics. 3 new tests.
  - **Engine `refresh_quotes` per-side threading**
    (`mm-engine::market_maker`). Derives `as_prob_bid` and
    `as_prob_ask` from the new tracker accessors via
    `cartea_spread::as_prob_from_bps` and populates the new
    `StrategyContext` fields. Either side returning `None`
    (under-sampled) cleanly falls back to the symmetric
    path inside the strategy — no conditional logic at the
    engine level.
  - **Pre-stage-3 byte-identical fallback**: when the
    tracker has fewer than 5 completed fills on a side,
    that side's bps is `None`, the strategy falls back
    to the symmetric `as_prob`, and quotes are byte-
    identical to Epic D stage-2 output. The per-side path
    only fires when the tracker has enough data on BOTH
    sides simultaneously.
  - **Workspace stats**: 1001 → **1011 tests** (+10),
    workspace clippy `-D warnings` clean, workspace fmt
    clean. Zero new dependencies.

- **Stage-2 parallel push — 4 tracks across A/B/D/F epics**
  (Apr 2026). After all 6 SOTA gap closure epics closed
  stage-1, the user requested parallel execution of the
  highest-ROI stage-2 follow-ups via 4 isolated agent
  tracks running concurrently in-tree with strict file
  ownership. Each track followed the same 4-sprint
  discipline (planning + study → impl → impl → wiring +
  tests + report). All 4 tracks landed clean — workspace
  jumped from 920 → **1001 tests** (+81 net) with
  workspace clippy and fmt clean.
  - **Track 1 — "Make advisory live" (Epic A + B
    stage-2).** Closes the advisory-only gap that both
    Epic A's cross-venue SOR and Epic B's stat-arb driver
    shipped with in stage-1. New
    `crates/engine/src/sor/dispatch.rs` (~560 LoC) with
    `DispatchOutcome` + `LegOutcome` types and a
    `dispatch_route` helper that issues real per-venue
    `place_order` calls instead of just emitting an
    advisory `RouteDecision`. New
    `MarketMakerEngine::dispatch_route(side, qty, urgency)`
    public method that calls `recommend_route` then
    dispatches. New `pnl_strategy_class` discriminator on
    the engine that routes hedge-side fills into the
    correct per-strategy PnL bucket
    (`stat_arb_<pair>` vs the funding-arb default).
    `StatArbDriver` extended with
    `try_dispatch_legs_for_entry` / `try_dispatch_legs_for_exit`
    methods that issue real IOC limit orders on both legs
    via the driver's already-held `Arc<dyn ExchangeConnector>`
    references. New `pending_exit_legs` field caches the
    direction/qty at Close time before the position is
    cleared so the exit dispatch knows which sides to
    reverse. **22 new tests** (10 SOR dispatch +
    8 stat-arb driver + 4 e2e in `stage2_track1_integration`).
  - **Track 2 — "Epic D polish".** Three Epic D stage-2
    follow-ups: TOML round-trip persistence on
    `LearnedMicroprice` (`from_toml` / `to_toml` via
    serde + `#[serde(skip)]` on the transient
    `spread_samples` accumulator); new offline CLI fit
    binary `mm-learned-microprice-fit` at
    `crates/strategy/src/bin/mm_learned_microprice_fit.rs`
    (~370 LoC) implementing the two-pass quantile-edge
    fit + JSONL parser; GLFT integration of the Cartea
    AS spread component (additive `(1 − 2ρ) · σ · √(T−t)`
    post-clamp, byte-identical when `ctx.as_prob == None`);
    per-side asymmetric `quoted_half_spread_per_side` pure
    function in `cartea_spread.rs` (independent `ρ_bid`
    and `ρ_ask`, both clamped at zero). **19 new tests**
    (4 lMP TOML + 3 CLI smoke + 5 GLFT AS + 7 per-side
    `cartea_spread`). The per-side ρ engine integration
    via `StrategyContext` is deferred to stage-3 because
    threading new fields through `market_maker.rs`
    conflicted with Track 1's file ownership; the per-side
    function is callable directly by any future caller.
  - **Track 3 — "Listing sniper" (Epic F #1 stage-2).**
    Closes the deferred Epic F #1 sub-component. New
    `ExchangeConnector::list_symbols` async trait method
    with default `Err("not supported")` implementation
    for backward compat. **5 venue implementations**:
    Binance spot (`/api/v3/exchangeInfo`), Binance USDⓈ-M
    futures (`/fapi/v1/exchangeInfo` with
    `contractStatus → TradingStatus` mapping including
    delivery contracts), Bybit V5 (per-category
    `/v5/market/instruments-info`), HyperLiquid (perp +
    spot via the `meta` action), and the custom
    `mm-exchange-client` connector inheriting the
    default-fallback `Err`. Per-venue parser helpers
    factored out for testability. New
    `crates/engine/src/listing_sniper.rs` (~490 LoC)
    with the standalone `ListingSniper` struct, per-venue
    known-symbol cache, first-scan seeding (so existing
    listings don't fire spurious `Discovered` events on
    startup), and `ListingEvent { Discovered, Removed }`
    diff output. **20 new tests** (12 sniper unit + 8
    venue parser tests). Engine integration (auto-spawn
    probation engine on `Discovered`) is a stage-3
    follow-up tracked in ROADMAP — v1 ships the
    discovery half, operators wire the orchestration
    layer.
  - **Track 4 — "Defensive layer #2" (Epic F #2 +
    multi-leader).** Two Epic F stage-2 follow-ups.
    `NewsRetreatStateMachine` upgraded from case-
    insensitive substring keyword matching to **real
    regex** via the `regex = "1"` workspace dep
    (orchestrator added it in pre-flight). Each pattern
    is wrapped with `(?i)` so case-insensitivity is
    baked in transparently — operators get word
    boundaries (`\b`), alternation (`|`), and wildcards
    (`.*`) for free. `NewsRetreatStateMachine::new` now
    returns `anyhow::Result<Self>` so malformed patterns
    surface via `Context`-wrapped errors instead of
    panicking; engine call sites in `market_maker.rs`
    test fixtures absorbed `.expect("valid news config")`
    updates. New `MultiLeaderLeadLagGuard` struct in
    `lead_lag_guard.rs` with weighted-max aggregation
    (`M_agg = max over L of (w_L · (M_L − 1) + 1)`,
    floored at 1.0) — operators register multiple leader
    venues with per-leader weights, the loudest leader
    wins. Existing single-leader `LeadLagGuard` stays
    byte-identical for backward compat. Re-registering
    a leader preserves its EWMA state so operators can
    re-weight at runtime without losing warmup. **20 new
    tests** (6 regex-based news retreat tests + 14
    multi-leader tests including hand-verified 2-leader
    fixture).
  - **Cross-track coordination resolved cleanly.**
    Track 4's `NewsRetreatStateMachine::new → Result`
    breaking change required `.expect()` patches in 4
    test call sites in `market_maker.rs` (Track 1's
    territory) — Track 1 absorbed the patches as part of
    its own changes. Track 3's listing sniper extension
    of `MockConnector::list_symbols` in `test_support.rs`
    was strictly additive (one new field + 2 setters +
    1 trait method impl) and didn't conflict with
    Track 1's existing counter fields. The pre-flight
    workspace `Cargo.toml` add of `regex = "1"` happened
    before tracks fired so no concurrent edits.
  - **Workspace stats**: 920 → **1001 tests** (+81),
    workspace clippy `-D warnings` clean, workspace
    fmt clean. Zero new dependencies beyond
    `regex = "1"` (workspace add) and `toml`/`serde_json`
    enables on `mm-strategy` (already workspace deps).

- **Epic E — Execution polish (stage-1)** (SOTA gap closure
  epic, sixth and final). User reordered F → E so the
  defensive surface landed first; Epic E closes the
  execution-infra polish items that improve tail latency
  and operational deployment without touching the alpha
  or defensive stacks. Stage-1 ships **2/4 sub-components**
  — io_uring runtime and Coinbase Prime FIX explicitly
  deferred to stage-2 (each is a 1-2 week individual
  sub-epic that doesn't fit the polish budget).
  - **Sub-component #1 — Batch order entry**
    (`mm-engine::order_manager::execute_diff` extension).
    New `MIN_BATCH_SIZE = 2` constant. Two new private
    helpers `place_quotes_batched` and `cancel_orders_batched`
    that chunk the diff's `to_place` / `to_cancel` slices
    by `connector.capabilities().max_batch_size`, call the
    venue's `place_orders_batch` / `cancel_orders_batch`
    methods, fall back to per-order on any error or
    returned-id-count mismatch, and stay on the per-order
    path for single-order diffs (where the JSON overhead
    of a batch call has no benefit). Existing per-order
    placement logic factored out into `place_one_quote` /
    `cancel_one` helpers used by both the single-order
    path and the batch fallback path. **Net effect on
    venue coverage:** Bybit V5 + HyperLiquid get the full
    5-20× round-trip reduction on big diffs; Binance
    futures gets 5× coalescing via `/fapi/v1/batchOrders`;
    Binance spot + custom client are no-benefit-no-regression
    because their `place_orders_batch` impls already
    fallback-loop internally. 12 unit tests covering
    chunking at max=5 and max=20, partial-failure
    fallback, single-order routing, empty-input no-op,
    pathological max=1 venue, both place and cancel paths.
  - **Sub-component #3 — High-performance deployment guide**
    (`docs/deployment.md` extension + new
    `deploy/systemd/mm.service` template). New ~270-line
    "High-performance deployment" section covering 7 deploy
    levers in rough effort-to-impact order:
    1. File descriptor limits (`LimitNOFILE=65535`)
    2. Disable swap (`swapoff -a` + `vm.swappiness=0`)
    3. OOM score adjustment (`OOMScoreAdjust=-500`)
    4. NUMA pinning (`CPUAffinity` / `NUMAMask` / `NUMAPolicy`)
    5. IRQ steering (`irqbalance` disable + manual `smp_affinity`)
    6. Transparent hugepages (`/sys/kernel/mm/transparent_hugepage`)
    7. PREEMPT_RT kernel (advanced — when to use, when not)
    Each sub-section: what + why + how + verification snippet
    + persistence path. Cumulative impact table at the
    bottom summarising the per-lever tail-latency gain.
    New file `deploy/systemd/mm.service` ships a complete
    validated unit template with `${PLACEHOLDER}` markers
    bundling levers 1, 3, 4, and 7 plus filesystem +
    capability + memory hardening (`ProtectSystem=strict`,
    `NoNewPrivileges`, `CapabilityBoundingSet=`,
    `MemoryHigh=4G`, `MemoryMax=6G`, `MemorySwapMax=0`,
    `ReadWritePaths=data`).
  - **Sub-component #2 — io_uring runtime** ←
    **deferred to stage-2.** The runtime change requires
    adding `tokio-uring` as a dep, replacing tokio's
    work-stealing scheduler in the WS read path,
    validating that `rustls` works under `tokio-uring`
    (the existing `tokio-tungstenite` + rustls combo
    needs adapter work), and a Linux kernel ≥ 5.6 gate.
    1-2 weeks of focused work that doesn't fit the polish
    budget — tracked in ROADMAP closure note.
  - **Sub-component #4 — Coinbase Prime FIX 4.4** ←
    **deferred to stage-2.** A new venue connector needs
    auth, order types, message routing, error mapping,
    per-venue rate limiting, and integration with the
    existing `mm-portfolio` / `mm-risk` pipelines on top
    of the existing `crates/protocols/fix/` codec. 2 weeks
    of focused work — tracked in ROADMAP closure note.
  - **Engine integration.** No new engine fields needed
    — batch entry is a pure swap of the per-order loops
    inside `OrderManager::execute_diff`. Engine wiring is
    "do nothing, the diff path picks it up automatically."
    `MockConnector` (in `crates/engine/src/test_support.rs`)
    gained per-call counters (`place_batch_calls` /
    `place_single_calls` / `cancel_batch_calls` /
    `cancel_single_calls`), one-shot batch-failure
    injection (`arm_batch_place_failure` /
    `arm_batch_cancel_failure`), and a builder
    `with_max_batch_size(n)` so engine integration tests
    can assert which path the diff routed through.
  - **Two new engine integration tests** in
    `epic_e_integration` driving the full
    `refresh_quotes → execute_diff → batch` path:
    - `refresh_quotes_routes_through_batch_on_first_diff`
      asserts that with `max_batch_size=20` the engine's
      first refresh fires exactly one
      `place_orders_batch` call (carrying both deduped
      quotes after tick rounding) and zero per-order
      `place_order` calls.
    - `refresh_quotes_stays_per_order_when_max_batch_size_is_one`
      asserts the pathological `max=1` venue stays on
      the per-order path.
  - **Zero new dependencies.** 12 new unit tests in
    `mm-engine::order_manager` + 2 new engine integration
    tests + 7 new lever sub-sections in `docs/deployment.md`
    + 1 new `deploy/systemd/mm.service` file. Workspace
    clippy `-D warnings` clean, `cargo fmt --check` clean.

- **Epic F — Defensive layer (stage-1)** (SOTA gap closure
  epic, fifth — user reordered F before E so the defensive
  surface lands before execution polish). Closes the
  predictive-defensive gap from the April 2026 research
  pass: existing wave-1 controls (kill switch L1-L5, VPIN
  spread widening, market-resilience score, autotuner
  regime shifts) all react to **observable** danger.
  Production prop desks ship two **predictive** controls
  on top — signals that say "danger is *about to* arrive"
  so the MM can retreat in the 100-500 ms window before
  adverse fills land. Epic F adds them as two new risk
  primitives plus engine wiring, landed over 4 one-week
  sprints.
  - **Sub-component #1 — Lead-lag guard**
    (`mm-risk::lead_lag_guard`). New `LeadLagGuard`
    holding EWMA mean + variance on a leader-venue mid
    feed (typically Binance Futures perpetual for crypto)
    and exposing a piecewise-linear ramp multiplier on
    the latest |z-score|. Defaults: half-life 20 events,
    z_min=2, z_max=4, max_mult=3. Pure `Decimal`,
    auto-seeds on the first `on_leader_mid` call,
    decay-to-neutral after a quiet stream, symmetric
    trigger on positive and negative shocks. 14 unit
    tests including a hand-verified sequence and a
    saturation-at-max test. Source: Makarov-Schoar 2020,
    *J. Financial Economics* 135(2) 293–319, §4 on
    crypto lead-lag.
  - **Sub-component #2 — News retreat state machine**
    (`mm-risk::news_retreat`). New `NewsRetreatStateMachine`
    with a 3-class promotion ladder (`Low / High /
    Critical`), per-class cooldown (30 / 5 / 0 minutes),
    case-insensitive substring keyword classification,
    promotion-only transitions (lower class in higher
    state is suppressed), refresh resets the cooldown
    clock, and a `force_clear` operator override. v1
    ships **no built-in feed source** — operators wire
    their own (Telegram bot, file tail, paid Tiingo
    adapter) and call `on_headline(text)`. 14 unit tests.
    Sources: Cartea-Jaimungal-Penalva 2015 ch.10 §10.4
    "Trading on News" + Wintermute Trading public
    material on operational news retreat.
  - **Sub-component #3 — Listing sniper** ←
    **deferred to stage-2.** The sniper needs a new
    `ExchangeConnector::list_symbols` trait method
    shipped across all 4 venue adapters (Binance, Bybit,
    HyperLiquid, custom client). That's a multi-venue
    sub-epic on its own. Stage-1 ships the two
    predictive defensive signals and tracks the listing
    sniper as a follow-up in ROADMAP.
  - **`AutoTuner` extension** (`mm-strategy::autotune`).
    Two new soft-widen multiplier fields parallel to the
    existing `toxicity_spread_mult` / `market_resilience`
    pattern: `lead_lag_mult` and `news_retreat_mult`.
    `set_lead_lag_mult` and `set_news_retreat_mult`
    setters clamp at 1.0 (defensive controls never
    *narrow* the spread). `effective_spread_mult` folds
    them in multiplicatively; defaults of 1.0 are
    byte-identical to pre-Epic-F. 6 new unit tests
    covering identity, widening, clamp, and composition.
  - **Engine integration.**
    `MarketMakerEngine::with_lead_lag_guard(guard)` and
    `with_news_retreat(state_machine)` builders. Two new
    public push APIs: `update_lead_lag_from_mid(mid)`
    (idempotent — operators with a separate orchestration
    layer call it manually) and `on_news_headline(text)`
    (operators wire any feed source). The hedge-connector
    book event handler auto-feeds the lead-lag guard
    when both are attached, so engines with a hedge
    connector don't need separate orchestration.
    `tick_news_retreat()` runs on the periodic 30 s
    summary tick to drive cooldown expiry forward without
    waiting for a fresh headline. Three new audit event
    types: `LeadLagTriggered` (fires only on the
    `1.0 → > 1.0` transition, not on every leader mid),
    `NewsRetreatActivated` (every promotion), and
    `NewsRetreatExpired` (cooldown expiry transitions).
    Critical-class news headlines escalate the kill
    switch to L2 `StopNewOrders` automatically.
  - **Six new engine integration tests** in
    `defensive_layer_integration`: builder smoke test,
    end-to-end lead-lag pipeline (synthetic shock →
    autotuner widening + saturation), end-to-end news
    Critical → kill switch L2, news High → autotuner
    widening without kill switch, no-match silence,
    and "no defensive controls attached" no-op behavior.
  - **v1 simplification — substring keywords, not regex.**
    The original sprint plan called for regex priority
    lists; v1 ships case-insensitive substring matching
    instead. Operationally identical for the canonical
    examples ("SEC", "fraud", "hack", "FOMC", "CPI",
    "exploit"). Stage-2 can upgrade to full regex if
    operators need wildcards.
  - **Zero new dependencies.** 28 new unit tests in
    `mm-risk` (14 lead-lag + 14 news retreat) + 6 new
    `mm-strategy` autotune tests + 6 engine integration
    tests + 3 new audit event types. Workspace clippy
    `-D warnings` clean, `cargo fmt --check` clean.

- **Epic D — Signal wave 2 (stage-1)** (SOTA gap closure
  epic, fourth after Epic C, A, B). Closes the largest
  microstructure-signal gap from the April 2026 research
  pass: production prop desks ship four signal families
  beyond our wave-1 menu (book imbalance, trade flow,
  classic micro-price, tick-rule VPIN). Epic D adds the
  CKS L1 OFI process, Stoikov 2018 learned micro-price,
  Easley-de Prado-O'Hara BVC, and the Cartea closed-form
  adverse-selection spread component as four new
  primitives plus strategy + engine wiring. Landed over
  4 one-week sprints.
  - **Sub-component #1 — Cont-Kukanov-Stoikov OFI**
    (`mm-strategy::cks_ofi`). New `OfiTracker` that holds
    a previous L1 snapshot and emits a signed observation
    per update. Auto-seeds on the first call so callers
    don't need an explicit `seed()`. Implements the
    canonical CKS 2014 eqs. (2)-(4) with all six
    `(price moved up / unchanged / down) × (bid / ask)`
    cases. 12 unit tests including a hand-verified
    4-event fixture that pins the sign convention. Source:
    Cont, Kukanov, Stoikov — "The Price Impact of Order
    Book Events," *J. Financial Econometrics*, 12(1),
    47–88 (2014).
  - **Sub-component #2 — Stoikov learned micro-price**
    (`mm-strategy::learned_microprice`). New
    `LearnedMicroprice` struct that builds a histogram fit
    of `G(imbalance, spread) → E[Δmid]` from a stream of
    training observations. `accumulate` / `finalize` /
    `predict` API; under-sampled buckets clamp to zero;
    two-pass `with_spread_edges` + `accumulate_with_edges`
    for multi-spread-bucket fits. 14 unit tests including
    monotone-prediction-from-monotone-training, idempotent
    finalize, and bucket-boundary edge cases. v1 ships the
    in-memory core; TOML / JSON persistence + the offline
    CLI fit binary deferred to a follow-up. Source:
    Stoikov — "The Micro-Price: A High-Frequency Estimator
    of Future Prices," *Quantitative Finance*, 18(12),
    1959–1966 (2018).
  - **Sub-component #3 — Bulk Volume Classification**
    (`mm-risk::toxicity::BvcClassifier` + new
    `VpinEstimator::on_bvc_bar` entry point). Classifies
    a bar's total volume into buy / sell fractions via the
    CDF of standardised price changes — no per-trade
    tick-rule classification required. Student-t CDF
    implemented via Numerical Recipes regularized
    incomplete beta + Lentz continued fraction + Lanczos
    log-gamma in f64 (same boundary-conversion pattern as
    `features::hurst_exponent` and `features::log_price_ratio`).
    For `ν ≥ 30` falls back to the Normal approximation
    via Abramowitz-Stegun erf 7.1.26. The new
    `VpinEstimator::on_bvc_bar` is byte-parity-tested
    against the existing `on_trade` tick-rule path: same
    underlying buy/sell split → identical VPIN output. 12
    unit tests covering warmup, classification direction,
    total-volume invariant, Student-t CDF saturation, and
    parity with tick-rule VPIN. Source: Easley, López de
    Prado, O'Hara — "Flow Toxicity and Liquidity in a
    High-Frequency World," *Review of Financial Studies*,
    25(5), 1457–1493 (2012), eq. 4.
  - **Sub-component #4 — Cartea closed-form
    adverse-selection spread** (`mm-strategy::cartea_spread`).
    New `quoted_half_spread(γ, κ, σ, T−t, ρ)` function
    implementing CJP 2015 ch.4 §4.3 eq. 4.20:
    `δ* = (1/γ)·ln(1 + γ/κ) + (1 − 2ρ)·σ·√(T−t)`. When
    `ρ = 0.5` (uninformed flow), the additive term
    vanishes and the formula collapses to the wave-1
    Avellaneda half-spread; `ρ < 0.5` widens (uninformed
    flow → MM has time to skew); `ρ > 0.5` narrows
    (informed flow → MM gets out of the way). New
    `as_prob_from_bps(bps)` piecewise-linear map from the
    existing `AdverseSelectionTracker` bps-scale output
    to ρ ∈ [0, 1] via a ±20 bps saturation. New pure-
    `Decimal` `decimal_ln` helper via range reduction +
    Taylor series (10-decimal accuracy on
    `x ∈ [1e-6, 1e6]`). Output clamped at zero so high
    `ρ` with large `σ·√(T−t)` never produces a sub-zero
    quoted spread. 17 unit tests. Source:
    Cartea-Jaimungal-Penalva 2015 ch.4 §4.3 (corrected
    against the SOTA research doc — ch.4 is the AS
    chapter, ch.6 was the prior mis-citation, same
    correction pattern as Epic B's ch.11 fix).
  - **Strategy integration.**
    `StrategyContext` gains an `as_prob: Option<Decimal>`
    field (16 construction sites updated with `None` as
    the default). `AvellanedaStoikov::compute_quotes`
    reads it and applies the Cartea additive component
    to the quoted spread post min-spread clamp;
    re-clamps at the `min_spread_bps` floor so high `ρ`
    cannot drive the quote sub-minimum. `MomentumSignals`
    gains two builder-pattern hooks: `with_ofi()` attaches
    a `OfiTracker` (engine feeds via new
    `on_l1_snapshot`) and `with_learned_microprice(model)`
    attaches a finalized `LearnedMicroprice`. `alpha()`
    rebalances component weights dynamically: each
    optional signal pulls 0.1 of weight off the wave-1
    baseline (`book × 0.4 + flow × 0.4 + micro × 0.2`).
    All extensions are byte-identical to pre-Epic-D when
    no optional signal is attached.
  - **Engine integration.** `MarketMakerEngine::refresh_quotes`
    threads `self.adverse_selection.adverse_selection_bps()`
    through `cartea_spread::as_prob_from_bps` into
    `StrategyContext.as_prob` so the existing wave-1
    `AdverseSelectionTracker` measurements flow directly
    into the new closed-form spread widening. New
    `AuditEventType::OfiFeatureSnapshot` (periodic 30 s
    summary) and `AuditEventType::AsSpreadWidened`
    (transition events). End-to-end test in
    `signal_wave_2_integration` drives the full chain
    `OfiTracker → as_prob → StrategyContext → quoted spread`
    and asserts the spread widens under uninformed flow,
    collapses to wave-1 under neutral flow, and respects
    the `min_spread_bps` floor under informed flow.
  - **Stage-1 advisory-only on the OFI / learned-MP
    consumers.** OFI and learned MP are wired into
    `MomentumSignals` as opt-in builder knobs but the
    engine's wave-1 `MomentumSignals::new(window).with_hma(...)`
    construction does not yet attach them — operators
    enable per config in stage-2. The Cartea AS path is
    fully wired and live (the engine threads the existing
    `AdverseSelectionTracker` measurement into the
    strategy via `StrategyContext.as_prob` on every
    refresh tick). GLFT integration of the Cartea
    component, the offline learned-MP fit CLI binary,
    per-side asymmetric `ρ_b` / `ρ_a`, and online
    streaming MP fit are all stage-2 follow-ups.
  - **Zero new dependencies** beyond the workspace
    baseline. `decimal_ln` is pure `Decimal` Taylor series;
    Student-t CDF math goes through `f64` (same pattern
    as `features::log_price_ratio`). 55 new unit tests in
    `mm-strategy` + 12 in `mm-risk::toxicity` + 1 engine
    e2e integration test. Workspace clippy `-D warnings`
    clean, `cargo fmt --check` clean.

- **Epic B — Cointegrated-pair stat-arb driver (stage-1)**
  (SOTA gap closure epic, third after Epic C then Epic A).
  Closes the single largest strategy-family gap from the
  April 2026 research pass: every production prop desk
  runs cointegrated pairs, and the existing `BasisStrategy`
  only covered the *same-asset* case. Epic B ships a new
  `stat_arb` module inside `mm-strategy` composed of four
  sub-components plus an engine integration layer, landed
  over four sprints (planning, cointegration+kalman,
  signal+driver, engine wiring).
  - **Sub-component #1 — Engle-Granger cointegration test**
    (`mm-strategy::stat_arb::cointegration`). Pure function
    `EngleGrangerTest::run(y, x) -> Option<CointegrationResult>`
    running OLS → residuals → basic ADF (no constant, no
    lag terms) against a MacKinnon 1991 5% critical-value
    lookup table with linear interpolation. Source
    attribution + formulas in
    `docs/research/stat-arb-pairs-formulas.md`
    (Engle-Granger 1987 + Cartea-Jaimungal-Penalva ch.11
    + MacKinnon 1991). Sample-size floor at
    `MIN_SAMPLES_FOR_TEST = 25`; 14 unit tests covering
    degenerate inputs, synthetic cointegrated pairs,
    independent random walks, MacKinnon table clamping,
    and interpolation.
  - **Sub-component #2 — Kalman filter for hedge ratio**
    (`mm-strategy::stat_arb::kalman`). Scalar linear-
    Gaussian state-space (`β[t] = β[t-1] + w[t]`,
    `Y[t] = β[t]·X[t] + v[t]`) with tunable `Q`/`R` noise
    variances. Default crypto-pair tuning `Q=1e-6`,
    `R=1e-3`. Predict/update fully `Decimal`, degenerate
    `x=0`-plus-zero-prior guard returns prior β unchanged.
    `with_initial_beta` warm-start accepts the Engle-
    Granger OLS β so the filter does not start at the
    neutral `β=1`. 11 unit tests covering convergence,
    regime shift, `Q`/`R` ratio effects, numerical
    stability.
  - **Sub-component #3 — Z-score signal with hysteresis**
    (`mm-strategy::stat_arb::signal`). Rolling-window
    spread → z-score generator maintaining running
    `sum`/`sum_sq` totals for O(1) updates and sample
    variance via `var = (Σs² − n·mean²) / (n−1)`. Two-
    level hysteresis (`entry_threshold` default 2.0,
    `exit_threshold` default 0.5) prevents oscillation
    around the band edges. `SignalAction` enum
    (`Open { direction, z }` / `Close { z }` / `Hold { z }`)
    and `SpreadDirection { SellY, BuyY }` drive the
    driver's state machine. Panics on inverted thresholds
    and `window < 2` (caller mistakes). 14 unit tests
    covering warmup, numerical stability over 10k updates,
    naive-recompute parity, hysteresis invariants.
  - **Sub-component #4 — `StatArbDriver`**
    (`mm-strategy::stat_arb::driver`). Composes #1 + #2 +
    #3 into a standalone tokio-task driver mirroring the
    `FundingArbDriver` pattern from v0.2.0 Sprint H.
    Holds two `Arc<dyn ExchangeConnector>` references
    (the two legs, possibly on different venues), one
    `KalmanHedgeRatio`, one `ZScoreSignal`, cached
    `Option<CointegrationResult>`, and an optional
    `StatArbPosition`. `tick_once` fetches both mids via
    `get_orderbook`, drives the Kalman/signal state
    machine, and emits a `StatArbEvent { Entered, Exited,
    Hold, NotCointegrated, Warmup, InputUnavailable }`.
    `run(shutdown_rx)` owns the async tick loop on
    `tick_interval`. `recheck_cointegration(y, x)` lets
    the engine reseed the cached ADF result on a slow
    cadence (default 60 min), and seeds the Kalman with
    the OLS β on the first accepted test. Clean
    `StatArbEventSink` trait + `NullStatArbSink` mirror
    the funding-arb sink shape without reaching into
    `mm-engine`. 11 unit tests covering warmup,
    cointegration gate, entry/exit round-trip on a
    synthetic spread shock, input-unavailable paths,
    shutdown semantics.
  - **Engine integration.**
    `MarketMakerEngine::with_stat_arb_driver(driver, tick_interval)`
    builder attaches an optional driver to the engine's
    select loop. New `stat_arb_interval` arm gated on
    `self.stat_arb_driver.is_some()` polls the driver on
    its tick cadence, routes every event through
    `handle_stat_arb_event`, and fires `StatArbEntered` /
    `StatArbExited` audit records (new `AuditEventType`
    variants). Same take/tick/put borrow-check dance as
    the funding-arb arm. `test_support::MockConnector`
    gained a `set_mid(Decimal)` helper so driver-level
    engine tests can feed synthetic mids through the
    connector trait. Four engine integration tests:
    silent-variants-no-escalation, entered/exited audit
    routing, `with_stat_arb_driver` builder smoke test,
    and a full end-to-end pipeline that drives a
    synthetic cointegrated pair through Kalman → signal
    → driver → engine handler and asserts the
    entered-then-exited transition.
  - **Stage-1 is advisory-only.** Same call-site pattern
    as Epic A's SOR: the driver tracks its state machine
    and emits intent events, but does NOT dispatch leg
    orders. Operators read the audit trail to sign off
    what the driver would have done before stage-2 wires
    inline leg execution through `ExecAlgorithm`
    (TWAP entry) + `OrderManager::execute_unwind_slice`
    (market-take exit). No real per-pair PnL bucket
    routing yet — that lands when real fills flow through
    stage-2 dispatch. The Portfolio per-strategy labeling
    infrastructure Epic C shipped already accepts
    arbitrary `stat_arb_{pair}` class keys, so stage-2 is
    a pure wiring task.
  - **Zero new dependencies.** Pure `Decimal` math,
    `decimal_sqrt` reused from `mm-strategy::volatility`.
    50 new unit tests in `mm-strategy` + 4 new engine
    integration tests. Workspace clippy `-D warnings`
    clean, `cargo fmt --check` clean.

- **Epic A — Cross-venue Smart Order Router (stage-1)**
  (SOTA gap closure epic, second after Epic C). Every
  primitive a cost-aware cross-venue router needs was
  already in the codebase — seven venue × product
  connectors, live per-account fee tiers from P1.2, the
  queue-position fill model from v0.4.0, `BalanceCache`
  keyed on `(asset, WalletType)`. What was missing was
  the coordinator that looks at them all together and
  decides where to route a given fill. Epic A closes that
  gap with four connected sub-components sharing one
  `VenueStateAggregator`, one `VenueCostModel`, and one
  `GreedyRouter`.
  - **Prerequisite plumbing.** `ConnectorBundle` gains an
    `extra: Vec<Arc<dyn ExchangeConnector>>` field plus a
    `with_extra` builder so the SOR can route across
    3+ venues (primary + optional hedge + `extra`). New
    `all_connectors()` iterator walks every slot in a
    deterministic order the aggregator relies on for
    tie-breaking. Every existing call site gets a
    default-empty `extra` vec, so single-connector and
    dual-connector modes are byte-for-byte compatible.
    New `ExchangeConnector::rate_limit_remaining() -> u32`
    async trait method with a default `u32::MAX`
    ("unlimited") — overridden on Binance spot, Binance
    USDⓈ-M futures, Bybit V5, and HyperLiquid to
    delegate to their existing `RateLimiter::remaining()`
    accessor. The custom `mm-exchange-client` connector
    inherits the default.
  - **Sub-component #1 — Venue cost model**
    (`mm-engine::sor::cost`). Pure function
    `VenueCostModel::price(snapshot, side, urgency)`
    emits a `RouteCost { venue, taker_cost_bps,
    maker_cost_bps, effective_cost_bps }` in basis
    points. v1 formula:
    `effective = urgency · taker_fee_bps +
    (1 − urgency) · (maker_fee_bps +
    queue_wait_bps_per_sec · queue_wait_secs)`.
    Negative `maker_fee_bps` (rebate) carries through
    end-to-end so rebate venues legitimately produce
    negative `effective_cost_bps` at low urgency.
    Urgency is clamped to `[0, 1]` inside the function.
    10 unit tests pin the zero / half / one urgency
    boundaries, the rebate sign flow, the linear-in-
    inputs scaling, the clamping, the side symmetry, and
    the venue tag carry-through.
  - **Sub-component #2 — Venue state aggregator**
    (`mm-engine::sor::venue_state`). `VenueSnapshot`
    pure data struct + `VenueSeed` per-venue config seed
    + `VenueStateAggregator` that owns a
    `HashMap<VenueId, VenueSeed>` and exposes
    `register_venue(venue, seed)` /
    `update_book(venue, bid, ask)` /
    `update_queue_wait(venue, secs)` mutators plus an
    async `collect(bundle, side) -> Vec<VenueSnapshot>`
    walk path that pulls live
    `rate_limit_remaining()` from every connector in
    the bundle. The **"seed + optional runtime
    refresh"** split keeps the aggregator testable as
    pure data — `collect_synthetic(&[(venue,
    remaining)])` is the sync variant the tests and the
    engine-integration path drive. 9 unit tests cover
    register idempotency, book / queue wait mutation
    scope, deterministic venue iteration, synthetic
    collect, mid-price averaging + zero guard, and the
    `is_available()` gate.
  - **Sub-component #3 — Greedy router**
    (`mm-engine::sor::router`). `GreedyRouter::route(
    side, qty, urgency, snapshots) -> RouteDecision`.
    Algorithm: filter `is_available()` venues, price
    each via the cost model, sort ascending by
    `effective_cost_bps` with venue-ordinal tiebreaker
    for determinism, greedy fill up to the target qty
    with per-venue cap-or-remainder allocation. Emits a
    `RouteDecision { target_side, target_qty,
    filled_qty, is_complete, legs: Vec<RouteLeg> }`
    where each leg carries `(venue, qty, is_taker,
    expected_cost_bps)`. Taker/maker classification
    uses a single `TAKER_THRESHOLD = 0.5` inclusive —
    `urgency ≥ 0.5` marks every leg as a take.
    Partial-fill semantics: an `is_complete = false`
    decision still lists whatever legs did fill plus a
    `filled_qty` below the target. 13 unit tests pin
    zero-target → empty-complete, single-venue full
    fill, two-venue cheaper-first, target-exceeds-single-
    venue split, partial fill on insufficient universe,
    unavailable / rate-limited venue filter, urgency
    threshold classification, deterministic tiebreaker,
    rebate venue winning over non-rebate, total cost
    rollup, empty-snapshots incomplete decision, and a
    property-style never-overfills invariant across
    five random-ish scenarios.
  - **Sub-component #4 — Engine hook + audit + metrics.**
    `MarketMakerEngine` gains `sor_aggregator` +
    `sor_router` fields initialised at construction
    time. The primary venue is **auto-seeded** from
    `self.product` inside `new()` — operators do not
    need to call anything for the single-venue default
    to work. New `with_sor_venue(venue, seed)` builder
    lets the server add hedge-leg and any `extra`
    venues before handing the engine to the run loop.
    `async recommend_route(side, qty, urgency)
    -> RouteDecision` is the public advisory API that
    collects a live snapshot, runs the router,
    publishes per-venue Prometheus gauges
    (`mm_sor_route_cost_bps{venue}`,
    `mm_sor_fill_attribution{venue}`), fires a
    `RouteDecisionEmitted` audit event with the full
    per-leg breakdown, and returns the decision.
    **Does not dispatch** — Epic A is explicitly
    advisory, stage-2 wires inline dispatch through an
    `ExecAlgorithm`. 2 engine-level integration tests
    pin the auto-seed path (single-venue full fill)
    and the multi-venue split (cheap Bybit fills 3 of
    5 units, remainder rolls to Binance).
  - **Source attribution.** The v1 cost model is a
    vanilla urgency-weighted blend of fee + queue wait.
    No academic reference — the shape is from the
    SOTA research doc's Axis 3 "cost-aware mix"
    description of Hummingbot's `smart_order_placement`
    flag, pinned to pure `Decimal` arithmetic without
    new dependencies. The full formula transcription +
    audit findings live in
    `docs/sprints/epic-a-cross-venue-sor.md` Sprint
    A-1 section.
  - **Stage-2 follow-ups (tracked under ROADMAP Epic A).**
    LP solver for the constrained quadratic variant
    of the cost minimisation, inline dispatch via an
    `ExecAlgorithm` (Epic A-stage-1 is strictly
    advisory), real trade-rate estimator wired into
    the queue-wait cost (v1 uses a fixed
    `queue_wait_bps_per_sec` config constant), full
    `B` matrix cross-beta routing across assets,
    multi-symbol snapshot per venue, auto-refresh of
    venue seeds from the P1.2 fee-tier refresh task,
    server-side composition of a multi-venue
    `ConnectorBundle` for the operator's production
    config, stage-2 `mm-route` CLI dry-runner for
    calibration.
- **Epic C — Portfolio-level risk view** (ROADMAP SOTA gap
  closure epic, first of the six A–F candidates surfaced
  by the April 2026 desk-research pass in
  `docs/research/production-mm-state-of-the-art.md`). The
  pre-Epic-C engine had per-strategy, per-symbol, and
  per-asset-class risk layers but nothing at the
  **portfolio** level — eight strategies quoting on three
  venues in four assets had no coherent aggregation point.
  Epic C closes that gap with five connected sub-components
  that share one shared Portfolio, one cross-asset hedge
  optimizer, and one per-strategy VaR guard.
  - **Sub-component #1 — per-factor delta aggregation.**
    `mm-portfolio::Portfolio` gains a
    `register_symbol(symbol, base, quote)` seed call plus
    `factor_delta(asset)` / `factors()` accessors that
    aggregate signed exposure per base / quote asset across
    every registered symbol. **The subtle part**:
    cross-quote pairs like `ETHBTC` contribute to BOTH their
    base factor (`+qty` ETH) AND their quote factor
    (`-qty·mark` BTC, because the ETHBTC quote leg is an
    implicit BTC short). The dust threshold prunes
    fee-rounding residuals from the `factors()` iterator so
    the daily report stays honest. New `PortfolioSnapshot.per_factor`
    field carries the roll-up for the dashboard; new
    Prometheus gauge `mm_portfolio_factor_delta{asset}`.
    Engine calls `register_symbol` inside `with_portfolio`
    for both the primary leg and the optional hedge leg
    (with a best-effort `split_symbol_bq` helper for the
    hedge symbol's base/quote).
  - **Sub-component #2 — per-strategy PnL labeling.**
    `Portfolio::on_fill` takes a new `strategy_class: &str`
    argument, accumulates realised PnL into a separate
    `per_strategy_pnl: HashMap<String, Decimal>` bucket,
    and exposes `per_strategy_sorted()` for deterministic
    daily-report ordering. Funding-arb driver fills push
    the basis engine's strategy class label (same shape
    `Strategy::name()` returns), so basis + funding-arb
    on the same leg do **not** commingle in the portfolio
    snapshot. Closes the deferred per-strategy attribution
    gap that had blocked Epic B (stat-arb pairs) from
    having a clean PnL view. New gauge
    `mm_portfolio_strategy_pnl{strategy}`.
  - **Sub-component #3 — cross-asset hedge optimizer.**
    New `mm-risk::hedge_optimizer` module with
    `HedgeInstrument { symbol, factor, funding_bps, position_cap }`,
    `HedgeBasket`, and `HedgeOptimizer::optimize(exposure,
    universe, factor_variances)`. v1 implements the classic
    Markowitz 1952 / Merton 1972 diagonal-β mean-variance
    closed form **in pure `Decimal` math** — no LP solver,
    no `nalgebra`, no matrix ops. The optimizer reduces to a
    one-loop-over-K-factors computation that issues
    `-exposure` per factor with an L1 funding-cost shrinkage
    (`shrinkage = λ · f · κ`, `κ = 1/variance`) and a hard
    `position_cap` clamp. Source attribution is corrected
    against the SOTA research doc's misattribution to
    Cartea-Jaimungal ch.6 — the real reference is Markowitz,
    documented in the new
    `docs/research/hedge-optimizer-and-var-formulas.md`.
    13 unit tests pin every branch (flat exposure, trivial
    hedge, funding-penalty zero vs intermediate vs large,
    hard cap, missing factor, long/short mix, zero variance,
    duplicate-instrument first-match-wins, property-based
    hedge-never-exceeds-cap). Engine integrates via a new
    `MarketMakerEngine::recommend_hedge_basket()` accessor
    refreshed on every dashboard tick; non-empty basket
    transitions fire the new
    `AuditEventType::HedgeBasketRecommended` audit event.
  - **Sub-component #4 — per-strategy VaR guard.**
    New `mm-risk::var_guard` module with `VarGuard`,
    `VarGuardConfig { limit_95, limit_99 }`. Standard
    RiskMetrics-style parametric Gaussian VaR with frozen
    compile-time z-score constants
    (`Z_SCORE_95 = 1.645`, `Z_SCORE_99 = 2.326`). One
    ring buffer (`VecDeque<Decimal>`) per strategy class,
    capped at `MAX_SAMPLES_PER_CLASS = 1440` for symmetry
    with the P2.2 presence buckets. 60-second sample
    cadence — the engine pushes one PnL-delta sample per
    minute from the existing `sla_interval` arm, gated on
    `tick_count % 60 == 0`. Warm-up below
    `MIN_SAMPLES_FOR_VAR = 30` returns `1.0` (no throttle)
    to avoid the "first few minutes after start have a
    random throttle" failure mode. Throttle tiers 1.0 /
    0.5 / 0.0 on 95 % / 99 % breaches; composition with
    the kill switch / Market Resilience / Inventory-γ
    multipliers via `min()` (max-restrictive wins). New
    `AuditEventType::VarGuardThrottleApplied` fires only on
    throttle **transitions** so the audit trail isn't
    spammed during stable throttle states. Self-contained
    `decimal_sqrt` Newton-Raphson helper because
    `rust_decimal` does not ship a sqrt of its own. 11
    unit tests including multi-strategy isolation,
    rolling-window eviction, and the composition
    precedence with MR / kill switch.
  - **Sub-component #5 — stress replay library + CLI.**
    New `mm-backtester::stress` module with the five
    canonical crypto-crash scenarios (COVID-19 March 2020,
    China ban May 2021, LUNA May 2022, FTX November 2022,
    USDC depeg March 2023) defined as `StressScenario`
    catalogue entries. Each carries a `ShockProfile` with
    peak price move / peak time / volume multiplier /
    spread multiplier / depth fraction / sell-flow share.
    Synthetic-first per the Sprint C-1 decision:
    `generate_ticks` produces a deterministic piecewise-
    linear price path from baseline → peak → recovery at a
    1-minute cadence, with no RNG, no external data, and no
    Tardis subscription required. `run_stress` drives the
    tick stream through a simulated
    Portfolio + VarGuard + KillSwitch + HedgeOptimizer
    combo and captures `StressReport { max_drawdown,
    time_to_recovery_secs, inventory_peak_value,
    kill_switch_trips, var_throttle_hits,
    hedge_baskets_recommended, final_total_pnl }`. New
    `mm-stress-test` binary with `--scenario=<slug>`,
    `--all`, `--output=<path>`, `--list` flags. 9 unit
    tests on the generator (cadence, baseline, peak,
    monotone legs, determinism) and 4 on the runner
    (end-to-end covid path, `run_all` catalogue, report
    markdown shape, VaR opt-out zeros the throttle hits).
    Real historical Tardis replay is a **stage-2**
    follow-up tracked in ROADMAP Epic C.
  - **Source-attribution correction.** The SOTA research
    doc cited "Cartea-Jaimungal-Penalva ch.6 for the
    hedge optimizer closed form" and "ch.7 for VaR /
    drawdown". Both were wrong. Web-verified TOC
    (Cambridge Uni Press frontmatter) shows ch. 6-8 are
    execution, ch. 10 is Market Making, ch. 11 is pairs
    trading. The correct references are Markowitz 1952 /
    Merton 1972 for the hedge optimizer and the
    RiskMetrics 1996 technical document for the Gaussian
    VaR formula. Full corrected bibliography in
    `docs/research/hedge-optimizer-and-var-formulas.md`.
  - **Stage-2 follow-ups (tracked under ROADMAP Epic C).**
    Real historical Tardis replay for the five scenarios,
    cross-beta hedging (off-diagonal β from rolling
    regression), off-diagonal factor covariance, LP solver
    for the constrained LASSO variant of the hedge
    optimization, EWMA variance in the VaR guard,
    historical-simulation VaR as a cross-check, CVaR /
    expected-shortfall, and the full-engine stress
    integration test (Sprint C-4 runs the stress path
    through the synthetic runner only; the full
    `MarketMakerEngine` end-to-end drive is deferred).

## [0.4.0] - 2026-04-15

**Production-spot-MM gap closure epic.** Closes the eight-item
ROADMAP audit from April 2026 against how production prop desks
(Hummingbot, Keyrock, GSR, Flowdesk) actually run spot MM.
Every P0 / P1 / P2 item from `ROADMAP.md` lands in this release;
three of them (P1.3, P1.4, P2.3) ship as **stage-1** with the
heavier stage-2 work (margin-mode order routing, USDC↔USDT FX
micro-hedge, dynamic engine spawn for new listings) explicitly
deferred to the next epic and tracked inline in each entry.
The release also folds in a parallel cluster of microstructure
cherry-picks landed mid-epic (ISAC γ-policy, Market Resilience,
OTR surveillance, Tick/Volume/MultiTrigger candles + HMA,
queue-position fill model, DE optimiser, market-impact walker,
lead-lag transform, Hurst R/S, BBW + multi-depth imbalance,
soft spread gate, weighted microprice, event deduplicator).

### Added

- **P2.3 stage-1 — Pair lifecycle automation: halt detection +
  tick/lot drift** (ROADMAP production-spot-MM gap closure;
  partial — stage-1 ships single-symbol lifecycle tracking,
  stage-2 will add dynamic engine spawn for new listings and
  the 7-day probation mode for auto-onboard). The pre-P2.3
  engine fetched `ProductSpec` exactly once at startup, which
  meant new listings, delistings, trading-status transitions
  (PRE_TRADING, HALT, BREAK) and tick/lot updates all required
  a process restart to pick up. Halt handling was particularly
  dangerous: venues sometimes send fills *after* a halt, and
  the engine had no state to reject them. Stage-1 closes the
  halt + tick/lot drift halves of that gap with a
  per-symbol lifecycle state machine and a periodic refresh
  task that polls the venue every 5 minutes.
  - **Type surface.** New `TradingStatus { Trading, Halted,
    PreTrading, Break, Delisted }` enum in `mm-common`.
    `ProductSpec` gains a `trading_status: TradingStatus` field
    (default `Trading` via `#[serde(default)]` so existing
    fixtures and the funding-arb / strategy struct literals
    still compile). `ProductSpec` also derives `PartialEq` so
    the lifecycle diff path can compare snapshots structurally.
  - **Binance Spot wiring.** `BinanceConnector::get_product_spec`
    now parses the venue's `status` field
    (`TRADING` → `Trading`, `HALT` → `Halted`,
    `BREAK`/`END_OF_DAY`/`POST_TRADING` → `Break`,
    `PRE_TRADING`/`AUCTION_MATCH` → `PreTrading`). Bybit, HL,
    and the custom client default to `Trading` — wiring those
    venues' status fields is a stage-2 follow-up. Binance
    Spot's "delisted symbols disappear from `exchangeInfo`"
    behaviour is handled at the engine layer: any
    `get_product_spec` error during the lifecycle refresh is
    treated as a delisting candidate and latches the manager
    via `PairLifecycleManager::on_delisted`.
  - **State machine.** New `mm-engine::pair_lifecycle` module
    with `PairLifecycleEvent` enum
    (`Listed`, `Delisted`, `Halted { from, to }`,
    `Resumed { from }`, `TickLotChanged`, `MinNotionalChanged`)
    and `PairLifecycleManager`. The manager owns a single
    `Option<ProductSpec>` snapshot plus a `delisted_latched: bool`
    flag — once delisted, subsequent `diff` calls become
    no-ops so a transient venue glitch cannot un-delist a
    symbol the operator believes is gone. 8 unit tests pin
    every branch: first-poll listing, identical-spec no-op,
    Trading→Halted, Halted→Trading, tick/lot drift,
    min_notional drift, multi-field drift in priority order
    (status → tick/lot → min_notional), and delisted-latch
    semantics.
  - **Engine wiring.** `MarketMakerEngine` gains
    `pair_lifecycle: Option<PairLifecycleManager>` (built when
    `config.market_maker.pair_lifecycle_enabled`) and a
    `lifecycle_paused: bool` flag. New `refresh_pair_lifecycle`
    method polls `connector.get_product_spec`, applies tick/lot
    drift into `self.product` in place, and routes the diff
    via `handle_lifecycle_event` into the audit trail and the
    paused flag. `Halted` and `Delisted` events trigger an
    inline `OrderManager::cancel_all` on the primary leg so
    the venue book has none of our quotes when it re-opens
    (or never re-opens). New select arm
    `pair_lifecycle_interval` runs the refresh on the
    configured cadence; `refresh_quotes` returns early
    whenever `lifecycle_paused` is set.
  - **Audit + config.** Six new `AuditEventType` variants
    (`PairLifecycleListed/Delisted/Halted/Resumed/TickLotChanged/MinNotionalChanged`).
    `MarketMakerConfig.pair_lifecycle_enabled` (default `true`
    so operators get the tick/lot drift detection for free
    even on venues without a status field) and
    `pair_lifecycle_refresh_secs` (default `300` = 5 min).
  - **Stage-2 follow-ups (tracked under ROADMAP P2.3).**
    Bybit V5 + HL + custom client `trading_status` parsing,
    dynamic engine spawn for new listings via a
    `Vec<MarketMakerEngine>` registry on the server, 7-day
    probation mode for auto-onboarded pairs (wider spreads,
    smaller size, observation window), `PairLifecycleManager`
    sharing across multi-symbol deployments via
    `Arc<Mutex<>>` so the discovery loop runs once per
    venue rather than once per symbol.
- **P2.2 — Per-pair per-minute SLA presence buckets** (ROADMAP
  production-spot-MM gap closure). The pre-P2.2 `SlaTracker`
  exposed a single lifetime `uptime_pct` aggregate, which is
  unusable for paid MM agreements that audit "X % presence at
  Y bps for Z hours per day per pair". A breach in
  hour 14 is invisible if hour 15 brings the lifetime average
  back above the floor. P2.2 replaces the single counter with
  a per-minute bucket array so the rebate-clawback story is
  reconstructible from the daily report alone.
  - **State machine.** `SlaTracker` gains a
    `Box<[PresenceBucket; 1440]>` indexed by
    `now.hour() * 60 + now.minute()` plus a
    `presence_day_key: Option<(year, ordinal)>`. Each
    `tick()` routes the same compliant/two_sided/spread
    sample into the matching minute bucket, and the array is
    wiped on the first tick that crosses UTC midnight so each
    day is independent. `PresenceBucket` carries
    `total_seconds`, `compliant_seconds`, `two_sided_seconds`
    and a `min_spread_bps` / `max_spread_bps` envelope.
  - **Accessors.** New `presence_pct_for_minute(m)`,
    `presence_pct_for_range(start, end)` (observation-weighted
    so a 60-sample minute outweighs a 30-sample one), and
    `daily_presence_summary() -> DailyPresenceSummary`.
    Empty buckets default to `100 %` so a fresh start at
    14:00 UTC does not look like 58 % uptime — the engine
    reports `minutes_with_data` alongside the percentage so
    operators can spot true gaps.
  - **Engine + dashboard wiring.** `SymbolState` gains
    `presence_pct_24h`, `two_sided_pct_24h`,
    `minutes_with_data_24h`. The engine's `update_dashboard`
    pushes them on every refresh tick. New Prometheus gauge
    `mm_sla_presence_pct_24h` fires from
    `DashboardState::update`. `SymbolDailyReport` exposes the
    three new fields so `GET /api/v1/report/daily` is now
    rebate-clawback-grade for paid MM agreements.
  - **Tests.** Seven new unit tests in `mm-risk::sla` pin
    (a) every tick lands in exactly one minute bucket,
    (b) multiple ticks in the same minute aggregate without
    drifting the spread envelope, (c) empty bucket reports
    `100 %`, (d) `presence_pct_for_range` is
    observation-weighted, (e) empty range returns `100 %`,
    (f) `daily_presence_summary` skips empty minutes and
    rolls up correctly across multiple non-contiguous
    minutes, (g) the fresh-start defaults are
    `100 % / 0 minutes` not `NaN`.
- **P2.1 — Per-asset-class kill switch with shared escalation
  state** (ROADMAP production-spot-MM gap closure). The
  pre-P2.1 `KillSwitch` was a per-engine state machine — when
  ETH-family pairs needed a coordinated halt (stETH depeg,
  Ronin bridge incident, single-venue outage on one asset),
  the only knob was the global per-engine switch on each ETH
  symbol individually, with no way to escalate them as a group
  without touching unrelated BTC pairs. P2.1 closes the gap
  by introducing a parallel asset-class layer.
  - **State machine sharing.** `MarketMakerEngine` gains an
    optional `asset_class_switch: Option<Arc<Mutex<KillSwitch>>>`
    field plus a `with_asset_class_switch(arc)` builder.
    Engines whose symbols belong to the same class call the
    builder with the **same** `Arc<Mutex<_>>`, so a
    coordinated escalation halts every pair in the class
    simultaneously without touching unrelated symbols. The
    `Arc<Mutex<_>>` shape mirrors the shared `Portfolio`
    pattern from Sprint I.
  - **Hard-vs-soft separation.** A new
    `MarketMakerEngine::effective_kill_level()` helper
    returns `global.max(asset_class)` so soft-decision call
    sites (`ks_spread` / `ks_size` in `refresh_quotes`, the
    dashboard `kill_level` exposure) honour the asset-class
    layer immediately. Hard-decision call sites
    (`CancelAll` / `FlattenAll` / `Disconnect`) keep reading
    `self.kill_switch.level()` directly so an asset-wide
    widening cannot accidentally flatten another pair's
    inventory. `KillLevel` gains free-fn
    `spread_multiplier()` / `size_multiplier()` helpers so
    the multipliers can be derived from a level the engine
    composed itself rather than from any single kill-switch
    instance.
  - **Server composition.** `mm-server::main` parses
    `config.kill_switch.asset_classes` up front, builds one
    `Arc<Mutex<KillSwitch>>` per class, and looks up each
    engine's class via the inverted `symbol → class` map. The
    matching arc is passed via the new builder; engines whose
    symbol has no class get `None` and run with the global
    layer only.
  - **Config.** `KillSwitchCfg` gains an `asset_classes:
    Vec<AssetClassKillSwitchCfg>` field (default empty). Each
    entry carries `name`, `symbols`, and a full
    `limits: KillSwitchCfg` (same shape as the global one).
    `validate.rs` errors when an asset-class entry references
    a symbol that does not exist in the top-level
    `[[symbols]]` array, when a symbol appears in two
    classes, or when a class has an empty name; warns when a
    class has zero symbols.
  - **Tests.** Two new free-fn unit tests in `mm-risk` pin
    the per-level `spread_multiplier`/`size_multiplier`
    constants and the `KillLevel: Ord` invariant the
    effective-level max relies on. Three new engine-level
    tests prove (a) the no-asset-class path falls through to
    the global level verbatim, (b) `effective_kill_level`
    takes `max(global, asset_class)` and respects the
    "max not replace" semantics in both directions, and
    (c) two engines pointing at the same `Arc<Mutex<_>>` see
    each other's escalations instantly — the regression
    anchor for the "halt all ETH-family pairs" use case.
- **P1.4 stage-1 — Cross-venue basis with hedge-book staleness
  gate** (ROADMAP production-spot-MM gap closure; partial —
  stage-1 ships the strategy upgrade, audit events, and the
  Prometheus surface, stage-2 will land the USDC↔USDT FX
  micro-hedge connector and rolling per-venue funding PnL
  accrual). The existing `BasisStrategy` was already
  venue-agnostic at the strategy layer (it just consumes
  `ref_price` + `hedge_book` from the engine), so the
  primary/hedge ConnectorBundle already supports cross-venue
  pairs. The actual P1.4 gap was operational: cross-venue WS
  feeds jitter 200-800 ms in steady state and pause for
  multi-second windows under load, and a stale hedge mid is a
  much louder failure mode for cross-venue than for
  same-venue. Stage-1 closes that gap with a strategy-side
  staleness gate plus the audit + dashboard surface that
  cross-venue ops needs to see what the bot is doing.
  - **Strategy upgrade.** New
    `BasisStrategy::cross_venue(shift, max_basis_bps,
    max_staleness_ms)` constructor sits next to the existing
    same-venue `new`. Adds a `max_hedge_staleness_ms:
    Option<i64>` field — `None` is the legacy same-venue
    behaviour, `Some(ms)` is the cross-venue mode. New
    `StrategyContext.hedge_book_age_ms: Option<i64>` field is
    threaded by the engine as
    `now_ms - hedge_book.last_update_ms`. The strategy
    stands down on every refresh tick where the age exceeds
    the gate, AND when the age reading is missing entirely
    (no hedge book yet, or engine forgot to thread it) —
    cross-venue is opt-in to "fail safe by default". Four
    new unit tests pin the four branches: fresh book quotes
    normally, stale book stands down, no-age-reading stands
    down, same-venue mode (`None` gate) ignores staleness
    completely. The last test is the regression anchor for
    the "P1.4 must not break P0/P1.x" invariant.
  - **Audit + dashboard.** Two new `AuditEventType` variants
    `CrossVenueBasisEntered` / `CrossVenueBasisExited` fired
    by the engine the first refresh tick after the basis
    crosses `config.hedge.pair.basis_threshold_bps` (the
    same number that gates the strategy). State machine
    `cross_venue_basis_inside: bool` on
    `MarketMakerEngine` debounces the events so each
    round-trip emits exactly one entered + one exited rather
    than spamming the audit trail every refresh. New
    Prometheus gauge `mm_cross_venue_basis_bps` is published
    on every refresh tick whenever both legs have a mid.
  - **Engine wiring.** `refresh_quotes` reads the cross-venue
    basis BEFORE building the immutable hedge-book reference
    that the `StrategyContext` holds — borrow-checker dance
    so the audit-state mutation does not conflict with the
    strategy-context borrow. `MarketMakerConfig.cross_venue_basis_max_staleness_ms`
    (default `1500`, fits cross-venue WS feed jitter) is the
    new operator knob. New `StrategyType::CrossVenueBasis`
    variant routes the server's strategy builder into
    `BasisStrategy::cross_venue(...)`; same-venue pairs still
    pick `Basis`.
  - **Stage-2 follow-ups (tracked under ROADMAP P1.4).**
    USDC↔USDT FX micro-hedge connector (5-10 bps of silent
    leakage if ignored on cross-stable pairs), per-venue
    rolling 24h funding PnL accrual, third-connector slot in
    `ConnectorBundle` for the FX leg, audit events
    `FxMicroHedgeAdjusted`, and a per-pair settlement-currency
    selector in `HedgePairConfig`.
- **P1.3 stage-1 — Borrow-cost surcharge in the spot ask
  reservation** (ROADMAP production-spot-MM gap closure;
  partial — stage-1 ships the rate fetch and the strategy
  shim, stage-2 will land actual loan execution + margin-mode
  order routing). A spot MM that starts flat in the base asset
  cannot quote the ask side without borrowing — and the
  borrow rate the venue charges is a real carry cost the
  strategy needs to bake into its reservation, otherwise
  captured spread leaks into interest expense. Stage-1 is the
  foundation that closes the **pricing** half of that gap.
  - **Trait surface.** New
    `ExchangeConnector::get_borrow_rate(asset)`,
    `borrow_asset(asset, qty)`, and `repay_asset(asset, qty)`
    methods, all defaulting to `Err(BorrowError::NotSupported)`.
    New `BorrowRateInfo { asset, rate_apr, rate_bps_hourly,
    fetched_at }` + `BorrowError { NotSupported, Other }`
    types. The `BorrowRateInfo::from_apr` helper centralises
    the APR → hourly-bps conversion (`× 10_000 / 8_760`) so
    every venue override speaks the same unit.
  - **Binance Spot rate fetch.** `BinanceConnector::get_borrow_rate`
    calls `GET /sapi/v1/margin/interestRateHistory?asset=&size=1`
    and parses the `dailyInterestRate` field; the pure helper
    `parse_binance_borrow_rate_response` multiplies by 365 to
    return an APR fraction. Two unit tests pin both the
    array-shape and the bare-object response forms. Bybit V5
    and HyperLiquid keep the trait-default `NotSupported` —
    Bybit UTA borrow is implicit in the unified collateral
    pool and HL has no margin product. `borrow_asset` /
    `repay_asset` remain `NotSupported` on every venue
    pending stage-2 (margin-mode order routing).
  - **State machine.** New `mm-risk::borrow` module with
    `BorrowState { asset, rate_apr, rate_bps_hourly,
    fetched_at }` and `BorrowManager { state,
    expected_holding_secs, max_borrow, buffer }`. The manager
    owns an `effective_carry_bps()` accessor that converts the
    APR into the strategy-side surcharge:
    `carry_bps = APR × 10_000 × (holding_secs / 31_536_000)`.
    Returns `Decimal::ZERO` before the first refresh — the
    "strategy reverts to pre-P1.3 behaviour when borrow data
    is missing" invariant. 7 unit tests pin the conversion
    constants, the linear-in-holding-time scaling, and the
    `max_borrow` / `buffer` accessor contract that stage-2
    will load against.
  - **Strategy integration.** New
    `StrategyContext.borrow_cost_bps: Option<Decimal>` field.
    `AvellanedaStoikov` and `BasisStrategy` add the surcharge
    fraction-of-mid to the reservation price *up*, shifting
    both bid and ask in lockstep — equivalent to widening the
    ask half-spread by the carry while tightening the bid by
    the same amount, which makes the strategy less willing
    to accumulate the short side that pays the loan. New
    test `borrow_cost_shifts_reservation_up` is the
    regression anchor.
  - **Engine refresh task.** `MarketMakerEngine` gains an
    optional `borrow_manager: Option<BorrowManager>`,
    initialised when `config.market_maker.borrow_enabled`.
    A new `refresh_borrow_rate` method sits next to
    `refresh_fee_tiers` in the periodic-tick pattern and
    runs on a fresh `borrow_rate_interval` arm. On each
    successful tick it pushes the APR into `BorrowManager`
    and updates two new Prometheus gauges
    (`mm_borrow_rate_bps_hourly`, `mm_borrow_carry_bps`).
    `refresh_quotes` reads `effective_carry_bps()` and
    threads it into `StrategyContext.borrow_cost_bps` so
    the surcharge lands on the very next quote refresh.
    `NotSupported` is a quiet debug-level fall-through;
    `Other` warn-logs and keeps the previous APR.
  - **Config knobs.** `MarketMakerConfig.borrow_enabled`
    (default `false` — opt-in so existing operators are not
    silently re-priced into a wider book),
    `borrow_rate_refresh_secs` (default `1800`),
    `borrow_holding_secs` (default `3600` — converts APR
    into the expected-carry bps), `borrow_max_base` and
    `borrow_buffer_base` (defaults `0`; persisted by
    stage-1, enforced by stage-2 when the loan execution
    path lands).
  - **Stage-2 follow-ups (tracked under ROADMAP P1.3).**
    Margin-mode connector for Binance Spot
    (`/sapi/v1/margin/order` routing + `POST /sapi/v1/margin/loan`
    execution + opposing-fill repay), Bybit UTA borrow
    accounting via `BalanceCache.borrowed_in(asset)`,
    audit-trail `BorrowOpened` / `BorrowRepaid` events, and
    the strategy-side dynamic max-borrow cap.
- **P1.2 — Dynamic fee-tier refresh + rebate-aware accounting**
  (ROADMAP production-spot-MM gap closure). Until now
  `ProductSpec.maker_fee` / `taker_fee` were frozen at startup
  from the connector defaults — a month-end VIP tier crossing
  silently shaved 1-2 bps off captured edge until the next
  process restart. P1.2 closes the gap with a periodic refresh
  task that reads the venue's authoritative per-account fee
  schedule and hot-swaps it into the live `PnlTracker`.
  - **Trait surface.** New
    `ExchangeConnector::fetch_fee_tiers(symbol)` with default
    `Err(FeeTierError::NotSupported)`. New `FeeTierInfo {
    maker_fee, taker_fee, vip_tier, fetched_at }` and
    `FeeTierError { NotSupported, Other }` types in
    `mm-exchange-core::connector`. Negative `maker_fee` is the
    documented rebate convention.
  - **Venue overrides.**
    `BybitConnector::fetch_fee_tiers` calls
    `GET /v5/account/fee-rate?category=&symbol=` and parses the
    `result.list` row for the queried symbol via the pure
    helper `parse_bybit_fee_rate_response` (two unit tests pin
    the wire shape and the multi-symbol row-pick).
    `BinanceConnector::fetch_fee_tiers` calls
    `GET /sapi/v1/asset/tradeFee?symbol=` and accepts both the
    array and the bare-object response shapes via
    `parse_binance_spot_fee_response` (two tests cover both
    shapes).
    `BinanceFuturesConnector::fetch_fee_tiers` calls
    `GET /fapi/v1/commissionRate?symbol=` and parses
    `makerCommissionRate` / `takerCommissionRate` via
    `parse_binance_futures_fee_response` (one wire-shape test).
    HyperLiquid and the custom `mm-exchange-client` keep the
    default `NotSupported` and the engine logs the
    fallthrough at debug level.
  - **PnL tracker hot-swap.** New `PnlTracker::set_fee_rates(maker, taker)`
    plus read-only `maker_fee()` / `taker_fee()` accessors.
    Subsequent `on_fill` calls attribute against the new rates;
    previously accrued `fees_paid` and `rebate_income` are not
    retroactively rewritten — that would conflict with the
    audit trail. New regression test
    `set_fee_rates_hot_swaps_for_subsequent_fills` is the
    anchor.
  - **Engine refresh task.** `MarketMakerEngine::refresh_fee_tiers`
    is wired into a new `fee_tier_interval` arm in
    `run_with_hedge`. On each tick it calls
    `connector.fetch_fee_tiers(symbol)`, updates
    `self.product.maker_fee` / `taker_fee`, hot-swaps the
    `PnlTracker` rates, and pushes two new Prometheus gauges
    (`mm_maker_fee_bps`, `mm_taker_fee_bps` — both
    venue-reported, in basis points). The first tick fires
    immediately so the operator sees the authoritative rates
    before the first quote refresh; subsequent ticks honour
    the configured cadence. `NotSupported` is a quiet
    fall-through (HL / custom); `Other` errors warn-log and
    keep the previous rates.
  - **Config knobs.** New
    `MarketMakerConfig.fee_tier_refresh_enabled` (default
    `true`) and `fee_tier_refresh_secs` (default `600`).
    Setting `enabled=false` or `secs=0` disables the refresh
    arm entirely — useful for paper mode and venues with
    rate-limited account endpoints.
  - **Honest capability flag bonus.** As a side effect of
    walking every venue, `BinanceConnector` (Spot)
    `supports_amend` was already flipped to a honest `false`
    in P1.1 — same epic, but called out here because the fee
    helper makes the inconsistency between Spot
    (`order.cancelReplace` only) and Futures
    (`PUT /fapi/v1/order` modify) visible.
- **P1.1 — Queue-priority-preserving amend on the order diff**
  (ROADMAP production-spot-MM gap closure). Until now every
  quote refresh hit the venue as a cancel + place pair, even
  when the new price was a single tick away from the live one
  — and every cancel costs queue priority. Tardis measured
  this at 2-5 bps of captured spread on tight pairs. The fix
  lands in three layers:
  - **Diff layer.** `OrderManager::diff_orders` now returns a
    new `OrderDiffPlan { to_cancel, to_amend, to_place }`
    instead of the legacy `(Vec<OrderId>, Vec<Quote>)` tuple,
    and takes a fresh `amend_epsilon_ticks: u32` argument.
    When non-zero, a greedy nearest-pair pass on each side
    matches stale orders against new quotes whose qty is
    unchanged and whose price is within
    `epsilon * tick_size` of the old price; matches collapse
    into an `AmendPlanEntry` instead of a cancel + place
    pair. Pure function — testable without a connector.
    Six new unit tests pin the algorithm: same-side same-qty
    tweak collapses, qty change defeats pairing, price diff
    > epsilon defeats pairing, `epsilon = 0` is the legacy
    cancel + place path (regression anchor for the disabled
    state), cross-side matching is rejected, and the
    `reprice_order` post-amend bookkeeping moves the
    `price_index` slot atomically while keeping the `OrderId`.
  - **Execution layer.** `OrderManager::execute_diff` reads
    the connector's `VenueCapabilities::supports_amend` flag
    and downgrades planned amends back to cancel + place
    when the venue does not support a real native amend
    (HyperLiquid, Binance Spot). On success the local state
    is updated via `reprice_order` so the same `OrderId`
    keeps its queue position. On amend failure the entry
    falls back to cancel + place by appending into the
    next-up buckets — no quote is silently dropped. New
    `amended` / `amend_failures` counters surface in the
    `order diff executed` log line.
  - **Venue layer.** `BybitConnector::amend_order` overrides
    the trait default to call `POST /v5/order/amend` with the
    V5-mandatory `category` field, identifying the order via
    `orderId` to match the existing cancel path. Body
    construction is factored into a pure
    `build_amend_body(category, amend)` helper so the wire
    shape is unit-tested without an HTTP client (two new
    tests pin the required fields and the "omit unset
    optional" rule).
    `BinanceFuturesConnector::amend_order` overrides the
    default to call `PUT /fapi/v1/order` with `symbol`,
    `origClientOrderId`, `quantity` and `price`; the same
    pure-helper pattern (`build_amend_query`) carries two
    wire-shape tests including a programmer-error bail when
    `new_price` or `new_qty` is missing.
    `BinanceConnector` (Spot) flips `supports_amend` to an
    honest `false` — Binance Spot only has
    `order.cancelReplace`, which is a cancel+place under the
    hood and loses queue priority. The `capabilities_match_implementation`
    test now asserts the false value with an explanatory
    comment so a future contributor cannot silently flip it
    back on.
    HyperLiquid was already honestly `false`. No change to
    the trait default — venues that don't override it still
    fall back to cancel+place via the trait body.
  - **Config.** Already-defined `MarketMakerConfig.amend_enabled`
    (default `true`) and `amend_max_ticks` (default `2`)
    were unwired until this change; they are now threaded
    through `MarketMakerEngine::refresh_quotes` into
    `OrderManager::execute_diff`. Setting either off
    degrades to the pre-P1.1 cancel+place behaviour without
    a code change.
- **P0.1 — HyperLiquid `webData2` balance pushes wired into the
  engine** (ROADMAP production-spot-MM gap closure, third
  venue after Binance and Bybit; **closes the P0.1 cluster**).
  HL is architecturally different from Binance and Bybit: it
  multiplexes `userEvents` + `orderUpdates` onto the same WS
  connection as `l2Book` + `trades`, so `MarketEvent::Fill`
  events were already reaching the engine through the existing
  connector subscribe path. The actual gap was that HL has no
  separate "wallet snapshot" topic — without it, balance state
  in `BalanceCache` only refreshed via the 60 s reconcile poll
  even though fills landed instantly. The fix is purely
  additive in `crates/exchange/hyperliquid/src/connector.rs`:
  the subscribe loop now also subscribes to
  `{"type":"webData2","user":"<addr>"}`, and `parse_hl_event`
  gained a `webData2` branch that emits `BalanceUpdate` events
  on every push. Two helper parsers,
  `parse_hl_perp_balance` and `parse_hl_spot_balances`, mirror
  the field layout of the existing REST `clearinghouseState` /
  `spotClearinghouseState` readers in `get_balances`, so a
  schema drift breaks both the test and the live parser
  symmetrically. `parse_hl_event` now takes an `is_spot` flag
  and routes `webData2` payloads disjointly: perp connectors
  emit a single USDC `BalanceUpdate` against
  `WalletType::UsdMarginedFutures` (`marginSummary.accountValue`
  → total, `withdrawable` → available, the difference → locked),
  while spot connectors emit one event per non-zero
  `spotState.balances[]` entry tagged with `WalletType::Spot`.
  No `mm-server::spawn_event_merger` changes: HL doesn't need
  the merger pattern because its private stream rides the
  public WS — the wiring is a single new subscribe message.
  New `parse_hl_event_for_test` re-export on the crate root is
  the test hook downstream crates pin against. New integration
  test `hl_webdata2_frame_feeds_balance_cache` in
  `crates/engine/tests/integration.rs` takes a hand-crafted
  `webData2` perp frame, parses it through the public hook,
  plugs the emitted `BalanceUpdate` into a fresh
  `BalanceCache::new_for(UsdMarginedFutures)`, and asserts
  both `available_in("USDC", _)` and `total_in("USDC", _)`
  reflect the frame. 6 unit tests on the parser itself
  (perp happy path, fallback when `accountValue` missing,
  spot per-coin emission, disjoint spot/perp routing,
  silent-on-empty, public-hook pass-through). The full P0.1
  cluster (Binance listen-key, Bybit V5 private WS, HL
  webData2) is now closed — every venue the bot trades on
  has a real-time balance push wired into `BalanceCache`,
  so P0.2's drift reconciler is no longer load-bearing for
  the steady state.
- **P0.1 — Bybit V5 private WS user stream wired into the
  engine** (ROADMAP production-spot-MM gap closure, second
  venue after Binance). New `crates/exchange/bybit/src/user_stream.rs`
  module opens Bybit V5's `wss://stream.bybit.com/v5/private`
  endpoint, signs in with the V5 auth op
  (`HMAC_SHA256(secret, "GET/realtime" + expires)`), subscribes to
  `execution` + `wallet` + `order`, and emits
  `MarketEvent::Fill` / `MarketEvent::BalanceUpdate` events on
  the same channel the public subscribe task uses. The
  `wallet` parser is defensive about the V5 UTA wallet
  schema: it prefers `availableToWithdraw` over the deprecated
  `free` field, falls back to `walletBalance - locked` when
  neither is sent, and reads `totalOrderIM` as the locked
  collateral on Unified accounts. `mm-server::spawn_event_merger`
  now branches on `ExchangeType::Bybit{,Testnet}` and spawns
  `mm_exchange_bybit::user_stream::start` against the same
  merged channel that already feeds the engine, gated behind
  `user_stream_enabled` and an empty-credentials short-circuit
  (the auth handshake requires both api_key and api_secret on
  Bybit, unlike Binance's listen key which only needs the
  api_key). New integration test
  `bybit_private_wallet_frame_feeds_balance_cache` in
  `crates/engine/tests/integration.rs` is the regression
  anchor: it takes a hand-crafted V5 wallet snapshot, parses
  it through `parse_user_event_for_test`, plugs both emitted
  `BalanceUpdate` events into a fresh `BalanceCache::new_for(Unified)`,
  and asserts `available_in("USDT", Unified)` and
  `available_in("BTC", Unified)` surface the
  `availableToWithdraw` values from the frame. 9 unit tests
  on the parser itself (auth-frame shape, execution
  Trade/non-Trade, UTA wallet, classic spot wallet, fallback
  arithmetic, multi-account frames, unknown-topic ignore,
  config-helper sanity). HyperLiquid `userEvents` remains
  tracked under ROADMAP P0.1.
- **P0.2 — Inventory-vs-wallet drift reconciliation** (ROADMAP
  production-spot-MM gap closure). Closes the second
  correctness blocker after P0.1: even with the listen-key
  stream wired, a dropped WS frame or a parser miss can still
  silently desync `InventoryManager.inventory()` from the
  wallet. The new `mm_risk::inventory_drift` module
  (`InventoryDriftReconciler` + `DriftReport`) snapshots the
  wallet total at first reconcile and compares the wallet
  delta against the tracked inventory delta on every
  subsequent cycle. Any mismatch above
  `inventory_drift_tolerance` (default `0.0001` base-asset
  units) fires a `DriftReport` that the engine routes into
  the audit trail as the new `InventoryDriftDetected` event
  type. `InventoryManager::force_reset_inventory_to` is the
  opt-in auto-correct hook — gated behind
  `inventory_drift_auto_correct` (default `false`,
  alert-only) so operators can investigate before the system
  rewrites tracker state. `BalanceCache::total_in(asset,
  wallet)` is the new ground-truth accessor: returns
  `free + locked` before local reservations, matching how
  the venue reports the wallet. Wired into
  `engine::market_maker::reconcile` as
  `check_inventory_drift()`. Two new E2E integration tests
  (`baseline_then_missed_buy_surfaces_drift_report`,
  `auto_correct_drift_force_resets_inventory_manager`) prove
  the full chain from `BalanceCache` through the reconciler
  into `InventoryManager::force_reset_inventory_to`. 9 unit
  tests on the reconciler itself.
- **P0.1 — Binance listen-key user-data stream wired into the
  engine** (ROADMAP production-spot-MM gap closure). The
  `crates/exchange/binance/src/user_stream.rs` module has been
  feature-complete since the spot epic but was never spawned by
  any server call site — so fills arriving out-of-band (REST
  fallback, partial fills after the WS-API response envelope,
  manual UI orders, RFQ/OTC trades touching the same account)
  never reached `InventoryManager` until the 60 s reconcile
  cycle. This closes the gap by introducing
  `mm-server::spawn_event_merger`, which multiplexes the
  public ws subscribe feed with an optional Binance user-data
  stream into a single merged `MarketEvent` channel before
  passing it to `engine.run_with_hedge`. The engine's
  `handle_ws_event::BalanceUpdate` branch that was previously
  dead code is now the canonical path for spot balance updates.
  Gated behind the new `MarketMakerConfig.user_stream_enabled`
  flag (default `true`); the merger short-circuits cleanly on
  non-Binance venues, missing credentials, and the `false`
  toggle. Bybit V5 and HyperLiquid equivalents remain tracked
  under ROADMAP P0.1. New integration test
  `binance_user_stream_frame_feeds_balance_cache` in
  `crates/engine/tests/integration.rs` takes a hand-crafted
  `outboundAccountPosition` frame, parses it via the new
  `parse_user_event_for_test` entry point, plugs both emitted
  `BalanceUpdate` events into a fresh `BalanceCache`, and
  asserts the free-balance values surface through
  `available_in(asset, wallet)` — first regression anchor for
  the full listen-key → cache path.
- **Operator surface for the new signals: config toggles, E2E
  test, dashboard panel, `mm-probe` CLI.**
  - `MarketMakerConfig` gains four opt-in fields
    (`market_resilience_enabled`, `otr_enabled`, `hma_enabled`,
    `hma_window`), all defaulted to `true` / `9`. When a
    toggle is off, the engine skips the corresponding
    `on_trade` / `on_book` / `refresh_quotes` feed and the
    autotuner's MR channel is explicitly cleared so the
    effective spread multiplier returns to the
    regime+toxicity baseline.
  - `config/default.toml` documents the new knobs inline.
  - New E2E integration test
    `crates/engine/tests/integration.rs::
    large_trade_widens_autotuner_spread_mult_and_recovers`
    — feeds a synthetic trade+book stream through a
    `MarketResilienceCalculator` paired with an `AutoTuner`,
    asserts that the effective spread multiplier widens after
    a large trade and recovers exactly to the baseline past
    the decay window. First end-to-end check that the MR →
    autotuner → strategy plumbing isn't broken by future
    refactors.
  - New `SignalsPanel.svelte` in the Svelte frontend —
    three-row panel showing Market Resilience (with
    kill-switch threshold bar), Order-to-Trade Ratio and
    HMA-vs-mid delta. Auto-populates from `/api/status`. Demo
    mode synthesises the three fields so operators can see
    the panel animated without running the engine.
  - New `mm-probe` CLI binary (`cargo run -p mm-backtester
    --bin mm-probe -- --events <path.jsonl>`). Streams a
    recorded event JSONL through a standalone
    `MarketResilienceCalculator` + `OrderToTradeRatio` + `Hma`
    trio and prints a tab-separated time series to stdout.
    Useful for calibrating thresholds and sanity-checking the
    signals on real market data without lifting the whole
    engine. Flags: `--stride N`, `--mr-warmup N`,
    `--hma-window N`.
- **Tick / Volume / MultiTrigger candles + Hull Moving
  Average — mm-toolbox cherry-pick**
  (`mm_indicators::{candles,hma,weights}`). Port of the three
  non-trivial building blocks from
  `github.com/beatzxbt/mm-toolbox` (MIT), adapted to Rust and
  landed in the `mm-indicators` leaf crate.
  - `TickCandles`, `VolumeCandles`, `MultiTriggerCandles`:
    trade-aggregated candle buckets with three independent
    trigger modes. Tick buckets close after N trades
    regardless of wall-clock time. Volume buckets close after
    N base-asset units traded, splitting straddling trades so
    the bucket fills exactly (a single huge trade can emit
    several candles in one `update` call). MultiTrigger closes
    on whichever of `(max_duration_ms, max_ticks, max_volume)`
    fires first — useful when you want volume-normalised
    candles but need a hard floor on candle-close latency in
    a dead market. Ring-buffered into a `VecDeque<Candle>`
    with capacity eviction. Candle struct carries
    `open/high/low/close/buy_volume/sell_volume/vwap/total_trades/open_ts/close_ts`
    in `Decimal` (upstream Numba version uses `f64` — the
    Rust port is strictly more precise and plays nicely with
    the rest of the `Decimal`-everywhere invariant).
  - `Hma` + `Wma`: Hull Moving Average (Alan Hull, 2005) built
    from three `Wma`s —
    `HMA(n) = WMA(2·WMA(n/2) − WMA(n), √n)`. Both smoother and
    **lower-lag** than EMA/SMA of the same window. Wired into
    `MomentumSignals::with_hma` as an optional 5th alpha
    component that captures the HMA slope on mid-price
    updates. When attached, the alpha weights are re-split
    `book 0.3 / flow 0.3 / micro 0.2 / hma 0.2`; when absent,
    the legacy `0.4 / 0.4 / 0.2` split is preserved. Engine
    defaults to `DEFAULT_HMA_WINDOW = 9`, matching the
    upstream quickstart. Exposed as the Prometheus gauge
    `mm_hma_value`.
  - `geometric_weights(n, r)` + `ema_weights(n, alpha)`:
    standalone kernel-weight generators. Normalised to sum to
    1.0. Used by alpha-signal code that wants a hand-shaped
    weight vector without owning a full moving-average state
    machine. Upstream default `r = 0.75` for geometric,
    `α = 3 / (window + 1)` for EMA weights.
  - 36 new tests (14 candles, 9 HMA/WMA, 7 weights, 3
    MomentumSignals HMA wiring, 3 dashboard/metrics).
    Workspace test count: 549 → 582.
- **Market Resilience (MR) score + Order-to-Trade Ratio
  surveillance — VisualHFT cherry-pick**
  (`mm_strategy::market_resilience`, `mm_risk::otr`). Port of
  the two non-trivial analytics plugins from
  `github.com/visualHFT/VisualHFT` (Apache-2.0), adapted to
  Rust and wired end-to-end through the engine.
  - `MarketResilienceCalculator`: event-driven detector that
    reacts to **just-happened** liquidity shocks. Tracks trade
    shocks (z-score on rolling trade-size window), spread
    shocks (robust z on a `P2Quantile` median baseline) and
    depth depletion (robust z on immediacy-weighted depth,
    recovery measured against a per-event trough/baseline
    pair). Emits the VisualHFT weighted score formula
    `(trade 30 % / spread-recovery 10 % / depth-recovery 50 %
    / spread-magnitude 10 %)` in `[0, 1]`, decays linearly
    back toward `1.0` over 5 s after each shock. Wired into
    `AutoTuner::set_market_resilience` so the effective
    spread multiplier is divided by `max(mr, 0.2)` — a low
    score widens the book post-shock, the floor caps widening
    at 5×. Also wired into `KillSwitch::update_market_resilience`
    so a sustained dip below `0.3` for ≥3 s escalates to L1
    (`WidenSpreads`), while harder levels stay driven by PnL
    / position value alone.
  - `InventoryGammaPolicy` (stationary state-space widen) and
    `MarketResilienceCalculator` (event-driven shock
    reaction) are now complementary inputs to the autotuner:
    one says *how much* γ should react to inventory load, the
    other says *when* the spread should react to a
    just-happened shock.
  - `OrderToTradeRatio`: regulatory surveillance counter
    `(adds + 2·updates + cancels) / max(trades, 1) - 1`. A
    high OTR is the canonical spoofing / layering proxy that
    regulators (MiCA, ESMA, SEBI, MAS) monitor. Exported
    through `AuditLog::order_to_trade_ratio_snapshot` every
    60 ticks so the full time series is reconstructable from
    the JSONL audit trail — MiCA compliance. Also exposed as
    the Prometheus gauge `mm_order_to_trade_ratio`.
  - `MarketResilienceCalculator` is fed from both `on_trade`
    and `on_book` in `engine::market_maker::handle_ws_event`;
    the OTR counter increments on every book event
    (`on_update`) and trade (`on_trade`). Two new
    `GaugeVec`s in `mm_dashboard::metrics`:
    `mm_market_resilience` and `mm_order_to_trade_ratio`.
  - **Supporting primitives** landed alongside the calculator:
    - `mm_common::P2Quantile` — Jain & Chlamtac 1985 running
      quantile estimator. O(1) memory, O(1) per-update. Drop-
      in replacement for a fixed-window rolling median when
      the goal is a robust baseline for z-score detection.
    - `mm_strategy::features::immediacy_depth_bid/ask` —
      rank-churn-invariant depth metric
      `Σ qty · 1 / (1 + d)²`, `d` in spread units. Unlike a
      plain top-k qty sum, this metric drops when inner
      levels are replaced with outer levels, which is the
      actual behaviour we want to see in a depletion
      detector.
  - 36 new tests (5 P², 4 immediacy depth, 11 MR, 6 OTR, 6
    autotune MR wiring, 4 kill-switch MR). Workspace test
    count: 513 → 549.
- **ISAC-inspired closed-form inventory γ policy and risk
  penalty** (`mm_strategy::autotune`). Port of the analytical
  backbone from the ISAC SAC-gamma agent
  (`github.com/im1235/ISAC`), minus the PyTorch training
  loop. `InventoryGammaPolicy { max_inventory, q_weight,
  q_exp, t_weight, t_exp, min_mult, max_mult }` computes
  `γ_mult = clamp(1 + q_weight·(|q|/q_max)^q_exp +
  t_weight·(1 − t_remaining)^t_exp, [min_mult, max_mult])`,
  so γ widens smoothly with inventory load and as the
  session horizon runs out — the same state surface the RL
  agent learns after 2000 training paths, but in closed
  form with no GPU dependency. Defaults
  (`q_weight=1.5, q_exp=2, t_weight=0.5, t_exp=3`) mirror
  the ISAC policy output on a `q_max=0.1` sim. Attached via
  `AutoTuner::with_inventory_gamma_policy` and fed state
  through `update_policy_state(q, t_remaining)` each tick;
  `effective_gamma_mult()` folds the policy into the
  regime × toxicity product. Also exports
  `inventory_risk_penalty(q, σ, dt) = 0.5·|q|·σ·√dt` — the
  mean-variance risk charge from the original
  Avellaneda-Stoikov paper, ready for hyperopt loss
  functions that score on risk-adjusted PnL instead of raw
  PnL. Wired into `engine::market_maker::refresh_quotes`
  via `auto_tuner.update_policy_state(inventory,
  t_remaining)`. 13 tests.
- **Queue-position-aware backtest fill model**
  (`mm_backtester::queue_model`). Port of the canonical
  `hftbacktest` design. New `Probability` trait with two
  concrete implementations: `LogProbQueueFunc` (`f(x) = ln(1 + x)`)
  and `PowerProbQueueFunc` (`f(x) = x^n`, with `n` tunable) that
  split a qty decrease at a price level between "ahead of
  us" and "behind us" via `f(back) / (f(back) + f(front))`.
  Stateful `QueuePos { front_q_qty, cum_trade_qty }` tracker
  per live maker order with `new(book_qty)`, `on_trade(qty)`,
  `on_depth_change(prev, new, prob_model)`, and
  `consume_fill()` — the last lifts overshoot out of the
  tracker as filled base qty. Closes the biggest accuracy
  gap in our backtester: the existing
  `ProbabilisticFiller::prob_fill_on_touch` scalar
  systematically over-reports MM PnL by 10–30 % because it
  ignores whether the queue ahead of a maker order has
  actually cleared. The module is standalone — callers that
  want queue-aware fills route market events through
  `QueuePos` alongside the existing filler. A future
  simulator rewrite can replace the scalar coin-flip with
  this stateful tracker. 18 tests.
- **Latency model abstraction**
  (`mm_backtester::latency_model`). Port of
  `hftbacktest::backtest::models::latency`. New
  `LatencyModel` trait with `entry(ts)` (local submission →
  exchange matching-engine acceptance) and `response(ts)`
  (exchange ack → local receipt) methods, plus a
  `ConstantLatency` implementation that supports positive
  latencies and the upstream **negative-latency-equals-
  rejection** sign convention. Convenience constructors
  `from_ms(entry_ms, response_ms)` and
  `symmetric_us(us)`. The separation of entry and response
  matters because real venues have asymmetric transport
  (fast order path, slow ack path) and our current single
  `latency_ms` scalar hides that. Also ships a small
  homegrown extension `BackoffOnTrafficLatency` that scales
  the base latency linearly with recent event rate inside a
  rolling bucket — models the "local queue backs up under
  load" failure mode that public-internet retail MMs hit
  during volatility spikes. 9 tests.
- **BBW (Bollinger Band Width) accessors** on
  `mm_indicators::BollingerValue`. New `width()` returns
  `upper - lower`; new `width_ratio()` returns
  `(upper - lower) / middle` as a normalised volatility-regime
  indicator popularised by John Bollinger ("BandWidth"). Useful
  as a fast vol-regime switch without a second volatility
  estimator. 4 tests including a pinned monotonicity check
  (tighter window → smaller ratio than a wider window with the
  same mean).
- **`bba_imbalance`** in `mm_strategy::features` — best-bid/
  best-ask top-of-book imbalance normalised to `[-1, +1]`. A
  one-level companion to the existing multi-level
  `book_imbalance` / `book_imbalance_weighted`. Reacts on every
  touch update and is the fastest cue for imminent touch
  pressure when deeper levels are thin.
- **`log_price_ratio`** in `mm_strategy::features` — returns
  `100 · ln(base / follow)` as a symmetric, additively-
  composable venue-spread / basis proxy. Rejects non-positive
  inputs. Pinned textbook value `100 · ln(1.01) ≈ 0.995`.
- **`ob_imbalance_multi_depth`** in `mm_strategy::features` —
  aggregates `book_imbalance` evaluated at each of several
  depth horizons with geometric `alpha · (1-alpha)^i` weights.
  Robust against liquidity-distribution changes that distort
  the existing single-depth `book_imbalance_weighted`.
- **`WindowedTradeFlow`** in `mm_strategy::features` — a
  rolling fixed-window snapshot of signed trade flow with
  `log(1 + qty)` weighting. Complements the continuous-EWMA
  `TradeFlow`: the EWMA gives you the slow trend, the window
  gives you the fast snapshot, and the difference between the
  two is itself a flow-acceleration signal. The log-qty weight
  dampens whale prints so one outsized trade doesn't swamp
  the signal.
- **Differential Evolution optimiser** for the hyperopt loop
  (`mm_hyperopt::de::DifferentialEvolution`). Ported from
  `hft-lab-core/src/optimization.rs`. Classic DE/rand/1/bin
  (three-vector mutation + binomial crossover + greedy
  selection) over the existing `SearchSpace` / `LossFn`
  traits, so operators can swap `RandomSearch` for
  `DifferentialEvolution` without touching their backtest
  driver. Deterministic via a seeded `ChaCha8Rng`. Logs every
  evaluation as a `Trial` so downstream JSONL analysis stays
  uniform. 9 tests including a Rosenbrock 2-D benchmark.
- **Market-impact walker**
  (`mm_strategy::features::market_impact`). Walks a book side
  against a target qty, returns VWAP + filled qty + notional
  + signed slippage vs a reference price (positive always
  means unfavourable to the taker). Intended for XEMM hedge-
  leg cost checks, basis-strategy edge calculation, and
  paired-unwind slice urgency. 6 tests for fill inside level,
  spillover into level 2, sell-side sign flip, partial fill
  flag, zero slippage on reference=VWAP, and empty-side
  guard.
- **Lead-lag path transform**
  (`mm_strategy::features::lead_lag_transform`). Converts a
  1-D price series into the 2-D interleaved lead-lag pairs
  from Gyurkó et al. (2013), consumable by downstream
  signature / autocorrelation feature extractors. 3 tests
  including the canonical 3-point example.
- **Hurst exponent via rescaled-range R/S**
  (`mm_strategy::features::hurst_exponent`). Mandelbrot &
  Wallis (1969) R/S estimator on logarithmically-spaced
  window sizes with a 95 % confidence interval derived from
  residuals. Returns `HurstResult { hurst, ci_95,
  is_mean_reverting, window_count }`. Orthogonal to the
  existing velocity-based `RegimeDetector` in
  `autotune.rs` — combine both for stronger regime signals
  for parameter scaling. Pure f64, no deps. 4 tests: short
  series guard, constant-series guard, iid white noise →
  H ≈ 0.5, monotonic trend → H > 0.8.
- **Soft spread gate** on the quote-refresh path. New
  `RiskConfig.max_spread_to_quote_bps: Option<Decimal>` — when
  set, `refresh_quotes` skips quoting for a single tick if the
  current book spread exceeds the threshold, **without**
  tripping the circuit breaker. Covers transient wide-spread
  events (book resync, thin-book volatility blip) where a full
  cancel-all is overkill. The hard `max_spread_bps` circuit
  breaker still catches sustained blowouts at a higher
  threshold. 3 engine tests pin the semantics (None = inert,
  wide-book = blocked without CB trip, tight-book = allowed).
- **Multi-level weighted microprice** in
  `mm-strategy::features::micro_price_weighted(bids, asks, depth)`.
  Averages the per-level microprice formula
  `(bid_px * ask_qty + ask_px * bid_qty) / (bid_qty + ask_qty)`
  across the top `depth` levels with linearly decaying weights
  `w(i) = depth - i`. Robust against thin inside quotes where a
  single dusting order can skew the top-of-book microprice. 6
  tests cover single-level parity with the plain `micro_price`,
  symmetric books, heavy-ask asymmetry, depth clamping, empty
  sides, and skipping zero-qty levels.
- **`EventDeduplicator`** in `mm-backtester::deduplicator` —
  `max_seen` watermark + bounded HashSet with periodic prune
  at 100k entries down to the last 50k. Accepts fresh and
  late-arriving out-of-order sequences; rejects duplicates.
  Used by the backtester replay path so a rotated or
  re-appended JSONL event log cannot double-fire strategy
  callbacks. Same pattern is lined up to guard live WS
  reconnect backlogs on Binance/Bybit. 7 tests cover fresh,
  duplicate, out-of-order, seeded resume, interleaved stream,
  tracked-len, and prune behaviour.

### Notes

- The three spread-gate / weighted-microprice / deduplicator
  additions are cherry-picks from the
  [atomic-mesh](https://github.com/Faraone-Dev/atomic-mesh)
  research repo. The DE optimiser, market-impact walker,
  lead-lag transform, and Hurst exponent are ports from
  [hft-lab-core](https://github.com/ThotDjehuty/hft-lab-core)
  (MIT). BBW accessors, `bba_imbalance`, `log_price_ratio`,
  `ob_imbalance_multi_depth`, and `WindowedTradeFlow` are
  ports from the [beatzxbt/smm](https://github.com/beatzxbt/smm)
  Python Bybit bot. The queue-position fill model and the
  latency model abstraction are ports from
  [nkaz001/hftbacktest](https://github.com/nkaz001/hftbacktest)
  (MIT) — the canonical open-source HFT backtesting library
  for both. All upstream repos are research / educational
  showcases; we deliberately skipped their heavier pieces
  (C++ FFI hot path, QUIC peer mesh, integer-only arithmetic,
  Rough Heston option pricing, agent-based market simulation,
  geometric / topological signals, proprietary mean-reversion
  modules, Python-specific OMS and WS feeds, L3 full-order-
  book queue models, NPZ-backed historical latency
  interpolation, depth fuser for multi-feed merging) because
  they either conflict with the `Decimal`-for-money discipline,
  sit in a latency regime our public-WebSocket transport
  cannot exploit, duplicate our existing Rust infrastructure,
  or are out of our product scope. No new external
  dependencies introduced.

## [0.3.1] - 2026-04-14

Cross-compile fix release. Ships the full [0.3.0] feature set —
the previous tag's `release.yml` workflow failed to produce
`aarch64-unknown-linux-gnu` artifacts because `openssl-sys`
cannot be cross-compiled without a target sysroot. 0.3.1 is
the first release with working artifacts on every target in the
matrix.

### Fixed

- `reqwest` and `tokio-tungstenite` workspace dependencies
  dropped their default `native-tls` feature in favour of
  `rustls-tls-webpki-roots`. This removes `openssl`,
  `openssl-sys`, `openssl-macros`, `openssl-probe`,
  `native-tls`, and `hyper-tls` from the dependency tree
  entirely. `rustls` is pure Rust (ring + webpki), so
  cross-compile to `aarch64-unknown-linux-gnu` no longer needs
  a libssl-dev sysroot on the runner. The code surface is
  unchanged — `reqwest::Client::new()` and
  `tokio_tungstenite::connect_async` keep the same signatures.

## [0.3.0] - 2026-04-14

Multi-venue, multi-product market maker with cross-product basis and
funding-arbitrage capabilities, full fast-path (WebSocket) order entry
on every supported venue, and a unified multi-currency portfolio view
across symbols.

**Highlights**
- Fast-path WS order entry on Binance, Bybit, and HyperLiquid with
  REST fallback; generic id-correlated `WsRpcClient` under the hood.
- FIX 4.4 codec and session engine ready for venues that require it.
- Cross-product trading: basis quoting and funding-rate arbitrage
  between a spot leg and a perp/futures leg on the same or
  different venues, with atomic pair dispatch and automatic
  compensating reversal on partial failures.
- Venue coverage expanded: Binance **spot + USDⓈ-M futures**, Bybit
  V5 **spot / linear / inverse**, HyperLiquid **perps + spot**, plus
  Binance listen-key user-data streams for both products.
- Shared multi-currency `Portfolio` aggregates realised and
  unrealised PnL across all symbols in a single reporting currency,
  published as Prometheus gauges and via a new HTTP endpoint.
- Kill switch L4 automatically picks a paired-unwind executor when
  an instrument pair is configured, flattening both legs in
  lockstep instead of leaving one side exposed.
- Competitor-feature parity on the highest-leverage items from
  Hummingbot / Freqtrade / Nautilus: protections stack, lookahead-
  bias detector, probabilistic fill model, hyperopt loop, indicator
  library, DCA planner, TWAP / VWAP / POV / Iceberg execution
  algorithms, client-side stop / trailing / OCO / GTD emulator, ML
  feature extractors, dedicated XEMM executor.

**Baseline**
- `cargo test --workspace` — **384 passed, 0 failed**
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- 18 crates, 121 files, ~29.7K lines of Rust

### Added

#### Fast-path order entry and protocol layer
- **HyperLiquid DEX connector** (`crates/exchange/hyperliquid`)
  implementing `ExchangeConnector` over HyperLiquid's REST `/info`
  and `/exchange` endpoints. Hand-rolled **EIP-712 signing** over
  secp256k1 (`k256` + `sha3` + `rmp-serde`) instead of a heavyweight
  Ethereum SDK — fully deterministic with test vectors covering
  address derivation, keccak256 spec vectors, signature recovery,
  and mainnet/testnet divergence. msgpack shape tests pin the
  cloid `skip_serializing_if` behaviour.
- **FIX 4.4 codec** (`crates/protocols/fix`) — standalone `Message`
  type with deterministic encode, owned BeginString / BodyLength /
  CheckSum, hand-computed checksum test vector, constructors for
  Logon / Heartbeat / TestRequest / NewOrderSingle / OrderCancelRequest.
- **FIX 4.4 session engine** — pure synchronous state machine
  (`Disconnected → LogonSent → LoggedIn → LogoutSent`) with
  heartbeat watchdog, gap-detection ResendRequest, SequenceReset
  handling, graceful Logout, and a pluggable `SeqNumStore` trait.
  All state transitions tested with synthetic `Instant` values.
- **Generic WebSocket RPC** (`crates/protocols/ws_rpc`) —
  `WsRpcClient` owns a persistent WS connection, correlates
  requests by id, times out pending requests, reconnects with
  configurable backoff, and routes server-initiated pushes to a
  callback. `WireFormat` trait lets venues describe their
  request/response shape in ~30 lines. `spawn_with_url_builder`
  re-derives the connection URL on every reconnect (for URL-signed
  endpoints like Bybit WS Trade). 10 integration tests against a
  mock WS server.
- **Binance `ws_trade`** adapter — per-request HMAC signing,
  canonical-query serialization, routes `order.place` /
  `order.cancel` / `openOrders.cancelAll` over
  `ws-api.binance.com/ws-api/v3`. Integrated into
  `BinanceConnector` with REST fallback.
- **HyperLiquid `ws_post`** adapter — posts signed L1 actions
  (order / cancelByCloid) over a dedicated WS connection,
  reusing the existing `sign_l1_action`. `HyperLiquidConnector`
  tries WS first and falls back to REST on error.
- **Bybit `ws_trade`** scaffold adapter — URL-based auth via
  `spawn_with_url_builder`, wire format and URL signing unit-
  tested. Not yet routed through `BybitConnector::place_order`
  (pending live-testnet auth verification; see Notes below).
- **Capability audit tests** per venue — pin
  `VenueCapabilities::supports_ws_trading` and `supports_fix` to
  actual adapter presence so the "declare but don't wire" failure
  mode cannot recur silently.
- **Order-entry latency histogram** — Prometheus
  `mm_order_entry_duration_seconds` labelled
  `(venue, path, method)` for side-by-side REST vs WS comparison.

#### Spot market making foundations
- **`VenueProduct`** enum in `mm-exchange-core::connector`:
  `Spot / LinearPerp / InversePerp / UsdMarginedFuture /
  CoinMarginedFuture / Option`, with `default_wallet()` and
  `has_funding()` helpers. Every `ExchangeConnector` implementation
  now reports a concrete product via `fn product(&self) -> VenueProduct`.
- **`WalletType`** enum on `Balance`: `Spot / UsdMarginedFutures /
  CoinMarginedFutures / Margin / Funding / Unified`. `BalanceCache`
  rekeyed on `(asset, WalletType)` — fixes a silent bug where
  running spot + futures on the same asset in one engine would
  overwrite one wallet's balance with the other. Regression test
  `wallet_types_do_not_collide` pins the invariant.
- **`BalanceCache::new_for(wallet)`** constructor for explicit
  per-product caches, plus `available_in` / `can_afford_in` /
  `reserve_in` / `release_in` for multi-wallet queries. Legacy
  methods delegate to the configured wallet.
- **Optional `get_funding_rate`** on `ExchangeConnector` with a
  default `Err(FundingRateError::NotSupported)` — spot connectors
  don't need to override. `FundingRate { rate, next_funding_time,
  interval }` struct. `VenueCapabilities.supports_funding_rate`
  flag.

#### Venue coverage expansion
- **Binance USDⓈ-M futures connector** (`BinanceFuturesConnector`
  in `crates/exchange/binance/src/futures.rs`) — sibling to the
  spot connector, reuses `auth.rs`. Mainnet + testnet constructors.
  Wired endpoints: `/fapi/v1/order`, `/fapi/v1/batchOrders` (native
  batch), `/fapi/v1/openOrders`, `/fapi/v1/allOpenOrders`,
  `/fapi/v2/balance`, `/fapi/v1/exchangeInfo`, `/fapi/v1/premiumIndex`
  (funding rate), `/fapi/v1/ping`. Post-only maps to `GTX`. Rate
  limiter sized to the futures bucket (2400 weight/min).
- **Bybit multi-category** — `BybitCategory { Spot, Linear, Inverse }`
  stored on the connector; all hardcoded `"category": "linear"`
  replaced. Per-category constructors `::spot() / ::linear() /
  ::inverse() / ::testnet / ::testnet_spot / ::testnet_inverse`.
  Per-category public WS URL. `with_wallet()` override for classic
  sub-account users. `supports_funding_rate` tracks category.
  Legacy `::new` = `::linear` for backward compat.
- **HyperLiquid spot** — `is_spot: bool` flag on the existing
  `HyperLiquidConnector` (chosen over a separate struct to avoid
  ~400 LoC duplication). Constructors `::spot` / `::testnet_spot`.
  Spot path queries `/info {type: "spotMeta"}`, builds an
  `@N`-indexed asset map with `SPOT_INDEX_OFFSET = 10_000`, uses
  `SPOT_MAX_DECIMALS = 8` for precision, and reads
  `spotClearinghouseState` for balances tagged
  `WalletType::Spot`.
- **Binance listen-key user-data streams** —
  `crates/exchange/binance/src/user_stream.rs`:
  `UserStreamProduct { Spot, UsdMarginedFutures }` picks the right
  REST path (`/api/v3/userDataStream` vs `/fapi/v1/listenKey`), WS
  host, and wallet tag. Owns the listen-key lifecycle: obtain,
  30-minute PUT keepalive, reconnect + re-obtain on expiry. Parses
  `executionReport` / `ORDER_TRADE_UPDATE` into `MarketEvent::Fill`,
  and `outboundAccountPosition` / `ACCOUNT_UPDATE` into a new
  `MarketEvent::BalanceUpdate` variant. Closes the silent bug
  where spot fills from REST-fallback submissions, manual trades,
  and partial-fill follow-ups were invisible to the engine.

#### Cross-product engine
- **`InstrumentPair`** type in `mm-common::types` — describes a
  `{primary_symbol, hedge_symbol, multiplier, funding_interval_secs,
  basis_threshold_bps}` mapping between two venues. Primary is the
  quoting leg, hedge is the reference/execution leg. Used by basis
  trade, funding arb, and any future cross-product strategy.
- **`ConnectorBundle { primary, hedge: Option<_>, pair: Option<_> }`**
  in `crates/engine/src/connector_bundle.rs`. `single(primary)`
  and `dual(primary, hedge, pair)` constructors. Single-connector
  mode is byte-for-byte equivalent to the previous single-connector engine.
- **Dual-connector run loop** — new
  `MarketMakerEngine::run_with_hedge(ws_rx, hedge_rx, shutdown_rx)`
  adds a second `tokio::select!` arm reading from the hedge WS
  channel; `run()` becomes a thin shim that passes `hedge_rx = None`
  so the single-connector branch compiles to the same select as
  before.
- **Second `BookKeeper`** on the engine for the hedge leg, populated
  on snapshot + delta events via `handle_hedge_event`. Its mid is
  exposed to strategies as `StrategyContext.ref_price: Option<Price>`
  (populated in `refresh_quotes`).
- **Second `OrderManager`** (`hedge_order_manager`) for the hedge
  leg. Built lazily when `connectors.hedge` is set; cleaned up on
  shutdown along with the primary OMS. Cross-product unwind slices
  route primary quotes through the primary OMS and hedge quotes
  through the new hedge OMS.
- **`OrderManager::execute_unwind_slice`** — new method places a
  single IOC limit slice and tracks it in `live_orders`, so
  `cancel_all` and fill routing still apply during kill-switch L4
  unwinds.
- **Fill routing** — `handle_hedge_event` gains a `Fill` arm that
  feeds `hedge_order_manager.on_fill`,
  `paired_unwind.on_hedge_fill`, the shared portfolio (keyed on
  the hedge symbol), and the funding-arb driver's position
  bookkeeping. The primary `Fill` arm gets matching
  `paired_unwind.on_primary_fill` and driver wiring.

#### Cross-product strategies
- **`BasisStrategy`** (`crates/strategy/src/basis.rs`) — shifts the
  reservation price toward the hedge mid by a configurable
  `shift ∈ [0, 1]` fraction: `reservation = spot_mid + shift *
  (perp_mid - spot_mid)`. Falls back to plain spot mid when
  `ctx.ref_price` is `None`. A **basis gate**
  (`|basis| > max_basis_bps`) pulls all quotes rather than chase a
  dislocated book. Post-only crossing safety on both legs drops
  any level that would cross the touch.
- **`FundingArbExecutor`** (`crates/strategy/src/funding_arb.rs`)
  — atomic pair dispatcher for `FundingSignal::Enter` / `exit(...)`.
  Stateless, holds only `Arc<dyn ExchangeConnector>` for both legs
  and an `InstrumentPair`. Dispatch order:
  1. Market IOC on the hedge leg (shorter confirmation latency).
  2. If taker rejects → `PairLegError::TakerRejected`, position
     still flat, primary leg never touched.
  3. Post-only limit on the primary leg.
  4. If maker rejects → fire a compensating market IOC in the
     reverse direction on the hedge leg to flatten, return
     `PairLegError::PairBreak { reason, compensated }` where
     `compensated` tracks whether the reversal succeeded.
  Compensation is always a new market order, never a cancel, so
  the audit trail sees a clean reversal. `multiplier` on
  `InstrumentPair` scales only the hedge-leg qty so spot-against-
  contract-perp pairs work without per-call arithmetic.
- **`FundingArbDriver`** (`crates/strategy/src/funding_arb_driver.rs`)
  — owned by the engine. Composes `FundingArbEngine` (decision core
  from `mm-persistence::funding`) and `FundingArbExecutor`. Periodic
  tick (`tick_interval`, default 60 s) samples the hedge venue's
  funding rate + both leg mids, calls `evaluate`, and dispatches the
  resulting `FundingSignal` through the executor. Caches open
  position sides so `Exit` reverses the correct legs.
  `FundingArbEngine::apply_spot_fill` / `apply_perp_fill` and the
  driver's `on_primary_fill` / `on_hedge_fill` / `on_funding_payment`
  / `state()` reconcile the decision core with real `Fill` events,
  replacing the "write once on entry" approach.
- **Driver ↔ engine wiring** — new engine field
  `funding_arb_driver: Option<FundingArbDriver>` and builder
  `with_funding_arb_driver(driver, tick_interval)`. `run_with_hedge`
  adds a gated tick arm that pulls one `DriverEvent` per tick.
  `handle_driver_event` routes events to the audit trail and
  escalates kill switch **L2 `StopNewOrders`** on
  `PairBreak { compensated: false }` (intentionally not L4 — L4
  would start a paired unwind on an already-broken pair,
  compounding the problem). The driver is dropped so it stops
  ticking until the operator restarts the engine.
- **`PairedUnwindExecutor`** (`crates/strategy/src/paired_unwind.rs`)
  — flattens both legs of a basis / funding-arb position in matched
  slices. Each tick emits a `SlicePair { primary, hedge }` with
  sides reversed from the held position. Multiplier scales the
  hedge-leg slice qty. Exposes progress, residual delta, and
  explicit cancellation. Accepts asymmetric slice progress without
  opportunistic rebalancing — the next slice catches up, and a
  hard L5 disconnect is the operator's escalation path.
- **Kill switch L4 branching** — in dual-connector mode L4 picks
  `PairedUnwindExecutor` built from `connectors.pair`; in single-
  connector mode it stays on `TwapExecutor`. The two are mutually
  exclusive so the primary leg is never double-flattened.
- **Audit trail** gains `PairDispatchEntered` / `PairDispatchExited`
  / `PairTakerRejected` / `PairBreak` variants so cross-product
  activity has first-class records without stuffing strings into
  a generic `RiskEvent`.

#### Multi-currency portfolio
- **`mm-portfolio` wired into the engine** — new
  `MarketMakerEngine.portfolio: Option<Arc<Mutex<Portfolio>>>`
  field and `with_portfolio(portfolio)` builder. Every `Fill` is
  fed to the shared portfolio with a **signed** qty so weighted-
  average cost basis correctly flips and closes positions. Every
  tick calls `mark_price(symbol, mid)` so unrealised PnL tracks
  the live mid.
- **`mm-server::main`** builds one
  `Arc<Mutex<Portfolio::new("USDT")>>` before spawning per-symbol
  tasks, so multi-symbol deployments converge on a single unified
  reporting-currency PnL view.
- **Dashboard** — `DashboardState::update_portfolio(snapshot)` /
  `get_portfolio()`, new `portfolio: Option<PortfolioSnapshot>`
  slot, five Prometheus gauges (`mm_portfolio_total_equity`,
  `mm_portfolio_realised_pnl`, `mm_portfolio_unrealised_pnl`,
  `mm_portfolio_asset_qty`, `mm_portfolio_asset_unrealised_reporting`),
  and a new `GET /api/v1/portfolio` HTTP endpoint returning the
  snapshot as JSON (or `null` when the operator has not wired a
  portfolio).

#### Risk and execution (competitor parity)
- **Protections stack** (`crates/risk/src/protections.rs`) —
  stackable per-pair guards so one misbehaving symbol does not
  trip the full-desk kill switch. Four guards: `StoplossGuard` (N
  stops in a window → lockout), `CooldownPeriod` (mandatory pause
  after any stop), `MaxDrawdownPause` (equity-peak mode with
  optional early recovery), `LowProfitPairs` (rolling-window PnL
  demotion). Pure sync state machines tested against synthetic
  `Instant` timelines.
- **Lookahead-bias detector** (`crates/backtester/src/lookahead.rs`)
  — generic `check_lookahead(events, f)` primitive that re-runs
  an indicator on every prefix and compares to the full-stream
  output. Tested against a clean SMA / EWMA baseline and a
  deliberately leaky "global max" indicator.
- **Probabilistic `FillModel` with latency**
  (`crates/backtester/src/fill_model.rs`) — adds fill probability
  on touch, probabilistic slippage, and a latency stamp to the
  backtester. Seeded `ChaCha8Rng` for reproducibility. Three
  presets: `price_cross()` (legacy optimistic), `queue_position(p)`
  (legacy probabilistic), `realistic_crypto()` (sensible default).
- **Hyperopt loop** (`crates/hyperopt/` new crate) — random-search
  over `SearchSpace` (`Uniform / LogUniform / IntUniform / Choice`)
  with five pluggable loss functions (`SharpeLoss`, `SortinoLoss`,
  `CalmarLoss`, `MaxDrawdownLoss`, `MultiMetricLoss`). JSONL trial
  log for notebook analysis.
- **Indicator library** (`crates/indicators/` new crate) — five
  fundamental indicators (`Sma`, `Ema`, `Rsi`, `Atr`,
  `BollingerBands`), all `Decimal`-based, lookahead-safe, uniform
  `new(period)` / `update(sample)` / `value() -> Option<Decimal>`.
- **DCA / position-adjustment planner** (`crates/risk/src/dca.rs`)
  — splits any `(current → target)` delta into scheduled child
  slices with configurable shape: `Flat`, `Linear { slope }`,
  `Accelerated`. Correct reduce-only tagging on long-to-short
  flips via running simulated position. `defaults_for_level`
  produces sensible slice counts per kill-switch stage.
- **`ExecAlgorithm` framework** (`crates/strategy/src/exec_algo.rs`)
  — `tick(ctx) → Vec<ExecAction>` and `on_fill(...)` trait.
  Shipped algorithms: `TwapAlgo`, `VwapAlgo`, `PovAlgo`,
  `IcebergAlgo`. All pure sync state machines (no I/O, no clock).
- **Local order emulator** (`crates/risk/src/order_emulator.rs`)
  — client-side `StopMarket / StopLimit / TrailingStop / OcoLeg /
  GtdCancel` for venues that lack native support (HyperLiquid and
  others). Deterministic and trivially testable.
- **ML feature extractors** (`crates/strategy/src/features.rs`) —
  pure numerical primitives: `book_imbalance` (top-k and linear-
  weighted), `TradeFlow` (signed volume EWMA), `micro_price`,
  `MicroPriceDrift`, `VolTermStructure`. No training loop, no
  PyTorch, no ONNX — just the engineering layer. All lookahead-
  safe.
- **`xemm` dedicated cross-exchange executor**
  (`crates/strategy/src/xemm.rs`) — explicit primary-leg / hedge-
  leg inventory tracking with a slippage band
  (`max_slippage_bps`) and edge guard (`min_edge_bps` →
  `HedgeWithWarning` when the cross is no longer profitable so
  the hedge still fires but the event is flagged).
- **Richer order types** — `TimeInForce` gains `Day` and `Gtd`
  variants. Venue connectors that lack native DAY / GTD fall back
  to `Gtc`.

#### Operator surface
- **Two-way Telegram control**
  (`crates/dashboard/src/telegram_control.rs`) — adds `/status`,
  `/stop`, `/pause SYMBOL`, `/resume SYMBOL`, `/force_exit SYMBOL`
  to the existing outbound-only Telegram alerts. Long-polling
  `getUpdates` with strict chat-id filter and `@BotName`
  normalisation for group-chat use.
- **Strategy selection** — `StrategyType` enum gains `Basis` and
  `FundingArb` variants. `MarketMakerConfig.basis_shift: Decimal`
  knob (default 0.5, validated to `[0, 1]`) tunes how far the
  `BasisStrategy` reservation tracks the hedge mid.
- **Configuration sections** — `AppConfig.hedge: Option<HedgeConfig>`
  (nested `ExchangeConfig` + `HedgePairConfig` serialisable into
  `InstrumentPair`), `AppConfig.funding_arb: Option<FundingArbCfg>`
  (`tick_interval_secs`, `min_rate_annual_pct`, `max_position`,
  `max_basis_bps`, `enabled`). `create_hedge_connector` helper in
  `mm-server` builds the hedge-leg connector from its own
  `ExchangeConfig` (Custom / Binance USDⓈ-M futures / Bybit linear
  / HyperLiquid).
- **`validate_config`** now fails fast at startup on cross-product
  misconfiguration:
  - `StrategyType::Basis` and `StrategyType::FundingArb` require
    a `[hedge]` section.
  - `StrategyType::FundingArb` also requires a `[funding_arb]`
    section with positive `min_rate_annual_pct`, `max_position`,
    `max_basis_bps`, and `tick_interval_secs`.
  - `market_maker.basis_shift` must be in `[0, 1]`.
  - Single-venue strategies with a stray `[hedge]` section warn
    (not error) for operators mid-experimentation.
  - `funding_arb.enabled = false` warns that the driver will tick
    but never dispatch.

#### Documentation
- **`docs/research/spot-mm-specifics.md`** — 15-section canonical
  reference on how spot MM differs from perp MM (fees, settlement,
  wallet topology, listen keys, liquidity profile, …).
- **`docs/protocols/*.md`** — one file per venue protocol
  (Binance WS API, Binance FIX, Bybit WS Trade, Bybit FIX,
  HyperLiquid WS post, OKX WS Trade, Deribit JSON-RPC) plus a
  single-page comparison matrix.
- **`docs/deployment.md`** operator guide covering fast-path
  order entry, capability audit harness, and the remaining
  operator follow-ups.
- **`CLAUDE.md`** updated with the new crate layout and the "one
  abstraction, many adapters" architectural rule.

### Changed

- **Crate layout** — venue adapters moved under
  `crates/exchange/{core,client,binance,bybit,hyperliquid}/`, with
  a new `crates/protocols/` sibling for shared wire / transport
  layers. Package names (`mm-exchange-*`) unchanged; only
  filesystem paths moved.
- **`Balance`** gains a `wallet: WalletType` field. Every
  connector's `get_balances` implementation and every
  `Balance { ... }` literal in tests is updated accordingly.
- **`BalanceCache`** rekeyed on `(asset, WalletType)` (was
  keyed on `asset` alone).
- **`MarketMakerEngine.connector`** → `connectors: ConnectorBundle`
  with a new optional `hedge_book: Option<BookKeeper>` and
  `hedge_order_manager: Option<OrderManager>`. Internal-only
  change; existing callers build a
  `ConnectorBundle::single(connector)` and keep working.
- **`StrategyContext`** gains `ref_price: Option<Price>` — all
  existing call sites (`AvellanedaStoikov`, `GlftStrategy`,
  `CrossExchangeStrategy`, simulator, benches, integration tests)
  updated to pass `None` explicitly.
- **`ExchangeConnector`** trait gains `fn product(&self) ->
  VenueProduct` and an optional `async fn get_funding_rate(...)`
  with a default `Err(NotSupported)` — existing connectors
  implement `product()`, perp connectors override `get_funding_rate`.
- **Bybit connector** now takes a `BybitCategory` parameter;
  legacy `::new(api_key, api_secret)` stays as `::linear` for
  backward compatibility.
- **HyperLiquid connector** gains an `is_spot: bool` flag (stored
  on the struct, surfaced via `product()`).
- **`ExchangeType`** enum gains `HyperLiquid` and `HyperLiquidTestnet`
  variants.
- **`VenueId`** enum gains a `HyperLiquid` variant.
- **`validate_config`** relaxes `rest_url` / `ws_url` requirements
  for HyperLiquid (endpoints are hardcoded in the connector) and
  requires `MM_API_SECRET` to be set to the hex-encoded wallet
  private key when HyperLiquid is selected.
- **`CLAUDE.md` stats** — 18 crates, 121 files, ~29.7K lines Rust,
  374 → 384 tests.

### Notes

- **Bybit WS Trade adapter** ships as a tested wire format with
  URL-based auth, but is not yet routed through
  `BybitConnector::place_order`. The integration is gated on live-
  testnet verification of the exact V5 Trade auth shape. The
  capability audit test keeps `supports_ws_trading = false` on the
  Bybit connector until this is wired.
- **FIX 4.4 session engine** covers the subset of FIX our target
  venues need (Logon, Heartbeat, TestRequest, ResendRequest,
  SequenceReset, Logout, gap detection). It is not tested against
  a FIX conformance suite — conformance validation is deferred
  until the first FIX venue comes online.
- **HyperLiquid `supports_ws_trading`** was originally declared
  `false` during initial implementation; fixed to `true` once
  `HlWsTrader` was wired.

## [0.1.0] - 2026-03-19

### Added

#### Core Engine
- Event-driven market making engine with per-symbol processing
- Order diffing — only cancel/place orders that actually changed
- Balance pre-check with reservation system before order placement
- Order ID mapping (internal UUID ↔ exchange native ID)
- Graceful shutdown (Ctrl+C → cancel all → checkpoint → final reports)

#### Strategies
- **Avellaneda-Stoikov** — optimal market making with inventory skew
- **GLFT** (Guéant-Lehalle-Fernandez-Tapia) — with live order flow calibration
- **Grid** — symmetric quoting around mid price
- **Cross-Exchange** — make on venue A, hedge on venue B
- **TWAP** — time-weighted execution for inventory unwinding
- **Momentum Alpha** — book imbalance + trade flow + micro-price (Cartea-Jaimungal)
- **EWMA Volatility** — exponentially weighted realized vol estimator

#### Risk Management
- 5-level kill switch (widen → stop → cancel → flatten via TWAP → disconnect)
- Circuit breaker (stale book, wide spread, max drawdown, max exposure)
- VPIN toxicity detection (Volume-Synchronized Probability of Informed Trading)
- Kyle's Lambda price impact estimation
- Adverse selection tracker (post-fill price movement analysis)
- Advanced inventory management (quadratic skew, dynamic sizing, urgency unwinding)
- Order + balance reconciliation vs exchange state (every 60s)
- Performance metrics (Sharpe, Sortino, max drawdown, fill rate, win rate, profit factor)

#### Auto-Tuning
- Market regime detection (Quiet / Trending / Volatile / Mean-Reverting)
- Toxicity-based parameter adjustment (VPIN → automatic spread widening)
- Per-regime gamma/size/spread multipliers

#### Exchange Connectivity
- Custom exchange connector (REST + WebSocket)
- Binance connector (spot + futures, HMAC-SHA256, combined WS streams)
- Bybit V5 connector (batch orders up to 20, amend support)
- `ExchangeConnector` trait for adding new exchanges
- Unified order book (aggregate liquidity across venues)
- Smart order router (route by effective price including fees)
- Token-bucket rate limiter per venue with safety buffer
- HTTP 429 exponential backoff with Retry-After header support

#### Compliance & Audit
- Append-only JSONL audit trail (MiCA compliant, 5-year retention ready)
- SLA compliance tracking (uptime, spread, depth, two-sided quoting)
- PnL attribution (spread capture / inventory / rebates / fees breakdown)
- Config validation at startup (ranges, required fields, logical checks)

#### Dashboard & Monitoring
- HTTP dashboard with REST API + WebSocket real-time updates
- 27 Prometheus metrics (PnL, inventory, spread, VPIN, kill level, regime, SLA)
- Telegram bot alerts (3 severity levels with dedup)
- Role-based authentication (Admin / Operator / Viewer)
- User management API (create/list users)

#### Client Portal
- Executive overview (spread compliance, uptime, depth, volume per symbol)
- Spread quality report (TWAS, VWAS, high-vol vs normal, compliance %)
- Depth report at multiple levels (0.5%, 1%, 2%, 5% from mid)
- Volume report by exchange with maker/taker split
- Token position tracking (where are loaned tokens, per exchange)
- Loan/option status (strike, expiry, ITM check)
- Daily client report (aggregated JSON)
- Login page with API key authentication

#### Web UI (Svelte Frontend)
- Dark theme professional dashboard (9 panels, 3-column grid)
- PnL chart (TradingView Lightweight Charts, real-time)
- Spread chart (bps over time)
- Order book visualization (top 10 levels with depth bars)
- Inventory & signals panel (VPIN, Kyle's λ, adverse selection, volatility)
- Controls panel with kill switch buttons (Admin/Operator only)
- Open orders table
- Fill history table
- Alert log stream
- Role-based UI (Viewer sees PnL attribution + SLA instead of controls/alerts)
- Login screen with API key input

#### Backtesting & Paper Trading
- Event-driven backtester with strategy replay
- Two fill models: PriceCross (optimistic) and QueuePosition (probabilistic)
- Paper trading mode (`mode = "paper"` in config)
- JSONL event recorder/loader for data capture and replay
- Backtest report with full PnL attribution

#### Persistence
- Atomic JSON checkpoint manager with auto-flush
- Crash recovery (load checkpoint → reconcile with exchange)
- Funding rate arbitrage engine (long spot + short perp)

#### Operations
- Docker multi-stage build (non-root, healthcheck)
- docker-compose with Prometheus + Grafana
- GitHub Actions CI (check, test, clippy, fmt, Docker build)
- File logging with daily rotation (stdout + JSON file)
- Secrets from environment variables (never in config files)
- Strategy benchmarks (criterion.rs)
- MIT license
- Full documentation (README, CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, docs/)
