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

2. **Set process secrets (not venue credentials):**
   ```bash
   cat > .env << 'EOF'
   # Dashboard JWT signing + HMAC-level secrets
   MM_AUTH_SECRET=<32+ random bytes>
   MM_MASTER_KEY=<64 hex chars = 32 bytes, for vault encryption>
   MM_MODE=live
   # Optional — Telegram alerts + 2-way control
   MM_TELEGRAM_TOKEN=your-telegram-bot-token
   MM_TELEGRAM_CHAT=your-telegram-chat-id
   # Optional — TLS
   MM_TLS_CERT=/path/to/cert.pem
   MM_TLS_KEY=/path/to/key.pem
   EOF
   ```

   **Venue credentials are NOT set via env vars.** They go into the
   encrypted vault — either via Vault UI (`/vault` on the dashboard) or
   by POST to `/api/admin/vault/{name}` on the controller. Each entry
   carries `kind=exchange`, `values={api_key, api_secret}` (encrypted
   on disk), and metadata tagging the venue + product. The controller
   pushes them to each agent via signed `PushedCredential` messages on
   register + rotate.

3. **Start:**
   ```bash
   docker compose up -d
   ```

4. **Seed vault + operators** (first-run only):
   - Open `http://localhost:9090/` → First-install wizard creates the
     admin user and generates `MM_AUTH_SECRET` / `MM_MASTER_KEY` if
     missing.
   - Add venue entries via Admin → Vault.

5. **Monitor:**
   - Dashboard: http://localhost:9090/api/status
   - Prometheus: http://localhost:9090/metrics (scrape via federated Prom)
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
| `MM_AUTH_SECRET` | Yes | JWT HMAC signing (32+ bytes). Server refuses to boot without it |
| `MM_MASTER_KEY` / `MM_MASTER_KEY_FILE` | Recommended | 32-byte AES-256-GCM key for the vault. If absent, generated on first boot and persisted to disk |
| `MM_CHECKPOINT_SECRET` | Recommended | Signs engine checkpoints (falls back to `MM_AUTH_SECRET` when unset) |
| `MM_MODE` | No | `live` or `paper` (default from config) |
| `MM_HTTP_ADDR` | No | Dashboard bind address (default `0.0.0.0:9090`) |
| `MM_AGENT_WS_ADDR` | No | Control-plane WS bind address |
| `MM_TLS_CERT` / `MM_TLS_KEY` | Prod | TLS cert + key paths (enable HTTPS + wss) |
| `MM_VAULT` | No | Vault JSON path (default `vault.json`) |
| `MM_USERS` | No | Users JSON path (default `users.json`) |
| `MM_APPROVALS` | No | Approval store path (default `approvals.json`) |
| `MM_TUNABLES` | No | Runtime tunables JSON path |
| `MM_AUDIT_PATH` | No | Audit log directory path |
| `MM_AUDIT_ARCHIVE_CMD` | No | External command to archive rotated audit files |
| `MM_TELEGRAM_TOKEN` | No | Telegram bot token (for alerts + 2-way commands) |
| `MM_TELEGRAM_CHAT` | No | Authorized Telegram chat id for commands |
| `MM_REQUIRE_TOTP_FOR_ADMIN` | No | Set `1` to force 2FA on admin login |
| `MM_REQUIRE_TOTP_ADMIN_BYPASS` | No | Escape hatch for first-boot bootstrap admin |
| `MM_TOTP_ISSUER` | No | Issuer label shown by authenticator apps |
| `MM_ALLOW_RESTRICTED` | No | `yes-pentest-mode` unlocks pentest strategies |
| `MM_FRONTEND_DIR` | No | Override bundled frontend `dist/` path |
| `MM_DASHBOARD_CORS_ORIGINS` | No | Comma-separated CORS origin allowlist |
| `RUST_LOG` | No | Log level (default: `info`) |

**Venue API keys live in the vault, not in env vars.** See the vault flow in §"Set process secrets" above.

## Security Checklist

- [ ] Venue API keys have **no withdrawal permission**
- [ ] Venue keys are in the vault (admin UI) — **not** in any env var or config file
- [ ] IP whitelisting enabled on the exchange side
- [ ] Dashboard port restricted (firewall / VPN) OR TLS + strong auth
- [ ] `MM_AUTH_SECRET` is 32+ random bytes, stored with `chmod 600`
- [ ] `MM_MASTER_KEY` is 32 random bytes (64 hex), stored with `chmod 600`
- [ ] Audit trail directory is backed up (encrypted)
- [ ] Admin accounts have 2FA enrolled (`MM_REQUIRE_TOTP_FOR_ADMIN=1`)
- [ ] TLS cert + key provisioned for prod (`MM_TLS_CERT` / `MM_TLS_KEY`)
- [ ] Telegram bot token is restricted (one authorized chat)
- [ ] Running as non-root user (Docker does this)

