# Market Maker

Production-grade market maker for the custom exchange at `../exchange/` with multi-venue support.

## Stats

**12 crates, 67 files, ~10K lines Rust, 51 tests**

## Architecture

```
server/              Entry point, config validation, secrets from env, file logging
engine/              Main event loop — ALL subsystems wired:
  ├── market_maker   Book → Strategy → Risk → Order diff → Exchange
  ├── order_manager  Cancel/place with order diffing
  ├── book_keeper    Local orderbook from WS
  ├── order_id_map   UUID ↔ exchange native ID mapping
  └── balance_cache  Pre-check + reservation before placing
common/              Types (Decimal, never f64), config, orderbook
exchange-core/       ExchangeConnector trait, unified book, SOR, rate limiter
exchange-client/     Custom exchange REST+WS + retry with 429 backoff
exchange-binance/    Binance spot/futures (HMAC, batch, combined WS)
exchange-bybit/      Bybit V5 (batch 20, amend, HMAC)
strategy/            Strategies + signals:
  ├── avellaneda     Avellaneda-Stoikov optimal MM
  ├── glft           Guéant-Lehalle-Fernandez-Tapia with calibration
  ├── grid           Symmetric grid quoting
  ├── momentum       Alpha signals (book imbalance, trade flow, micro-price)
  ├── twap           Time-weighted execution for inventory unwinding
  ├── autotune       Regime detection + toxicity-based parameter adjustment
  ├── inventory_skew Quadratic skew, dynamic sizing, urgency unwinding
  └── volatility     EWMA realized vol estimator
risk/                Risk management:
  ├── kill_switch    5-level emergency (widen→stop→cancel→flatten→disconnect)
  ├── circuit_breaker Stale book, wide spread detection
  ├── inventory      Position tracking, PnL, limits
  ├── exposure       Drawdown tracking
  ├── toxicity       VPIN, Kyle's Lambda, adverse selection
  ├── sla            Exchange obligation compliance tracking
  ├── pnl            Attribution (spread/inventory/rebates/fees)
  ├── audit          Append-only JSONL audit trail (MiCA compliant)
  └── reconciliation Order + balance reconciliation vs exchange
dashboard/           HTTP dashboard:
  ├── server         /health, /api/status, /metrics, /api/v1/*
  ├── metrics        27 Prometheus gauges/counters
  ├── alerts         Telegram bot + 3-level severity + dedup
  ├── client_api     /positions, /pnl, /sla, /report/daily
  └── state          Shared state updated from engine
backtester/          Backtesting + paper trading:
  ├── simulator      Replay events → strategy → simulated fills
  ├── paper          Real-time paper trading with fill simulation
  ├── data           JSONL event recorder/loader
  └── report         Performance report with PnL attribution
persistence/         State management:
  ├── checkpoint     Atomic JSON checkpoint with auto-flush
  └── funding        Funding rate arbitrage engine
```

## Commands

```bash
cargo build                    # build all
cargo test                     # 51 tests
cargo run -p mm-server         # run live
MM_MODE=paper cargo run -p mm-server   # paper trading
RUST_LOG=debug cargo run -p mm-server  # debug logging
```

## Config

`config/default.toml` or `MM_CONFIG` env var.

Secrets via environment (NEVER in config files):
- `MM_API_KEY` — exchange API key
- `MM_API_SECRET` — exchange API secret
- `MM_TELEGRAM_TOKEN` — Telegram bot token
- `MM_TELEGRAM_CHAT` — Telegram chat ID
- `MM_MODE` — "live" or "paper"

## Key Design

- `rust_decimal::Decimal` everywhere (never f64 for money)
- PostOnly orders only (always maker)
- Order diffing (only cancel/place changed levels)
- 5-level kill switch (automated, manual override only for reset)
- VPIN + Kyle's Lambda → auto spread widening
- Regime-based parameter auto-tuning (4 regimes)
- Momentum alpha shifts reservation price (Cartea-Jaimungal)
- Balance pre-check with reservation before order placement
- Audit trail: append-only JSONL for MiCA compliance
- Reconciliation every 60s (orders + balances vs exchange)
- TWAP executor for kill switch L4 (flatten all)
- Dashboard state pushed every 30s to Prometheus + HTTP API
- Graceful shutdown: Ctrl+C → cancel all → final reports → checkpoint flush
