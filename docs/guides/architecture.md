# Architecture Guide

## System Overview

MG Market Maker is a distributed, multi-tenant, multi-venue market-making platform. Three process tiers + the exchange adapters:

```
┌──────────────────┐
│   Controller     │  Single process (or HA pair).
│                  │  - Approvals, leases, fleet view
│  (HA optional)   │  - Audit, vault, config broadcast
│                  │  - MiCA report aggregation
└────────┬─────────┘  - Frontend static serve
         │ Ed25519-signed envelopes over TLS WS
         ▼
  ╔══════╩═══════╗
  ║   Agent N    ║   One per host / availability zone.
  ║  (lease ▲▼)  ║   - Holds a leader lease
  ║              ║   - Spawns engine tasks per deployment
  ║              ║   - Serves details-topic fetches
  ╚══════╦═══════╝
         │
         ▼
  ┌──────┴───────┐
  │  Engine (×N) │   One per (client, symbol, venue-role).
  │              │   - Book keeper, strategy, risk, orders
  │              │   - Publishes to shared DataBus
  │              │   - Owns its audit + PnL + SLA tracker
  └──────┬───────┘
         │
         ▼
  ┌──────┴───────┐
  │  Connectors  │   One per venue × product.
  │              │   - WS / FIX / REST transport
  │              │   - Auth + rate limiting
  │              │   - Capability flags
  └──────────────┘
```

Flow:
- Controller authorizes agents via signed lease + identity-key envelope
- Agent spawns an engine task per `(client, symbol, venue)` deployment
- Each engine owns its connectors, runs the tick loop, publishes state to the shared `DashboardState` and the process-wide `DataBus`
- Strategy graphs (when attached) read from DataBus for cross-venue signals
- Dashboard HTTP + WS frontend reads from DashboardState + the controller's fleet view

---

## Request Flow (one tick cycle)

```
1. WS event (BookSnapshot / BookDelta / Trade) arrives
   │
2. BookKeeper updates local L2 orderbook
   │
3. DataBus.publish_l1(key, snap)  ← other engines + graphs see this
   │
4. Engine refresh timer fires (500ms default, configurable)
   │
5. Pre-checks:
   ├── Kill switch level allows quoting?
   ├── Circuit breaker clear (no stale book, no venue stress)?
   ├── Pair lifecycle not halted / delisting?
   └── Balance pre-check (reservation)
   │
6. Signal update pass:
   ├── Volatility EWMA
   ├── VPIN (toxicity)
   ├── Kyle's lambda
   ├── Cont-Kukanov-Stoikov OFI
   ├── Learned microprice drift (Stoikov 2018)
   ├── Adverse selection tracker
   ├── Market resilience
   └── HMA (Hull moving average)
   │
7. Auto-tune multiplier stack:
   ├── Regime classifier (Quiet / Trending / Volatile / MeanReverting)
   ├── Toxicity widen mult
   ├── Market Resilience penalty
   ├── Lead-lag guard mult
   ├── News-retreat mult
   └── Social sentiment mult
   │
8. Strategy graph tick (if attached):
   ├── Harvest source values into source_inputs: HashMap<(NodeId, port), Value>
   ├── Evaluator.tick(ctx, source_inputs) → Vec<SinkAction>
   ├── Apply sink actions to autotuner / kill switch
   └── Optional: graph-authored quote bundle overrides strategy.compute_quotes
   │
9. strategy.compute_quotes(context) → Vec<QuotePair>
   │
10. Risk overlay (on quotes):
    ├── Inventory skew
    ├── Size multipliers from autotuner
    ├── Portfolio risk ratio
    ├── VaR throttle (per-strategy-class)
    └── Capital budget gate
    │
11. OrderManager.execute_diff(live_orders, new_quotes)
    ├── Amend (same side, price shift within tick budget — P1.1)
    ├── Cancel removed levels
    ├── Batch entry (if venue supports — Epic E)
    └── Place PostOnly new levels
    │
12. Fill arrives → PnlTracker + InventoryManager + Audit + Surveillance
```

---

## Crate Dependency Graph

```
server ─┬─▶ engine ─┬─▶ strategy
        │           ├─▶ strategy-graph (evaluator + node catalog)
        │           ├─▶ exchange/{core,binance,bybit,hyperliquid,client}
        │           ├─▶ protocols/{ws_rpc,fix}
        │           ├─▶ risk
        │           ├─▶ indicators
        │           ├─▶ portfolio
        │           ├─▶ persistence
        │           ├─▶ dashboard (publish DashboardState)
        │           ├─▶ sentiment
        │           └─▶ backtester (event recorder for paper + recording modes)
        │
        ├─▶ controller ─┬─▶ control (message types, lease, envelope)
        │               ├─▶ dashboard
        │               └─▶ vault (credentials)
        │
        ├─▶ agent ─┬─▶ control
        │          ├─▶ engine (spawns engines on deploy)
        │          ├─▶ dashboard (details_store + metrics)
        │          └─▶ strategy-graph
        │
        ├─▶ dashboard ─┬─▶ risk (AuditEventType, SLA, surveillance)
        │              ├─▶ portfolio
        │              └─▶ persistence
        │
        ├─▶ hyperopt (offline strategy calibration)
        └─▶ common (types, config, orderbook, classify_symbol)
```

