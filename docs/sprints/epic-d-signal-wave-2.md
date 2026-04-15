# Epic D — Signal wave 2

> Sprint plan for the fourth epic in the SOTA gap closure
> sequence (C → A → B → **D** → E → F). Epics C, A, B all
> closed stage-1 in Apr 2026. Epic D ships the four
> highest-impact microstructure signals production prop
> desks run that our feature layer currently lacks.

## Why this epic

The April 2026 SOTA research pass flagged the signal-to-
strategy ratio as lopsided: we have ~9 strategies but the
feature layer (`mm-strategy::features` + `mm-risk::toxicity`)
covers only the *first generation* of microstructure
signals — book imbalance, EWMA trade flow, basic
micro-price, tick-rule VPIN. Production research firms
(Cartea-Jaimungal-Penalva canon, Wintermute publications,
Flow Traders) have shipped four additional signal families
that materially tighten expected-edge calculation:

1. **Order Flow Imbalance** (Cont-Kukanov-Stoikov 2014).
   Not "signed trade volume" — the L1 bid / ask depth-
   change stochastic process. The 2014 paper shows it is
   a *significantly* stronger short-horizon predictor of
   price moves than trade flow alone on equity data, and
   Cartea ch.10 replicates the finding on crypto.
2. **Learned micro-price G-function** (Stoikov 2018).
   Our current `micro_price()` is the 1988 opposite-side-
   weighted form. Stoikov 2018 shows that on deep books
   the *learned* micro-price — where the mapping from
   `(imbalance, spread)` to expected future mid is fit
   empirically — beats the closed-form variant by
   50-70 bps-equivalent on short horizons.
3. **Bulk Volume Classification** (Easley-de Prado-O'Hara
   2012). Current `VpinEstimator` uses per-trade tick-rule
   classification, which suffers a well-documented 10-15%
   mis-classification rate on crypto's high-frequency
   tape. BVC classifies *bulk* volume via the CDF of price
   changes — more stable on volatile days and the canonical
   form paired with VPIN in every prop-desk implementation.
4. **Cartea adverse-selection closed-form pricing**
   (Cartea-Jaimungal ch.4 §4.3, Cartea-Donnelly-Jaimungal
   2017). Closed-form quoted spread widening as a function
   of the adverse-selection probability. Our current
   `AdverseSelectionTracker` just *measures* AS ex-post;
   it does not wire the measurement back into spread
   pricing as a closed-form component.

These four closures turn "signal wave 1" (v0.2-v0.4) into
"signal wave 2" (this epic) and move us from ~70% of the
Cartea-canon signal menu to ~95%.

## Scope (4 sub-components)

| # | Component | New module / extension | Why |
|---|---|---|---|
| 1 | Cont-Kukanov-Stoikov OFI | `mm-strategy::features::cks_ofi` (new) | L1 depth-change imbalance process. ~150 LoC, pure function of two consecutive `(bid_px, bid_qty, ask_px, ask_qty)` snapshots |
| 2 | Learned micro-price G-function | `mm-strategy::features::learned_microprice` (new) | Fitted `G(imbalance, spread_bucket) -> Δmid_bps` lookup; online batch fit, not online gradient descent. ~250 LoC |
| 3 | BVC for VPIN | `mm-risk::toxicity::BvcClassifier` + `VpinEstimator::feed_bvc` (extension) | Price-change CDF classification instead of tick rule. ~120 LoC net delta |
| 4 | Cartea AS closed-form spread | `mm-strategy::cartea_spread` (new module) | `quoted_half_spread(as_prob, gamma, sigma, T_minus_t)` closed form, integrates with existing Avellaneda/GLFT spread computation |

## Pre-conditions

- ✅ `LocalOrderBook::best_bid` / `best_ask` accessors (since v0.1.0)
- ✅ `Trade` type with `taker_side` field (since v0.1.0)
- ✅ `VpinEstimator` / `KyleLambda` / `AdverseSelectionTracker`
  from `mm-risk::toxicity` (since v0.2.0)
- ✅ `features::book_imbalance` / `micro_price` /
  `WindowedTradeFlow` as the wave-1 baseline
