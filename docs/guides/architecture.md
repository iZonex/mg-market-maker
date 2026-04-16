# Architecture Guide

## System Overview

```
┌─────────────┐     ┌──────────────┐     ┌────────────────┐
│   Config     │────▶│    Server     │────▶│  Engine (×N)   │
│  (TOML+env)  │     │  (main.rs)   │     │ (per symbol)   │
└─────────────┘     └──────┬───────┘     └───────┬────────┘
                           │                      │
                    ┌──────▼───────┐      ┌───────▼────────┐
                    │  Dashboard   │      │  Exchange      │
                    │  (HTTP API)  │      │  Connectors    │
                    └──────────────┘      └────────────────┘
```

## Request Flow (one tick cycle)

```
1. WS Event arrives (BookSnapshot/Trade)
   │
2. BookKeeper updates local orderbook
   │
3. Engine refresh timer fires (500ms default)
   │
4. Pre-checks:
   ├── Kill switch allows?
   ├── Circuit breaker clear?
   ├── Lifecycle not paused?
   └── Book not stale?
   │
5. Strategy.compute_quotes(context) → Vec<QuotePair>
   │
6. Risk overlay:
   ├── Inventory skew adjustment
   ├── AutoTuner regime multipliers
   ├── A/B split multipliers (if active)
   ├── Portfolio risk multiplier
   ├── VaR throttle
   └── Balance pre-check
   │
7. OrderManager.execute_diff(old_orders, new_quotes)
   ├── Cancel removed levels
   ├── Amend shifted levels (if venue supports)
   └── Place new levels
   │
8. Fill arrives → PnlTracker → InventoryManager → Audit
```

## Crate Dependency Graph

```
server ─┬─▶ engine ─┬─▶ strategy
        │           ├─▶ exchange/{binance,bybit,hyperliquid,client}
        │           ├─▶ risk
        │           ├─▶ dashboard
        │           ├─▶ portfolio
        │           ├─▶ persistence
        │           └─▶ backtester (event recorder)
        │
        ├─▶ dashboard ─┬─▶ risk
        │              ├─▶ portfolio
        │              └─▶ persistence
        │
        └─▶ common (types, config, orderbook)
```

## Key Abstractions

### ExchangeConnector (trait)

One implementation per venue. Handles:
- WS subscription (book + trades)
- Order placement / cancel / amend / batch
- Balance queries
- Product spec (tick/lot/fees)
- Health check
- Rate limiting

Implementations: `CustomConnector`, `BinanceConnector`, `BinanceFuturesConnector`, `BybitConnector`, `HyperLiquidConnector`

### Strategy (trait)

One method: `compute_quotes(ctx) → Vec<QuotePair>`. Everything else is handled by the engine.

### DashboardState (shared state)

`Arc<RwLock<StateInner>>` — thread-safe shared state between engines and HTTP handlers. Partitioned by client (Epic 1).

Contains:
- Per-client symbol states (prices, PnL, SLA)
- Fill history (per-client ring buffer)
- Webhook dispatchers (per-client)
- Config override channels (per-symbol)
- Portfolio risk summary
- Correlation matrix
- Loan agreements
- Optimization state

### Kill Switch (5 levels)

| Level | Action | Trigger |
|-------|--------|---------|
| 0 | Normal | — |
| 1 | Widen Spreads | Daily loss warning |
| 2 | Stop New Orders | Position limit hit |
| 3 | Cancel All | Daily loss limit |
| 4 | Flatten All | Manual or extreme |
| 5 | Disconnect | Manual only |

Escalation is automatic; de-escalation requires manual reset.

## Data Flow

### Market Data
```
Exchange WS → mpsc::channel → Engine.handle_ws_event()
  ├── BookKeeper (L2 orderbook)
  ├── VPIN estimator
  ├── Kyle's Lambda
  ├── MomentumSignals (book imbalance, trade flow, OFI, HMA)
  ├── MarketResilience detector
  ├── VolatilityEstimator (EWMA)
  ├── EventRecorder (if recording enabled)
  └── FactorCovarianceEstimator (shared, for portfolio VaR)
```

### Order Flow
```
Strategy.compute_quotes()
  → InventorySkew.adjust()
  → BalanceCache.pre_check()
  → OrderManager.execute_diff()
    ├── Amend (if same side, within tick budget)
    ├── Cancel (removed levels)
    └── Place (new levels, PostOnly)
  → Connector.place_order() / cancel_order()
  → Fill callback → PnlTracker, AuditLog, DashboardState
```

### Persistence
```
data/
├── audit/{symbol}.jsonl      # MiCA compliance audit trail
├── fills.jsonl               # fill history (dashboard)
├── checkpoint.json           # engine state (inventory, PnL)
├── recorded/{symbol}.jsonl   # market data for backtesting
├── loans.jsonl               # loan agreements
└── transfers.jsonl           # cross-venue transfer log
```

## Configuration Hierarchy

```
1. config/default.toml        # base config
2. MM_CONFIG env var          # override path
3. MM_* env vars              # secrets (API_KEY, API_SECRET, etc.)
4. Hot-reload via admin API   # runtime overrides (gamma, spread, etc.)
```

## Multi-Client Architecture (Epic 1)

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
- Isolated symbol state + fill history
- Separate webhook dispatcher
- Scoped API authentication
- Per-client SLA certificates
- Per-client PnL attribution

When `clients` is empty → legacy mode (single "default" client).
