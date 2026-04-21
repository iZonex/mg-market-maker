# Operations & Troubleshooting Guide

## Modes

| Mode | Env | Behavior |
|------|-----|----------|
| `live` | `MM_MODE=live` | Real orders. Preflight must pass. Validation gates placeholder config. |
| `paper` | `MM_MODE=paper` | Real market feed. `OrderManager` + `BalanceCache` short-circuit all order egress to simulated UUIDs; paper fills synthesised from public trades. |
| `smoke` | `MM_MODE=smoke` | Connector test only: subscribe, place/cancel ONE test order, exit. |

### Paper-mode guard (Epic 26)

`mode == "paper"` is not just a log message — it is enforced at the
`OrderManager` / `BalanceCache` level. Every `place_order`,
`cancel_order`, `amend_order`, `place_orders_batch`,
`cancel_orders_batch`, and unwind-slice path checks the paper flag
before calling the connector and returns a simulated response
instead. Covered by 7 unit tests in `crates/engine/src/order_manager.rs`.

Live smoke verified with a Binance spot `canTrade:true` key:
45 s of paper run, 0 calls to `/api/v3/order`, `/openOrders` stayed `[]`.

## Per-Venue Quickstart

| Venue | Products | Auth | Public keyless | Env vars | Notes |
|-------|----------|------|----------------|----------|-------|
| Binance | spot + USDM futures | HMAC-SHA256 | yes | `MM_BINANCE_API_KEY`, `MM_BINANCE_API_SECRET` | Shared key pair covers both. `user_stream_enabled = true` on live merges out-of-band fills via listen-key. `LIMIT_MAKER` used for post-only. |
| Bybit V5 | spot, linear (USDT-M), inverse (coin-margined) | HMAC-SHA256 | yes (spot) | `MM_BYBIT_API_KEY`, `MM_BYBIT_API_SECRET` | One connector per category (`BybitConnector::spot() / ::linear() / ::inverse()`). Private WS (no listen-key). Batch size 20, native `amend`. |
| HyperLiquid | spot + perps | EIP-712 / secp256k1 | yes (`/info`) | `MM_HL_PRIVATE_KEY` | Two constructors: `new()` for perps, `spot()` for spot. Asset index offset (`SPOT_INDEX_OFFSET = 10_000`) handled internally. USDC-only collateral. 32-byte hex private key; address is derived. |
| Custom exchange | spot | HMAC | yes | `MM_API_KEY`, `MM_API_SECRET` | Development/integration target only. |

Each venue × product is a **separate connector instance** — an engine
can run `binance_spot + binance_futures + bybit_spot + bybit_linear +
hyperliquid_spot + hyperliquid_perp` side-by-side, each with its own
book-keeper, risk state, and audit feed. Venue credentials are scoped
per venue (not per product) — the same Binance key covers spot + futures.

```bash
# Paper run against Binance — simulated orders, real feed.
MM_CONFIG=config/binance-paper.toml \
MM_MODE=paper \
MM_BINANCE_API_KEY=<read-only key> \
MM_BINANCE_API_SECRET=<secret> \
MM_AUTH_SECRET="$(openssl rand -base64 48)" \
cargo run --release -p mm-server
```

Swap the config path + env pair for Bybit (`MM_BYBIT_*`) or HL
(`MM_HL_PRIVATE_KEY`).

## Pre-Flight Checks

Run automatically on startup. In `live` mode, any failure aborts.

| Check | What | Fail means |
|-------|------|-----------|
| `venue_connectivity` | `health_check()` | Exchange unreachable |
| `clock_skew` | `server_time_ms()` vs local clock | Drift > 2 s ⇒ hard fail (Binance `-1021` silent fails); 500 ms – 2 s ⇒ warn |
| `{symbol}_product_spec` | `get_product_spec()` | Symbol not found or invalid |
| `{symbol}_tick_size` | tick > 0 | Can't place orders |
| `{symbol}_fees` | Non-default fees | VIP tier not loaded |
| `balances` | Any balance > 0 | Account empty |
| `rate_limit` | Remaining > 100 | Rate budget low |
| `config_gamma` | gamma ≤ 5 | Spread will be too wide |
| `config_order_size` | > 0 | No orders will be placed |

## Stale Book Protection

If no book update for `stale_book_timeout_secs` (default 10s):
1. Engine cancels all orders
2. Pauses quoting
3. Logs `CircuitBreakerTripped` to audit
4. **Auto-resumes** when fresh data arrives

No manual intervention needed.

## Kill Switch

### Automatic Escalation

