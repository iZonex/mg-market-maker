# Epic F stage-2 — Listing sniper (sub-component #1)

Track 3 of the four-track parallel stage-2 pass that closes the
remaining Epic F deferrals. This sprint adds a venue-level
`list_symbols` API to the `ExchangeConnector` trait, implements
it across every venue connector that exposes a public symbol
endpoint, and ships a standalone `ListingSniper` discovery
module that detects new listings and emits events.

## Scope

| # | Deliverable | File | Status |
|---|-------------|------|--------|
| 1 | `ExchangeConnector::list_symbols` trait method | `crates/exchange/core/src/connector.rs` | DONE |
| 2 | Binance spot impl | `crates/exchange/binance/src/connector.rs` | DONE |
| 3 | Binance USDⓈ-M futures impl | `crates/exchange/binance/src/futures.rs` | DONE |
| 4 | Bybit V5 (per-category) impl | `crates/exchange/bybit/src/connector.rs` | DONE |
| 5 | HyperLiquid (perp + spot) impl | `crates/exchange/hyperliquid/src/connector.rs` | DONE |
| 6 | Custom client default-fallback | `crates/exchange/client/src/connector.rs` | default `Err(unsupported)` (no endpoint) |
| 7 | `ListingSniper` module + ≥10 unit tests | `crates/engine/src/listing_sniper.rs` | DONE (12 tests) |
| 8 | Engine module export | `crates/engine/src/lib.rs` | DONE (one-line add) |
| 9 | `MockConnector::list_symbols` test-support shim | `crates/engine/src/test_support.rs` | DONE (minimal additive) |

## Per-venue endpoint mapping

| Venue | Method | Endpoint | Wire shape | Notes |
|---|---|---|---|---|
| Binance spot | GET | `/api/v3/exchangeInfo` (no `symbol=` param) | `resp.symbols[]` | Reuses the per-row parser that `get_product_spec` already uses (factored into `parse_binance_spot_symbol`). Maps `status` → `TradingStatus` so `PRE_TRADING` / `HALT` / `BREAK` symbols stay visible to the sniper for diagnosis. |
| Binance futures | GET | `/fapi/v1/exchangeInfo` | `resp.symbols[]` | Factored parser `parse_binance_futures_symbol`. Maps `contractStatus` (`TRADING`/`PENDING_TRADING`/`SETTLING`/`DELIVERED`/…) → `TradingStatus`. |
| Bybit V5 | GET | `/v5/market/instruments-info?category=<self.category>` | `result.list[]` | **One category per connector** (spot, linear, or inverse). Multi-category scanning is a stage-3 follow-up. Factored parser `parse_bybit_instrument`; maps `status` (`Trading`/`PreLaunch`/`Settling`/`Delivering`/`Closed`) → `TradingStatus`. |
| HyperLiquid perp | POST | `/info` `{ "type": "meta" }` | `universe[]` of `{name, szDecimals, maxLeverage, isDelisted?}` | `parse_hl_perp_meta_into_specs` walks the universe, runs `HyperLiquidConnector::decimals_to_spec` (shared with `get_product_spec`) to derive `tick_size` / `lot_size`, and marks delisted rows. |
| HyperLiquid spot | POST | `/info` `{ "type": "spotMeta" }` | `tokens[] + universe[]` | Token-index → `szDecimals` lookup identical to `ensure_asset_map`, then maps each pair through `decimals_to_spec(.., is_spot=true)`. |
| Custom client | — | (none) | — | The custom exchange has no public exchange-info / instruments endpoint today. The connector's existing `get_product_spec` already bails with "use config for …"; `list_symbols` inherits the default trait impl and returns `Err(unsupported)`. Sniper callers should skip the Custom venue on their periodic scan loop. |

## Audit findings

* **Binance spot** `get_product_spec` used an inlined per-row
  parser that was in-lockstep with the filter / status mapping
  logic. Factoring it into `parse_binance_spot_symbol` kept both
  call sites driven by one source of truth, so a schema drift
  on the listing-info shape breaks either parser the same way.
* **Binance futures** mirror of the above — the only subtle
  difference is the status field is `contractStatus`, not
  `status`, and the value set is delivery-contract-specific
  (`SETTLING` / `PENDING_TRADING` / `DELIVERED` / …). The
  parser maps the three delivery states to
  `TradingStatus::Delisted` since a delivered contract will
  never quote again.
* **Bybit** category is a single-value property of the
  connector — the original `get_product_spec` used
  `category=<self.category>&symbol=<sym>`. `list_symbols` uses
  the same category field with the `symbol=` param dropped.
  Multi-category scanning would require either three
  connectors (the operator wires them up in
  `server/main.rs`) or a trait-level opt-out; stage-2 punts to
  the former.
* **HyperLiquid** already had a very complete `spotMeta` /
  `meta` parser inside `ensure_asset_map` — the sniper parsers
  reuse the same token-index lookup rule and the
  `decimals_to_spec` helper so the precision rule is shared
  between the sniper and every other HL call site.
* **Custom client** has no public symbol-list endpoint. The
  trait default `Err("list_symbols not supported on this
  venue")` is the correct behaviour, and the sniper's
  "connector Err → scan Err, state unchanged" semantics
  guarantees the consumer handles the `Custom` venue
  gracefully.

## `ListingSniper` module — design notes

