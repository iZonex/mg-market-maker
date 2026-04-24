# Alerts & Webhooks

How to route operational events (kill-switch escalation, SLA breach, manipulation score spike) to Telegram and webhook subscribers.

Two triggering layers, two real-time delivery channels (Telegram + webhooks), plus an SMTP path used primarily for compliance report delivery:

| Layer | Source | Purpose |
|-------|--------|---------|
| **Alert rules** | `AlertRule` in DashboardState | Threshold-based alerts — PnL drop, spread widen, inventory breach, uptime drop, fill-rate drop |
| **Audit + risk events** | Engine-side audit trail (`AuditEventType`) | Narrative events — kill-switch, surveillance, graph-deploy, SLA violation, etc. |

---

## 1. Telegram bot

### 1.1 Setup

1. Create a Telegram bot via [@BotFather](https://t.me/BotFather). Copy the token.
2. Create (or pick) a chat. Add the bot as a member; if it's a channel, make the bot admin.
3. Discover the chat ID — send any message to the chat, fetch `https://api.telegram.org/bot<TOKEN>/getUpdates`, read `result[].message.chat.id`.
4. Set env vars on the server:
   ```bash
   export MM_TELEGRAM_TOKEN="123456:ABC-DEF..."
   export MM_TELEGRAM_CHAT="-1001234567890"
   ```
5. Restart the server. On boot, the bot verifies its credentials via `getMe` and logs a line like `telegram bot verified: @mm_alerts_bot`. Failure here = bad token; alerts silently drop.

### 1.2 Severity levels

Alerts carry a severity from 3 tiers:

| Severity | Emoji | Example triggers |
|----------|-------|------------------|
| **info** | ℹ️ | Graph deployed, manual incident ack, vault rotation |
| **warn** | ⚠️ | Kill-switch L1-L2, spread > SLA threshold, calibration trial with worse loss |
| **critical** | 🚨 | Kill-switch L3-L5, VaR breach, reconciliation mismatch, audit-chain verify fail |

Filter (suggested, client-side): set your operator-noise filter in the rule's severity → only `warn+` alerts hit Telegram while `info` goes to webhooks. See §3.

### 1.3 Dedup

Identical alerts firing in rapid succession collapse. Dedup happens in `crates/dashboard/src/alerts.rs`; consult that module for the exact key + window since the rules have evolved.

### 1.4 Two-way commands

The bot listens for commands **only** from the single `MM_TELEGRAM_CHAT` id. Messages from any other chat are silently dropped (no "unauthorized" reply, so a leaked token does not confirm the bot is live).

| Command | Effect |
|---------|--------|
| `/status` | Engine posts back a status summary (kill level, PnL, inventory, SLA) |
| `/positions` or `/pos` | Per-symbol position snapshot |
| `/help` | List supported commands |
| `/pause SYMBOL` | Stop emitting new orders for `SYMBOL` (kill-switch L2 on that symbol) |
| `/resume SYMBOL` | Resume quoting a paused symbol |
| `/stop` | Trigger kill-switch L5 disconnect (fleet-wide) |
| `/force_exit SYMBOL` | Emergency flatten a symbol's inventory |

Polling: Telegram `getUpdates` long-poll, 30-second timeout; the bot ignores updates that arrived before it started (control commands reflect current intent, not backlog).

---

## 2. Webhooks

### 2.1 Setup

Webhooks are managed per-client. Admin adds platform-level URLs via `POST /api/admin/webhooks`; clients manage their own via `POST /api/v1/client/self/webhooks`. Minimal body:

```json
{ "url": "https://ops.acme.com/hooks/mm-alerts" }
```

The current dispatcher (`crates/dashboard/src/webhooks.rs::WebhookDispatcher`) stores URLs per client and fires events to every URL in the list.

### 2.2 Delivery envelope

Each event posts a JSON body with `{ timestamp: RFC3339, event: <tagged enum> }`:

```json
{
  "timestamp": "2026-04-24T12:34:56.789Z",
  "event": {
    "kind": "kill_switch_escalated",
    "symbol": "BTCUSDT",
    "from_level": 2,
    "to_level": 3,
    "reason": "inventory breach"
  }
}
```

Headers: `Content-Type: application/json`. **No HMAC signing header today** — downstream verification relies on network-layer auth (TLS + IP allowlist). If HMAC-per-delivery signing is required for your compliance posture, track it as a TODO (`crates/dashboard/src/webhooks.rs` is where to add it).

### 2.3 Delivery semantics

- **Single POST, 5 s timeout.** No automatic retry, no backoff queue, no DLQ in the current implementation. A failing webhook is logged once (`warn!(url, status, "webhook delivery failed")`) and the dispatcher moves on.
- **Delivery log** — the last `DELIVERY_LOG_CAP` attempts are kept in memory per client for `/api/v1/client/self/webhook-deliveries` (and the admin equivalent). Inspect here for "why didn't my hook fire".
- If you need durable retry or DLQ, put a message-queue intermediary in front of the final receiver (e.g. `mmaker → SQS → your-consumer`) and treat the SQS as the retry fabric.

### 2.4 Client self-test

A client can verify their webhook flow end-to-end:
```
POST /api/v1/client/self/webhooks/test
```
Server fires a synthetic event to every URL the client has registered and returns the outcomes (status code, latency, response body). Useful during onboarding.

---

## 3. Alert rules (threshold-based)

Rules live in `DashboardState.alert_rules` — operator-editable via Rules page or POST `/api/admin/alerts/rules`.

```rust
AlertRule {
  id: String,           // stable id; UI editor uses this as dedup key
  description: String,
  condition: AlertCondition,
  enabled: bool,
}
```

Condition shapes:

| Condition | Fires when |
|-----------|-----------|
| `PnlBelow { threshold }` | symbol's total PnL < threshold (quote asset) |
| `SpreadAbove { threshold_bps }` | symbol's spread_bps > threshold for 1+ ticks |
| `InventoryAbove { threshold }` | abs(inventory) > threshold (base asset) |
| `UptimeBelow { threshold_pct }` | 24h presence % < threshold |
| `FillRateBelow { threshold_per_min }` | fills-per-minute < threshold |

Rules evaluate every 30 s on the dashboard heartbeat. A rule firing writes an `AlertFired` audit event + dispatches to all subscribed channels (Telegram + every matching webhook).

### 3.1 Per-channel routing

Each rule can optionally scope its delivery:
```json
{
  "id": "pnl-10k-drop",
  "description": "Alert ops when PnL drops below -10k USDT",
  "condition": { "type": "PnlBelow", "threshold": "-10000" },
  "enabled": true,
  "channels": ["telegram:-1001234567890", "webhook:oncall-pager"]
}
```
Omitted `channels` → broadcast to every subscriber matching the rule's severity (default `warn`).

---

## 4. Control-plane event types

The engine emits audit events defined in `crates/risk/src/audit.rs::AuditEventType`. A non-exhaustive list of the interesting ones that downstream subscribers typically care about:

| Audit event | Typical severity | When it fires |
|-------------|------------------|---------------|
| `KillSwitchEscalated` | warn (L1-L2) / critical (L3+) | Guard trips OR manual operator escalation |
| `KillSwitchReset` | info | Admin resets from L5 |
| `CircuitBreakerTripped` | critical | Stale book / spread-wide / feed-dead |
| `InventoryLimitHit` | critical | abs(inventory) exceeds configured max |
| `InventoryDriftDetected` | warn | Internal inventory vs venue reconcile mismatch |
| `BalanceReconciled` | info | Every reconcile tick with summary |
| `StrategyGraphDeployed` / `StrategyGraphRolledBack` / `StrategyGraphDeployRejected` | info / warn | Graph lifecycle |
| `StrategyGraphRestrictedDeployAcked` | warn | Pentest graph deployed |
| `SurveillanceAlert` | warn / critical (per score) | Manipulation-score breach |
| `SlaViolation` | warn | Uptime / spread compliance below threshold |
| `HedgeBasketRecommended` | info | Hedge optimizer proposes a new basket |
| `VarGuardThrottleApplied` | warn | Per-strategy-class VaR throttle |
| `LoginSucceeded` / `LoginFailed` | info / warn | Auth-layer audit |
| `EngineStarted` / `EngineShutdown` | info | Lifecycle |
| `CheckpointSaved` | info | Crash-recovery snapshot taken |

Not every audit event fans out to Telegram / webhooks automatically — the current dispatch path is driven by **alert rules** (§3) and the threshold-based evaluator. If you want a specific audit event type to push a webhook, the wiring is in `DashboardState::record_incident` + the alert evaluator; review/extend there.

---

## 5. Email (SMTP)

Optional third channel. Configure via `[email]`:
```toml
[email]
smtp_host = "smtp.sendgrid.net"
smtp_port = 587
smtp_user = "apikey"
smtp_password = "env:MM_SMTP_PASSWORD"
from = "alerts@mmaker.example.com"
recipients = ["oncall@acme.com"]
min_severity = "critical"
```

Used for critical-tier backstop when Telegram is unreachable. Retry policy is the `lettre` crate's default — one attempt; if the SMTP relay is down at send time, the delivery fails and is logged. Operator re-runs the report or fixes the relay.

---

## 6. Test the full chain

Before shipping alert config to prod:

1. **Telegram** — confirm the bot verified at boot (log line `telegram bot verified: @<name>`); send a manual `/status` from an authorized chat to verify the two-way path
2. **Webhooks** — Platform page → each webhook → Test, or `POST /api/v1/client/self/webhooks/test` for the per-client self-check
3. **Email** — fire a monthly MiCA report to a test recipient and verify delivery + signed manifest
4. **End-to-end** — temporarily set a permissive rule (e.g. `SpreadAbove { threshold_bps: "1" }`) so it trips on normal traffic, let it fire once, verify delivery on all channels, then restore the threshold

Inspect `/api/v1/client/self/webhook-deliveries` (or the admin equivalent) to see the last N delivery attempts.

---

## 7. Common pitfalls

- **Telegram rate limit** — the Bot API caps ~30 msg/sec globally; a noisy rule with 20 symbols tripping simultaneously can hit this. Consider filtering by severity and reserving critical for a dedicated on-call chat.
- **Webhook timeouts count as failure and there's no retry** — the dispatcher fires one POST with a 5 s timeout and moves on. If your downstream can be slow under load, put a queue between us and them (we push to SQS, they drain at their own pace).
- **Delivery log is in-memory only** — dispatcher keeps the last `DELIVERY_LOG_CAP` attempts per client. A restart drops them. Persist externally if you need long history.
- **`MM_TELEGRAM_TOKEN` rotation requires restart** — hot-reload covers alert rules, not the bot token itself.
- **No HMAC signing on webhook bodies today** — if your receiver needs HMAC for impersonation protection, add it in `webhooks.rs::WebhookDispatcher::dispatch` (currently just sends `Content-Type: application/json`).