25 crates total. One abstraction per shared pattern — see `protocols/` for the FIX + WS-RPC shared transports.

---

## Key abstractions

### ExchangeConnector (trait)

One implementation per venue × product. Handles:
- WS subscription (book + trades + own fills)
- Order placement / cancel / amend / batch
- Balance + position queries
- Product spec (tick / lot / fees)
- Health check
- Rate limiting (429 backoff)
- `VenueCapabilities` flags — advertises `supports_ws_trading`, `supports_fix`, `supports_amend`, `supports_batch` (only set when wired; CI enforces)

Implementations: `CustomConnector`, `BinanceConnector` (spot), `BinanceFuturesConnector`, `BybitConnector`, `HyperLiquidConnector`.

### Strategy (trait)

Method: `compute_quotes(ctx: &StrategyContext) -> Vec<QuotePair>`. Everything else (inventory, risk, order placement) handled by the engine. `StrategyContext` carries book, mid, inventory, time-to-close, regime, toxicity signals, etc.

Strategies implemented as this trait: `AvellanedaStoikov`, `GLFT`, `Grid`, `Basis`, `CrossExchange`, `Xemm`, plus pentest variants (Spoof, Layer, Wash, Ignite, Mark, Stuff, pump-and-dump orchestrator).

### Strategy graph (parallel policy layer)

`mm_strategy_graph::Evaluator` evaluates a DAG of typed nodes. Engines can attach a graph via `swap_strategy_graph()`; the tick loop then:
1. Populates `source_inputs` from engine state (book, risk, portfolio, surveillance, etc.)
2. Calls `Evaluator::tick(ctx, source_inputs)` → `Vec<SinkAction>`
3. Applies actions to autotuner multipliers / kill switch / quote-bundle override

Graph nodes are declared in `crates/strategy-graph/src/nodes/`. New nodes go through the catalog — see [graph-authoring.md](graph-authoring.md).

### DataBus (multi-venue signal hub)

Process-wide `Arc<RwLock<...>>` keyed on `(venue, symbol, product)`. Every engine publishes its primary + hedge + SOR-extra L1 (and L2, trades, funding, balances) to the bus. Cross-venue strategies read back:

```rust
pub struct DataBus {
    pub books_l1: Arc<RwLock<HashMap<StreamKey, BookL1Snapshot>>>,
    pub books_l2: Arc<RwLock<HashMap<StreamKey, BookL2Snapshot>>>,
    pub trades: Arc<RwLock<HashMap<StreamKey, VecDeque<TradeTick>>>>,
    pub funding: Arc<RwLock<HashMap<StreamKey, FundingRate>>>,
    pub balances: Arc<RwLock<HashMap<(String, String), BalanceEntry>>>,
    pub venue_regimes: Arc<RwLock<HashMap<StreamKey, VenueRegimeSnapshot>>>,
}
```

Graph source nodes with `venue` / `symbol` / `product` config read from the bus. Empty config = "this engine's own venue".

### DashboardState (shared UI state)

`Arc<RwLock<StateInner>>` — per-client partitioned, updated every tick by engines, read by HTTP + WS handlers.

Contains:
- Per-symbol state (mid, spread, PnL attribution, inventory, kill level, SLA, regime)
- Fill history (per-client ring buffer)
- Webhook dispatchers (per-client)
- Alert rules + history
- Portfolio risk summary + correlation matrix
- Loan agreements, incidents, surveillance roster

### Agent / Controller / Lease

The distributed layer (Apr 2026 reshape):
- **LeaderLease** — controller grants an agent the right to run a deployment. Lease expires; agent refreshes at 1/3 lifetime. Miss → walks the fail-ladder.
- **SignedEnvelope** — every control-plane message carries an Ed25519 signature over the inner envelope. Agents reject unsigned / mismatched.
- **FetchDeploymentDetails** — RPC-style command from controller to agent: "give me details-topic `graph_trace_recent` for symbol X". Agent responds with the matching payload from its per-process ring buffer (`details_store`).
- **Telemetry cursor** — agent emits telemetry (state, deployment heartbeat) on a sequence cursor; controller uses it to deduplicate and order.

### Kill Switch (5 levels)

| Level | Action | Trigger |
|-------|--------|---------|
| 0 | Normal | — |
| 1 | Widen Spreads | VPIN threshold, Market Resilience < 0.3 for 3s+ |
| 2 | Stop New Orders | Drawdown, VaR, news-retreat Critical |
| 3 | Cancel All | Hard inventory / exposure breach |
| 4 | Flatten All | Uncompensated pair-break, disaster drawdown |
| 5 | Disconnect | Manual reset required |

