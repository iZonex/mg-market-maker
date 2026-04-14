# Deployment Guide

## Quick Start (Paper Trading)

```bash
git clone https://github.com/your-org/market-maker.git
cd market-maker
cargo run -p mm-server
```

This starts in paper mode by default with localhost exchange.

## Docker (Recommended for Production)

### Prerequisites
- Docker and Docker Compose

### Steps

1. **Configure:**
   ```bash
   cp config/default.toml config/production.toml
   # Edit config/production.toml with your exchange URLs
   ```

2. **Set secrets:**
   ```bash
   cat > .env << 'EOF'
   MM_API_KEY=your-exchange-api-key
   MM_API_SECRET=your-exchange-api-secret
   MM_MODE=live
   MM_TELEGRAM_TOKEN=your-telegram-bot-token
   MM_TELEGRAM_CHAT=your-telegram-chat-id
   EOF
   ```

3. **Start:**
   ```bash
   docker compose up -d
   ```

4. **Monitor:**
   - Dashboard: http://localhost:9090/api/status
   - Prometheus: http://localhost:9091
   - Grafana: http://localhost:3000 (admin/admin)
   - Audit trail: `docker compose exec market-maker cat data/audit/btcusdt.jsonl`

### Endpoints

| URL | Description |
|-----|-------------|
| `GET /health` | Health check |
| `GET /api/status` | All symbols state |
| `GET /api/v1/positions` | Current positions |
| `GET /api/v1/pnl` | PnL breakdown |
| `GET /api/v1/sla` | SLA compliance |
| `GET /api/v1/report/daily` | Daily report |
| `GET /metrics` | Prometheus metrics |

## Native Binary

```bash
cargo build --release
./target/release/mm-server
```

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `MM_CONFIG` | No | Config file path (default: `config/default.toml`) |
| `MM_API_KEY` | For live | Exchange API key |
| `MM_API_SECRET` | For live | Exchange API secret |
| `MM_MODE` | No | `live` or `paper` (default: `live`) |
| `MM_TELEGRAM_TOKEN` | No | Telegram bot token |
| `MM_TELEGRAM_CHAT` | No | Telegram chat ID |
| `RUST_LOG` | No | Log level (default: `info`) |

## Security Checklist

- [ ] API keys have **no withdrawal permission**
- [ ] IP whitelisting enabled on exchange
- [ ] Dashboard port restricted (firewall/VPN)
- [ ] `.env` file has `chmod 600`
- [ ] Audit trail directory is backed up
- [ ] Telegram bot token is kept secret
- [ ] Running as non-root user (Docker does this)

## Fast protocol paths

Binance, Bybit, and HyperLiquid each expose a low-latency WebSocket
path for order entry in addition to REST. The protocol-coverage epic
(see `docs/epics/protocols-coverage.md`) wires these paths with REST as
a transparent fallback so a WS disconnect degrades to REST without
surfacing an error to the engine.

### Per-venue status

| Venue | WS order entry | FIX | How to enable |
|---|---|---|---|
| Binance | wired (`BinanceConnector::enable_ws_trading`) | scaffold (codec + session engine) | call `enable_ws_trading("wss://ws-api.binance.com/ws-api/v3")` on the connector before handing it to the engine |
| Bybit | scaffold (adapter exists, not yet routed) | scaffold | pending live auth verification |
| HyperLiquid | wired (`HyperLiquidConnector::enable_ws_trading`) | — | call `enable_ws_trading()` on the connector |

REST remains the default path. The fast path is opt-in — if
`enable_ws_trading` is never called, every order entry flows through
REST exactly as before.

### Rollback

Disabling a fast path is zero-downtime: restart the server without the
`enable_ws_trading` call and the connector returns to the REST-only
code path with no state migration.

### Observability

Add this Prometheus metric to your dashboard to compare REST vs WS
latency side by side during rollout:

```
histogram_quantile(0.9,
  rate(mm_order_entry_duration_seconds_bucket{}[5m]))
```

Labels: `venue`, `path` (`rest`|`ws`), `method` (`place_order`, …).

### Secrets

HyperLiquid uses a wallet private key, not an HMAC secret. Set
`MM_API_SECRET` to the hex-encoded 32-byte private key (0x prefix
optional). `MM_API_KEY` is ignored for HyperLiquid — the Ethereum
address is derived from the private key.

## Operator next steps (deferred from the protocol-coverage epic)

Three items were intentionally left open because they require live
testnet credentials that were unavailable during implementation.
Close them in order once credentials are provisioned.

### 1. Capture real venue fixtures

`crates/protocols/ws_rpc/fixtures/` and the per-venue adapter test
modules currently use hand-crafted JSON based on spec documents.
Replace them with real captured traffic from:

- Binance WS API testnet (`wss://testnet.binance.vision/ws-api/v3`)
- Bybit V5 WS Trade testnet (`wss://stream-testnet.bybit.com/v5/trade`)
- HyperLiquid testnet (`wss://api.hyperliquid-testnet.xyz/ws`)

Hand-crafted tests exercise the wire format correctly but will miss
fields the venue adds that the spec omits (request-ids, connection
identifiers, etc.). A short ~10-minute capture per venue is enough
to replace all fixtures.

### 2. Benchmark REST vs WS latency per venue

`docs/protocols/_comparison.md` contains a placeholder table for
`p50 / p90 / p99` order-entry latency per `(venue × path)`. Populate
it by running the capability-audit harness under `cargo test` against
each testnet:

```bash
MM_MODE=paper \
MM_API_SECRET=<testnet-privkey> \
MM_BENCH_VENUE=hyperliquid_testnet \
cargo test -p mm-exchange-hyperliquid -- --nocapture --ignored
```

The `mm_order_entry_duration_seconds` Prometheus histogram is already
labelled `(venue, path, method)`, so a side-by-side comparison becomes
a single Grafana query against a stored Prometheus dump.

### 3. Wire Bybit WS Trade into `BybitConnector::place_order`

`crates/exchange/bybit/src/ws_trade.rs` ships a tested
`BybitWsTrader` adapter with URL-based auth. The integration into
`BybitConnector`'s `place_order` / `cancel_order` / `cancel_all_orders`
fallback chain is deferred until the exact V5 Trade authentication
mechanism is verified against testnet — Bybit's documentation is
inconsistent between revisions on whether auth is URL-param or
`op: auth` based.

The closing sequence is identical to the HyperLiquid and Binance
integrations already in tree (`HyperLiquidConnector::enable_ws_trading`
and `BinanceConnector::enable_ws_trading`): add a `ws_trader:
Option<Arc<BybitWsTrader>>` field, an `enable_ws_trading` constructor
helper, and mirror the WS-first-with-REST-fallback routing pattern in
the three order-entry methods.

### 4. FIX venue adapters

`crates/protocols/fix` ships a tested codec and a session engine, but
no venue adapter yet consumes them. When a FIX-supporting venue comes
online (Binance spot β, Deribit, OKX, Coinbase Prime), add an adapter
under `crates/exchange/<venue>/src/fix_trade.rs` on the same shape as
the existing WS adapters:

1. Venue-specific logon payload (Ed25519 for Binance, password
   field for Bybit / Deribit).
2. Business message builders that reuse
   `mm_protocols_fix::Message::new_order_single` etc.
3. `FixSession::on_message` drives the session state; the adapter
   maps `SessionAction::DeliverApp` into `MarketEvent::Fill` /
   `MarketEvent::OrderUpdate` for the engine.

No new work in `protocols/fix` is required for the first venue — the
session engine already handles logon, heartbeat, gap detection, and
sequence-number persistence via the `SeqNumStore` trait.
