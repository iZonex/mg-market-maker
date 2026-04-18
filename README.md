<p align="center">
  <h1 align="center">MG Market Maker</h1>
  <p align="center">
    <strong>Production-grade algorithmic market making engine written in Rust</strong>
  </p>
  <p align="center">
    <a href="#quick-start">Quick Start</a> &bull;
    <a href="docs/guides/quickstart.md">Full Guide</a> &bull;
    <a href="docs/guides/writing-strategies.md">Write a Strategy</a> &bull;
    <a href="docs/guides/configuration-reference.md">Config Reference</a> &bull;
    <a href="docs/guides/architecture.md">Architecture</a>
  </p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/tests-1431_passing-brightgreen" alt="Tests">
  <img src="https://img.shields.io/badge/clippy-zero_warnings-brightgreen" alt="Clippy">
  <img src="https://img.shields.io/badge/crates-18-blue" alt="Crates">
  <img src="https://img.shields.io/badge/lines-83K-blue" alt="LoC">
  <img src="https://img.shields.io/badge/license-MIT-green" alt="License">
</p>

---

High-performance, multi-venue market making engine with institutional-grade risk management, toxicity detection, and MiCA compliance. `Decimal` arithmetic everywhere — never `f64` for money. 18 crates, 83K lines, 1431 tests.

## Why MG?

| | MG Market Maker | Hummingbot | Freqtrade | NautilusTrader |
|---|:---:|:---:|:---:|:---:|
| **Language** | Rust | Python | Python | Rust+Python |
| **Strategies** | 9+ (A-S, GLFT, Grid, Basis, XEMM, FundingArb, StatArb, PairedUnwind, ExecAlgos) | 5 | 3 | 4 |
| **Venues** | 4 × dual-product (Binance spot+USDM futures, Bybit spot+linear+inverse, HyperLiquid spot+perps, Custom) | 20+ | 10+ | 5 |
| **Latency** | ~2us/quote | ~1ms | ~10ms | ~50us |
| **Kill Switch** | 5-level auto-escalation + typed-echo confirm | Manual | No | No |
| **Toxicity (VPIN/Kyle/AS)** | Built-in | No | No | No |
| **MiCA Audit Trail** | JSONL + SHA-256 hash chain + HMAC signed export | No | No | No |
| **Portfolio Risk** | Factor VaR + correlation matrix + Markowitz hedge | No | No | Partial |
| **Multi-Client** | Per-client isolation + SLA certificates | No | No | No |
| **FIX 4.4** | Session engine + codec | No | No | Partial |
| **Adaptive Calibration** | PairClass templates + hyperopt recalibrate flow | No | No | No |
| **Auth Surface** | Role-gated (Admin/Operator/Viewer), HMAC tokens, audited login/logout | Basic | No | Basic |

## Features

<details>
<summary><b>Strategies</b> — 9+ built-in, write your own in one file</summary>

- **Avellaneda-Stoikov** — optimal spread with inventory skew (gamma/kappa/sigma)
- **GLFT** — Guéant-Lehalle-Fernandez-Tapia with live order flow calibration
- **Grid** — symmetric grid quoting around mid
- **Basis** — spot + reference price from hedge leg
- **Cross-Venue** — make on venue A, hedge on venue B
- **XEMM** — dedicated cross-exchange executor with slippage band
- **Funding Arb** — atomic spot-perp funding rate arbitrage with compensating-reversal on break
- **Stat Arb** — cointegrated pairs (Engle-Granger + Kalman + z-score driver)
- **Paired Unwind** — L4 kill-switch flatten for paired basis/funding positions
- **Exec Algos** — TWAP / VWAP / POV / Iceberg

Alpha pipeline: book imbalance, trade flow, microprice, OFI (Cont-Kukanov-Stoikov), HMA slope, learned microprice (Stoikov 2018), Cartea adverse-selection spread.

Regime detection: Quiet / Trending / Volatile / MeanReverting with per-regime auto-tuning. Pair-class templates: MajorSpot / AltSpot / MemeSpot / MajorPerp / AltPerp / StableStable with adaptive γ feedback loop.

**[Writing custom strategies](docs/guides/writing-strategies.md)** — implement one trait, get order management, risk, PnL for free.
</details>

<details>
<summary><b>Risk Management</b> — 5-level kill switch, VPIN, VaR, portfolio limits</summary>