- ✅ `AvellanedaStoikov` + `GlftStrategy` spread computation
  path — the Cartea AS component is a multiplier applied
  to the existing quoted half-spread, not a replacement
- ✅ `autotune::AutoTuner` regime-detection shifts — Epic D
  signals reinforce these, they do not duplicate them

## Total effort

**4 sprints × 1 week = 4 weeks** matching Epic C / A / B
cadence:

- **D-1** Planning + Study (no code) — audit the wave-1
  feature layer, transcribe the four formula families
  from the source papers into
  `docs/research/signal-wave-2-formulas.md`, resolve
  open questions, pin APIs + DoD
- **D-2** Cont-Kukanov-Stoikov OFI + Learned micro-price
  G-function (sub-components #1 + #2)
- **D-3** BVC-classified VPIN + Cartea AS closed-form
  spread (sub-components #3 + #4)
- **D-4** Strategy integration (momentum alpha wiring +
  Avellaneda / GLFT spread-widening wiring) + audit +
  CHANGELOG + ROADMAP + memory + single epic commit

---

## Sprint D-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before code
lands. End the sprint with a per-sub-component design
note plus a resolved open-question list.

### Phase 1 — Planning

- [ ] Walk every relevant existing primitive
  (`features::book_imbalance`, `features::micro_price`,
  `features::WindowedTradeFlow`, `toxicity::VpinEstimator`,
  `toxicity::KyleLambda`, `toxicity::AdverseSelectionTracker`,
  `momentum::MomentumSignals`, `autotune::AutoTuner`,
  `AvellanedaStoikov`, `GlftStrategy`) and write a
  field-by-field delta of what Signal Wave 2 needs from
  each
- [ ] Pin the public API for the four sub-components
- [ ] Define DoD per sub-component
- [ ] Decide call-site shape: pure functions vs stateful
  structs (OFI needs state to remember previous snapshot;
  Learned MP needs a training/prediction split; BVC is
  stateful over the classification bars; Cartea AS is
  pure given inputs)

### Phase 2 — Study

- [ ] Read **Cont, Kukanov, Stoikov — "The Price Impact of
  Order Book Events"** (2014, *J. Financial
  Econometrics*). Pin the exact OFI definition (eqs. 2-5)
  so there is no "is this the right sign convention"
  rework in D-2
- [ ] Read **Stoikov — "The Micro-Price: A High-Frequency
  Estimator of Future Prices"** (2018, *Quantitative
  Finance*). Understand the `G(i, s)` function definition,
  the iterative fixed-point construction, and the
  batch-fit procedure
- [ ] Read **Easley, López de Prado, O'Hara — "Flow
  Toxicity and Liquidity in a High-Frequency World"**
  (2012, *Review of Financial Studies*). Pin the BVC
  formula (eq. 4: `V_B = V · CDF_ν((ΔP − μ) / σ)`) and
  the relationship between `ν` (degrees of freedom) and
  the classification stability
- [ ] Read **Cartea-Jaimungal-Penalva chapter 4 §4.3**
  ("Market Making with Adverse Selection"). Understand
  the closed-form spread widening derivation —
  specifically the `ρ` (AS probability) → spread
  multiplier path
- [ ] Audit `mm-strategy::features::micro_price` +
  `micro_price_weighted` — the existing micro-price is
  the *1988 Gatheral* form (opposite-side weighted),
  NOT the 2018 Stoikov learned form. Signal Wave 2 adds
  the learned form as a parallel function; the old form
  stays for its current callers
- [ ] Audit `mm-risk::toxicity::VpinEstimator` — the
  current per-trade classification lives in the `on_trade`
  entry point via `trade.taker_side`. BVC adds a parallel
  `on_bar` entry point that batches trades into bars
  and classifies volume via CDF. Both paths coexist —
  operators pick per config

### Open questions to resolve

