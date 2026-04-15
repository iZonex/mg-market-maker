# Epic D stage-3 — per-side ρ end-to-end wiring

> Closes the deferral that Epic D stage-2 Track 2
> documented: per-side asymmetric `ρ_b` / `ρ_a` for the
> Cartea closed-form spread shipped only as a pure function
> in `cartea_spread.rs` because threading it through
> `StrategyContext` would have conflicted with Track 1's
> file ownership of `crates/engine/src/market_maker.rs`.
> Now no concurrent agents — the wiring goes end to end.

## Why this stage-3

Stage-2 Track 2 shipped the per-side math
(`cartea_spread::quoted_half_spread_per_side`) but no
strategy or engine consumes it. This stage-3:

1. Adds two new optional fields on `StrategyContext`:
   `as_prob_bid: Option<Decimal>` and `as_prob_ask:
   Option<Decimal>`.
2. Updates `AvellanedaStoikov::compute_quotes` to compute
   `(bid_half_spread, ask_half_spread)` separately when
   both per-side fields are populated; falls back to the
   symmetric `as_prob` path when either is `None`.
3. Mirrors the per-side path in `GlftStrategy::compute_quotes`.
4. Extends `mm-risk::toxicity::AdverseSelectionTracker` with
   `adverse_selection_bps_bid` / `adverse_selection_bps_ask`
   convenience methods + `adverse_selection_bps_for_side`
   under the hood. The existing per-trade fill data already
   carries `Side`, so the per-side filter is a 30-LoC
   extension with no new state.
5. Threads the per-side bps through
   `MarketMakerEngine::refresh_quotes` via the existing
   `cartea_spread::as_prob_from_bps` mapping. Either side
   returning `None` (under-sampled) cleanly falls back to
   the symmetric path inside the strategy.

Net result: when the engine has ≥5 completed buy fills AND
≥5 completed sell fills, the strategy produces asymmetric
quotes that defend each side independently. When either
side is under-sampled, behaviour is byte-identical to the
pre-stage-3 Epic D stage-2 path.

## Source attribution

- **Cartea-Jaimungal-Penalva 2015 ch.4 §4.3 eq. (4.20)** —
  the closed-form per-side AS spread component
- The per-side function was already pinned in
  `docs/research/signal-wave-2-formulas.md` §"Sub-component
  #4" by Track 2; this sprint just wires it end to end

## Sub-components shipped

### #1 `AdverseSelectionTracker` per-side bps

`crates/risk/src/toxicity.rs`:

- New private helper `adverse_selection_bps_filter(side_filter)`
  that takes an `Option<Side>`. `None` reproduces the
  existing symmetric path; `Some(side)` filters the fill
  window before averaging.
- New public methods `adverse_selection_bps_for_side(side)`,
  `adverse_selection_bps_bid()` (= `Buy`), `adverse_selection_bps_ask()`
  (= `Sell`).
- Existing `adverse_selection_bps()` delegates to the new
  helper with `None`. Byte-identical output guaranteed by
  the unit tests.
- 4 new unit tests: under-threshold returns `None` per
  side, buy-only filter, sell-only filter, one-sided
  fill stream produces matching symmetric/per-side output.

### #2 `StrategyContext` per-side fields

`crates/strategy/src/trait.rs`:

- New optional fields `as_prob_bid: Option<Decimal>` and
  `as_prob_ask: Option<Decimal>`.
- The existing `as_prob: Option<Decimal>` field stays as
  the symmetric fallback. **When both per-side fields are
  `Some`, strategies use the per-side path and ignore
  `as_prob`.** When either is `None`, fall back to
  symmetric.
- All ~10 construction sites updated with `as_prob_bid:
  None, as_prob_ask: None` defaults via a Python bulk
  insert; manual fix-up on two test helper function
  signatures (`avellaneda::tests::ctx_with_as_prob` and
  `glft::tests::glft_ctx_with_as_prob`) to introduce a
  parallel `_with_per_side_as_prob` overload.

### #3 `AvellanedaStoikov` per-side path

`crates/strategy/src/avellaneda.rs`:

- `compute_quotes` now computes
  `(bid_half_spread, ask_half_spread)` via a `match`
  on `(ctx.as_prob_bid, ctx.as_prob_ask)`.
- `(Some, Some)` → per-side path: `base = spread/2`, then
  `bid_half = (base + (1-2·ρ_b)·σ·√(T-t)).max(half_min)`,
  `ask_half = (base + (1-2·ρ_a)·σ·√(T-t)).max(half_min)`.
  Each side individually clamped at `min_spread/2`.