- **5-Level Kill Switch** — Widen Spreads -> Stop New Orders -> Cancel All -> Flatten All (TWAP) -> Disconnect
- **VPIN + Kyle's Lambda** — toxicity detection, auto spread widening
- **Per-Strategy VaR Guard** — parametric Gaussian, EWMA, throttle to 0 on 99% breach
- **Portfolio-Level Risk** — factor delta limits, cross-symbol VaR, correlation matrix
- **Circuit Breaker** — stale book, wide spread, max drawdown, max exposure
- **Stale Book Watchdog** — auto-pause quoting when no data, auto-resume when fresh
- **Balance Pre-Check** — reservation system prevents over-submitting
- **Inventory Drift Reconciler** — detects tracker vs wallet divergence
</details>

<details>
<summary><b>Exchange Connectivity</b> — 4 venues, both spot + derivatives, WS order entry, FIX 4.4</summary>

Every CEX/DEX in the roster supports both spot and derivatives. Each
product-type is a separate connector instance (so an engine can run
`binance_spot + binance_futures + bybit_spot + bybit_linear +
hyperliquid_spot + hyperliquid_perp` side-by-side).

- **Binance** — spot + USDM futures. WS API order entry, listen-key user data, batch orders. `LIMIT_MAKER` for post-only (no more `timeInForce=GTC` fallback bug). Clock-skew preflight catches `-1021` before startup.
- **Bybit V5** — spot + linear (USDT-M) + inverse (coin-margined) perps. Unified category-parametrised API, batch 20, native amend, WS Trade.
- **HyperLiquid** — spot + perps (two constructors: `new()` for perps, `spot()` for spot; asset index offset handled internally). EIP-712 signing (secp256k1), WS post. USDC-only collateral.
- **Custom Exchange** — full REST + WS connector.
- **FIX 4.4** — session engine + codec (for institutional venues).
- **Shared Protocol Layer** — one WS-RPC abstraction, many adapters.

**[Adding a new exchange](docs/guides/adding-exchange.md)** — implement one trait, 8 steps.
</details>

<details>
<summary><b>Multi-Client & Compliance</b> — per-client isolation, MiCA audit, SLA tracking</summary>

- **Multi-Client Isolation** — each client owns symbols with separate PnL, fills, webhooks, API auth
- **MiCA Audit Trail** — append-only JSONL, microsecond timestamps, 5-year retention, **SHA-256 hash chain** (tamper-evident, restart-safe), HMAC-signed export
- **fsync on critical events** — OrderFilled / KillSwitchEscalated / CircuitBreakerTripped durably land before returning
- **MiCA HMAC Full-Body Signature** — reports sign the full UnsignedReport (period, strategy, OTR, risk controls, SLA, timestamp), not cherry-picked fields; constant-time verify
- **SLA Tracking** — per-minute presence buckets, spread compliance, two-sided quoting obligations
- **SLA Certificates** — HMAC-signed JSON for client reporting
- **Article 17 Report** — algorithmic trading report template (strategy, OTR stats, risk controls)
- **Token Lending** — loan agreements, utilization tracking, return schedules, PnL cost amortization
- **Webhook Delivery** — per-client event routing (SLA breach, kill switch, large fill, reports)
</details>

<details>
<summary><b>Dashboard & Auth</b> — Svelte 5, role-gated, audited, WCAG AA</summary>

- **Svelte 5 runes** — modern reactivity (`$state`, `$derived`, `$effect`, `$props`)
- **Role-based auth** — Admin / Operator / Viewer with HMAC-signed 24 h Bearer tokens + X-API-Key header
- **Login / logout audited** — `LoginSucceeded` / `LoginFailed` / `LogoutSucceeded` rows in MiCA trail with source IP + user_id / key prefix
- **IP rate-limit on login** — 20/min per source IP to blunt credential stuffing
- **Timing-equalization** on login failure — no trivial oracle on key membership
- **Typed-echo kill confirmation** — operator must type `{symbol} {action}` (e.g. `BTCUSDT FLATTEN`) for L3/L4 destructive ops; 3-second cooldown with countdown before fire button arms
- **Stale-data indicators** — every WS payload stamped with `_rx_ms`; `StaleBadge` component shows fresh (≤2s green) / stale (≤5s amber) / frozen (>5s red pulse)
- **Symbol switcher** — header pills drive `activeSymbol` flowing to every panel
- **Design tokens + WCAG AA** — centralised `tokens.css` for background, foreground, semantic, pair-class colours, 4pt spacing grid, typography scale; muted text passes AA contrast; `prefers-reduced-motion` handler; `:focus-visible` default ring on every interactive
- **Adaptive panel** — live γ / pair class / feedback state per symbol
- **Preflight** — clock-skew (±500 ms / ±2000 ms budgets), rate-limit budget, venue server time, exchange info, balance sanity — fails live startup on hard errors
</details>

