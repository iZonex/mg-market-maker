# Bybit FIX 4.4 (institutional)

**Status:** codec + session engine live in `crates/protocols/fix`; Bybit FIX venue adapter not yet wired (access gated, institutional tier)
**Canonical spec:** <https://bybit-exchange.github.io/docs/v5/fix>

## Purpose

Institutional-tier FIX gateway for order entry + drop-copy. Typical latency: WS Trade ~5–15 ms → FIX ~2–8 ms. Intended for firms running HFT market making.

**Access requires explicit approval from Bybit institutional sales.** Retail API keys cannot log in to the FIX gateway.

## Endpoints

| Environment | Host | Purpose |
|---|---|---|
| Mainnet OE | `fix.bybit.com:9880` | Order entry |
| Mainnet DC | `fix-dc.bybit.com:9881` | Drop copy (fills) |
| Testnet | ⚠ not publicly documented — contact sales |

TLS mandatory.

## Auth

Logon (35=A) fields:

| Tag | Name | Value |
|---|---|---|
| 49 | SenderCompID | assigned |
| 56 | TargetCompID | `BYBIT` |
| 34 | MsgSeqNum | 1 |
| 52 | SendingTime | UTCTimestamp |
| 98 | EncryptMethod | 0 |
| 108 | HeartBtInt | 30 |
| 553 | Username | API key |
| 554 | Password | HMAC-SHA256 hex of `SendingTime + MsgType + MsgSeqNum + SenderCompID + TargetCompID` with API secret ⚠ |

The exact "Password" computation differs between venues — **Bybit's derivation is not well documented publicly**; confirm with institutional docs at approval time.

## Message types

Same as Binance FIX — `D/F/G/8/9/q/r` for orders, `0/1/2/3/4/5/A` for session. Bybit additionally supports:

- `AF` — OrderMassStatusRequest
- `AN` — RequestForPositions
- `AP` — PositionReport

## Rate limits

| Bucket | Limit |
|---|---|
| Messages / second | ⚠ 100 msgs/s per session (institutional tier) |
| Orders / second | Per-UID limit, shared with WS Trade and REST |
| Concurrent sessions per UID | ⚠ Typically 4–8 |

## Heartbeat

- `HeartBtInt = 30s` default, configurable.
- Session drop after 2 × HeartBtInt of silence.

## Session lifecycle

- **Seq nums persist per session.** Reconnect with the next expected numbers.
- `ResetSeqNumFlag(141)=Y` at logon forces a fresh session starting from 1.
- **Disconnect cancels nothing** — resting orders on the book persist.
- **Max session lifetime**: ⚠ reported as 24h by some integrators; confirm.
- `orderLinkId` idempotency window: 10 minutes, matching the WS Trade behaviour.

## Gotchas

- Bybit FIX uses the V5 unified account model — category (spot / linear / inverse / option) is a mandatory custom tag. ⚠ Tag number to be confirmed.
- Execution reports carry Bybit-specific fields via custom tags in the 50000+ range (category, position mode, etc.).
- `TransactTime(60)` and `SendingTime(52)` must match server clock within the configured `MaxLatency` — the gateway rejects stale messages.

## Testing approach

Identical to Binance FIX: codec + session engine unit tests, mock-counterparty integration tests. No live-testnet tests without access approval.

## Sample fixtures

To be produced for tests:
- `fixtures/bybit/fix/logon_request.txt`
- `fixtures/bybit/fix/new_order_single_linear.txt`
- `fixtures/bybit/fix/execution_report_fill.txt`
- `fixtures/bybit/fix/mass_cancel_request.txt`

## Open items (verify when access is granted)

- ⚠ Exact Password (554) derivation.
- ⚠ Testnet availability and hostname.
- ⚠ Custom tag numbers for `category`, position mode, etc.
- ⚠ Max concurrent sessions per UID.
- ⚠ Daily message/order caps.
