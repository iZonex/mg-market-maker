# Binance WebSocket API (spot + USDⓈ-M futures)

**Status:** target for implementation — Sprint 3
**Canonical spec:** <https://developers.binance.com/docs/binance-spot-api-docs/web-socket-api/general-api-information>

## Purpose

Persistent-connection order entry. Replaces REST `/api/v3/order` for latency-sensitive workloads. Request/response over a single WS connection with `id` correlation. Typical round-trip latency improvement: REST ~20–50 ms → WS API ~3–10 ms.

## Endpoints

| Environment | URL |
|---|---|
| Spot mainnet | `wss://ws-api.binance.com:443/ws-api/v3` |
| Spot testnet | `wss://testnet.binance.vision/ws-api/v3` |
| USDⓈ-M futures | `wss://ws-fapi.binance.com/ws-fapi/v1` |
| USDⓈ-M futures testnet | `wss://testnet.binancefuture.com/ws-fapi/v1` |

All require TLS (`wss://`). Port 443 for spot, standard TLS.

## Auth

Two modes; we pick Ed25519 as primary (faster verification, cleaner session flow) with HMAC-SHA256 as fallback for accounts that haven't registered an Ed25519 key.

### Mode A — Ed25519 (preferred)

1. Generate an Ed25519 keypair locally once, register the public key via Binance UI.
2. On connection, send a `session.logon` request signed with the private key. The server authenticates the session; subsequent order requests do **not** need `apiKey` or `signature` on each call.
3. Session ends on disconnect; re-logon on reconnect.

```json
{
  "id": "logon-1",
  "method": "session.logon",
  "params": {
    "apiKey": "<pubkey-base64>",
    "timestamp": 1700000000000,
    "signature": "<ed25519-sig-base64>"
  }
}
```

Signature payload is the canonical query string of `apiKey`, `timestamp`, and any other params, sorted by key — see spec for exact canonicalisation rules.

### Mode B — HMAC-SHA256 (per-request)

Every order-entry request carries its own `apiKey`, `timestamp`, and `signature` params. Signature is HMAC-SHA256 of the canonical query string over the API secret. Same key used for REST works here.

## Request shape

```json
{
  "id": "<client-chosen, unique per connection>",
  "method": "order.place",
  "params": {
    "symbol": "BTCUSDT",
    "side": "BUY",
    "type": "LIMIT",
    "timeInForce": "GTC",
    "price": "42000.00",
    "quantity": "0.001",
    "newClientOrderId": "<cloid>",
    "apiKey": "...",           // Mode B only
    "signature": "...",        // Mode B only
    "timestamp": 1700000000000,
    "recvWindow": 5000
  }
}
```

`id` may be a string or an integer; must be unique within the WS connection lifetime. We will use monotonic `u64`s.

## Response shape

Success:
```json
{
  "id": "req-1",
  "status": 200,
  "result": {
    "symbol": "BTCUSDT",
    "orderId": 12569099453,
    "orderListId": -1,
    "clientOrderId": "...",
    "transactTime": 1700000000000,
    "price": "42000.00",
    "origQty": "0.001",
    "executedQty": "0.00000000",
    "status": "NEW",
    "timeInForce": "GTC",
    "type": "LIMIT",
    "side": "BUY"
  },
  "rateLimits": [
    {"rateLimitType": "REQUEST_WEIGHT", "interval": "MINUTE", "intervalNum": 1, "limit": 1200, "count": 10},
    {"rateLimitType": "ORDERS", "interval": "SECOND", "intervalNum": 10, "limit": 50, "count": 1},
    {"rateLimitType": "ORDERS", "interval": "DAY", "intervalNum": 1, "limit": 160000, "count": 1}
  ]
}
```

Error:
```json
{
  "id": "req-1",
  "status": 400,
  "error": { "code": -1021, "msg": "Timestamp for this request is outside of the recvWindow." },
  "rateLimits": [...]
}
```