<details>
<summary><b>Backtesting & Optimization</b> — replay, probabilistic fills, hyperopt</summary>

- **Event Replay** — JSONL recorded data through strategy simulator
- **Probabilistic Fill Model** — queue position, slippage, latency (seeded ChaCha8 RNG)
- **Lookahead-Bias Detector** — catches data leaks in indicators
- **Hyperopt** — random search + differential evolution with Sharpe/Sortino/Calmar/MaxDD loss
- **Parameter Calibration** — GO / NEEDS_MORE_DATA / UNPROFITABLE recommendations
- **A/B Testing** — time-based or symbol-based split, per-variant performance tracking
- **Demo Data Generator** — synthetic events with configurable volatility + mean-reversion
</details>

<details>
<summary><b>Operations & Monitoring</b> — Prometheus, Telegram, hot reload, smoke test</summary>

- **45+ Prometheus Gauges** — PnL, spread, inventory, VPIN, SLA, VaR, fill slippage, portfolio
- **Telegram** — two-way: alerts (3-level severity) + commands (/status, /stop, /pause, /force_exit)
- **Hot Config Reload** — change gamma/spread/size via admin API without restart
- **Pre-Flight Checks** — auto-validates venue, symbol, balance, fees, rate limits before trading
- **Smoke Test Mode** — `MM_MODE=smoke` validates connector stack in 30 seconds
- **Market Data Recording** — `record_market_data = true` for offline backtesting
- **Graceful Shutdown** — Ctrl+C -> cancel all -> checkpoint flush -> exit
- **Checkpoint Recovery** — restore inventory/PnL from last saved state
</details>

## Quick Start

### 1. Build

```bash
git clone https://github.com/mgmarket/market-maker.git
cd market-maker
cargo build --release
```

### 2. Smoke Test (validate connectivity)

Exchange keys are venue-scoped (no shared `MM_API_KEY` fallback). Auth
secret protects the dashboard — 32+ random bytes in production.

```bash
# Pick the venues you care about — each pair is optional.
export MM_BINANCE_API_KEY=...
export MM_BINANCE_API_SECRET=...
export MM_BYBIT_API_KEY=...
export MM_BYBIT_API_SECRET=...
export MM_HL_PRIVATE_KEY=...        # 0x-prefixed hex secp256k1

# Dashboard auth secret (32+ bytes). Rotate to invalidate all tokens.
export MM_AUTH_SECRET="$(openssl rand -base64 48)"

MM_MODE=smoke cargo run --release -p mm-server
```

### 3. Paper Trading (no real orders)

```bash
MM_MODE=paper cargo run --release -p mm-server
```

### 4. Record Data (for calibration)

```toml
# config/default.toml
record_market_data = true
```

Let it run 7+ days, then backtest with the recorded data.

### 5. Live Trading

```bash
MM_MODE=live cargo run --release -p mm-server
```

Pre-flight checks run automatically. If any fail in live mode, the system exits.

### Docker

```bash
docker compose up -d
# Dashboard: http://localhost:9090/api/status
# Prometheus: http://localhost:9091
# Grafana: http://localhost:3000
```

## API Endpoints

### Client API

| Endpoint | Description |
|----------|-------------|
| `GET /api/v1/positions` | Current inventory per symbol |
| `GET /api/v1/pnl` | PnL breakdown (spread/inventory/rebates/fees) |
| `GET /api/v1/sla` | SLA compliance report |
| `GET /api/v1/sla/certificate` | HMAC-signed compliance certificate |
| `GET /api/v1/fills/recent` | Recent fills with NBBO capture |
| `GET /api/v1/report/daily` | Daily performance report (JSON) |
| `GET /api/v1/report/daily/csv` | Daily report (CSV) |
| `GET /api/v1/portfolio` | Multi-currency portfolio snapshot |
| `GET /api/v1/portfolio/risk` | Portfolio risk summary |
| `GET /api/v1/portfolio/correlation` | Cross-symbol correlation matrix |
| `GET /api/v1/loans` | Loan agreements |
| `GET /api/v1/system/preflight` | System health check |
| `GET /api/v1/audit/export?from=&to=` | Signed audit log export |
| `GET /metrics` | Prometheus metrics |

