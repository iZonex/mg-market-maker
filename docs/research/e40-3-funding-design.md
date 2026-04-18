# E40.3 — Funding-Rate P&L Accrual Design

Research + design for funding attribution on any open perp position.
Target crates: `risk/pnl`, `portfolio`, `exchange/*`, `engine`,
`dashboard`.

## 1. Per-venue funding schedule (verified Apr 2026)

| Venue | Cadence | Settlement UTC | REST | WS topic | Notes |
|---|---|---|---|---|---|
| Binance USDⓈ-M | 8h default | 00/08/16 | `GET /fapi/v1/premiumIndex` (wired at `binance/futures.rs:184`), `GET /fapi/v1/fundingInfo` for 1h/4h exceptions | `<symbol>@markPrice@1s` (fields `r`, `T`) | **Bug**: default `interval=8h` hardcoded at `futures.rs:218`; symbols like `1000PEPE` settle 4h → accrual 2× wrong |
| Bybit linear | 8h | 00/08/16 (most); some 04/12/20 | `GET /v5/market/tickers?category=linear` → `fundingRate`, `nextFundingTime` | `tickers.<symbol>` | `get_funding_rate` **not implemented** (trait stub) |
| HyperLiquid perp | **1h** | every hour :00 | `POST /info {"type":"metaAndAssetCtxs"}` → `funding`, `predictedFundings` | `activeAssetCtx` | `get_funding_rate` **not implemented**. 24× more accruals than CEX |
| OKX perp | 8h | 00/08/16 or 04/12/20 | `GET /api/v5/public/funding-rate` | `funding-rate` channel | No connector crate yet |
| Deribit | 8h settle, continuous mark | — | `/public/get_funding_chart_data` | `deribit_price_index.<symbol>` | Inverse perps — out of scope for E40.3 |

## 2. Code touchpoints

| File | Change |
|---|---|
| `crates/risk/src/pnl.rs:15-30` | Add `funding_pnl_realised: Decimal` and `funding_pnl_mtm: Decimal` to `PnlAttribution`. Include `funding_pnl_realised` in `total_pnl()`; keep `mtm` excluded from realised total |
| `crates/risk/src/pnl.rs:49-181` | Add to `PnlTracker`: `current_funding_rate`, `funding_interval`, `next_funding_time`, `last_accrual_tick`, `notional_at_period_start`. New methods: `on_funding_update(rate, next, interval)`, `accrue_funding_mtm(now, inventory, mark)`, `settle_funding(now, inventory, mark)` |
| `crates/portfolio/src/lib.rs:89-117` | Extend `PortfolioSnapshot` with `total_funding_pnl`. Per-symbol `funding_accrued_native` on `AssetSnapshot`. New `Portfolio::accrue_funding(symbol, rate, elapsed_frac, mark_price)` hook |
| `crates/exchange/core/src/connector.rs:110` | `FundingRate` struct already has rate / next / interval — no change |
| `crates/exchange/bybit/src/connector.rs` + `hyperliquid/src/connector.rs` | **Missing**: implement `get_funding_rate()` override. HL must hardcode `interval: Duration::from_secs(3600)` |
| `crates/exchange/binance/src/futures.rs:218-223` | Wire `/fapi/v1/fundingInfo` lookup for 1h/4h-cadence symbols; drop 8h hardcode |
| `crates/engine/src/market_maker.rs:1828` (`tick_second`) | Call `pnl_tracker.accrue_funding_mtm(now, inventory, mid)` every sec. Poll `connector.get_funding_rate()` on `tick_count % 30 == 0`. When `now ≥ next_funding_time` call `settle_funding`. Only when `product.has_funding()` |
| `crates/dashboard/src/state.rs:515-524` | `PnlSnapshot`: add `funding: Decimal` (realised), `funding_mtm: Decimal` |
| `crates/dashboard/src/metrics.rs:14-28` | Two new `GaugeVec`: `PNL_FUNDING_REALISED`, `PNL_FUNDING_MTM`, both `&["symbol"]`. Register in `init_metrics()` |
| `crates/risk/src/audit.rs:74` | New `AuditEventType::FundingAccrued`. Emit on **settlement only** (not MTM — audit spam) |
| `crates/backtester/src/report.rs:36` | Print `Funding: {}` in summary |