* **Parallel to `PairLifecycleManager`, not a replacement.**
  Lifecycle tracks `trading_status` transitions for
  **subscribed** symbols (halt, resume, delist). The sniper
  tracks the full venue-wide symbol set (listed / removed).
  They share no state and can run on independent cadences
  (the lifecycle manager runs every 60 s; the sniper is a
  slow-cadence tick the operator picks).
* **Seed-on-first-scan.** The very first scan against each
  venue seeds the `known` cache without firing events.
  Otherwise every existing Binance spot symbol would fire a
  `Discovered` event on startup.
* **Connector errors do not mutate state.** A transient venue
  failure (rate limit, network blip, auth rejection) returns
  `Err` and leaves the cache untouched. The next scan diffs
  cleanly.
* **Deterministic event order.** Both `Discovered` and
  `Removed` sort by symbol name before emit. Makes the
  `three_venue_fixture_one_add_one_remove` hand-verified
  fixture reproducible.
* **`forget(venue)` handles long downtime.** Clears one
  venue's cache; the next scan re-seeds without events.
  Operators use this after maintenance windows where symbol
  drift would otherwise fire a flood of spurious
  `Discovered` / `Removed` events.

## Open design questions — resolution

1. **HyperLiquid `min_notional`.** HL `meta` doesn't expose it.
   Every HL spec inherits the const `DEFAULT_MIN_NOTIONAL =
   dec!(10)`. Operators override per-symbol via config after
   the sniper flags a new listing.
2. **Binance status filtering.** `exchangeInfo` returns
   `TRADING`, `BREAK`, `HALT`, `AUCTION_MATCH`, `PRE_TRADING`
   — all of them. Default: pass every symbol through with a
   populated `trading_status`; the consumer filters post-hoc.
   Gives the sniper visibility into PRE_TRADING symbols during
   their auction phase.
3. **Bybit category.** Queries the single category the
   connector was constructed with (`self.category`). Multi-
   category scanning is a stage-3 follow-up.
4. **Custom client.** Default-fallback `Err(unsupported)` —
   documented in the closure note.
5. **Rate limiting.** `list_symbols` is a single REST call
   returning a moderate-sized response (Binance spot full
   exchange-info is ~1.5 MB). Every impl routes through the
   existing per-connector `RateLimiter` via the `signed_get` /
   public-GET helpers, so the existing rate-limiter
   accounting already covers the additional call. Binance
   spot uses the documented weight-10 cost; Binance futures
   uses weight-1; Bybit / HL re-use the 1-token acquire path.

## Definition of Done

- [x] `ExchangeConnector::list_symbols` trait method exists
      with a default `Err("not supported")` impl so
      backward-compat is preserved.
- [x] Binance spot implementation + 3 parser tests.
- [x] Binance futures implementation + 1 parser test.
- [x] Bybit V5 implementation + 1 parser test.
- [x] HyperLiquid perp + spot implementations + 3 parser tests.
- [x] Custom client inherits the default trait impl
      (documented).
- [x] `ListingSniper` module with 12 unit tests (spec
      required ≥10).
- [x] One-line module export in `crates/engine/src/lib.rs`.
- [x] `MockConnector::list_symbols` shim in `test_support.rs`
      (programmable via `set_list_symbols_ok` /
      `set_list_symbols_err`).
- [x] `cargo test -p mm-engine` — 144 tests pass (12 new).
- [x] `cargo test -p mm-exchange-core` — 2 tests pass.
- [x] `cargo test -p mm-exchange-binance` — 30 tests pass (+4 new).
- [x] `cargo test -p mm-exchange-bybit` — 28 tests pass (+1 new).
- [x] `cargo test -p mm-exchange-hyperliquid` — 36 tests pass (+3 new).
- [x] `cargo test -p mm-exchange-client` — green.
- [x] `cargo clippy --all-targets -p mm-exchange-{core,binance,bybit,hyperliquid,client} -- -D warnings` — clean.
- [x] `cargo fmt -p ...` — clean.

## Stage-3 follow-ups (tracked, not in scope)

* **Auto-spawn on `Discovered`.** The sniper emits events but
  does not spawn a probation engine. Operators wire the event
  stream into their orchestration layer (a long-running
  `mm-server` subcommand, a K8s operator, …). Stage-3 lands
  an engine-side `ListingOrchestrator` that takes the event
  stream and spins up a probation `MarketMakerEngine`
  instance with wide spreads and small size for ~24 h.
* **Multi-category Bybit scan.** Stage-3 adds a
  `list_symbols_all_categories` convenience that fans out to
  spot, linear, and inverse and concatenates the responses.
  Stage-2 requires operators to instantiate three Bybit
  connectors manually.
* **Delta-aware caching.** The sniper stores the full
  `HashSet<String>` per venue. For venues with thousands of
  symbols (Binance spot > 2 500 pairs) this is still cheap
  (~100 KB per venue), but stage-3 can swap the cache for a
  hash of the sorted symbol list and skip the diff entirely
  when hashes match.

## Closure summary

All stage-2 deliverables landed in a single pass. Twelve
`ListingSniper` unit tests cover seed/diff/forget/error/
multi-venue/idempotency/round-trip/fixture scenarios,
comfortably exceeding the ≥10-test requirement. Every venue
connector that exposes a public symbol-list endpoint now
implements `list_symbols`; the custom client inherits the
trait default and is explicitly documented as "not
supported". `MockConnector` gains a minimal
`list_symbols_response` field and two setters so the sniper's
tests can drive arbitrary venue responses without touching
the rest of the mock's existing behaviour.