## Fast protocol paths

Binance, Bybit, and HyperLiquid each expose a low-latency WebSocket
path for order entry in addition to REST. The connector layer wires
these paths with REST as a transparent fallback, so a WS disconnect
degrades to REST without surfacing an error to the engine.

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

## Operator follow-ups

Three items are intentionally left open because they require live
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

## High-performance deployment (Epic E)

A market maker that quotes thousands of orders per second on a real
prop-desk-grade box leaves 20-40% of latency on the table when run
on a default Linux install. This section covers the seven
operational levers every production prop desk pulls before going
live, in rough effort-to-impact order. Skip nothing — each lever is
small individually but the cumulative effect on tail latency is
material.

A complete validated systemd unit template that bundles most of
these levers is shipped at `deploy/systemd/mm.service`. The
sub-sections below explain *why* each setting in that template
matters so operators can adjust intelligently for their box.

> **Scope.** This section assumes a dedicated Linux box (bare-metal
> or a VM with passthrough CPUs). Mac, Windows, and shared-tenant
> cloud VMs cannot pull most of these levers; for those targets
> the Docker deployment story above is the right path.

### 1. File descriptor limits

A multi-venue MM holds dozens of WebSocket connections per venue
plus the dashboard HTTP server plus the audit-log file handle plus
the Prometheus exporter. The default Linux ulimit of 1024 file
descriptors is too low and will surface as cryptic "Too many open
files" errors in the WS reconnect path on a busy day.

**Set both the soft and hard limit to 65535 (or higher):**

```bash
# Per-process via systemd
LimitNOFILE=65535

# Per-shell via ulimit
ulimit -n 65535

# System-wide in /etc/security/limits.conf
*  soft  nofile  65535
*  hard  nofile  65535
```

**Verify:**

```bash
# Inside the running process
cat /proc/$(pidof mm-server)/limits | grep "Max open files"
# Should show: 65535  65535  files
```

### 2. Disable swap

Swap on a low-latency hot path is catastrophic — a single page-in
during a market spike can stall the WS read loop for tens of
milliseconds. Production MM boxes always disable swap entirely.

```bash
# Runtime: turn swap off immediately
sudo swapoff -a

# Persistent: comment out swap entries in /etc/fstab
sudo sed -i.bak '/\sswap\s/s/^/# /' /etc/fstab

# Kernel tunable: discourage paging even when swap is on
echo 'vm.swappiness=0' | sudo tee /etc/sysctl.d/99-mm.conf
sudo sysctl -p /etc/sysctl.d/99-mm.conf
```

**Verify:**

```bash
free -h
# Swap line should show 0B / 0B / 0B
```

### 3. OOM score adjustment

Linux's out-of-memory killer ranks processes by `oom_score` and
kills the highest scorer when memory pressure hits. The MM should
NEVER be a candidate — kill the dashboard, the metrics scraper,
the log shipper, but never the engine.

**Set `OOMScoreAdjust=-500` in the systemd unit** (already in the
template). For non-systemd setups:

```bash
# Manually after launch
echo -500 | sudo tee /proc/$(pidof mm-server)/oom_score_adj
```

**Verify:**

```bash
cat /proc/$(pidof mm-server)/oom_score_adj
# Should show: -500
```

### 4. NUMA pinning

On any multi-socket server (and most modern single-socket boxes
with chiplet CPUs like AMD EPYC), memory access latency depends on
which NUMA node the memory was allocated from relative to the
core that touches it. Cross-NUMA access can add 100+ ns per cache
line. The MM should pin its hot threads to a single NUMA node and
allocate all its memory there.

```bash
# Discover the topology
numactl --hardware

# Pin to NUMA node 0 (typical: cores 0-15 on a 32-core dual-socket)
numactl --cpunodebind=0 --membind=0 ./target/release/mm-server
```

**In the systemd unit (already in the template):**

```ini
CPUAffinity=0-15
NUMAMask=0
NUMAPolicy=bind
```

**Verify:**

```bash
# Should show node 0 for all RSS pages
numastat -p $(pidof mm-server)
```

### 5. IRQ steering

Network card interrupts default to landing on CPU 0, which means
the MM's WS read loop competes with kernel network softirqs for
that core. The fix is to steer NIC IRQs to the cores the MM is
NOT using, so the network softirq path runs on a dedicated core
and the MM's hot path runs uninterrupted.

```bash
# Stop irqbalance (it will undo any manual steering)
sudo systemctl stop irqbalance
sudo systemctl disable irqbalance

# Find the NIC's IRQ numbers
grep -E '(eth0|ens|enp)' /proc/interrupts

# Pin each IRQ to a specific core (cores 16-31 if MM is on 0-15)
# Example for IRQ 24 → core 16 (mask 0x10000):
echo 10000 | sudo tee /proc/irq/24/smp_affinity
```

