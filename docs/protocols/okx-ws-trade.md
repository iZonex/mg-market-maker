# OKX V5 WebSocket Private (trade)

**Status:** reference only — no OKX connector yet
**Canonical spec:** <https://www.okx.com/docs-v5/en/#overview-websocket>

## Purpose

OKX's private WebSocket endpoint combines account state (positions, balances) with order entry on the same persistent connection. Order entry via `order` and batch ops.

## Endpoints

| Environment | URL |
|---|---|
| Mainnet | `wss://ws.okx.com:8443/ws/v5/private` |
| Aws (alt regional) | `wss://wsaws.okx.com:8443/ws/v5/private` |
| Demo | `wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999` |

## Auth

After connect, send a `login` op:

```json
{
  "op": "login",
  "args": [
    {
      "apiKey": "...",
      "passphrase": "...",
      "timestamp": "1700000000",
      "sign": "<base64(hmac_sha256(secret, timestamp + 'GET' + '/users/self/verify'))>"
    }
  ]
}
```

Note: OKX requires a **passphrase** (set at API key creation) in addition to key + secret.

## Request shape

```json
{
  "id": "<unique>",
  "op": "order",
  "args": [
    {
      "instId": "BTC-USDT",
      "tdMode": "cash",
      "side": "buy",
      "ordType": "limit",
      "px": "42000",
      "sz": "0.001",
      "clOrdId": "<our-cloid>"
    }
  ]
}
```

## Ops

| Op | Purpose |
|---|---|
| `order` | New single order |
| `batch-orders` | Up to 20 in one frame |
| `cancel-order` | Cancel single |
| `batch-cancel-orders` | Cancel up to 20 |
| `amend-order` | Amend price/qty |
| `batch-amend-orders` | Amend up to 20 |

## Rate limits

| Bucket | Limit |
|---|---|
| `order` | ⚠ 60 req/s per sub-account |
| `batch-orders` | ⚠ 20 req/s per sub-account |
| Connections | 30 per IP, 30 per sub-account |

## Ping / pong

- Client should send plain text `"ping"` every 25–30 seconds.
- Server replies with `"pong"`.

## Session lifecycle

- Disconnect does not cancel orders.
- `clOrdId` must be alphanumeric, max 32 chars — **our UUID hex (32 chars) fits**.
- Max message size: ⚠ 65 KB per frame.

## Notes for future implementation

When OKX becomes an active target:
- Would be a fourth WireFormat implementation on top of our `ws_rpc` abstraction.
- `login` op is very similar to Bybit's `auth` — expected to slot in cleanly.
- Passphrase adds one extra secret to env: `MM_OKX_PASSPHRASE`.
