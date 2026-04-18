# E40.4 + E40.7 — Margin Guard + Margin Mode Design

Pre-order maintenance-margin guard + `margin_mode: {isolated, cross}`
config. Prevents own-account liquidation on Binance/Bybit/HL perps.

## 1. Venue margin endpoints (Apr 2026)

| Venue | Endpoint | Auth | Refresh | Notes |
|---|---|---|---|---|
| Binance USDⓈ-M | `GET /fapi/v2/account` | signed | 5 s poll (weight 5) | `totalMarginBalance`, `totalInitialMargin`, `totalMaintMargin`, `availableBalance`, per-position `isolatedMargin`, `maintMargin`, `liquidationPrice`. Set mode via `POST /fapi/v1/marginType` (`ISOLATED`/`CROSSED`) per-symbol |
| Bybit linear UTA | `GET /v5/account/wallet-balance?accountType=UNIFIED` + `GET /v5/position/list?category=linear&symbol=` | signed | 5 s poll | Wallet: `totalEquity`, `totalInitialMargin`, `totalMaintenanceMargin`, `accountMMRate`. Positions: `positionIM`, `positionMM`, `liqPrice`, `markPrice`. Set mode: `POST /v5/account/set-margin-mode` (account-wide) |
| HyperLiquid perp | `POST /info {type:"clearinghouseState", user:<addr>}` | unsigned | 3 s poll (WS `webData2` preferred) | `marginSummary.{accountValue,totalNtlPos,totalMarginUsed}`, `crossMaintenanceMarginUsed`, per-position `assetPositions[].position.{liquidationPx,marginUsed,leverage}`. Set per-asset: `POST /exchange {action:{type:"updateLeverage",asset,isCross,leverage}}` |

## 2. Liquidation price

**Rule: consume venue-supplied `liq_price` verbatim. Never recompute.**
Local recomputation risks divergence from venue's tiered-bracket MMR.
Only compute aggregate `margin_ratio = totalMaintMargin / totalMarginBalance`
locally for the guard.

Reference formulas (for sanity, not reimplementation):
- **Binance USDⓈ-M (linear)**: `liqPrice = (wB + cumB − side × pos × entry) / (pos × (MMR − side))` per Binance Futures docs "Leverage & Margin"
- **Bybit linear** (V5 "Position" doc, USDT perp): `liqPrice_long = entry × (1 − IMR + MMR) − availBal / size`
- **HyperLiquid** ("Perpetuals - Overview"): `liqPrice = entry + side × marginAvailable / size` (cross), or with isolated bucket

## 3. Code change list

- `crates/exchange/core/src/connector.rs:226` — extend `VenueCapabilities` with `supports_margin_info: bool`, `supports_margin_mode: bool`
- `crates/exchange/core/src/connector.rs:248` — new trait methods (default `NotSupported`):
  - `async fn account_margin_info() -> Result<AccountMarginInfo, MarginError>`
  - `async fn set_margin_mode(symbol, mode)`
  - `async fn set_leverage(symbol, leverage)`
- New structs `AccountMarginInfo`, `PositionMargin`, enum `MarginMode {Isolated, Cross}`, `MarginError {NotSupported, Stale, Other(anyhow::Error)}`
- Per-venue impls:
  - `crates/exchange/binance/src/futures.rs:531` — `/fapi/v2/account` + `/fapi/v2/positionRisk` + `/fapi/v1/marginType`
  - `crates/exchange/bybit/src/connector.rs:623` — wallet-balance + position list + set-margin-mode
  - `crates/exchange/hyperliquid/src/connector.rs:811` — expand `clearinghouseState` parser + `updateLeverage`
- `crates/risk/src/margin_guard.rs` (new, ~300 lines) — state machine: `update(info)`, `projected_ratio(delta_notional)`, `level() -> Option<KillLevel>`, `stale(now)`
- `crates/common/src/config.rs:255` — `MarginConfig` struct + `PerSymbolMargin` map
- `crates/engine/src/market_maker.rs:2498` — `MarginGuard` pre-order check ahead of `kill_switch.allow_new_orders()`
- `crates/engine/src/market_maker.rs:1828` (`tick_second`) — refresh + escalate via new `kill_switch.update_margin_ratio()`
- `crates/server/src/lib.rs` — startup hook: for each perp symbol call `connector.set_margin_mode(symbol, cfg)`; abort on anything other than `Ok`/`NotSupported`

