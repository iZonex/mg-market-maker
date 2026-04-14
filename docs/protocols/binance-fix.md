# Binance FIX 4.4 (spot, beta)

**Status:** codec + session engine live in `crates/protocols/fix`; Binance FIX venue adapter not yet wired (access is gated, institutional tier)
**Canonical spec:** <https://developers.binance.com/docs/binance-spot-api-docs/fix-api>

## Purpose

Lowest-latency order-entry path Binance offers for MMs. TLS TCP socket carrying FIX 4.4 messages with a dedicated Order Entry gateway. Typical latency: WS API ~3–10 ms → FIX ~1–5 ms.

**Currently beta** (as of spec snapshot). Feature-flag the path until it graduates and we observe stability.

## Endpoints

| Environment | Host | Purpose |
|---|---|---|
| Mainnet OE | `fix-oe.binance.com:9000` | Order entry |
| Mainnet DC | `fix-dc.binance.com:9000` | Drop copy (execution reports stream) |
| Mainnet MD | `fix-md.binance.com:9000` | Market data |
| Testnet OE | `fix-oe.testnet.binance.vision:9000` | ⚠ verify current host |

TLS wrapping mandatory. No plaintext TCP.

## Auth

Binance FIX uses **Ed25519 only** — no HMAC on this path. Register the Ed25519 public key with the same API account. During logon the client sends:

| Tag | Name | Value |
|---|---|---|
| 35 | MsgType | `A` |
| 49 | SenderCompID | assigned by Binance |
| 56 | TargetCompID | `SPOT` (main) / `SPOTTEST` (test) |
| 34 | MsgSeqNum | 1 on new session |
| 52 | SendingTime | UTCTimestamp (ms) |
| 98 | EncryptMethod | 0 |
| 108 | HeartBtInt | 30 |
| 553 | Username | API key id |
| 25035 | MessageHandling | 1 (unordered) — ⚠ verify tag number |
| 25036 | ResponseMode | `everything` — ⚠ verify |
| 96 | RawData | Base64 Ed25519 signature |
| 95 | RawDataLength | Length of 96 |

Signing payload: canonical concatenation of session identity fields — see spec for exact format. **Do not guess** — read the spec section "Ed25519 logon signing".

## Supported message types (in scope)

### Session layer
| Tag 35 | Name | Direction |
|---|---|---|
| `0` | Heartbeat | both |
| `1` | TestRequest | both |
| `2` | ResendRequest | both |
| `3` | Reject (session-level) | both |
| `4` | SequenceReset | both |
| `5` | Logout | both |
| `A` | Logon | both |

### Application layer
| Tag 35 | Name | Direction |
|---|---|---|
| `D` | NewOrderSingle | → |
| `F` | OrderCancelRequest | → |
| `G` | OrderCancelReplaceRequest | → |
| `q` | OrderMassCancelRequest | → |
| `8` | ExecutionReport (new/fill/cancel/reject) | ← |
| `9` | OrderCancelReject | ← |
| `r` | OrderMassCancelReport | ← |

## Rate limits

| Bucket | Limit |
|---|---|
| Messages / second (OE session) | ⚠ 50 msgs/s |
| Orders / 10s | 50 (shared with REST + WS API) |
| Orders / day | 160,000 (shared) |

Binance enforces per-session message rate separately from per-account order rate.

## Heartbeat

- Default `HeartBtInt = 30s` negotiated at logon; we pick 30s.
- Each side sends a Heartbeat (35=0) every 30s of silence.
- TestRequest (35=1) sent if no inbound traffic in 1.2 × HeartBtInt.
- Connection dropped if no response within 2 × HeartBtInt.

## Session lifecycle

- **Seq nums persist per session between SenderCompID + TargetCompID.** A new logon with lower seq num is rejected unless a `ResetSeqNumFlag(141)=Y` is set.
- **Gap fill**: if we observe a gap in received seq nums, send ResendRequest (35=2) for the missing range; peer responds with repeated messages with `PossDupFlag(43)=Y`, or a SequenceReset (35=4) with `GapFillFlag(123)=Y` if the skipped messages are no longer relevant.
- **Logout**: send Logout (35=5), wait for peer's Logout, then close TCP.
- **Max session lifetime**: ⚠ Binance rotates sessions every 24h; reconnect with the next sequence numbers.

## Rate-limit & error codes

Session-level rejects (35=3) carry `SessionRejectReason(373)`:
- `0` — invalid tag
- `1` — required tag missing
- `4` — tag appears more than once
- `5` — value incorrect for tag
- `10` — SendingTime accuracy problem
- `11` — invalid MsgType
- `99` — other

Business rejects via ExecutionReport with `OrdStatus(39)=8 (Rejected)` + `OrdRejReason(103)` codes paralleling REST error numbers.

## Gotchas

- BodyLength must be computed exactly per FIX spec — our `crates/protocols/fix` codec does this; reuse it.
- CheckSum is `sum(all_bytes) mod 256`, **3-digit zero-padded**, over every byte up to (not including) the `10=` field.
- Price and quantity fields are strings; no locale-specific decimal separators.
- `TransactTime(60)` and `SendingTime(52)` in UTCTimestamp format `YYYYMMDD-HH:MM:SS.sss` — our codec accepts this as a `&str` to keep it deterministic.
- Binance requires `MsgSeqNum` on every outbound — the session layer in `crates/protocols/fix::session` owns this.
- No drop-copy merging: if you want to receive fills via FIX, connect a separate DC session — OE does not include fill reports.

## Testing approach

- Unit tests with captured FIX messages (ASCII with explicit `\x01`) — our codec handles encoding/decoding.
- Integration test: mock FIX counterparty (simple TCP server that replies to Logon with a logon back, parrots heartbeats). Verifies full session machine.
- Cannot test live without Binance FIX access approval (institutional tier).

## Sample fixtures

To be produced for tests (not captured live since FIX access is gated):
- `fixtures/binance/fix/logon_request.txt`
- `fixtures/binance/fix/logon_response.txt`
- `fixtures/binance/fix/new_order_single.txt`
- `fixtures/binance/fix/execution_report_new.txt`
- `fixtures/binance/fix/execution_report_fill.txt`
- `fixtures/binance/fix/order_cancel_reject.txt`

## Open items to verify against testnet

- ⚠ Exact Ed25519 signing payload for Logon (tag 95/96 content).
- ⚠ Current tag numbers for Binance-custom MessageHandling / ResponseMode (25035/25036 are placeholders).
- ⚠ Testnet FIX OE hostname — may have moved since spec snapshot.
- ⚠ Whether FIX sessions count against the same daily-order limit as REST+WS API.