1. **OFI horizon — per-event or per-bar?** Cont-Kukanov-
   Stoikov define OFI per-event (every L1 change fires
   one OFI observation). For HFT shops running direct
   L1 feeds this is the right granularity. For us, our
   `BookKeeper` state-updates on every WS message, so
   per-event is cheap. **Default: per-event**, with an
   optional EWMA smoother for downstream consumption —
   matches the CKS paper and the existing `TradeFlow`
   shape. Stage-2 can add per-bar aggregation if a
   consumer wants it.

2. **Learned micro-price G-function — closed form vs
   lookup?** Stoikov 2018 derives `G` via an iterative
   fixed-point that converges to the expected future
   mid conditional on `(imbalance, spread)`. Two
   implementation paths:
   - (a) **Closed-form Markov chain iteration**: maintain
     empirical transition probabilities between
     `(imbalance_bucket, spread_bucket)` states and run
     fixed-point iteration offline, emit a lookup table.
   - (b) **Simple histogram fit**: for each
     `(imbalance_bucket, spread_bucket)`, store the
     empirical average of `(next_mid − current_mid)`
     observed in the historical tape. Converges to the
     closed-form answer in the limit of infinite data,
     far simpler to implement, and is what Flow Traders
     publicly describes in their 2022 microstructure talk.
   - **Default: (b), histogram fit**. Simpler, ~80 LoC
     less than (a), and operationally identical at our
     sample sizes. Stage-2 can upgrade to the iterative
     fixed-point if sparse buckets surface.

3. **BVC degrees-of-freedom `ν`.** The CDF uses a
   Student-t distribution parameterised by `ν`. Easley
   et al. use `ν = 0.25` in the 2012 paper for
   S&P E-minis. Crypto's heavier tails suggest a
   different value. **Default: `ν = 0.25` from the
   source paper**, make it a config knob, operators tune
   per venue on the first live run.

4. **Cartea AS component — spread multiplier vs additive?**
   The Cartea derivation writes the optimal quoted half-
   spread as `δ* = (1/γ) ln(1 + γ/κ) + (1 − 2ρ) · σ · √(T−t)`
   where `ρ` is the AS probability. The `(1 − 2ρ)` term
   is effectively an *additive bps-scale* adjustment to
   the reservation-price-centered spread — skews the
   bid down and the ask up when AS is elevated.
   **Default: additive**. Matches the Cartea derivation
   and plugs cleanly into `AvellanedaStoikov::quotes`
   without breaking the existing `gamma` / `kappa` /
   `sigma` inputs. Stage-2 can add a symmetry-breaking
   per-side variant if adverse flow is asymmetric.

5. **Which strategy consumes the new signals in v1?**
   Avellaneda-Stoikov is the canonical MM quoter; GLFT
   is the Cartea-derived variant. Both take a
   `quoted_half_spread` input. **Default: both consume
   #4 (Cartea AS spread), only AvellanedaStoikov
   consumes #1+#2 as new alpha inputs via
   `MomentumSignals` in v1**. GLFT has its own
   closed-form alpha path; adding Epic D signals to GLFT
   is a stage-2 extension.

6. **Learned micro-price training data source.** The
   histogram fit needs historical `(imbalance, spread,
   Δmid)` triples. Sources:
   - (a) Live engine's `BookKeeper` — accumulate a
     rolling window while quoting
   - (b) Backtester's JSONL replay — offline batch fit
     from recorded tape
   - **Default: (b) offline fit via a CLI**
     (`mm-learned-microprice-fit --input=fixture.jsonl
     --output=learned_mp.toml`). Matches Epic B's
     `mm-pair-screen` pattern — offline helper, operator
     runs it on fixture data and drops the resulting
     TOML into config. Online streaming fit is a stage-2
     operational nicety.

### Deliverables

- Audit findings inline at the bottom of this sprint
  doc (same shape as Epic C / A / B Sprint 1 audit
  sections)
- Per-sub-component public API sketch
- All 6 open questions resolved with defaults or
  explicit "decide in Sprint D-2"
- Companion formulas doc at
  `docs/research/signal-wave-2-formulas.md`

### DoD

- Every sub-component has a public API sketch, a tests
  list, and a "files touched" estimate
- The next 3 sprints can execute without further open
  rounds
- Formulas doc passes a review for source attribution
  (no "Cartea ch.6 misattribution"-type errors — every
  formula cites its paper + section explicitly)

