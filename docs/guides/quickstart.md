# Quick Start Guide

## Prerequisites

- Rust 1.75+ (`rustup update stable`)
- Exchange API key (Binance, Bybit, or custom)
- $0 for paper mode, $2K+ for live

## 1. Build

```bash
git clone <repo> && cd market-maker
cargo build --release
```

## 2. Configure

Copy and edit the config:

```bash
cp config/default.toml config/my-config.toml
```

Key settings to change:

```toml
# Which exchange
[exchange]
exchange_type = "binance"   # binance, bybit, hyperliquid, custom
rest_url = ""               # ignored for binance/bybit (hardcoded)
ws_url = ""                 # ignored for binance/bybit

# What to trade
symbols = ["BTCUSDT"]

# Strategy parameters (start conservative)
[market_maker]
strategy = "avellaneda_stoikov"
gamma = 0.3          # risk aversion (higher = wider spread)
kappa = 1.5          # order arrival intensity
sigma = 0.02         # volatility estimate (annualized)
order_size = 0.001   # base asset per level
num_levels = 3       # quote levels per side
min_spread_bps = 5   # never quote tighter than this
refresh_interval_ms = 500

# Risk limits
[risk]
max_inventory = 0.01    # max position in base asset
max_exposure_quote = 1000
max_drawdown_quote = 100
```

Set secrets via environment (NEVER in config files):

```bash
export MM_API_KEY="your-api-key"
export MM_API_SECRET="your-api-secret"
export MM_MODE="paper"   # paper | live | smoke
```

## 3. Smoke Test (first time)

Validates connector without trading:

```bash
MM_MODE=smoke MM_CONFIG=config/my-config.toml cargo run --release -p mm-server
```

This will:
- Connect to the exchange WS
- Subscribe to your symbol
- Measure WS latency
- Place a test order far from market and cancel it
- Fetch balances
- Print a report and exit

**Fix any errors before proceeding.**

## 4. Record Market Data

Before going live, record data for parameter calibration:

```toml
# Add to config
record_market_data = true
```

```bash
MM_MODE=paper MM_CONFIG=config/my-config.toml cargo run --release -p mm-server
```

Data writes to `data/recorded/{symbol}.jsonl`. Let it run 7+ days for meaningful calibration.

## 5. Paper Trading

```bash
MM_MODE=paper MM_CONFIG=config/my-config.toml cargo run --release -p mm-server
```

Monitor via:
- Dashboard: `http://localhost:9090/api/v1/positions`
- Health: `http://localhost:9090/api/v1/system/preflight`
- PnL: `http://localhost:9090/api/v1/pnl`
- Metrics: `http://localhost:9090/metrics` (Prometheus)

## 6. Go Live

Only after paper mode runs clean for 48h+:

```bash
MM_MODE=live MM_CONFIG=config/my-config.toml cargo run --release -p mm-server
```

The system runs pre-flight checks automatically. If any fail, it exits with an error message.

## 7. Monitoring

### Dashboard API

| Endpoint | Description |
|----------|-------------|
| `GET /api/v1/positions` | Current inventory per symbol |
| `GET /api/v1/pnl` | PnL breakdown (spread/inventory/rebates/fees) |
| `GET /api/v1/sla` | SLA compliance per symbol |
| `GET /api/v1/fills/recent` | Recent fills with NBBO |
| `GET /api/v1/report/daily` | Daily performance report |
| `GET /api/v1/system/preflight` | System health check |
| `GET /api/v1/portfolio` | Multi-symbol portfolio snapshot |
| `GET /api/v1/portfolio/risk` | Portfolio risk summary |
| `GET /metrics` | Prometheus gauges (45+) |

### Admin API

| Endpoint | Description |
|----------|-------------|
| `POST /api/admin/config/{symbol}` | Hot-reload config for a symbol |
| `POST /api/admin/config` | Broadcast config change to all |
| `POST /api/admin/symbols/{symbol}/pause` | Pause quoting |
| `POST /api/admin/symbols/{symbol}/resume` | Resume quoting |
| `GET /api/admin/optimize/status` | Hyperopt run status |

### Telegram

Set `MM_TELEGRAM_TOKEN` and `MM_TELEGRAM_CHAT` for alerts. Commands:
- `/status` — current positions and PnL
- `/stop` — cancel all orders
- `/pause BTCUSDT` — pause one symbol
- `/force_exit BTCUSDT` — force flatten

## 8. Graceful Shutdown

`Ctrl+C` triggers:
1. Cancel all orders on all venues
2. Final checkpoint flush
3. Daily report snapshot
4. Clean exit

Checkpoint is restored on next startup if `checkpoint_restore = true`.