## 4. Config additions

```toml
[margin]
refresh_interval_secs = 5
widen_ratio   = 0.50     # L1 WidenSpreads
stop_ratio    = 0.80     # L2 StopNewOrders
cancel_ratio  = 0.90     # L3 CancelAll
max_stale_secs = 30
default_mode  = "isolated"   # "isolated" | "cross"
default_leverage = 5

[margin.per_symbol]
BTCUSDT = { mode = "isolated", leverage = 3 }
ETHUSDT = { mode = "cross",    leverage = 5 }   # requires hedge_optimizer
```

**Validation at startup**: any `per_symbol.X.mode = "cross"` AND no `hedge_optimizer` → hard-fail. Preserves "cross only safe when hedged" invariant.

## 5. Kill-switch integration

```rust
// crates/risk/src/kill_switch.rs
pub fn update_margin_ratio(&mut self, ratio: Decimal, stale: bool, cfg: &MarginKillCfg) {
    if stale {
        self.escalate(KillLevel::WidenSpreads, "margin info stale");
        return;
    }
    if      ratio >= cfg.cancel_ratio { self.escalate(KillLevel::CancelAll,     "margin critical"); }
    else if ratio >= cfg.stop_ratio   { self.escalate(KillLevel::StopNewOrders, "margin high");     }
    else if ratio >= cfg.widen_ratio  { self.escalate(KillLevel::WidenSpreads,  "margin elevated"); }
}
```

Uses existing monotonic `escalate` — no auto-de-escalation on transient recovery. Combines with MR, PnL, position-value, error-rate subsystems via `max` escalation.

**Pre-order hook** at `market_maker.rs:2498` also calls `MarginGuard::projected_ratio(notional_delta)` — short-circuits quote if projected post-fill ratio would cross `stop_ratio`. Prevents "quote was OK, fill crossed the line" race.

## 6. Failure modes

| Failure | Behaviour |
|---|---|
| 4xx auth | `MarginError::Other` → `kill_switch.on_error()`; after `max_consecutive_errors` → L2 |
| 5xx / timeout | Keep last cached `AccountMarginInfo`; `last_refresh` ages |
| `last_refresh > max_stale_secs` | `stale=true` → auto L1 WidenSpreads, log once |
| Venue `NotSupported` (spot) | Guard is no-op; `update_margin_ratio` never called |
| Bootstrap `set_margin_mode` fails | **Hard-fail process start**. Live account on wrong mode = unacceptable |
| Already-in-mode (Binance `-4046`, Bybit `110026`) | Treat as success, log info |
| Cross mode without live hedge | Refuse boot (see §4) |

## 7. Test plan

**Unit — `margin_guard.rs`**:
- `projected_ratio` monotonic in notional (proptest)
- Stale detection: time-travelled `update` → `stale(now)` after `max_stale_secs`
- Ratio thresholds → correct `KillLevel` (proptest sweeping ratio ∈ [0,1])
- Zero-equity edge case: `total_equity == 0` → treat as `ratio = 1`, force L3, no panic

**Unit — `kill_switch.rs`**:
- `update_margin_ratio` monotonic (extends existing `auto_escalation_is_monotonic`)
- Combined escalation: ratio + PnL + MR — highest wins

**Unit — connectors**:
- Pure-helper parsers per venue against fixtures in `crates/exchange/{venue}/tests/fixtures/margin_*.json`

**Integration — engine**:
- Synthetic venue mock returning escalating `maintMargin/marginBalance` — assert `kill_switch.level()` walks `Normal → Widen → Stop → Cancel` at thresholds; `order_manager.cancel_all` runs at L3
- Stale-feed: mock returns error 10× — auto-widen after `max_stale_secs`
- Cross-mode-without-hedge: `AppConfig` with `mode="cross"` + `hedge=None` → `validate()` fails

**Regression anchors**: capability audit — each venue crate asserts `supports_margin_info/mode` flags match trait impl (mirror `supports_ws_trading` pattern).

## Key constraints carried forward

- Monotonic escalation (never auto-de-escalate)
- Venue-supplied `liq_price` preferred over local recompute
- Cross-margin gated on live `hedge_optimizer`
- No `f64` anywhere in margin math
- Hard-fail on boot-time mode-setting inconsistency