HTTP-style `status` values in use: 200 (OK), 400 (bad request), 403 (WAF limit), 409 (cancelReplace partial), 418 (IP ban), 429 (rate limit), 500 (internal), 503 (service unavailable).

## Methods in scope

| Method | Purpose |
|---|---|
| `session.logon` | Start Ed25519 session |
| `session.status` | Health / rate-limit snapshot |
| `session.logout` | Graceful session end |
| `order.place` | New order |
| `order.cancel` | Cancel single order |
| `order.cancelReplace` | Atomic cancel + place (keeps place if cancel fails, configurable) |
| `order.status` | Query single order |
| `openOrders.status` | Query all open orders |
| `openOrders.cancelAll` | Cancel all open orders for a symbol |
| `account.status` | Full account snapshot with balances and commissions |

## Rate limits

Shared budget with REST:

| Limit | Value | Header to inspect |
|---|---|---|
| Request weight | 1200 / min | `X-MBX-USED-WEIGHT-1M` (REST), `rateLimits` in response |
| Orders | 50 / 10s | `X-MBX-ORDER-COUNT-10S`, `rateLimits` |
| Orders | 160,000 / day | `X-MBX-ORDER-COUNT-1D`, `rateLimits` |
| Raw requests | 6100 / 5min | — |

`rateLimits` field is included on every response → single source of truth for budget accounting in our `WsRpcClient`.

## Ping / pong

- Server sends WebSocket ping frame **every 3 minutes** (per spec).
- Client must respond with pong within **10 minutes** or the server closes.
- `tokio-tungstenite` handles frame-level ping/pong automatically — we only need to ensure the read loop is pulling frames.

## Session lifecycle & reconnect

- Disconnect fails all in-flight requests (they must be re-sent after reconnect).
- Orders persist on the server — not tied to session. Pending orders remain on the book after a WS disconnect.
- On reconnect: re-run `session.logon` (Mode A) or just resume sending with per-request signatures (Mode B).
- Max connection duration: **24 hours** (per spec) — plan an orderly reconnect before that.

## Error codes we care about

| Code | Meaning | Action |
|---|---|---|
| `-1013` | Invalid quantity / price filter | Fatal — fix order builder |
| `-1021` | Timestamp outside recvWindow | Transient (clock skew) — retry after sync |
| `-1022` | Signature invalid | Fatal — fix auth |
| `-1102` | Mandatory param missing | Fatal — fix request builder |
| `-2010` | Order rejected by matching engine | Business — propagate |
| `-2011` | Unknown order cancel | Transient race — ignore |
| `-2013` | No such order | Race with fill or cancel |
| `-2015` | Invalid API key / IP | Fatal |

## Gotchas

- Order requests must carry `timestamp`; using server time from `session.status` is safer than local clock.
- `newClientOrderId` must be unique per symbol — we already generate UUIDs, pass them through.
- `cancelReplaceMode`: `"STOP_ON_FAILURE"` (cancel must succeed) vs `"ALLOW_FAILURE"` (place even if cancel fails). We want `STOP_ON_FAILURE` for MM.
- Binance distinguishes "user data stream" (fills, account updates) from "WS API" (actions). Fills still arrive on the streaming WS at `wss://stream.binance.com:9443/ws/<listenKey>`, obtained via REST `/api/v3/userDataStream`. **Two separate WS connections** even when WS API is in use.

## Sample fixtures

To be captured during Sprint 1 against testnet:
- `fixtures/binance/logon_response.json`
- `fixtures/binance/order_place_success.json`
- `fixtures/binance/order_place_reject_filter.json`
- `fixtures/binance/order_cancel_unknown.json`
- `fixtures/binance/rate_limit_exceeded.json`

## Open items (verify in Sprint 1)

- ⚠ Exact Ed25519 signature canonicalisation — re-read spec section "Encoding and signing for Ed25519".
- ⚠ Confirm that `newClientOrderId` accepts 32-char hex (our UUID→hex form).
- ⚠ Whether WS API counts against the same `X-MBX-USED-WEIGHT-1M` budget as REST — response says yes, but verify the interaction when both paths are active.