### Audit findings — existing primitives we reuse

Phase-2 audit done against the live tree on 2026-04-15.
Line counts are from `wc -l` at the time of the audit.

#### A — `features.rs` (1701 LoC, wave-1 feature library)

Existing wave-1 primitives that Signal Wave 2 **reuses
as-is**:

- `book_imbalance(bids, asks, k)` — sum-of-top-k signed
  imbalance. Used directly as the `I` input to the
  learned micro-price G-function. v1 passes `k=1`
  (L1 only) to match the Stoikov 2018 definition.
- `micro_price(bids, asks)` — 1988 opposite-side-
  weighted form. **Not replaced.** Existing callers
  (`BasisStrategy`, `CrossExchangeStrategy`) keep using
  it. The new `LearnedMicroprice::predict` is a
  parallel function with a different signature; call
  sites pick per strategy.
- `market_impact(levels, side, qty, ref_px)` — taker
  VWAP walker. Signal Wave 2 does not touch it.
- `TradeFlow` / `WindowedTradeFlow` — signed trade EWMA
  / windowed sum. The CKS OFI is conceptually parallel
  (both are flow signals) but semantically different
  (depth vs trades); v1 ships both side-by-side.
- `MicroPriceDrift` — already tracks the first-
  derivative of the *classic* microprice for momentum
  purposes. Signal Wave 2's learned microprice adds a
  second field to this struct (optional `learned_mp`
  snapshot) rather than replacing it.

**New submodules added by Epic D:**
- `features::cks_ofi::OfiTracker` — L1 depth-change
  observer
- `features::learned_microprice::LearnedMicroprice` —
  fitted `G(I, S)` lookup + `predict` function

**Verdict.** No breaking changes to wave-1 features.
Epic D is purely additive — two new primitives sitting
alongside the existing ones.

#### B — `toxicity.rs` (~270 LoC, wave-1 toxicity)

Existing wave-1 primitives:

- `VpinEstimator::on_trade` — per-trade tick-rule
  classification. **Kept.** New `on_bvc_bar` is a
  parallel entry point that skips the tick-rule path
  and feeds already-classified volumes directly into
  the bucketiser. Operators pick per-config which path
  to use; v1 defaults to the existing tick-rule path
  for backward compat.
- `VpinEstimator::vpin()` / `::is_toxic()` — consumers
  (`AutoTuner`, `AvellanedaStoikov` spread-widening)
  stay unchanged; they see the same output regardless
  of classification path.
- `KyleLambda` — already has a `signed_volume` input
  which is semantically equivalent to OFI at the bar
  level. Stage-2 could feed the new OFI-derived signed
  volume into KyleLambda for a tighter estimate, but
  that's out of scope for D-2.
- `AdverseSelectionTracker::adverse_selection_bps()` —
  **the bridge to sub-component #4**. Cartea AS spread
  reads this output, maps it to `ρ` via the logistic
  function in the formulas doc, feeds it into the
  Avellaneda spread computation. No changes to the
  tracker itself.

**New additions by Epic D:**
- `toxicity::BvcClassifier` — CDF-based bar
  classification with configurable `ν`
- `VpinEstimator::on_bvc_bar(buy_vol, sell_vol)` —
  new entry point
- Student-t CDF helper (pure `Decimal`, Abramowitz-
  Stegun rational approximation)

**Verdict.** Tick-rule and BVC paths coexist.
VpinEstimator's public `vpin()` output is identical in
shape; only the input path differs.

#### C — `momentum.rs` (341 LoC, alpha signal shift)

Current `MomentumSignals::alpha()` folds 5 inputs:
1. book imbalance (via `features::book_imbalance`)
2. signed trade flow (internal VecDeque)
3. classic micro-price drift
4. optional HMA slope
5. trade flow EWMA

Signal Wave 2 adds:
6. **optional OFI contribution** — when `with_ofi(tracker)`
   is called, `alpha()` adds `ofi_weight · ewma(OFI)`
7. **optional learned micro-price drift** — when
   `with_learned_microprice(model)` is called, the
   alpha adds `lmp_weight · (learned_mp − mid)`

