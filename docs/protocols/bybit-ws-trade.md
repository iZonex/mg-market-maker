# Bybit V5 WebSocket Trade API

**Status:** target for implementation — Sprint 3
**Canonical spec:** <https://bybit-exchange.github.io/docs/v5/websocket/trade/guideline>

## Purpose

Dedicated low-latency WebSocket channel for order entry only (not market data). One-shot HMAC auth on logon; all subsequent requests are implicitly authenticated by session. Typical latency: REST ~30–80 ms → WS Trade ~5–15 ms.

## Endpoints

| Environment | URL |
|---|---|
| Mainnet | `wss://stream.bybit.com/v5/trade` |
| Testnet | `wss://stream-testnet.bybit.com/v5/trade` |

## Auth

After connecting, the client sends an `auth` op. Once the server accepts it, the session is authenticated until disconnect.

Payload to sign (HMAC-SHA256 hex over the API secret):
```
val = "GET/realtime" + expires
```
where `expires` is a Unix millisecond timestamp in the future (usually `now + 1000`).

Auth frame:
```json
{
  "op": "auth",
  "args": [
    "<api_key>",
    1700000001000,
    "<hmac_sha256_hex(secret, 'GET/realtime' + expires)>"
  ]
}
```

Response:
```json
{
  "success": true,
  "ret_msg": "",
  "op": "auth",
  "conn_id": "..."
}
```

On failure: `"success": false, "ret_msg": "<reason>"`.

## Request shape

```json
{
  "reqId": "<unique string>",
  "header": {
    "X-BAPI-TIMESTAMP": "1700000000000",
    "X-BAPI-RECV-WINDOW": "5000",
    "Referer": "optional-broker-tag"
  },
  "op": "order.create",
  "args": [
    {
      "category": "linear",
      "symbol": "BTCUSDT",
      "side": "Buy",
      "orderType": "Limit",
      "qty": "0.01",
      "price": "42000.0",
      "timeInForce": "PostOnly",
      "orderLinkId": "<cloid>"
    }
  ]
}
```

`reqId` must be unique within the session; we will use monotonic `u64` rendered as string.

## Response shape

```json
{
  "reqId": "42",
  "retCode": 0,
  "retMsg": "OK",
  "op": "order.create",
  "data": {
    "orderId": "1234567890",
    "orderLinkId": "our-cloid"
  },
  "header": {
    "Timenow": "1700000000123",
    "Traceid": "...",
    "X-Bapi-Limit": "10",
    "X-Bapi-Limit-Status": "9"
  },
  "connId": "..."
}
```

Error:
```json
{
  "reqId": "42",
  "retCode": 10001,
  "retMsg": "params error: price should be a divisor of tick size",
  "op": "order.create",
  "data": null,
  "header": {...}
}
```

## Ops in scope

| Op | Purpose |
|---|---|
| `auth` | Authenticate session (once) |
| `ping` | Keepalive |
| `order.create` | New single order |
| `order.amend` | Amend price/qty in place (keeps queue priority) |
| `order.cancel` | Cancel single order |
| `order.create-batch` | Place up to 20 orders in one frame |
| `order.amend-batch` | Amend up to 20 |
| `order.cancel-batch` | Cancel up to 20 |

Batch ops return `data.list[]` with one entry per sub-order, each with its own `retCode`.

## Rate limits

Per-UID, category-specific. Shared across WS Trade, REST, and FIX for the same UID.

| Category | Limit |
|---|---|
| Spot orders | ⚠ 20 req/s |
| Linear/Inverse orders | ⚠ 10 req/s per UID |
| Options | ⚠ 10 req/s per UID |
| Batch op | Counts as N single ops |

Current-window usage comes back in response headers `X-Bapi-Limit` / `X-Bapi-Limit-Status`.

## Ping / pong

- Client sends `{"op": "ping", "args": [<unix_ms>], "req_id": "..."}` **every 20 seconds**.
- Server replies `{"op": "pong", "ret_msg": "pong", "req_id": "..."}`.
- Server drops connections idle for **60 seconds**.
- Protocol-level WebSocket ping frames are also acceptable but Bybit prefers the app-level op.

## Session lifecycle & reconnect

- **Max connection duration: 24 hours.** Bybit closes sockets older than that.
- On disconnect, in-flight `order.create` requests **may or may not** have reached the matching engine — we must reconcile via REST `/v5/order/realtime` on reconnect.
- **Bybit does not cancel orders on WS disconnect** (different from some venues). Resting orders persist.
- Max 500 private WS connections per UID.
- `orderLinkId` (our cloid) is idempotent for 10 minutes — sending the same `orderLinkId` twice within that window returns the original order, not a duplicate.

## Error codes we care about

| retCode | Meaning | Action |
|---|---|---|
| `0` | Success | — |
| `10001` | Params error / tick-size violation | Fatal — fix order builder |
| `10002` | Request not authorised | Transient — re-auth |
| `10003` | API key invalid | Fatal |
| `10004` | Sign auth error | Fatal |
| `10006` | Rate limit | Transient — back-pressure |
| `10016` | Service unavailable | Transient — retry |
| `110001` | Order does not exist | Race — ignore |
| `110007` | Insufficient balance | Business — propagate |
| `110020` | Too many orders | Transient — back-pressure |

## Gotchas

- **`category` is mandatory** in every request — forgetting it returns a cryptic "params error".
- Price/qty must be strings (even for whole numbers) to preserve precision.
- `timeInForce: "PostOnly"` is the MM default; also supported: `GTC`, `IOC`, `FOK`.
- `X-BAPI-TIMESTAMP` must be within `recvWindow` of server time — clock sync matters.
- Responses can arrive out of order relative to request send order. Correlation via `reqId` is mandatory.

## Sample fixtures

To be captured during Sprint 1 against testnet:
- `fixtures/bybit/auth_success.json`
- `fixtures/bybit/auth_reject_bad_sig.json`
- `fixtures/bybit/order_create_success.json`
- `fixtures/bybit/order_create_reject_tick_size.json`
- `fixtures/bybit/order_create_batch_mixed.json`
- `fixtures/bybit/rate_limit_exceeded.json`

## Open items (verify in Sprint 1)

- ⚠ Confirm current category-specific rate limits — they've changed between V5 revisions.
- ⚠ Exact behaviour when the same `reqId` is sent twice in one session — reject or echo previous response?
- ⚠ Whether `order.amend-batch` is available for spot or only derivatives.