### Per-Client API

| Endpoint | Description |
|----------|-------------|
| `GET /api/v1/client/{id}/sla` | Client SLA aggregate |
| `GET /api/v1/client/{id}/sla/certificate` | Client compliance certificate |
| `GET /api/v1/client/{id}/pnl` | Client PnL summary |
| `GET /api/v1/client/{id}/fills` | Client fill history |

### Admin API

| Endpoint | Description |
|----------|-------------|
| `POST /api/admin/config/{symbol}` | Hot-reload config per symbol |
| `POST /api/admin/config` | Broadcast config to all |
| `POST /api/admin/config/bulk` | Bulk per-symbol config patch |
| `POST /api/admin/symbols/{symbol}/pause` | Pause quoting |
| `POST /api/admin/symbols/{symbol}/resume` | Resume quoting |
| `POST /api/admin/clients` | Create client |
| `POST /api/admin/loans` | Create loan agreement |
| `POST /api/admin/optimize/trigger` | Kick off hyperopt random-search |
| `GET /api/admin/optimize/status` | Hyperopt run status |
| `GET /api/admin/optimize/results` | Completed jobs summary |
| `GET /api/admin/optimize/pending` | Calibrations awaiting sign-off |
| `POST /api/admin/optimize/apply` | Apply pending calibration to live config |
| `POST /api/admin/optimize/discard` | Drop pending calibration |

### Auth

| Endpoint | Method | Auth | Role |
|---|---|---|---|
| `/api/auth/login` | POST | — | — (IP rate-limited 20/min) |
| `/api/auth/logout` | POST | Bearer | any |
| `/api/status`, `/api/v1/*` (read-only) | GET | Bearer | any |
| `/metrics` | GET | Bearer | Admin / Operator |
| `/api/v1/ops/*`, `/api/admin/*` | POST | Bearer | Admin only |
| `/ws` | GET upgrade | `?token=…` | role-derived |

Destructive operator endpoints (`/api/v1/ops/widen|stop|cancel-all|flatten|disconnect|reset`) are Admin-only and rate-limited. WebSocket accepts `?token=` only because browsers cannot set headers on the upgrade — this is not an HTTP path. See [docs/guides/operations.md](docs/guides/operations.md#dashboard-auth--network-exposure) for the full HTTPS / secret-rotation playbook.

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌────────────────┐
│   Config     │────>│    Server     │────>│  Engine (xN)   │
│  (TOML+env)  │     │  preflight   │     │  per symbol    │
└─────────────┘     │  smoke test  │     │                │
                    └──────┬───────┘     │  Book Keeper   │
                           │             │  -> Strategy   │
                    ┌──────v───────┐     │  -> Risk       │
                    │  Dashboard   │     │  -> Orders     │
                    │  HTTP + WS   │<────│  -> PnL/SLA    │
                    │  Prometheus  │     │  -> Audit      │
                    │  Telegram    │     └───────┬────────┘
                    └──────────────┘             │
                                         ┌──────v────────┐
                                         │  Connectors   │
                                         │  Binance      │
                                         │  Bybit        │
                                         │  HyperLiquid  │
                                         │  Custom       │
                                         └───────────────┘
```

**[Full architecture guide](docs/guides/architecture.md)** — data flow, crate graph, persistence layout.

## Write Your Own Strategy

```rust
impl Strategy for MyStrategy {
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let half_spread = ctx.mid * dec!(5) / dec!(20000); // 5 bps
        let skew = ctx.inventory * dec!(0.0001); // inventory skew

        vec![QuotePair {
            bid_price: ctx.mid - half_spread - skew,
            ask_price: ctx.mid + half_spread - skew,
            bid_qty: dec!(0.001),
            ask_qty: dec!(0.001),
        }]
    }
}
```

The engine handles order management, risk limits, PnL tracking, audit trail, and exchange connectivity. You focus on the math.

**[Full strategy guide](docs/guides/writing-strategies.md)** — alpha signals, regime detection, cross-product, testing.

## Configuration

```toml
symbols = ["BTCUSDT"]

