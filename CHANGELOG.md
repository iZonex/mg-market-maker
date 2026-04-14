# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-04-14

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
  mode is byte-for-byte equivalent to the pre-0.2.0 engine.
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
