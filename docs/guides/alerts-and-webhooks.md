# Alerts & Webhooks

How to route operational events (kill-switch escalation, SLA breach, deployment incident, calibration ready, manipulation score spike) to Telegram, webhooks, and email.

Three delivery channels, two triggering layers:

| Layer | Source | Purpose |
|-------|--------|---------|
| **Alert rules** | `AlertRule` in DashboardState | Threshold-based alerts — PnL drop, spread widen, inventory breach, uptime drop, fill-rate drop |
| **Control-plane events** | Engine audit + risk events | Narrative events — kill-switch level change, incident opened, graph deployed, vault rotated |

Each event fans out to zero or more subscribers based on severity + per-channel filters.

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

Per-chat filters (in the `[telegram]` config section) can suppress below a severity cutoff:
```toml
[telegram]
bot_token = "env:MM_TELEGRAM_TOKEN"
chats = [
  { chat_id = "-1001234567890", min_severity = "warn" },      # oncall room
  { chat_id = "-1009876543210", min_severity = "critical" },  # senior escalation
]
```

### 1.3 Dedup

Identical alerts firing in rapid succession collapse. Dedup key is `(rule_id | event_type, symbol, severity)` with a 60 s window — a spread-above-100-bps rule tripping 10× in the first minute sends one message, not ten.

### 1.4 Two-way commands

The bot listens for commands from authorized chat IDs:

| Command | Role | Behaviour |
|---------|------|-----------|
| `/status` | Any chat | Summary: kill level, PnL, inventory, SLA % for every symbol |
| `/status BTCUSDT` | Any chat | Detail for one symbol |
| `/pause BTCUSDT` | Operator-authorized chat | Escalates kill switch to L2 StopNewOrders |
| `/resume BTCUSDT` | Operator-authorized chat | Resets L2 if the breach has cleared |
| `/stop` | Admin-authorized chat | Escalates every symbol to L3 CancelAll |
| `/force_exit BTCUSDT` | Admin-authorized chat | Escalates to L4 FlattenAll |

Authorization: `telegram.authorized_chats` in config lists which chats can issue mutations. A non-authorized chat receives "authorization required" reply + the event gets logged to the login audit.

---

## 2. Webhooks

### 2.1 Setup

Webhooks live per-client (platform webhooks at the cluster level). Admin adds them via Platform page or POST `/api/admin/webhooks`:

```json
POST /api/admin/webhooks
{
  "url": "https://ops.acme.com/hooks/mm-alerts",
  "client_id": "acme",             // or null for platform-level
  "secret": "<hmac_key>",          // server signs each delivery
  "min_severity": "warn",
  "events": ["kill_switch", "incident", "reconcile_mismatch"]
}
```

Clients can self-manage their own webhooks via `/api/v1/client/self/webhooks` (same shape, `client_id` implied by token).

### 2.2 Delivery envelope

Each event posts JSON:
```json
{
  "id": "<uuid>",
  "ts": "2026-04-24T12:34:56.789Z",
  "client_id": "acme",
  "event_type": "kill_switch_escalated",
  "severity": "critical",
  "symbol": "BTCUSDT",
  "payload": { "from_level": 2, "to_level": 3, "reason": "inventory breach" }
}
```

Headers:
```
Content-Type: application/json
X-MM-Signature: sha256=<hmac(secret, body)>
X-MM-Timestamp: <unix_ms>
X-MM-Delivery-Id: <uuid>
```

### 2.3 Retry + DLQ

Delivery uses exponential backoff — 1 s, 5 s, 30 s, 5 min, 30 min, 2 h. After the last retry, the delivery lands in the **webhook dead-letter** queue; admin inspects via `/api/admin/webhooks/deliveries?status=failed`.

Test a webhook from the UI: Platform → Webhooks → Test. Sends a synthetic `webhook.test` event and shows the response body / timing.

### 2.4 Self-test endpoint (client)

A client can verify their webhook flow end-to-end:
```
POST /api/v1/client/self/webhooks/{id}/test
```
Server fires a synthetic `webhook.test` event signed with the webhook's HMAC, returns the full delivery result (status code, latency, response body). Useful during client onboarding.

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

The engine emits these event types (from `crates/risk/src/audit.rs` `AuditEventType`):

| Event | Severity | Channel default |
|-------|----------|-----------------|
| `KillSwitchEscalated` | warn (L1-L2) / critical (L3+) | telegram + webhook |
| `KillSwitchManualReset` | info | telegram + webhook |
| `StrategyGraphDeployed` | info | webhook |
| `IncidentOpened` | warn | telegram + webhook |
| `IncidentAcknowledged` | info | webhook |
| `IncidentResolved` | info | webhook |
| `VaultRotated` | info | webhook (admin audit) |
| `ReconcileMismatch` | critical | telegram + webhook |
| `HedgeBasketRecommended` | info | webhook |
| `CalibrationTrialBest` | info | webhook |
| `SurveillanceScoreBreach` | warn / critical depending on score | telegram + webhook |
| `AuthLogin` (audit) | info | — (login audit only) |
| `AuthLoginFailed` | warn | webhook (security) |

The default channel mapping is configurable per-deployment; the Rules page exposes per-event routing.

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

Used for critical-tier backstop when Telegram is unreachable. Retry + DLQ same as webhooks.

---

## 6. Test the full chain

Before shipping alert config to prod:

1. **Telegram** — `/api/admin/alerts/test-telegram` sends a test message to every configured chat
2. **Webhooks** — Platform page → each webhook → Test button
3. **Email** — `/api/admin/alerts/test-email` sends a test email
4. **End-to-end** — temporarily set `PnlBelow { threshold: 999999 }` (so it always trips), let it fire once, verify delivery on all channels, then restore the threshold

Check `/api/admin/alerts/history?limit=20` after — every fire-event is recorded with delivery outcomes.

---

## 7. Common pitfalls

- **Telegram rate limit** — the bot API caps at 30 messages/sec per chat, 20/min per group. Heavy-traffic symbols with noisy rules can hit this; consider a `min_severity = "warn"` filter and reserve critical for the on-call chat.
- **Webhook timeouts count as failure** — the dispatcher waits 10 s for a 2xx. Slow downstream (e.g. PagerDuty on a cold start) exceeds that. Raise the limit via `[webhooks] delivery_timeout_secs`.
- **Dead-letter bloat** — deliveries in DLQ don't auto-expire. Admin should periodically drain or the storage grows unbounded. A scheduled `delete_delivery_older_than(30d)` job is the pattern.
- **Config reload vs env reload** — changing `MM_TELEGRAM_TOKEN` requires restart; changing `[telegram.chats]` in config is hot-reloadable via the config-override path.
