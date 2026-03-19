# Market Maker

**Production-grade algorithmic market maker written in Rust.**

High-performance, multi-venue market making engine with institutional-grade risk management, toxicity detection, and compliance features. Built for speed — Rust from the ground up, `Decimal` arithmetic everywhere (never `f64` for money).

## Features

### Strategies
- **Avellaneda-Stoikov** — optimal bid/ask with inventory skew
- **GLFT** (Guéant-Lehalle-Fernandez-Tapia) — with live order flow calibration
- **Grid** — symmetric grid quoting around mid
- **Cross-Exchange** — make on venue A, hedge on venue B
- **TWAP** — time-weighted execution for inventory unwinding
- **Momentum Alpha** — book imbalance + trade flow + micro-price shifts reservation price

### Risk Management
- **5-Level Kill Switch** — widen → stop → cancel → flatten (TWAP) → disconnect
- **Circuit Breaker** — stale book, wide spread, max drawdown, max exposure
- **VPIN** — Volume-Synchronized Probability of Informed Trading (toxicity)
- **Kyle's Lambda** — price impact estimation
- **Adverse Selection Tracker** — monitors post-fill price movement
- **Inventory Limits** — quadratic skew + dynamic sizing + urgency unwinding
- **Balance Pre-Check** — reservation system prevents over-submitting orders

### Auto-Tuning
- **Regime Detection** — Quiet / Trending / Volatile / Mean-Reverting
- **Toxicity-Based Adjustment** — VPIN automatically widens spreads
- **Per-Regime Parameters** — gamma, size, spread multipliers adapt to market state

### Exchange Connectivity
- **Custom Exchange** — full REST + WebSocket connector
- **Binance** — spot + futures, HMAC auth, combined WS streams
- **Bybit** — V5 API, batch orders (20), amend support
- **Exchange Connector Trait** — add new exchanges by implementing one trait
- **Unified Order Book** — aggregate liquidity across venues
- **Smart Order Router** — routes by effective price (including fees)
- **Rate Limiter** — token-bucket with safety buffer per venue
- **429 Retry** — exponential backoff with Retry-After header support

### Compliance & Audit
- **Audit Trail** — append-only JSONL log of all actions (MiCA compliant, 5-year retention)
- **SLA Tracking** — uptime, spread, depth, two-sided quoting obligations
- **Order + Balance Reconciliation** — periodic comparison vs exchange state
- **PnL Attribution** — spread capture / inventory / rebates / fees breakdown

### Monitoring & Alerting
- **HTTP Dashboard** — `/api/status`, `/api/v1/positions`, `/api/v1/pnl`, `/api/v1/sla`, `/api/v1/report/daily`
- **Prometheus Metrics** — 27 gauges/counters (PnL, inventory, spread, VPIN, kill level, regime, etc.)
- **Telegram Alerts** — 3-level severity with dedup (Info / Warning / Critical)
- **Grafana** — pre-configured via docker-compose

### Backtesting & Paper Trading
- **Backtester** — replay recorded events through strategies with simulated fills
- **Fill Models** — PriceCross (optimistic) or QueuePosition (probabilistic)
- **Paper Trading** — live market data, simulated fills, no real orders
- **Event Recorder** — record live data to JSONL for later replay

### Performance Metrics
- Sharpe ratio, Sortino ratio, Max drawdown
- Fill rate, Inventory turnover, Win rate
- Spread capture efficiency (bps), Profit factor

### Operations
- **Docker + Compose** — one command deployment with Prometheus + Grafana
- **GitHub Actions CI** — check, test, clippy, fmt, Docker build
- **Graceful Shutdown** — Ctrl+C → cancel all orders → checkpoint flush → final reports
- **File Logging** — stdout + daily-rotated JSON file
- **Config Validation** — all parameter ranges checked at startup
- **Secrets from ENV** — `MM_API_KEY`, `MM_API_SECRET` (never in config files)
- **Checkpoint Recovery** — atomic JSON checkpoint for crash recovery

## Quick Start

### Paper Trading (recommended first)

```bash
# Clone
git clone https://github.com/your-org/market-maker.git
cd market-maker

# Run in paper mode (no real orders)
MM_MODE=paper cargo run -p mm-server

# Or with Docker
MM_MODE=paper docker compose up
```

### Live Trading

```bash
# Set secrets
export MM_API_KEY=your-key
export MM_API_SECRET=your-secret
export MM_MODE=live

# Run
cargo run -p mm-server --release
```

### Docker (full stack with monitoring)

```bash
docker compose up -d

# Dashboard: http://localhost:9090/api/status
# Prometheus: http://localhost:9091
# Grafana: http://localhost:3000 (admin/admin)
```

## Configuration

Edit `config/default.toml` or set `MM_CONFIG` to a custom path.