Both additions are builder-pattern knobs so existing
`MomentumSignals` call sites stay byte-compatible. The
`autotune::AutoTuner` regime shifts can optionally gate
the weights (e.g. zero them during a quiet regime).

**Verdict.** Additive extension. ~100 LoC of new code
inside `momentum.rs` plus ~5 LoC per existing test to
verify the no-signal-attached case stays byte-identical.

#### D — `avellaneda.rs` spread computation

`AvellanedaStoikov::quotes` currently computes the
quoted half-spread via the wave-1 closed form
`δ = γσ²(T − t) + (2/γ) · ln(1 + γ/κ)`. v1 Epic D adds
an optional `as_prob: Option<Decimal>` input: when
`Some`, the quoted spread is additively widened by the
Cartea AS component. When `None`, behaviour is byte-
identical to pre-Epic-D.

Integration point: new `quotes_with_as(self, &ctx,
as_prob)` method alongside the existing `quotes(self,
&ctx)`. The engine's `refresh_quotes` reads the latest
`AdverseSelectionTracker` output, converts to `ρ`, and
calls the `_with_as` variant.

**Verdict.** No breaking change to the wave-1 spread
computation; new path is fully opt-in.

### Open questions — resolved

All six open questions resolved against the defaults
from the sprint plan:

1. **OFI horizon** → ✅ per-event. `OfiTracker::update`
   fires one observation per L1 snapshot. Downstream
   consumers smooth via `MomentumSignals`-level EWMA if
   they want the bar-level view.
2. **Learned microprice fit method** → ✅ histogram
   fit, offline CLI. `mm-learned-microprice-fit` reads
   a JSONL fixture and emits a TOML file the engine
   loads at startup. No online streaming fit in v1.
3. **BVC degrees of freedom** → ✅ default `ν = 0.25`
   from Easley et al. 2012, configurable per venue.
4. **Cartea AS component** → ✅ additive symmetric,
   matching CJP 2015 eq. 4.20. Per-side asymmetric
   variant deferred to stage-2.
5. **Which strategy consumes** → ✅ `AvellanedaStoikov`
   consumes #4 (Cartea AS) in v1. `MomentumSignals`
   consumes #1 + #2 as new alpha inputs. GLFT
   integration deferred to stage-2.
6. **Learned microprice training data** → ✅ offline
   backtester JSONL replay via the CLI. Same pattern
   as Epic B's `mm-pair-screen`.

### Per-sub-component API surface — pinned

Full formulas live in
`docs/research/signal-wave-2-formulas.md`. API types
pinned below.

#### #1 CKS OFI — `mm_strategy::features::cks_ofi`

```rust
pub struct OfiTracker { /* private */ }

impl OfiTracker {
    pub fn new() -> Self;
    pub fn seed(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    );
    pub fn update(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    ) -> Option<Decimal>;
    pub fn prev_snapshot(&self) -> Option<(Decimal, Decimal, Decimal, Decimal)>;
}
```

Files touched: `crates/strategy/src/features.rs`
(new `cks_ofi` submodule, ~150 LoC).

Tests list (≥10):
- first update returns `None` (no prior state)
- `seed` then `update` returns the correct diff
- bid moves up → positive bid contribution = new `Q_b'`
- bid unchanged → bid contribution = `Q_b' − Q_b`
- bid moves down → bid contribution = `−Q_b`
- ask moves down → negative ask contribution
- ask unchanged → `Q_a' − Q_a`
- ask moves up → positive `Q_a`
- sign convention: aggressive bid, passive ask → OFI > 0
- symmetric book (no change) → OFI = 0
- fixture from hand-computed CKS example matches

#### #2 Learned microprice — `mm_strategy::features::learned_microprice`

