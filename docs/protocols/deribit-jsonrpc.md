# Deribit JSON-RPC 2.0 over WebSocket

**Status:** reference only — no Deribit connector yet, future epic
**Canonical spec:** <https://docs.deribit.com/>

## Purpose

Deribit's primary API is **JSON-RPC 2.0 over a single WebSocket**. The same connection multiplexes:

- Authentication (`public/auth`)
- Market data (`public/subscribe`)
- Private subscriptions (order updates, fills, positions)
- Order entry (`private/buy`, `private/sell`, `private/cancel`, …)

There is also a REST API (`https://www.deribit.com/api/v2/`) and a FIX 4.4 gateway, but WebSocket is the canonical path.

## Endpoint

| Environment | URL |
|---|---|
| Mainnet | `wss://www.deribit.com/ws/api/v2` |
| Testnet | `wss://test.deribit.com/ws/api/v2` |

## JSON-RPC 2.0 wire format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "private/buy",
  "params": {
    "instrument_name": "BTC-PERPETUAL",
    "amount": 10,
    "type": "limit",
    "price": 42000,
    "post_only": true,
    "label": "our-cloid"
  }
}
```

Response:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "order": {
      "order_id": "ETH-123456",
      "order_state": "open",
      "price": 42000.0,
      ...
    },
    "trades": []
  },
  "usIn": 1700000000000000,
  "usOut": 1700000000000123,
  "usDiff": 123,
  "testnet": false
}
```

Error:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": { "code": 10004, "message": "invalid_arguments", "data": {...} }
}
```

`usIn`, `usOut`, `usDiff` give microsecond-precision latency breakdown — valuable for benchmarking.

## Auth

```json
{
  "jsonrpc": "2.0",
  "id": 100,
  "method": "public/auth",
  "params": {
    "grant_type": "client_credentials",
    "client_id": "...",
    "client_secret": "..."
  }
}
```

Successful response returns `access_token` with a TTL. We must refresh before expiry via `public/auth` with `grant_type=refresh_token` or re-auth with credentials.

Alternative: signed-request auth using HMAC over a canonical string — not needed for our use case.

## Key methods

| Method | Purpose |
|---|---|
| `public/auth` | Authenticate |
| `public/get_time` | Server time for clock sync |
| `public/subscribe` | Subscribe to channel list |
| `private/subscribe` | Subscribe to user channels (fills, orders) |
| `private/buy`, `private/sell` | New order |
| `private/cancel` | Cancel by order_id |
| `private/cancel_all` | Cancel all |
| `private/cancel_all_by_instrument` | Cancel per instrument |
| `private/edit` | Amend price/qty (keeps queue priority) |
| `private/get_open_orders_by_instrument` | List opens |
| `private/get_positions` | Account positions |

## Rate limits

Tiered based on MM status:

| Tier | Requests/sec |
|---|---|
| Default | ⚠ 30 |
| Market Maker Tier 1 | ⚠ 100 |
| Market Maker Tier 2 | ⚠ 200 |

MM tiers are granted based on maker volume. Rate limit enforcement is per-`client_id`.

## Heartbeat

Deribit enforces heartbeat via a dedicated method:

```json
{"jsonrpc": "2.0", "id": 1, "method": "public/set_heartbeat", "params": {"interval": 30}}
```

After that, the server sends a `test_request` message every `interval` seconds that the client must respond to via `public/test`. Failure → disconnect.

## Session lifecycle

- Disconnect **does not** cancel orders.
- `access_token` persists until explicit TTL expiry — the session itself is short-lived, but the auth token can be reused over a reconnect.
- Max message size: ⚠ 50 KB per frame.

## Notes for future implementation

When Deribit becomes an active target:
- The `ws_rpc` abstraction can be parameterised with a JSON-RPC 2.0 `WireFormat` impl — mostly identifying the request by `jsonrpc: "2.0"` and extracting `id`, `result`, `error`.
- Heartbeat handling is more active than on other venues — the session engine must respond to `test_request` via `public/test`.
- The same abstraction could serve **Derive (Lyra v2)**, which uses an almost identical JSON-RPC 2.0 API.