[exchange]
exchange_type = "binance"

[market_maker]
strategy = "avellaneda_stoikov"
gamma = 0.1          # risk aversion
kappa = 1.5          # order arrival intensity
sigma = 0.02         # volatility (annualized)
order_size = 0.001   # per level
num_levels = 3
min_spread_bps = 5

[risk]
max_inventory = 0.1
max_drawdown_quote = 500

[kill_switch]
daily_loss_limit = 1000
```

**[Full config reference](docs/guides/configuration-reference.md)** — every field documented.

## Project Structure

```
crates/
  server/          Entry point, preflight, smoke test, config validation
  engine/          Main event loop, order manager, book keeper, rebalancer, health
  strategy/        6 strategies + A/B split + autotune + momentum + indicators
  risk/            Kill switch, VPIN, VaR, portfolio risk, audit, SLA, PnL, reconciliation
  exchange/
    core/          ExchangeConnector trait, SOR, rate limiter
    binance/       Binance spot + futures
    bybit/         Bybit V5
    hyperliquid/   HyperLiquid (EIP-712)
    client/        Custom exchange
  dashboard/       HTTP API, Prometheus, Telegram, webhooks, MiCA reports
  portfolio/       Multi-currency position + PnL aggregation
  persistence/     Checkpoint, fill replay, loan store, transfer log
  backtester/      Simulator, fill models, event recorder, demo data, hyperopt
  protocols/       WS-RPC, FIX 4.4 (shared transport layers)
  indicators/      SMA, EMA, HMA, RSI, ATR, Bollinger, candles
  hyperopt/        Random search, differential evolution, calibration
```

## Benchmarks

```bash
cargo bench -p mm-strategy
```

| Operation | Latency |
|-----------|---------|
| Avellaneda-Stoikov (5 levels) | ~2 us |
| GLFT (5 levels) | ~5 us |
| Grid (5 levels) | ~1 us |
| Orderbook delta update | ~200 ns |

## Testing

```bash
cargo test                                    # 1431 tests
cargo clippy --all-targets -- -D warnings     # zero warnings
cargo bench -p mm-strategy                    # strategy benchmarks
```

## Documentation

| Guide | Audience | Topics |
|-------|----------|--------|
| **[Quick Start](docs/guides/quickstart.md)** | New users | Build, configure, smoke test, paper, live |
| **[Strategy Catalog](docs/guides/strategy-catalog.md)** | Everyone | Every strategy + signal + modulator — formulas, params, gotchas, selection matrix, per-PairClass recipes |
| **[Writing Strategies](docs/guides/writing-strategies.md)** | Strategy devs | Trait, context, alpha signals, testing |
| **[Architecture](docs/guides/architecture.md)** | System devs | Crate graph, data flow, persistence |
| **[Operations](docs/guides/operations.md)** | Operators | Modes, troubleshooting, daily checklist, auth surface |
| **[Adding Exchanges](docs/guides/adding-exchange.md)** | Connector devs | 8-step guide, auth, capabilities |
| **[Config Reference](docs/guides/configuration-reference.md)** | Everyone | Every TOML field + env vars |
| **[Adaptive Calibration](docs/guides/adaptive-calibration.md)** | Strategy devs | PairClass templates, tuner feedback loop, hyperopt flow |
| **[Crash Recovery](docs/guides/crash-recovery.md)** | Operators | Checkpoint restore, fill replay from audit log |
| **[Competitor Gap Analysis](docs/research/competitor-gap-analysis-apr17.md)** | Planning | What peers have we don't — STP, OCO, drop-copy, queue model |

## Contributing

1. Fork the repo
2. Create a feature branch (`git checkout -b feat/my-feature`)
3. Add tests for new functionality
4. Ensure `cargo clippy --all-targets -- -D warnings` passes
5. Ensure `cargo test` passes (1431+ tests)
6. Open a PR with a clear description

## License

MIT -- see [LICENSE](LICENSE).

## Disclaimer

This software is provided for educational and research purposes. Trading cryptocurrencies involves substantial risk of loss. The authors are not responsible for any financial losses incurred through the use of this software. Always test thoroughly in paper mode before deploying real capital.
