# Configuration Reference

All config via TOML file (`config/default.toml` or `MM_CONFIG` env). Secrets via environment variables.

## Environment Variables

Exchange credentials are **venue-scoped**. The legacy shared
`MM_API_KEY` / `MM_API_SECRET` fallback was removed — set only the
pair(s) for venues you actually trade on.

| Variable | Required | Description |
|----------|----------|-------------|
| `MM_BINANCE_API_KEY` | Per-venue | Binance spot + futures API key |
| `MM_BINANCE_API_SECRET` | Per-venue | Binance HMAC secret |
| `MM_BYBIT_API_KEY` | Per-venue | Bybit V5 API key |
| `MM_BYBIT_API_SECRET` | Per-venue | Bybit V5 secret |
| `MM_HL_PRIVATE_KEY` | Per-venue | HyperLiquid 32-byte hex secp256k1 (address is derived) |
| `MM_MODE` | No | `live`, `paper` (default), `smoke` |
| `MM_CONFIG` | No | Config file path (default: `config/default.toml`) |
| `MM_TELEGRAM_TOKEN` | No | Telegram bot token for alerts |
| `MM_TELEGRAM_CHAT` | No | Telegram chat ID |
| `MM_ADMIN_KEY` | No | Default admin API key (prefer `[[users]]` in config) |
| `MM_AUTH_SECRET` | **Yes for dashboard** | 32+ random bytes, HMAC secret for session tokens. Default placeholder warns at startup. Rotate on operator offboarding — it invalidates every outstanding token. |
| `MM_SENTRY_DSN` | No | Sentry DSN; when set, errors stream to Sentry with release + env tags |
| `MM_AUDIT_ARCHIVE_CMD` | No | Post-rotation archival shell command (`{file}` = absolute path to gzipped log) |

## [exchange]

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `exchange_type` | string | `"custom"` | `custom`, `binance`, `binance_testnet`, `bybit`, `bybit_testnet`, `hyper_liquid`, `hyper_liquid_testnet` |
| `rest_url` | string | — | REST API base URL (ignored for HyperLiquid) |
| `ws_url` | string | — | WebSocket URL (ignored for HyperLiquid) |
| `api_key` | string | — | Override (prefer venue-scoped env, e.g. `MM_BINANCE_API_KEY`) |
| `api_secret` | string | — | Override (prefer venue-scoped env, e.g. `MM_BINANCE_API_SECRET`) |

## [market_maker]

### Core Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `strategy` | string | `"avellaneda_stoikov"` | Strategy type (see below) |
| `gamma` | decimal | `0.1` | Risk aversion (higher = wider spread) |
| `kappa` | decimal | `1.5` | Order arrival intensity (higher = tighter) |
| `sigma` | decimal | `0.02` | Volatility estimate (annualized) |
| `order_size` | decimal | `0.001` | Base order size per level |
| `num_levels` | int | `3` | Quote levels per side |
| `min_spread_bps` | decimal | `5` | Minimum spread floor (basis points) |
| `max_distance_bps` | decimal | `100` | Max distance from mid for outermost level |
| `refresh_interval_ms` | int | `500` | Quote refresh interval |
| `time_horizon_secs` | int | `300` | Strategy cycle duration |

### Strategy Types

| Value | Description |
|-------|-------------|
| `avellaneda_stoikov` | Optimal MM with γ/κ/σ (default, good starting point) |
| `glft` | Guéant-Lehalle-Fernandez-Tapia with calibration |
| `grid` | Symmetric grid quoting |
| `basis` | Basis-shifted reservation price (needs `[hedge]`) |
| `cross_venue_basis` | Cross-exchange basis (needs `[hedge]` on different venue) |
| `funding_arb` | Funding rate arbitrage (needs `[hedge]` + `[funding_arb]`) |

### Feature Toggles

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `momentum_enabled` | bool | `true` | Alpha signals shift reservation price |
| `hma_enabled` | bool | `true` | Hull Moving Average component |
| `market_resilience_enabled` | bool | `true` | Event-driven shock detector |
| `otr_enabled` | bool | `true` | Order-to-Trade ratio (MiCA) |
| `amend_enabled` | bool | `true` | Cancel-replace vs amend |
| `var_guard_enabled` | bool | `false` | Per-strategy VaR throttle |
| `borrow_enabled` | bool | `false` | Borrow cost surcharge |
| `pair_lifecycle_enabled` | bool | `true` | Symbol halt/delist detection |
| `user_stream_enabled` | bool | `true` | Binance listen-key user data |
| `sor_inline_enabled` | bool | `false` | SOR auto-dispatch orders |

## [risk]

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_inventory` | decimal | `0.1` | Max position (base asset) |
| `max_exposure_quote` | decimal | `10000` | Max position value (quote) |
| `max_drawdown_quote` | decimal | `500` | Circuit breaker threshold |
| `inventory_skew_factor` | decimal | `1.0` | How aggressively to skew (0=none) |
| `max_spread_bps` | decimal | `500` | Pause quoting if spread exceeds |
| `stale_book_timeout_secs` | int | `10` | Cancel all after N seconds no data |

## [kill_switch]

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `daily_loss_limit` | decimal | `1000` | L3: Cancel All threshold |
| `daily_loss_warning` | decimal | `500` | L1: Widen Spreads threshold |
| `max_position_value` | decimal | `50000` | L2: Stop New Orders threshold |
| `max_message_rate` | int | `100` | Runaway algo detection |
| `max_consecutive_errors` | int | `10` | API error threshold |

## [sla]

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_spread_bps` | decimal | `100` | Max spread to count as "quoting" |
| `min_depth_quote` | decimal | `2000` | Min depth per side (quote asset) |
| `min_uptime_pct` | decimal | `95` | Required uptime percentage |
| `two_sided_required` | bool | `true` | Must quote both sides |

## [toxicity]

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `vpin_bucket_size` | decimal | `50000` | VPIN bucket size (quote) |
| `vpin_num_buckets` | int | `50` | Number of VPIN buckets |
| `vpin_threshold` | decimal | `0.7` | Auto-widen trigger |
| `autotune_enabled` | bool | `true` | Regime-based parameter tuning |

## Top-Level Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `symbols` | string[] | `["BTCUSDT"]` | Symbols to trade |
| `dashboard_port` | int | `9090` | HTTP API port (0=disabled) |
| `checkpoint_path` | string | `"data/checkpoint.json"` | State file |
| `checkpoint_restore` | bool | `false` | Restore inventory from checkpoint |
| `record_market_data` | bool | `false` | Record WS events to JSONL |
| `mode` | string | `"live"` | Trading mode |
| `log_file` | string | `""` | Log file path (empty=stdout) |

## [[clients]] (Multi-Client)

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique client ID |
| `name` | string | Display name |
| `symbols` | string[] | Symbols owned by this client |
| `webhook_urls` | string[] | Webhook URLs for events |
| `api_keys` | string[] | API keys scoped to this client |
| `sla` | SlaConfig | Per-client SLA override |
| `report_branding` | ReportBranding | Company name, logo, footer |

## [portfolio_risk] (Optional)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_total_delta_usd` | decimal | `100000` | Global delta limit |
| `factor_limits[].factor` | string | — | Factor name ("BTC", "ETH") |
| `factor_limits[].max_net_delta` | decimal | — | Max absolute delta |
| `factor_limits[].widen_mult` | decimal | `2` | Spread multiplier on warn |
| `factor_limits[].warn_pct` | decimal | `0.8` | Warning threshold (% of max) |