```rust
pub struct LearnedMicroprice { /* private */ }

pub struct LearnedMicropriceConfig {
    pub n_imbalance_buckets: usize,  // default 20
    pub n_spread_buckets: usize,     // default 5
    pub horizon_ticks: usize,        // default 10
    pub min_bucket_samples: usize,   // default 100
}

impl LearnedMicroprice {
    pub fn empty(config: LearnedMicropriceConfig) -> Self;
    pub fn from_toml(path: &Path) -> anyhow::Result<Self>;
    pub fn predict(&self, imbalance: Decimal, spread: Decimal) -> Decimal;
    // Training-side API (used by the CLI):
    pub fn accumulate(&mut self, imbalance: Decimal, spread: Decimal, delta_mid: Decimal);
    pub fn finalize(&mut self);
    pub fn to_toml(&self, path: &Path) -> anyhow::Result<()>;
}
```

Files touched:
- `crates/strategy/src/features.rs` (new
  `learned_microprice` submodule, ~250 LoC)
- `crates/strategy/src/bin/mm_learned_microprice_fit.rs`
  (new CLI binary, ~80 LoC)

Tests list (≥10):
- `empty` config returns zero predictions everywhere
- accumulate + finalize builds the right bucket means
- sub-threshold buckets clamp to zero
- imbalance bucket boundary at exactly `I = 0`
- imbalance bucket at `I = 1` (rightmost)
- spread bucket via quantile edges
- round-trip `to_toml` → `from_toml` preserves fit
- prediction monotone in imbalance when data is monotone
- horizon-10 prediction matches naive forward-mean
- CLI entry point parses a minimal fixture

#### #3 BVC — `mm_risk::toxicity`

```rust
pub struct BvcClassifier { /* private */ }

impl BvcClassifier {
    pub fn new(nu: Decimal, window_size: usize) -> Self;
    pub fn classify(&mut self, bar_dp: Decimal, bar_volume: Decimal)
        -> Option<(Decimal, Decimal)>;
    pub fn rolling_mean(&self) -> Option<Decimal>;
    pub fn rolling_std(&self) -> Option<Decimal>;
}

// VpinEstimator extension:
impl VpinEstimator {
    pub fn on_bvc_bar(&mut self, buy_vol: Decimal, sell_vol: Decimal);
}

// Free function, used internally:
fn student_t_cdf(z: Decimal, nu: Decimal) -> Decimal;
```

Files touched:
- `crates/risk/src/toxicity.rs` (extension, ~120 LoC net)

Tests list (≥10):
- warmup returns `None` until window ≥ 10
- zero-variance window returns `None`
- positive `ΔP` classified as majority buy
- negative `ΔP` classified as majority sell
- exact zero `ΔP` after mean-0 window → 50/50 split
- Student-t CDF matches table at `z=0, ν=0.25` → 0.5
- Student-t CDF `z=+∞` → 1.0
- Student-t CDF `z=-∞` → 0.0
- total `buy + sell == bar_volume` always
- `on_bvc_bar` bucket finalization matches `on_trade`
  result on the same underlying buy/sell split

#### #4 Cartea AS spread — `mm_strategy::cartea_spread`

```rust
pub fn quoted_half_spread(
    gamma: Decimal,
    kappa: Decimal,
    sigma: Decimal,
    t_minus_t: Decimal,
    as_prob: Decimal,
) -> Decimal;

pub fn as_prob_from_bps(as_bps: Decimal) -> Decimal;

// Internal helper, exported for tests:
pub fn decimal_ln(x: Decimal) -> Decimal;
```

Files touched:
- `crates/strategy/src/cartea_spread.rs` (new, ~180 LoC)
- `crates/strategy/src/lib.rs` (module export)

Tests list (≥10):
- `ρ = 0.5` → formula collapses to wave-1 base
- `ρ = 1.0` → spread shrinks below base (but clamped at 0)
- `ρ = 0.0` → spread widens by full `σ √(T−t)` term
- `as_prob_from_bps(0)` → 0.5
- `as_prob_from_bps(20)` → 1.0
- `as_prob_from_bps(−20)` → 0.0
- `as_prob_from_bps` clamps outside ±20
- `decimal_ln(1)` → 0
- `decimal_ln(e)` within ε of 1
- `decimal_ln` on numerical edge-values stable
- clamp at 0 never produces negative quoted spread

### Per-sub-component DoD — pinned

