# Fast Protocol Comparison Matrix

Single-page source of truth for what protocols our venues expose, our implementation status, and expected latency. Update after Sprint 5 benchmarks.

## Implementation status

| Venue | REST | WebSocket API (trade) | FIX 4.4 | Binary | Status |
|---|---|---|---|---|---|
| Custom exchange | ✅ live | — | — | — | live |
| Binance spot | ✅ live | ✅ wired with REST fallback | 🧩 codec + session engine ready; venue adapter pending | ❌ out of scope (SBE) | WS live |
| Binance futures | ❌ not wired | 🧩 same abstraction | 🧩 | — | pending |
| Bybit | ✅ live | 🧩 adapter + unit tests; integration pending live auth check | 🧩 codec + session engine ready | — | partial |
| HyperLiquid | ✅ live | ✅ wired with REST fallback (WS post) | — | — | WS live |
| OKX | ❌ no connector | — | — | — | not started |
| Deribit | ❌ no connector | — | — | — | not started |
| dYdX v4 | ❌ no connector (gRPC/protobuf, own epic) | — | — | — | not started |

Legend: ✅ wired + tested  ·  🧩 scaffold (code exists, not routed)  ·  ⏳ planned  ·  ❌ not started

## Latency estimates (pre-benchmark)

Typical end-to-end `place_order` round-trip from our colo-adjacent infra (educated guess until Sprint 5 fills in measured numbers):

| Path | Latency | Notes |
|---|---|---|
| REST over HTTPS | 20–80 ms | TLS handshake dominates when cold, ~10–30 ms warm |
| WS API / WS Trade (JSON) | 3–15 ms | Persistent TLS, single frame round-trip |
| FIX 4.4 | 1–8 ms | Binary-ish, no JSON parse, sequence-numbered |
| Binance SBE binary | 1–3 ms | Only once SBE decoder exists |

Numbers here are placeholders. Sprint 5 replaces with p50/p90/p99 from the benchmark harness.

## Protocol families

| Family | Venues | Shared abstraction |
|---|---|---|
| Request/response JSON over persistent WS with `id` correlation | Binance, Bybit, HyperLiquid, OKX | `crates/protocols/ws_rpc` (Sprint 2) |
| JSON-RPC 2.0 over WS | Deribit, Derive | Can reuse `ws_rpc` with a JSON-RPC `WireFormat` |
| FIX 4.4 over TLS TCP | Binance, Bybit, Deribit (alt), OKX (alt) | `crates/protocols/fix` + session engine (Sprint 4) |
| gRPC + Cosmos SDK protobuf | dYdX v4 | Separate epic |
| Binance SBE | Binance only | Out of scope |

## Rate limits (order-entry relevant)

⚠ Verify before implementation; limits drift between spec revisions.

| Venue | Orders / second | Orders / day | Shared across paths? |
|---|---|---|---|
| Binance spot | 50 per 10s = 5/s | 160,000 | Yes — REST + WS API + FIX share one bucket |
| Binance futures | Variable per symbol | Variable | — |
| Bybit spot | ~20/s | — | Yes — REST + WS Trade + FIX per UID |
| Bybit linear | ~10/s | — | Yes |
| HyperLiquid | ~20/s per address | ~1200 requests/min overall | Yes — REST + WS post share |
| OKX spot | ~60/s per sub-account | — | Per-sub-account |
| Deribit (MM Tier 1) | ~100/s | — | Per client_id |

## Capability flags (target end-state)

After Sprint 3 completes, `VenueCapabilities` for each venue should read:

```rust
// Binance
VenueCapabilities {
    max_batch_size: 5,
    supports_amend: true,
    supports_ws_trading: true,  // wired in Sprint 3
    supports_fix: true,          // wired in Sprint 4
    max_order_rate: 300,         // per 10s
}

// Bybit
VenueCapabilities {
    max_batch_size: 20,
    supports_amend: true,
    supports_ws_trading: true,   // wired in Sprint 3
    supports_fix: false,          // Sprint 4 (beta / institutional)
    max_order_rate: 600,         // per 5s
}

// HyperLiquid
VenueCapabilities {
    max_batch_size: 20,
    supports_amend: false,       // native modify not exposed in v1
    supports_ws_trading: true,   // FIX capability bug — to be fixed in Sprint 3
    supports_fix: false,
    max_order_rate: 100,
}
```

Sprint 5 audit pass verifies these match the implementation.

## Benchmark placeholders

To be filled at Sprint 5:

| Path | p50 (ms) | p90 (ms) | p99 (ms) |
|---|---|---|---|
| Binance REST | — | — | — |
| Binance WS API | — | — | — |
| Binance FIX | — | — | — |
| Bybit REST | — | — | — |
| Bybit WS Trade | — | — | — |
| Bybit FIX | — | — | — |
| HyperLiquid REST | — | — | — |
| HyperLiquid WS post | — | — | — |
