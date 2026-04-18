# Adaptive MM Calibration — Design Doc (Epic 30)

Status: research complete, implementation pending
Owner: MM engine
Last updated: 2026-04-17

## Problem

Strategy parameters (γ, κ, σ-floor, order size, spread floor) ship as static TOML
values calibrated once per symbol. A BTCUSDT deployment uses the same defaults
as an ALTUSDT or a memecoin — which is wrong: memecoins have 100× the realised
vol of BTC, meme-alts have spread floors an order of magnitude wider, and
asymmetric fill intensities (κ_bid vs κ_ask) are ignored entirely.

We already have **offline** calibration (`mm-calibrate`, Epic 25) and
**regime-switched** parameters (`AutoTuner`, closed-form), but no online
feedback loop that moves γ based on realised fill rate or adverse selection,
and no pair-type taxonomy that picks sensible starting points.

## What already exists (audit)

| Component | Location | Behaviour |
|-----------|----------|-----------|
| `AutoTuner` | `crates/strategy/src/autotune.rs:9-628` | 4 regimes (Quiet / Trending / Volatile / MeanReverting) via variance + lag-1 autocorr + Hurst. Emits multiplicative factors on γ / spread / size / refresh. Feeds VPIN → toxicity_spread_mult and Market Resilience → MR inverse factor. **Not online-adaptive** — multipliers are per-regime constants. |
| `Hyperopt` | `crates/hyperopt/` | Offline random-search over `SearchSpace`. Loss functions: Sharpe, Sortino, Calmar, MaxDrawdown, MultiMetric. JSONL trial log. **No auto-apply** to engine. |
| `ConfigOverride` pipeline | `crates/dashboard/src/state.rs:16-65` + `crates/engine/src/market_maker.rs:3052-3145` | Hot-reloadable: `Gamma`, `MinSpreadBps`, `OrderSize`, `MaxDistanceBps`, `NumLevels`, `MaxInventory`, several feature toggles. Flows via `tokio::mpsc::UnboundedReceiver` attached on construction; engine applies on next refresh tick. Audited. |
| `PerformanceTracker` | `crates/risk/src/performance.rs` | Has `fill_rate` accessor but no per-minute rolling stream. Refreshed on every status tick (~500 ms). |
| `PnlTracker` | `crates/risk/src/pnl.rs` | Tracks `spread_pnl` / `inventory_pnl` / `rebate_income` / `fees_paid`. Refreshed on every fill. No time-series for adaptive feedback. |
| `AdverseSelectionTracker` | `crates/risk/src/toxicity.rs` | Per-side (`bid`/`ask`) fill probability → `as_prob` fed to `StrategyContext`. **Closest we have to per-side fill intensity** but it's not κ itself. |

## What's missing

1. **`PairClass` taxonomy.** No enum / no classifier. Every symbol treated the
   same.
2. **Per-minute fill-rate stream.** Needed to drive online γ feedback. Must be
   separate from `PerformanceTracker.fill_rate` (cumulative since start).
3. **Online κ_bid / κ_ask estimation.** `AdverseSelectionTracker` is the
   closest thing but it tracks `p_fill`, not arrival intensity in the A-S
   sense.
4. **Ship-tested per-class config templates.** `config/*-paper.toml` files
   today ship a single set of defaults tuned for BTCUSDT.
5. **Hyperopt trigger from dashboard.** Today hyperopt runs from a separate
   CLI; results don't flow back to a running engine.

## Integration risks surfaced by the audit

1. **Multiplier-stack staleness.** `AutoTuner::effective_gamma_mult()` reads
   cached `regime` + `inventory_snapshot` from the previous `update_policy_state`
   call. An external tuner updating asynchronously would lag by one tick or
   tear state. **Mitigation:** run adaptive tuner on the same tick boundary
   that `update_policy_state` fires.
2. **Dashboard publish cadence.** `TunableConfigSnapshot` is pushed on every
   symbol refresh (≈500 ms). An online tuner suggesting new γ at 50 ms cadence
   would out-pace the UI and operators would see stale values.
   **Mitigation:** dashboard pushes a `TunableConfigSnapshot` on every
   `AdaptiveTuner` adjustment, not just on the refresh tick.
3. **Regime vs adaptive precedence.** Today AutoTuner monopolises regime
   widening. A new AdaptiveTuner computing different signals could compound
   multiplicatively with no precedence rule. **Mitigation:** explicit
   `override_source: RegimeDetector | AdaptiveTuner | Manual` tag on each
   multiplier, and dashboard surfaces which source won per tick.

## Design

### 1 — Pair-class taxonomy

