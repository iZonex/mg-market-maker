# HyperLiquid WebSocket `post` method

**Status:** target for implementation — Sprint 3
**Canonical spec:** <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket>

## Purpose

HyperLiquid's WebSocket supports not only market-data subscriptions but also a `post` method that tunnels arbitrary `/info` or `/exchange` calls over the same persistent connection. For our MM, the practical win is:

1. **No second connection required** — the WS we already use for `l2Book`, `trades`, `userEvents`, `orderUpdates` can also carry signed `order`, `cancelByCloid`, and `modify` actions.
2. **Single round-trip** instead of setting up a fresh TLS/HTTP session per REST call.
3. **Same signing** — EIP-712 over secp256k1 is unchanged; the envelope is different.

Typical latency: REST ~30–60 ms → WS post ~5–15 ms.

## Endpoint

Same as market data:

| Environment | URL |
|---|---|
| Mainnet | `wss://api.hyperliquid.xyz/ws` |
| Testnet | `wss://api.hyperliquid-testnet.xyz/ws` |

## Auth

No session-level auth. Each `action`-type post carries its own EIP-712 signature over the action hash, **identical to the REST `/exchange` path**. Our existing `sign_l1_action()` produces the right signature with zero changes.

`info`-type posts are public — no auth.

## Request shape

### Action post (signed write)

```json
{
  "method": "post",
  "id": 1,
  "request": {
    "type": "action",
    "payload": {
      "action": {
        "type": "order",
        "orders": [
          {
            "a": 0,
            "b": true,
            "p": "42000.0",
            "s": "0.001",
            "r": false,
            "t": { "limit": { "tif": "Alo" } },
            "c": "0xabcdef..."
          }
        ],
        "grouping": "na"
      },
      "nonce": 1700000000000,
      "signature": { "r": "0x...", "s": "0x...", "v": 27 },
      "vaultAddress": null
    }
  }
}
```

The inner `payload` is **byte-for-byte identical** to what REST `/exchange` expects. Same msgpack canonicalisation, same action hash, same EIP-712 domain separator.

### Info post (public read)

```json
{
  "method": "post",
  "id": 2,
  "request": {
    "type": "info",
    "payload": { "type": "openOrders", "user": "0x..." }
  }
}
```

`id` is a client-chosen `u64`; must be unique per WS connection.

## Response shape

Success (action):
```json
{
  "channel": "post",
  "data": {
    "id": 1,
    "response": {
      "type": "action",
      "payload": {
        "status": "ok",
        "response": {
          "type": "order",
          "data": {
            "statuses": [ { "resting": { "oid": 123456 } } ]
          }
        }
      }
    }
  }
}
```

Error:
```json
{
  "channel": "post",
  "data": {
    "id": 1,
    "response": {
      "type": "error",
      "payload": "Order rejected: insufficient margin"
    }
  }
}
```

The `channel` is literally the string `"post"` for every response; correlation happens via `data.id`.

## Subscription and post multiplex on the same socket

All frames come in the same stream. We must classify incoming frames by `channel`:

| `channel` value | Type | Router |
|---|---|---|
| `"l2Book"` | Market data | existing book parser |
| `"trades"` | Market data | existing trades parser |
| `"user"` / `"userEvents"` | Fills | existing fills parser |
| `"orderUpdates"` | Order status | existing order parser |
| `"post"` | Response to a `method: "post"` request | new — route to correlation map by `data.id` |
| `"subscriptionResponse"` | Server ack of subscribe | existing ignore/log |

Our `WsRpcClient` owns the correlation map; our existing market-data parser owns everything else. Both read from the same frame stream.

## Rate limits

Shared budget with REST `/info` and `/exchange`:

| Bucket | Limit |
|---|---|
| Per-address REST+WS weight | ⚠ ~1200 requests / minute |
| Action posts | Counted separately from info posts |
| Subscriptions | ⚠ 100 per connection |

Details vary by account tier and market-maker status. Verify current numbers from spec.

## Ping / pong

- Server does not enforce app-level ping.
- WebSocket protocol-level ping frames are sent by the server; `tokio-tungstenite` auto-pongs.
- Idle timeout: ⚠ unclear — we should send a no-op ping every 30s defensively.

## Session lifecycle & reconnect

- **Disconnect does not cancel orders** — HL orders persist on the book.
- In-flight action posts whose response was not received must be reconciled on reconnect: query `openOrders` (by our address) and look for our cloid in the result.
- There is no `cancel-on-disconnect` option on HL. If we need it, we implement a watchdog that calls `cancelByCloid` on all tracked orders after N seconds without a heartbeat — outside the scope of this protocol doc.
- No hard session lifetime. Reconnect on any socket-level error with exponential backoff.

## Error handling

HL returns errors as strings in `response.payload` when `response.type == "error"`. There is no numeric error code taxonomy — we match on substrings:

| Substring | Meaning | Action |
|---|---|---|
| `"signature"` / `"sign"` | Auth failure | Fatal — check key |
| `"nonce"` | Nonce already used | Transient — bump nonce, retry |
| `"insufficient"` | Margin / balance | Business — propagate |
| `"tick"` / `"rounded"` | Price/qty precision | Fatal — fix rounding |
| `"unknown asset"` | Bad asset index | Fatal — refresh asset map |
| `"rate limit"` | Back-pressure | Transient — slow down |

## Gotchas

- **Same action hash as REST** — we MUST NOT modify the msgpack encoding of the `action` field when wrapping for WS post. The signature is bound to those exact bytes.
- `id` is shared with subscriptions? **No** — `id` lives in `data.id` for post responses; subscriptions use `channel` names directly, no id. Safe to use any u64 range.
- Vault address: if set, the same 20-byte address used in REST signing flows through unchanged here.
- `nonce` must be monotonically increasing per address across **all** paths (REST and WS). Use a single counter (`chrono::Utc::now().timestamp_millis() as u64`) and guarantee monotonicity across paths — two parallel posts with the same millisecond timestamp are rejected.

## Fix our existing bug

`HyperLiquidConnector::capabilities()` currently sets `supports_ws_trading: false`. **This is wrong.** The capability should be `true` as part of Sprint 3.

## Sample fixtures

To be captured during Sprint 1 against testnet:
- `fixtures/hyperliquid/post_order_success.json`
- `fixtures/hyperliquid/post_order_reject_insufficient.json`
- `fixtures/hyperliquid/post_info_open_orders.json`
- `fixtures/hyperliquid/post_cancel_by_cloid.json`
- `fixtures/hyperliquid/post_error_bad_signature.json`

## Open items (verify in Sprint 1)

- ⚠ Exact rate limit bucket for posts vs REST — are they summed or separate?
- ⚠ Socket idle timeout (probe empirically on testnet).
- ⚠ Max concurrent pending posts on one `id` — assume 1 per id, but confirm server doesn't reject if we reuse ids after response arrives.