| Trigger | Level | Action |
|---------|-------|--------|
| Daily PnL < `daily_loss_warning` | 1 | Widen spreads ×2 |
| Position value > `max_position_value` | 2 | Stop new orders |
| Daily PnL < `daily_loss_limit` | 3 | Cancel all orders |
| Manual or paired-unwind | 4 | Flatten all positions (TWAP) |
| Manual only | 5 | Disconnect from exchange |

### Manual Reset

```bash
# Via API
curl -X POST http://localhost:9090/api/admin/symbols/BTCUSDT/resume

# Via Telegram
/resume BTCUSDT
```

## Hot Config Reload

Change parameters without restart:

```bash
# Single symbol
curl -X POST http://localhost:9090/api/admin/config/BTCUSDT \
  -H "Content-Type: application/json" \
  -d '{"field": "Gamma", "value": 0.2}'

# All symbols
curl -X POST http://localhost:9090/api/admin/config \
  -H "Content-Type: application/json" \
  -d '{"field": "MinSpreadBps", "value": 8}'
```

Available overrides: `Gamma`, `MinSpreadBps`, `OrderSize`, `MaxDistanceBps`, `NumLevels`, `MomentumEnabled`, `AmendEnabled`, `MaxInventory`, `PauseQuoting`, `ResumeQuoting`, `PortfolioRiskMult`.

## Recording Market Data

```toml
record_market_data = true
```

Writes `data/recorded/{symbol}.jsonl` with `BookSnapshot` + `Trade` events. Append mode — survives restarts. Use for backtesting and calibration.

## Checkpoint Recovery

```toml
checkpoint_restore = true
```

On startup with this flag:
1. Loads `data/checkpoint.json`
2. Validates (timestamp not future, inventory/price sane)
3. Restores `InventoryManager` state
4. Logs restore event to audit trail

Without this flag (default): engines start with zero inventory and rely on reconciliation.

## Common Issues

### "preflight failed: symbol not found"

The symbol doesn't exist on the exchange or the product spec endpoint is wrong.
- Check `exchange_type` matches your API key
- Verify the symbol format (Binance: `BTCUSDT`, Bybit: `BTCUSDT`, HyperLiquid: `BTC`)

### "rate_limit_remaining = 0"

Too many API calls. Possible causes:
- `refresh_interval_ms` too low (minimum recommended: 200ms)
- Multiple instances sharing same API key
- Fix: increase `refresh_interval_ms` or use separate API keys

### "circuit breaker tripped: stale book"

No market data for `stale_book_timeout_secs`.
- Check WS connection (exchange may have dropped it)
- Check network connectivity
- Engine auto-resumes when data returns

### "inventory drift detected"

`InventoryManager` tracker disagrees with exchange balance.
- Possible causes: missed fill, external transfer, API key shared with another bot
- Check audit log: `data/audit/{symbol}.jsonl`
- If `inventory_drift_auto_correct = true`: tracker auto-resets to match exchange
- If `false`: operator must investigate and restart

### Fills not arriving

- Check `user_stream_enabled = true` (Binance listen-key)
- Check exchange API key has trading permissions
- Check symbol is in `Trading` status (not halted)

### High adverse selection

PnL negative despite making markets:
- Increase `gamma` (wider spread)
- Increase `min_spread_bps`
- Check VPIN gauge — if consistently > 0.7, the pair has toxic flow
- Consider switching to a less liquid pair where spread capture is higher

## Log Files

```bash
# Engine logs (if log_file set in config)
tail -f data/mm.log

# Audit trail (per symbol)
tail -f data/audit/btcusdt.jsonl | jq .

# Fill history
tail -f data/fills.jsonl | jq .

# Recorded market data
wc -l data/recorded/btcusdt.jsonl  # count events
```

## Prometheus Metrics

Key gauges to monitor:

| Metric | Alert if |
|--------|----------|
| `mm_pnl_total` | Negative and declining |
| `mm_spread_bps` | > 2× configured min_spread |
| `mm_inventory` | Near max_inventory |
| `mm_vpin` | > 0.7 sustained |
| `mm_kill_switch_level` | > 0 |
| `mm_sla_presence_pct_24h` | < 95% |
| `mm_portfolio_var_95` | Breaching limit |

### Shipped alert rules

`deploy/prometheus/mm-alerts.yml` carries the three-tier alert
set — `critical` (page: kill-switch L3+, archive broken, atomic
rollbacks), `warning` (notify: SLA slip, book-update p95 > 250 ms,
SOR error rate > 10 %, surveillance trip), `info` (ticket:
sustained drawdown, VPIN > 0.75, avg slippage > 2 bps). The main
`config/prometheus.yml` already references it via `rule_files`,
and `docker-compose.yml` mounts it into the Prometheus container
at `/etc/prometheus/mm-alerts.yml:ro`. Tune thresholds per
deployment by editing the YAML in-tree, then `SIGHUP` Prometheus:

```bash
docker compose kill -s SIGHUP prometheus
```

### Alertmanager routing

`deploy/alertmanager/alertmanager.yml` routes the three severity
tiers to separate Telegram channels + optional PagerDuty /
generic webhook sinks. Group-by covers
`(alertname, severity, symbol, venue)` so one incident produces
one notification, not twenty; inhibit rules suppress downstream
noise (kill-switch L5 Disconnect silences the warning tier for
the same symbol).

Configure via env vars (docker-compose reads them from the host,
Helm/K8s surface them via Secret):

| Variable | Purpose |
|----------|---------|
| `TELEGRAM_BOT_TOKEN_CRITICAL` / `TELEGRAM_CHAT_ID_CRITICAL` | Pager channel — wakes on-call on kill-switch L3+ / archive dead / atomic rollbacks |
| `TELEGRAM_BOT_TOKEN_OPS` / `TELEGRAM_CHAT_ID_OPS` | Ops channel — warnings (SLA slip, latency, SOR errors) |
| `PAGERDUTY_SERVICE_KEY` *(optional)* | Uncomment `pagerduty_configs` block to enable |
| `GENERIC_WEBHOOK_URL` *(optional)* | Generic webhook (Slack / Discord / custom) |

Container `entrypoint` runs `envsubst` over the YAML before
Alertmanager starts, so `$VAR` references resolve to the
injected values. Leaving a token blank turns that receiver into
a no-op so a fresh install never fails to start.

The docker-compose stack now ships Alertmanager at `:9093`;
`config/prometheus.yml` forwards alerts via the `alerting:`
block.

### Shipped Grafana dashboard

`deploy/grafana/dashboards/mm-overview.json` auto-provisions via
`deploy/grafana/provisioning/` on `docker compose up`. Opens
under **Dashboards → Market Maker → MM Overview** and covers:

1. Kill-switch state per symbol (stat card with severity colours)
2. SLA uptime gauge (thresholds at 95 / 98 / 99.5 %)
3. Live order count per symbol
4. Total PnL timeseries
5. PnL attribution stacked (spread / inventory / rebates / funding)
6. Inventory (base-asset)
7. Spread (bps)
8. VPIN (thresholds at 0.5 / 0.7 / 0.85)
9. Realised volatility
10. SOR dispatch rate success vs errors per venue
11. Book-update latency p50 / p95 / p99 per venue
12. Surveillance scores per pattern × symbol

Datasource is pinned to `uid: prometheus` so the dashboard
survives a `docker compose down -v` without manual re-linking.

## Dashboard Auth & Network Exposure

The dashboard must never be bound to a public interface without
a TLS-terminating reverse proxy in front. Tokens are Bearer-only,
HMAC-signed, and leak once over plaintext — the HTTP layer itself
has no TLS code path.

**Checklist before exposing the dashboard:**

- `MM_AUTH_SECRET` is set to **32+ random bytes** (e.g.
  `openssl rand -base64 48`). The default placeholder is refused
  with a warning at startup; do not ship with it.
- Front the listener with nginx/Caddy/ALB terminating TLS. Bind
  `mm-server` to `127.0.0.1:<port>` and proxy through.
- Users are created under `[[users]]` in config with explicit
  `role` (`admin` / `operator` / `viewer`) and long random
  `api_key` values (32+ bytes). Never reuse exchange keys.
- Rotate the `MM_AUTH_SECRET` on operator offboarding — it
  invalidates every outstanding token immediately.

**Auth surface summary:**

| Path | Method | Auth | Role gate |
|------|--------|------|-----------|
| `/api/auth/login` | POST | none (IP-rate-limited 20/min) | — |
| `/api/auth/logout` | POST | Bearer | any |
| `/health`, `/ready`, `/startup` | GET | none | — |
| `/api/status`, `/api/v1/*` (read-only) | GET | Bearer | any |
| `/metrics` | GET | Bearer | admin/operator |
| `/api/v1/ops/*`, `/api/admin/*` | POST/GET | Bearer | admin only |
| `/ws` | GET upgrade | `?token=…` | role-derived |

Tokens are 24 h HMAC-SHA256, stateless — logout emits an audit
event but cannot revoke the token (the client must drop it). If
a key is suspected compromised: remove it from `[[users]]`, then
rotate `MM_AUTH_SECRET` to cut every pre-issued token.