```rust
pub enum PairClass {
    /// Major spot (BTC, ETH, SOL, BNB on top-tier venues).
    MajorSpot,
    /// Top-100 alt spot.
    AltSpot,
    /// Memecoin / low-cap spot — wide default spreads, high VPIN sensitivity.
    MemeSpot,
    /// Major perp (BTC-PERP, ETH-PERP).
    MajorPerp,
    /// Top-alt perp.
    AltPerp,
    /// Stablecoin-stablecoin pair (USDC/USDT, etc) — very tight, low vol.
    StableStable,
}

pub fn classify_symbol(product: &ProductSpec, daily_volume_usd: Decimal) -> PairClass {
    // Rule-based taxonomy. Inputs:
    //  - product.base_asset  → MAJOR_ASSETS = {BTC, ETH, SOL, BNB, XRP, ADA}
    //  - product.quote_asset → STABLE_ASSETS = {USDT, USDC, BUSD, FDUSD, TUSD, DAI}
    //  - product.default_wallet() → Spot vs Futures
    //  - daily_volume_usd → tier (major > $1B, alt > $100M, meme ≤ $100M)
    …
}
```

Classifier is pure (no I/O), called once at startup after `get_product_spec`
lands. Result stored on `SymbolState::pair_class` for dashboard display.

### 2 — Per-class template loader

New `config/pair-classes/` with 6 TOML files. Each contains ONLY the
`[market_maker]` + `[risk]` sections with class-appropriate defaults:

| Class | γ | κ_base | σ-floor | min_spread_bps | order_size notes |
|-------|---|--------|---------|----------------|------------------|
| MajorSpot | 0.05 | 20 | 0.00003 | 2 | tight |
| AltSpot | 0.15 | 8 | 0.0001 | 8 | medium |
| MemeSpot | 0.40 | 3 | 0.001 | 30 | wide |
| MajorPerp | 0.08 | 15 | 0.00004 | 3 | + funding handling |
| AltPerp | 0.20 | 5 | 0.00015 | 10 | medium |
| StableStable | 0.01 | 50 | 0.000003 | 0.5 | ultra-tight |

Engine on startup: `load_config()` → `classify_symbol()` → merge class template
over defaults → user's per-venue `config/*.toml` overrides on top.

### 3 — AdaptiveTuner (online controller)

New struct in `crates/strategy/src/adaptive.rs`. Opt-in via
`config.market_maker.adaptive_enabled = true`.

Inputs (rolling window, 1-minute buckets, 60-minute ring):
- Fill rate per symbol (`fills_in_window / window_secs`).
- Realised spread capture (bps, from `PnlTracker.spread_pnl` delta).
- Adverse selection bps (from `AdverseSelectionTracker`).
- Inventory volatility (EWMA of `|inventory - mean|`).

Outputs (`AdaptiveAdjustment` struct, published via ConfigOverride
equivalent):
- `gamma_delta_pct` (e.g. +5 %, −3 %)
- `min_spread_bps_delta_pct`
- `order_size_delta_pct`

Update rules (first-order feedback):
- If rolling fill rate < target AND inventory vol low → **decrease γ** (tighter
  quotes attract more fills).