## 3. Accrual math

**Sign convention**: `funding_payment = −position_qty · mark_price · funding_rate`. Positive rate → longs pay shorts.

**MTM (continuous, display only)**:
```
elapsed_frac = (now - period_start) / funding_interval          ∈ [0, 1]
funding_pnl_mtm = −inventory × mark_price × funding_rate × elapsed_frac
```
**Recompute from scratch on every tick_second** — never integrate; `inventory` and `mark_price` change continuously, integration would drift.

**Realised (at settle instant)**:
```
when now ≥ next_funding_time:
    realised_delta = −inventory_at_settle × mark_at_settle × funding_rate
    funding_pnl_realised += realised_delta
    funding_pnl_mtm      := 0
    period_start         := next_funding_time
    next_funding_time    := next_funding_time + interval
```

**Edge cases**:
- **Partial-hour start**: first `next_funding_time` = what venue reports; MTM uses `1 − (next_funding_time − now) / interval`
- **Position flips**: MTM is stateless in inventory — flips handled automatically. Realised uses instantaneous `inventory` at settle
- **Rate changes mid-period**: MTM uses latest published; realised uses rate at settle tick. Mismatch reconciled daily via `/fapi/v1/income?incomeType=FUNDING_FEE` (deferred)
- **Clock skew**: gate settlement on `now ≥ next_funding_time + 5 s` guard band
- **Halted symbol**: `pair_lifecycle` halt → skip accrual (stale rate would over-accrue)

## 4. Test plan

Property tests in `risk/pnl.rs`:

1. **Reset invariant**: after `settle_funding`, `funding_pnl_mtm == 0`
2. **Continuity**: `Σ realised_at_settlement + current_mtm` over N full periods with constant inventory/rate/mark equals naive `−inventory × mark × rate × N_periods` within Decimal epsilon
3. **Sign**: long + positive rate → mtm ≤ 0; short + positive → mtm ≥ 0; long + negative → mtm ≥ 0
4. **Flat invariant**: `inventory == 0` at every tick/settlement → both realised and mtm stay 0
5. **Total-PnL identity**: existing `total_pnl_identity_holds` proptest at `pnl.rs:305` must include `funding_pnl_realised`
6. **MTM idempotent**: calling `accrue_funding_mtm` N times at same timestamp yields same value
7. **Settle monotonic in |rate|**: realised delta scales linearly in rate
8. **Spot**: `get_funding_rate` returns error; engine skips accrual via `product.has_funding()` gate

Integration test: backtester replays 24 h with known rate; verify `funding_pnl_realised` equals `Σ` 3 (or 24 for HL) settlements.

## 5. Integration risks

1. **Double-count with `funding_arb`** — `persistence/funding.rs:33` already tracks `accumulated_funding` via explicit `on_funding_payment(amount)`. If both the arb driver AND engine-wide accrual credit the same position, dashboard double-reports.
   **Mitigation**: remove `funding_arb` `on_funding_payment` call; read from `PnlTracker.funding_pnl_realised` as single source of truth.

2. **`cross_exchange_basis` double-count** — basis PnL and funding PnL are orthogonal. Verify `basis.rs:222` formula does not implicitly include funding.

3. **MTM jumps on connector reconnect** — after WS gap + resync, inventory may jump (force-reset at `market_maker.rs:1823`). MTM stateless → recomputes — OK.

4. **HyperLiquid 1h cadence** = 24 audit events/day/symbol. Audit-log size budget review. Mitigation: settle → audit; MTM → Prometheus only.

5. **Binance 8h hardcoded** at `futures.rs:218-223`. Must wire `/fapi/v1/fundingInfo` lookup (one-shot at symbol registration).

6. **Checkpoint persistence** — `funding_pnl_realised` must be in engine checkpoint (alongside `realized_pnl` at `market_maker.rs:762`). Without persistence, pure-funding crashes lose accrual.

7. **Dashboard ring-buffer** at `state.rs:942` should store `realised` only — including MTM makes time-series jagged at settle boundaries. Expose MTM as separate gauge.