| # | Component | Files | LoC (est) | Tests | DoD |
|---|---|---|---|---|---|
| 1 | `cks_ofi` | `features.rs` submodule | ~150 | ≥10 | Pure stateful tracker, first-update `None`, all 3 price cases on both sides, hand-verified sign fixture |
| 2 | `learned_microprice` | `features.rs` submodule + CLI binary | ~330 | ≥10 | Histogram fit, bucket clamping, TOML round-trip, CLI produces valid TOML from a fixture |
| 3 | BVC | `toxicity.rs` extension | ~120 | ≥10 | Student-t CDF table-match, warmup semantics, buy+sell == total invariant, VpinEstimator `on_bvc_bar` parity with `on_trade` on equivalent volumes |
| 4 | `cartea_spread` | new module | ~180 | ≥10 | `ρ=0.5` ↔ wave-1 base identity, zero clamp, `decimal_ln` accuracy vs known values, bps→probability map bounds |

**Epic total**: ~780 LoC of new code across three
existing files plus two new files, ≥40 unit tests,
plus 1 end-to-end pipeline test in Sprint D-4.

---

## Sprint D-2 — CKS OFI + Learned micro-price (week 2)

**Goal.** Land sub-components **#1 (OFI)** and **#2
(Learned microprice)** as the two new feature primitives.

### Phase 3 — Collection

- [ ] Pull a synthetic book-event fixture from the
  existing backtester JSONL corpus — ~1h of BTCUSDT
  snapshots + deltas is enough to exercise both
  functions
- [ ] Generate a known-truth OFI sequence from a
  hand-constructed book-update fixture so the
  unit tests have a deterministic reference

### Phase 4a — Dev

- [ ] **Sub-component #1** — `mm-strategy::features::cks_ofi`:
  - `OfiTracker::new()` — holds the previous L1 snapshot
  - `OfiTracker::update(bid_px, bid_qty, ask_px, ask_qty) -> Option<Decimal>`
    — returns the new OFI observation, `None` on the
    very first update
  - Formula: see `signal-wave-2-formulas.md` §1
  - Pure `Decimal`, no allocations beyond the single
    previous-snapshot slot
- [ ] **Sub-component #2** —
  `mm-strategy::features::learned_microprice`:
  - `LearnedMicroprice` struct holding the fitted
    `G(imbalance_bucket, spread_bucket)` lookup
  - `::from_toml(path)` constructor that reads the
    offline-fit output
  - `::predict(imbalance, spread) -> Decimal`
  - Companion CLI binary `mm-learned-microprice-fit`
    in the `mm-strategy` crate's `src/bin/` folder
    (separate deliverable)
- [ ] ≥10 unit tests on each

### Deliverables

- `crates/strategy/src/features.rs` — extension or new
  submodule
- `crates/strategy/src/bin/mm_learned_microprice_fit.rs`
- ≥20 tests total
- Workspace test + fmt + clippy green

---

## Sprint D-3 — BVC VPIN + Cartea AS spread (week 3)

**Goal.** Land sub-components **#3 (BVC)** and **#4
(Cartea AS closed-form spread)**.

### Phase 4b — Dev

- [ ] **Sub-component #3** — `mm-risk::toxicity`:
  - New `BvcClassifier` struct holding a rolling window
    of price changes + running mean/std
  - `BvcClassifier::classify(bar_dp, bar_volume) -> (buy_vol, sell_vol)`
    using the Student-t CDF with the config `ν`
  - `VpinEstimator::on_bvc_bar(bar_dp, bar_volume)` —
    new entry point that bypasses the existing
    `on_trade` tick-rule path
- [ ] **Sub-component #4** —
  `mm-strategy::cartea_spread`:
  - New `CarteaSpread` module (pure functions, no state)
  - `quoted_half_spread(gamma, kappa, sigma, T_minus_t, as_prob) -> Decimal`
    closed-form formula — see
    `signal-wave-2-formulas.md` §4
  - Integrates with `AvellanedaStoikov::quotes` via an
    optional `as_prob` input
- [ ] ≥10 unit tests on each

### Deliverables