Every `/api/auth/login` success and failure writes a row to the
MiCA audit trail (`LoginSucceeded` / `LoginFailed`), and every
`/api/auth/logout` writes a `LogoutSucceeded` row. Failures log
the source IP and a short key prefix for correlation.

## Daily Operations Checklist

1. Check `GET /api/v1/system/preflight` — all green?
2. Check `GET /api/v1/pnl` — positive spread capture?
3. Check `GET /api/v1/sla` — uptime > 95%?
4. Check inventory — not pinned at max?
5. Review audit log for warnings/errors
6. Backup `data/checkpoint.json`

## Observability

### Sentry (error aggregation)

Always compiled. Activates at runtime when `MM_SENTRY_DSN` is set.
Release tag is `mm-server@<cargo_pkg_version>`, environment tag
mirrors `MM_MODE` (live/paper/smoke). Override trace sample rate
with `SENTRY_TRACES_SAMPLE_RATE` if the default errors-only mode
is too narrow.

### OpenTelemetry OTLP tracing (optional)

Gated behind the `otel` cargo feature so default builds stay lean.

```bash
# Build with the feature
cargo build --release -p mm-server --features otel

# Point at a collector (tonic/gRPC endpoint)
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
cargo run --release -p mm-server --features otel
```

The layer sits beneath `EnvFilter` so `RUST_LOG` still controls
volume. Instrumented spans currently cover `run_with_hedge`,
`refresh_quotes`, `refresh_balances`, `reconcile`, and
`dispatch_route` — the hot engine paths operators care about
when tracing a symbol's pipeline latency. Unset the env var (or
build without `--features otel`) to get zero-network behaviour.

## Helm chart

`deploy/helm/mm/` is the canonical k8s install path. Wraps the
reference `deploy/k8s/deployment.yaml` with templated image
reference, resource quotas, secret injection mode, PVC sizing,
PDB, ServiceMonitor toggle, and a safe `Recreate` strategy (no
rolling update — two pods on the same symbol double-book).

### Install

```bash
# Dev / smoke — inline secret from values-dev.yaml (no real creds)
helm install mm-dev ./deploy/helm/mm \
  -f ./deploy/helm/mm/values.yaml \
  -f ./deploy/helm/mm/values-dev.yaml \
  --namespace mm-dev --create-namespace

# Prod — secret managed by ESO / SealedSecrets / SOPS,
# referenced by name via `secret.existingSecretName`
helm install mm-prod ./deploy/helm/mm \
  -f ./deploy/helm/mm/values.yaml \
  -f ./deploy/helm/mm/values-prod.yaml \
  --namespace mm --create-namespace
```

### Key values

| Path | Purpose |
|------|---------|
| `image.repository` / `image.tag` | Pinned image — never `latest` in prod |
| `replicaCount` | **Always 1.** Hard invariant per symbol |
| `env.MM_MODE` | `paper` / `live` / `smoke` |
| `secret.create` | `true` for dev (inline), `false` for prod (ESO/Vault-managed) |
| `secret.existingSecretName` | Prod: pre-provisioned Secret name |
| `config.body` | Inline TOML. Copy from `config/default.toml` and trim |
| `persistence.size` | Checkpoint / audit-log PVC size (default 20Gi) |
| `serviceMonitor.enabled` | Prometheus-Operator integration; off for docker-compose stacks |
| `pdb.enabled` / `pdb.minAvailable` | Block evictions during drains |

### Upgrade + rollback

```bash
# Confirm the diff before applying
helm diff upgrade mm-prod ./deploy/helm/mm \
  -f ./deploy/helm/mm/values.yaml \
  -f ./deploy/helm/mm/values-prod.yaml

helm upgrade mm-prod ./deploy/helm/mm \
  -f ./deploy/helm/mm/values.yaml \
  -f ./deploy/helm/mm/values-prod.yaml

# Rollback to a prior revision if the new deploy misbehaves —
# the engine restores from the last checkpoint on the PVC so
# inventory / PnL survive the flip.
helm rollback mm-prod <revision>
```

Pod restarts automatically on any ConfigMap / Secret change
because the Deployment annotates `checksum/config` +
`checksum/secret` off the rendered templates.

### Secrets — production-safe wiring

`secret.create: true` is for local / dev ONLY. Inline values end
up in `helm history` and the backing store (etcd for the release
ledger), which is not a credential-grade safe. For production:

1. Pre-provision the Secret via External Secrets Operator,
   SealedSecrets, SOPS+KSOPS, or Vault Agent.
2. Set `secret.create: false` and `secret.existingSecretName:
   <name>` in `values-prod.yaml`.
3. The Deployment's `envFrom.secretRef` pulls every key into
   the container env at pod start — no secret ever touches
   Helm.
