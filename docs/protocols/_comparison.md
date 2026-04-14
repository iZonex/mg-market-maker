# Fast Protocol Comparison Matrix

Single-page source of truth for what protocols our venues expose, our implementation status, and expected latency. Update as benchmark results land.

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
| dYdX v4 | ❌ no connector (gRPC/protobuf, separate effort) | — | — | — | not started |

Legend: ✅ wired + tested  ·  🧩 scaffold (code exists, not routed)  ·  ⏳ planned  ·  ❌ not started

## Latency estimates (pre-benchmark)

Typical end-to-end `place_order` round-trip from our colo-adjacent infra (educated guess until benchmarks land):

| Path | Latency | Notes |
|---|---|---|
| REST over HTTPS | 20–80 ms | TLS handshake dominates when cold, ~10–30 ms warm |
| WS API / WS Trade (JSON) | 3–15 ms | Persistent TLS, single frame round-trip |
| FIX 4.4 | 1–8 ms | Binary-ish, no JSON parse, sequence-numbered |
| Binance SBE binary | 1–3 ms | Only once SBE decoder exists |

Numbers here are placeholders until the benchmark harness fills in measured p50/p90/p99 values.

## Protocol families

| Family | Venues | Shared abstraction |
|---|---|---|
| Request/response JSON over persistent WS with `id` correlation | Binance, Bybit, HyperLiquid, OKX | `crates/protocols/ws_rpc` |
| JSON-RPC 2.0 over WS | Deribit, Derive | Can reuse `ws_rpc` with a JSON-RPC `WireFormat` |
| FIX 4.4 over TLS TCP | Binance, Bybit, Deribit (alt), OKX (alt) | `crates/protocols/fix` + session engine |
| gRPC + Cosmos SDK protobuf | dYdX v4 | Separate effort |
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

## Capability flags (current state)

`VenueCapabilities` currently read:

```rust
// Binance spot
VenueCapabilities {
    max_batch_size: 5,
    supports_amend: true,
    supports_ws_trading: true,   // BinanceConnector + ws_trade.rs
    supports_fix: false,          // codec + session engine ready; venue adapter pending
    max_order_rate: 300,          // per 10s
    supports_funding_rate: false,
}

// Bybit linear
VenueCapabilities {
    max_batch_size: 20,
    supports_amend: true,
    supports_ws_trading: false,   // adapter scaffold exists; pending live-testnet auth verification
    supports_fix: false,           // access gated (institutional tier)
    max_order_rate: 600,          // per 5s
    supports_funding_rate: true,
}

// HyperLiquid perp
VenueCapabilities {
    max_batch_size: 20,
    supports_amend: false,        // native modify not exposed in v1
    supports_ws_trading: true,    // HyperLiquidConnector + ws_post.rs
    supports_fix: false,
    max_order_rate: 100,
    supports_funding_rate: true,
}
```

Each venue's `capabilities_match_implementation` unit test pins these flags to the actual adapter types, so declared capabilities cannot drift from code.

## Benchmark placeholders

To be filled from the benchmark harness:

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