- `(_, _)` → symmetric fallback: existing stage-2 logic
  unchanged. When `as_prob` is also `None`, byte-identical
  to wave-1.
- Quote loop uses `bid_half_spread` and `ask_half_spread`
  separately instead of a single `half_spread`.
- 3 new unit tests: per-side `None` is byte-identical to
  symmetric, only-one-set falls back to symmetric,
  asymmetric widens one side independently.

### #4 `GlftStrategy` per-side path

`crates/strategy/src/glft.rs`:

- Same `match` shape as Avellaneda. Per-side path adds
  the AS additive term independently to each side; level
  offsets use the average half-spread to preserve wave-1
  level-spreading semantics.
- 3 new unit tests mirroring the Avellaneda set.

### #5 Engine `refresh_quotes` per-side threading

`crates/engine/src/market_maker.rs`:

- Adds `as_prob_bid` and `as_prob_ask` derivations from
  the existing `AdverseSelectionTracker` via the new
  `_bid()` / `_ask()` accessors and
  `cartea_spread::as_prob_from_bps`.
- Populates the new `StrategyContext` fields. Either side
  being `None` falls back inside the strategy — no
  conditional logic at the engine level.

## File ownership matrix

| File | Stage-3 changes |
|---|---|
| `crates/risk/src/toxicity.rs` | +50 LoC (per-side helper + 4 new tests) |
| `crates/strategy/src/trait.rs` | +20 LoC (2 new struct fields + docs) |
| `crates/strategy/src/avellaneda.rs` | per-side `match`, debug log update, 3 new tests |
| `crates/strategy/src/glft.rs` | per-side `match`, debug log update, 3 new tests |
| `crates/strategy/src/{basis,cross_exchange}.rs` | construction-site `None` defaults |
| `crates/strategy/benches/strategy_bench.rs` | construction-site `None` defaults |
| `crates/backtester/src/simulator.rs` | construction-site `None` default |
| `crates/engine/src/market_maker.rs` | per-side derivation in `refresh_quotes`, construction-site update |
| `crates/engine/tests/integration.rs` | construction-site `None` defaults |

## Definition of done

- ✅ All construction sites pass new `None` defaults
- ✅ `AdverseSelectionTracker` exposes `_bid` / `_ask`
  convenience methods + 4 new unit tests
- ✅ `AvellanedaStoikov` per-side path + 3 new unit tests
- ✅ `GlftStrategy` per-side path + 3 new unit tests
- ✅ Engine `refresh_quotes` populates per-side fields
- ✅ Per-side `None` produces byte-identical output to
  symmetric path
- ✅ `cargo test --workspace` green (1001 → 1011)
- ✅ `cargo clippy --workspace --all-targets -- -D warnings` clean
- ✅ `cargo fmt --all --check` clean
- ✅ Single epic-stage-3 commit

## Open questions resolved

1. **Per-side measurement source.** Reuse the existing
   `AdverseSelectionTracker` rather than building a new
   per-side tracker. The fill window already carries
   `Side` so a 30-LoC filter extension is sufficient.

2. **Fallback semantics.** Per-side wins only when **both**
   `as_prob_bid` and `as_prob_ask` are `Some`. If either
   side is under-sampled (< 5 completed fills), fall back
   to the symmetric `as_prob`. The strategy decides at
   call time; the engine just threads `Option`s.

3. **GLFT level-offset semantics.** The wave-1 GLFT level
   spreading uses `level_offset = level * half_spread_t`.
   With per-side asymmetry, use the average:
   `(bid_half + ask_half) / 2`. This preserves backward
   compatibility with the level-stacking expectation
   while allowing per-side widening on level 0.

4. **Engine-level vs strategy-level fallback.** Strategy-
   level. Keeps the engine code minimal and lets the
   strategy decide what "per-side wins" means.

## Remaining stage-3+ follow-ups (not in this push)

- Engine-side per-side ρ display in dashboard / Prometheus
  metrics (operators currently see only the symmetric
  `as_prob`)
- Per-side ρ for strategies beyond Avellaneda + GLFT
  (Basis, CrossExchange) — currently they ignore both
  per-side and symmetric `as_prob`
- Cartea AS integration through `BasisStrategy`'s reservation
  shift path (Basis has its own spread computation distinct
  from Avellaneda/GLFT)