For multi-queue NICs with N RX/TX queues, distribute the IRQs
evenly across the non-MM cores. The `set_irq_affinity.sh` script
shipped with most NIC drivers (`ixgbe`, `mlx5`, etc.) automates
this.

**Verify:**

```bash
# Watch /proc/interrupts during a market open — the NIC IRQ
# counters should grow on the steered cores, not on the MM cores.
watch -d cat /proc/interrupts
```

### 6. Transparent hugepages

Hugepages reduce TLB pressure on processes with large working
sets. The MM's working set is modest (a few hundred MB at most),
but the JSON parsers and WS framing buffers benefit from hugepage
backing on the hot path.

The default Linux setting is `madvise` (hugepages on request).
Production MM boxes typically set it to `always` so every
allocation that's hugepage-eligible gets one without the
application having to call `madvise`.

```bash
# Runtime
echo always | sudo tee /sys/kernel/mm/transparent_hugepage/enabled
echo always | sudo tee /sys/kernel/mm/transparent_hugepage/defrag

# Persistent via tuned profile or systemd-tmpfiles
echo 'w /sys/kernel/mm/transparent_hugepage/enabled - - - - always' \
  | sudo tee /etc/tmpfiles.d/mm-hugepages.conf
```

**Verify:**

```bash
cat /sys/kernel/mm/transparent_hugepage/enabled
# Should show: [always] madvise never
grep AnonHugePages /proc/$(pidof mm-server)/status
# Should show non-zero KB
```

### 7. PREEMPT_RT kernel (advanced)

A real-time kernel (PREEMPT_RT patches) gives the MM bounded
worst-case scheduling latency at the cost of slightly lower
average throughput. The trade-off is worth it for tail-latency-
sensitive workloads — a non-RT kernel can park the MM thread for
1-10 ms on a context switch, which is forever in HFT terms.

PREEMPT_RT is a kernel rebuild, not a runtime tunable. The most
common way to get it is the [Ubuntu Pro Real-time Kernel](https://ubuntu.com/real-time)
or building Linux from the [`linux-rt` git tree](https://wiki.linuxfoundation.org/realtime/start).

When to use it:
- ✅ Dedicated bare-metal box, no shared workloads
- ✅ p99 latency budget < 1 ms
- ❌ Cloud VMs (kernel is provided by the hypervisor)
- ❌ Shared-tenant boxes (the RT scheduler will starve other
  workloads)

**Verify:**

```bash
uname -v | grep -i preempt_rt
# Should mention PREEMPT_RT
```

### Validated systemd unit template

A complete unit file bundling levers 1-6 (fd limits, NUMA pinning,
OOM score, hardening, accounting) is shipped at
`deploy/systemd/mm.service`. The template uses `${PLACEHOLDER}`
markers for the operator's user / group / install path / config
path; substitute them before installing:

```bash
# Substitute placeholders and install
sudo mkdir -p /opt/market-maker
sudo cp target/release/mm-server /opt/market-maker/
sudo cp config/production.toml /opt/market-maker/config.toml

sed \
  -e 's|${USER}|mm|g' \
  -e 's|${GROUP}|mm|g' \
  -e 's|${INSTALL_DIR}|/opt/market-maker|g' \
  -e 's|${CONFIG_PATH}|/opt/market-maker/config.toml|g' \
  deploy/systemd/mm.service \
  | sudo tee /etc/systemd/system/mm.service

sudo systemctl daemon-reload
sudo systemctl enable --now mm
sudo systemctl status mm
```

The template is opinionated — it assumes a dedicated box with the
MM as the primary workload. Operators on shared boxes should
remove the `CPUAffinity` / `NUMAMask` / `NUMAPolicy` lines and
relax the `OOMScoreAdjust` value upward (e.g. `-100`).

### Cumulative impact

Public benchmarks from the Cont-Stoikov 2014 paper, the LMAX
Disruptor postmortems, and the kdb+ tick performance whitepapers
suggest the cumulative impact of levers 1-6 on a default Linux
install is roughly:

| Lever | Tail-latency impact |
|---|---|
| File descriptor limits | Eliminates "Too many open files" failure mode |
| Swap disable | Eliminates 10+ ms page-in stalls during market spikes |
| OOM score | Eliminates the "MM killed during a memory spike" failure mode |
| NUMA pinning | 5-15% p99 reduction on multi-socket boxes |
| IRQ steering | 10-25% p99 reduction on busy NIC days |
| Transparent hugepages | 2-5% p99 reduction (modest but free) |
| PREEMPT_RT (level 7) | Bounds p99 to within ~10× of p50 (vs unbounded) |

None of these add up to the kernel-bypass cost bracket (DPDK,
Solarflare Onload), which is correctly deferred. They turn a bot
that *runs* into a bot a prop desk would *deploy*.