```toml
symbols = ["BTCUSDT"]
mode = "paper"                      # "live" or "paper"
dashboard_port = 9090

[market_maker]
strategy = "avellaneda_stoikov"     # "glft", "grid", "avellaneda_stoikov"
gamma = "0.1"                       # risk aversion
order_size = "0.001"                # base asset per order
num_levels = 3                      # quote levels per side
momentum_enabled = true

[risk]
max_inventory = "0.1"
max_drawdown_quote = "500"

[kill_switch]
daily_loss_limit = "1000"

[sla]
max_spread_bps = "100"
min_uptime_pct = "95"

[toxicity]
autotune_enabled = true
```

See [config/default.toml](config/default.toml) for all options.

## Architecture

```
                    ┌─────────────┐
                    │  Dashboard  │ HTTP + Prometheus + Telegram
                    │  :9090      │
                    └──────┬──────┘
                           │ state updates
┌──────────┐     ┌─────────┴──────────┐     ┌──────────────┐
│ Exchange │◄────┤    Engine (per      │────►│  Audit Trail │
│ REST/WS  │     │    symbol)          │     │  (JSONL)     │
└──────────┘     │                     │     └──────────────┘
     ▲           │  Book Keeper        │
     │           │  ↓                  │     ┌──────────────┐
     │           │  Strategy (A-S/     │────►│  Checkpoint   │
     │           │   GLFT/Grid)        │     │  (JSON)      │
     │           │  ↓                  │     └──────────────┘
     │           │  Risk Engine        │
     │           │  (Kill Switch,      │
     │           │   VPIN, SLA, PnL)   │
     │           │  ↓                  │
     │           │  Order Manager      │
     └───────────┤  (diff + balance    │
   place/cancel  │   pre-check)        │
                 └────────────────────┘
```

## Strategies

| Strategy | Best For | Key Feature |
|----------|----------|-------------|
| **Avellaneda-Stoikov** | General MM | Optimal spread with inventory skew |
| **GLFT** | Calibrated MM | Live order flow intensity fitting |
| **Grid** | Simple MM | Symmetric levels around mid |
| **Cross-Exchange** | Multi-venue | Make on A, hedge on B |

## Benchmarks

```bash
cargo bench -p mm-strategy
```

Typical results (Apple M-series):
- Avellaneda-Stoikov (5 levels): ~2μs
- GLFT (5 levels): ~5μs
- Grid (5 levels): ~1μs
- Orderbook delta update: ~200ns

## Adding an Exchange

Implement the `ExchangeConnector` trait in `crates/exchange-core/src/connector.rs`:

```rust
#[async_trait]
impl ExchangeConnector for MyExchange {
    fn venue_id(&self) -> VenueId { ... }
    async fn place_order(&self, order: &NewOrder) -> Result<OrderId> { ... }
    async fn cancel_order(&self, symbol: &str, id: OrderId) -> Result<()> { ... }
    async fn subscribe(&self, symbols: &[String]) -> Result<Receiver<MarketEvent>> { ... }
    // ... see trait for full interface
}
```

## Project Structure

```
crates/
├── common/           Shared types (Decimal, Config, OrderBook)
├── exchange-core/    ExchangeConnector trait, unified book, SOR
├── exchange-client/  Custom exchange connector + retry
├── exchange-binance/ Binance connector (HMAC, batch, WS)
├── exchange-bybit/   Bybit V5 connector (batch 20)
├── strategy/         A-S, GLFT, Grid, CrossExchange, TWAP, Momentum, AutoTune
├── risk/             Kill switch, VPIN, Kyle, SLA, PnL, Audit, Reconciliation, Performance
├── engine/           Main event loop, order manager, balance cache, ID map
├── backtester/       Simulator, paper trading, event recorder
├── dashboard/        HTTP API, Prometheus metrics, Telegram alerts, client API
├── persistence/      Checkpoint recovery, funding rate arbitrage
└── server/           Binary entry point, config validation, logging
```

## Testing

```bash
cargo test              # 55+ tests
cargo clippy            # zero warnings
cargo fmt -- --check    # formatting
cargo bench             # strategy benchmarks
```

## License

MIT — see [LICENSE](LICENSE).

## Contributing

Contributions welcome! Please:
1. Fork the repo
2. Create a feature branch
3. Add tests for new functionality
4. Ensure `cargo clippy` passes with zero warnings
5. Open a PR

## Unique Advantages Over Alternatives

| Feature | market-maker | Hummingbot | Freqtrade | NautilusTrader |
|---------|:---:|:---:|:---:|:---:|
| **Language** | Rust | Python | Python | Rust+Python |
| **GLFT Strategy** | Yes | No | No | No |
| **VPIN Toxicity** | Yes | No | No | No |
| **5-Level Kill Switch** | Yes | No | No | No |
| **MiCA Audit Trail** | Yes | No | No | No |
| **SLA Compliance** | Yes | No | No | No |
| **Regime Auto-Tune** | Yes | No | No | No |
| **PnL Attribution** | Yes | No | No | No |
| **Balance Pre-Check** | Yes | No | No | No |
| **Reconciliation** | Yes | No | No | No |