- If rolling spread capture < fees AND adverse selection high → **increase γ
  + widen spread floor** (we're being picked off).
- If inventory vol > threshold × target → **increase γ** (reduce exposure).

Rate-limited: max ±5 % per minute, absolute bounds at 0.25× / 4× of
pair-class template defaults. Manual override wins — operator `ConfigOverride`
sets a floor/ceiling the adaptive tuner cannot cross.

### 4 — Multiplier stack (precedence)

Final γ at each tick =

```
γ_effective = γ_base(pair_class)
            × gamma_mult_regime(AutoTuner)    // 0.6..3.0, fast
            × gamma_mult_adaptive(AdaptiveTuner) // 0.25..4.0, slow
            × gamma_mult_manual(ConfigOverride) // operator floor/ceiling
```

Each multiplier is labelled with its `override_source` so dashboard can show
which layer won. Order is fixed: pair_class base → regime (fast, reversible) →
adaptive (slow, cumulative) → manual (sticky, operator).

### 5 — Hyperopt re-calibrate button (admin)

Existing hyperopt crate runs offline against a saved recording. Dashboard
admin endpoint:

```
POST /api/admin/optimize/trigger
  { symbol, strategy, recording_window_min, num_trials }
```

1. Tell the running engine: "record the next N minutes to `data/hyperopt/<run>.jsonl`".
2. When recording ends, spawn hyperopt as a tokio task over the JSONL.
3. Compare best-trial params to current live config.
4. Stage as `ConfigOverride` in a **pending state** — dashboard shows a
   "Apply calibration" card the operator approves before anything changes.

No auto-apply. The whole flow is auditable through the existing audit log.

## Tests

- Classifier determinism (proptest): same `ProductSpec` + volume → same class.
- Template parse test (like Epic 28 `shipped_paper_configs_parse_and_validate`):
  every `config/pair-classes/*.toml` parses.
- AdaptiveTuner direction-of-change unit tests: synthetic input streams
  (low fill rate → γ decreases; high AS → γ increases; bounded).
- Rate-limit test: repeated adverse updates cap at ±5 %/min.
- Manual override precedence test: operator ceiling beats adaptive suggestion.
- Paper smoke on Binance + Bybit with `adaptive_enabled = true` for 30+ min,
  verify γ drifts in expected direction and no runaway oscillation.

## Operator UI

- **PairClass badge** next to symbol in header.
- **Adaptive panel:** current γ (split into base × regime × adaptive ×
  manual), last adjustment reason, rolling fill rate / spread capture graphs.
- **"Re-calibrate" button** (admin-only) → staged ConfigOverride card.
- **Disable adaptive** toggle in admin config endpoints.

## Delivery plan (tasks)

E30.1 research (this doc) ✅
E30.2 `PairClass` enum + `classify_symbol` + `SymbolState.pair_class`
E30.3 `config/pair-classes/*.toml` + template loader
E30.4 `AdaptiveTuner` core (rolling window, update rules, rate-limits) + opt-in flag
E30.5 Hyperopt admin trigger + staged-override flow
E30.6 Dashboard UI — badge, panel, button
E30.7 Tests + paper smokes
E30.8 `docs/guides/adaptive-calibration.md` operator guide

## Committed design decisions

Resolved after research, not asking the operator:

1. **Enum scope = single-symbol-on-one-venue properties.** `PerpFundingArb` /
   `XEMM` are strategy compositions over pairs-of-symbols; they belong under
   `config.market_maker.strategy`, not in `PairClass`.

2. **Rate-limit = ±5 % / minute on γ.** Rationale: A-S optimal half-spread is
   `γσ²(T−t) + (2/γ)·ln(1 + γ/κ)` — γ enters both terms, so small changes
   compound fast. 5 % / min lets γ double over ~15 min, matching typical
   regime-transition horizons. Stricter (1 %) is too slow to respond; looser
   (10 %) overshoots under noisy inputs. Existing `AutoTuner` already steps 2–3×
   on regime flips, so 5 % / min on the adaptive layer is gentler by design.

3. **Ship online controller FIRST (E30.4), defer hyperopt trigger (E30.5).**
   Online controller is self-contained inside the engine; hyperopt trigger
   needs admin API + staged-override UI + recording management. Operators can
   still run `hyperopt` CLI by hand today.

4. **Daily-volume fetch: new `get_24h_volume(symbol)` connector method with a
   default returning `None`.** Venues that implement it: Binance
   `/api/v3/ticker/24hr`, Bybit `/v5/market/tickers`, HL `metaAndAssetCtxs`.
   `None` → classifier treats the symbol as alt-tier by default.

5. **Major-assets whitelist (initial): BTC, ETH, SOL, BNB.** Extend via
   follow-up based on operator feedback. XRP, ADA, DOGE, TON, AVAX, LINK
   candidates but need per-deployment validation.

6. **`PairClass` is venue-agnostic.** Venue-specific tuning lives in per-venue
   templates that *inherit* from the class template.

7. **Volume tiers: major ≥ $1 B / day, alt ≥ $100 M, meme < $100 M.**
   Asset-name regex hint (`/.*(inu|doge|pepe|shib|moon|elon).*/i`) biases to
   `MemeSpot` regardless of volume — protects against accidental tight
   quoting on viral low-cap tokens that happened to hit big volume on a
   single day.

8. **Classifier is a pure function**
   `classify_symbol(product: &ProductSpec, daily_volume_usd: Option<Decimal>) -> PairClass`.
   Volume fetched separately by `fetch_daily_volume(connector, symbol)` helper
   — keeps the classifier unit-testable without a network.

## Non-goals for this epic

- Bayesian / CMA-ES hyperopt search — stays as random search.
- Cross-pair adaptation ("BTC going down → zip spread on SOL"). Portfolio risk
  already handles correlation at the capital-allocation layer.
- ML-based regime classifier. Current heuristic (variance + autocorr + Hurst)
  is enough; add only if operators hit specific regime-misclassification bugs.