- `crates/risk/src/toxicity.rs` — extensions
- `crates/strategy/src/cartea_spread.rs` — new module
- ≥20 tests total
- Workspace test + fmt + clippy green

---

## Sprint D-4 — Strategy integration + audit + docs + commit (week 4)

**Goal.** Wire the new signals into the existing strategy
crates, close the epic: audit + CHANGELOG + CLAUDE +
ROADMAP + memory + single commit.

### Phase 4c — Dev

- [ ] `MomentumSignals` grows optional `ofi` +
  `learned_mp` inputs. When attached, the `alpha()`
  output folds in an OFI-weighted term and a
  learned-micro-price drift term alongside the existing
  book imbalance / trade flow / HMA components
- [ ] `AvellanedaStoikov::quotes` accepts an optional
  `as_prob` input. When `Some(p)`, the quoted half
  spread is widened by the Cartea additive component
  `(1 − 2p) · σ · √(T − t)` — clamped at zero so
  `p > 0.5` never produces a negative spread
- [ ] New audit event types `OfiFeatureSnapshot`,
  `AsSpreadWidened`. Stage-1 fires
  `OfiFeatureSnapshot` once per 30s summary interval
  (not per-event — would flood the audit log), and
  `AsSpreadWidened` on transitions (0 → wider and back)

### Phase 5 — Testing

- [ ] One end-to-end test that feeds a synthetic
  book-event stream through the full pipeline
  (OfiTracker → MomentumSignals → AvellanedaStoikov)
  and asserts the quoted spread widens when the
  synthetic adverse-selection probability spikes

### Phase 6 — Documentation

- [ ] CHANGELOG entry following the Epic A / B / C shape
- [ ] CLAUDE.md: add `cartea_spread` to the strategy
  crate module list, mention BVC/OFI/learned-MP in the
  features line, bump stats
- [ ] ROADMAP.md: mark Epic D as DONE stage-1, list
  stage-2 follow-ups (iterative fixed-point microprice
  fit, online-streaming learned MP, per-side AS
  asymmetry, GLFT AS integration)
- [ ] Memory: extend `reference_sota_research.md` with
  Epic D closure notes

### Deliverables

- `MomentumSignals` extension + `AvellanedaStoikov`
  extension
- 1+ end-to-end test
- CHANGELOG, CLAUDE, ROADMAP, memory all updated
- Single epic commit without CC-Anthropic line

---

## Definition of done — whole epic

- All 4 sub-components shipped or explicitly deferred
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per commit discipline
- `AvellanedaStoikov` accepts an `as_prob` input and
  widens the quoted spread via the Cartea closed form
- `MomentumSignals::alpha()` folds in OFI + learned-MP
  contributions
- `VpinEstimator` has a BVC entry point for operators
  who want the CDF-classified variant
- CHANGELOG, CLAUDE, ROADMAP, memory all updated

## Risks and open questions

- **OFI sign convention rework.** The CKS 2014 paper
  defines OFI such that positive = net bid-side
  pressure → price rises. A sign error in the v1 impl
  would produce alpha that fights the market. The unit
  tests must include a hand-verified fixture pinning the
  sign.
- **Learned micro-price overfitting.** Histogram fit on
  a small fixture produces sparse buckets. v1 clamps
  buckets with fewer than `MIN_BUCKET_SAMPLES` (default
  100) to zero — no prediction when the bucket is
  unreliable. Stage-2 can add a smoothing prior if
  operators want broader coverage.
- **BVC distribution assumption.** Student-t with
  `ν = 0.25` captures heavy tails but is ad-hoc for
  crypto. If the first live run shows VPIN-BVC
  diverging from VPIN-tick rule by > 15%, operators can
  tune `ν` or fall back to tick-rule via config.
- **Cartea AS closed form assumes symmetric adverse
  selection.** If the flow is persistently asymmetric
  (e.g. more informed buys than sells during a rally),
  the symmetric `(1 − 2ρ)` widening under-corrects one
  side. Stage-2 introduces per-side `ρ_b` / `ρ_a`.

## Sprint cadence rules

- **One week per sprint.** Friday EOD = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint D-1.** Planning + study only.
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of D-4.
