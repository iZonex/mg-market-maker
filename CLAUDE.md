# MG Market Maker

Production-grade market maker for the custom exchange at `../exchange/` with multi-venue support.

## Stats

**18 crates, 154 files, ~49K lines Rust, 920 tests**

## Architecture

```
server/                Entry point, config validation, secrets from env, file logging
engine/                Main event loop — ALL subsystems wired:
  ├── market_maker     Book → Strategy → Risk → Order diff → Exchange
  ├── order_manager    Cancel/place with order diffing + amend (P1.1) + batch entry (Epic E)
  ├── book_keeper      Local orderbook from WS
  ├── order_id_map     UUID ↔ exchange native ID mapping
  ├── pair_lifecycle   Halt / delisting / tick-lot drift (P2.3 s1)
  ├── sor              Cross-venue Smart Order Router (Epic A)
  └── balance_cache    Pre-check + reservation before placing
common/                Types (Decimal, never f64), config, orderbook
exchange/              Venue adapters (one abstraction, many connectors):
  ├── core/            ExchangeConnector trait, unified book, SOR, rate limiter
  ├── client/          Custom exchange REST+WS + retry with 429 backoff
  ├── binance/         Binance spot/futures — REST + WS API (HMAC)
  ├── bybit/           Bybit V5 — REST + WS Trade (HMAC, batch 20, amend)
  └── hyperliquid/     HyperLiquid perp DEX — REST + WS post (EIP-712 / k256)
protocols/             Shared wire/transport layers:
  ├── fix/             FIX 4.4 message codec + session engine
  └── ws_rpc/          Generic id-correlated WS request/response client
indicators/            Technical indicators (SMA, EMA, HMA/WMA, RSI, ATR, Bollinger, Tick/Volume/MultiTrigger candles, weight gens)
portfolio/             Multi-currency position tracker + PnL aggregation
hyperopt/              Random-search hyperparameter optimiser + loss functions
strategy/              Strategies + signals + execution:
  ├── avellaneda       Avellaneda-Stoikov optimal MM
  ├── glft             Guéant-Lehalle-Fernandez-Tapia with calibration
  ├── grid             Symmetric grid quoting
  ├── cross_exchange   Make on venue A, hedge on venue B
  ├── xemm             Dedicated cross-exchange executor with slippage band
  ├── basis            Basis-shifted reservation price (spot + ref_price)
  ├── funding_arb      Atomic pair dispatcher (market-take hedge, maker-post primary)
  ├── funding_arb_driver Periodic FundingArbEngine loop + DriverEventSink
  ├── stat_arb         Cointegrated pairs (Engle-Granger + Kalman + z-score + driver, Epic B)
  ├── paired_unwind    L4 kill-switch flatten for paired basis/funding positions
  ├── exec_algo        ExecAlgorithm trait + TWAP/VWAP/POV/Iceberg impls
  ├── features         Microstructure feature extractors (imbalance, trade flow, …)
  ├── cks_ofi          Cont-Kukanov-Stoikov L1 OFI tracker (Epic D)
  ├── learned_microprice Stoikov 2018 G-function histogram fit (Epic D)
  ├── cartea_spread    Cartea AS closed-form spread + decimal_ln helper (Epic D)
  ├── momentum         Alpha signals (book imbalance, trade flow, micro-price, OFI, learned MP)
  ├── twap             Time-weighted execution for single-leg inventory unwinding
  ├── autotune         Regime detection + toxicity-based parameter adjustment
  ├── market_resilience Event-driven shock detector + recovery score (MR)
  ├── inventory_skew   Quadratic skew, dynamic sizing, urgency unwinding
  └── volatility       EWMA realized vol estimator
risk/                  Risk management:
  ├── kill_switch      5-level emergency (widen→stop→cancel→flatten→disconnect)
  ├── protections      StoplossGuard / CooldownPeriod / MaxDrawdown / LowProfitPairs
  ├── circuit_breaker  Stale book, wide spread detection
  ├── dca              Position-adjustment planner (flat/linear/accelerated)
  ├── order_emulator   Client-side stops/trailing/OCO/GTD emulation
  ├── inventory        Position tracking, PnL, limits
  ├── exposure         Drawdown tracking
  ├── toxicity         VPIN, Kyle's Lambda, adverse selection
  ├── inventory_drift  Inventory-vs-wallet drift reconciler (P0.2)
  ├── borrow           Borrow-cost surcharge state machine (P1.3 s1)
  ├── hedge_optimizer  Markowitz mean-variance cross-asset hedge (Epic C)
  ├── var_guard        Parametric Gaussian VaR per strategy class (Epic C)
  ├── lead_lag_guard   EWMA z-score on leader-venue mid → soft widen (Epic F)
  ├── news_retreat     3-class headline state machine + cooldowns (Epic F)
  ├── otr              Order-to-Trade Ratio (MiCA surveillance metric)
  ├── sla              Exchange obligation compliance + per-minute presence (P2.2)
  ├── pnl              Attribution (spread/inventory/rebates/fees)
  ├── audit            Append-only JSONL audit trail (MiCA compliant)
  └── reconciliation   Order + balance reconciliation vs exchange
dashboard/             HTTP dashboard:
  ├── server           /health, /api/status, /metrics, /api/v1/*
  ├── metrics          28 Prometheus gauges/counters/histograms
  ├── alerts           Telegram bot + 3-level severity + dedup
  ├── telegram_control Two-way Telegram (/status, /stop, /pause, /force_exit)
  ├── client_api       /positions, /pnl, /sla, /report/daily
  └── state            Shared state updated from engine
backtester/            Backtesting + paper trading:
  ├── simulator        Replay events → strategy → simulated fills
  ├── fill_model       Probabilistic fill model with latency + slippage
  ├── lookahead        Generic lookahead-bias detector (O(N²) prefix check)
  ├── paper            Real-time paper trading with fill simulation
  ├── data             JSONL event recorder/loader
  ├── stress           Synthetic stress scenarios + runner + report (Epic C)
  └── report           Performance report with PnL attribution
persistence/         State management:
  ├── checkpoint     Atomic JSON checkpoint with auto-flush
  └── funding        Funding rate arbitrage engine
```

## Commands

```bash
cargo build                    # build all
cargo test                     # 920 tests
cargo clippy --all-targets -- -D warnings
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

## Protocol architecture rule

**One abstraction, many adapters.** When two venues share a transport
pattern (id-correlated WebSocket request/response, FIX 4.4 session,
JSON-RPC 2.0), the pattern lives once in `crates/protocols/*`. Venue
crates under `crates/exchange/*` provide thin adapters that map the
shared abstraction onto the venue's specific request/response shape,
error codes, and auth scheme.

WS order entry for Binance, Bybit, and HyperLiquid all sit on top of
`crates/protocols/ws_rpc`. FIX 4.4 for any future venue sits on top of
`crates/protocols/fix`. `VenueCapabilities::supports_ws_trading` and
`supports_fix` flags are never set unless the code path is actually
wired — covered by a capability-audit test in each exchange crate.

Venue-specific documentation lives in `docs/protocols/` — one file per
protocol with endpoint, auth scheme, rate limits, error codes, and
reconnect semantics.