Escalation automatic, de-escalation manual above L2.

---

## Data flow

### Market data
```
Exchange WS → mpsc::channel → Engine.handle_ws_event()
  ├── BookKeeper (L2 orderbook)
  ├── DataBus.publish_l1/publish_l2 (+ venue regime)
  ├── VPIN estimator
  ├── Kyle's Lambda
  ├── Cont-Kukanov-Stoikov OFI
  ├── MomentumSignals (imbalance, trade flow, OFI, HMA, micro-price)
  ├── MarketResilience detector
  ├── VolatilityEstimator (EWMA)
  ├── EventRecorder (if recording enabled)
  └── FactorCovarianceEstimator (shared, portfolio VaR)
```

### Order flow
```
strategy.compute_quotes()
  → InventorySkew.adjust()
  → AutoTuner multiplier stack
  → Strategy graph overlay (if attached)
  → BalanceCache.pre_check()
  → OrderManager.execute_diff()
    ├── Amend (same side, within tick budget — P1.1)
    ├── Cancel (removed levels)
    ├── Batch place (venue-supporting — Epic E)
    └── Place (PostOnly)
  → Connector.place_order / cancel_order / amend
  → Fill callback → PnlTracker, InventoryManager, AuditLog, DashboardState
```

### Persistence
```
data/
├── audit/{symbol}.jsonl          # MiCA SHA-256-chained audit trail
├── fills.jsonl                   # fill history (dashboard ring)
├── checkpoint.json               # engine state (inventory, PnL, autotuner) — atomic write
├── recorded/{symbol}.jsonl       # market events (for backtest replay)
├── loans.jsonl                   # loan agreements lifecycle
├── transfers.jsonl               # cross-venue transfer log (S6.4)
├── user_templates/{name}/        # saved strategy graphs
│   ├── <hash>.json               # content-addressed graph bodies
│   └── history.jsonl             # version chain
└── archive/                      # long-term audit archive (S3 or local)
```

---

## Configuration hierarchy

```
1. config/default.toml            # base config shipped with repo
2. MM_CONFIG env var              # override path
3. MM_* env vars                  # secrets (AUTH_SECRET, VAULT_KEY, ...)
4. ConfigOverride control msg     # runtime hot-reload via admin API
5. AdaptiveTuner proposals        # calibration engine proposes, operator approves
```

Precedence: later overrides earlier. Safe-to-hot-reload fields are marked in the TOML schema; unsafe fields require restart.

---

## Multi-client architecture

```toml
[[clients]]
id = "acme"
name = "Acme Capital"
symbols = ["BTCUSDT", "ETHUSDT"]
webhook_urls = ["https://acme.com/hook"]
api_keys = ["acme-viewer-key"]

[[clients]]
id = "beta"
name = "Beta Fund"
symbols = ["SOLUSDT"]
```

Each client gets:
- Isolated symbol state + fill history (DashboardState partitioning)
- Separate webhook dispatcher
- Scoped JWT tokens (`TokenClaims.client_id`)
- Per-client SLA certificates (HMAC-signed)
- Per-client PnL attribution in the client API
- Per-client vault entries (`VaultEntry.client_id`)

Empty `[[clients]]` = legacy single-tenant "default" client mode.

---

## Portfolio + risk composition

Portfolio-level risk runs as a shared object:
- `PortfolioRiskManager` — factor-model factor limits + global delta guard
- `PortfolioVarGuard` — parametric Gaussian VaR per strategy class (Epic C)
- `FactorCovarianceEstimator` — shared across engines, fed per-tick factor snapshots
- `HedgeOptimizer` — Markowitz mean-variance cross-asset hedge recommender

All engines publish their attribution + factor delta; the portfolio object aggregates and can throttle any single engine when the portfolio-wide limit is approached.

---

## Crash recovery

1. Engine starts with `checkpoint_restore: true`
2. Read `data/checkpoint.json` — restore inventory, PnL, autotuner state, last known mid
3. Reconcile against venue (orders + balances)
4. Replay tail of `audit/{symbol}.jsonl` for fills after the checkpoint timestamp
5. Cancel any orphaned orders (in venue, not in local state)
6. Resume tick loop

See [crash-recovery.md](crash-recovery.md) for the full flow.

---

## Pointers to deeper docs

- **[Strategy Catalog](strategy-catalog.md)** — every strategy + signal + modulator, formulas, params
- **[Graph Authoring](graph-authoring.md)** — add a node, compose a graph, deploy + replay
- **[Security Model](security-model.md)** — auth, vault, HMAC, MiCA audit
- **[Metrics Glossary](metrics-glossary.md)** — every `mm_*` Prometheus gauge
- **[Adding Exchanges](adding-exchange.md)** — 8-step connector walkthrough
- **[Multi-Venue Architecture (research)](../research/multi-venue-architecture.md)** — deeper DataBus + SOR design notes
