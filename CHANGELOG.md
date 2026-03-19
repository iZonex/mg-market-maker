# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
