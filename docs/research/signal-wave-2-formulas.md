# Signal wave 2 — Epic D reference

> Transcription pass for Sprint D-1 of Epic D. Pins the
> formulas the dev sprints execute against so there is no
> "is this the right equation" rework in D-2 / D-3.

## Source attribution

Four primary references for v1:

[1] **Cont, R., Kukanov, A., Stoikov, S. — "The Price
Impact of Order Book Events."** *Journal of Financial
Econometrics*, 12(1), 47–88 (2014). The canonical
definition of the L1 order flow imbalance process, with
empirical validation on NASDAQ equity data.

[2] **Stoikov, S. — "The Micro-Price: A High-Frequency
Estimator of Future Prices."** *Quantitative Finance*,
18(12), 1959–1966 (2018). Introduces the `G(imbalance,
spread)` function that maps L1 state to the expected
mid price some ticks ahead. Derives the closed-form
Markov-chain fixed-point construction and reports
empirical fit results on NASDAQ.

[3] **Easley, D., López de Prado, M., O'Hara, M. — "Flow
Toxicity and Liquidity in a High-Frequency World."**
*Review of Financial Studies*, 25(5), 1457–1493 (2012).
Introduces Bulk Volume Classification (BVC) for
estimating buy/sell volume fractions without trade-level
tick-rule classification. The paper that makes VPIN
operationally practical on fast tapes.

[4] **Cartea, Á., Jaimungal, S., Penalva, J. — "Algorithmic
and High-Frequency Trading,"** Cambridge University Press,
2015. **Chapter 4 §4.3 — Market Making with Adverse
Selection.** The canonical closed-form derivation of the
optimal quoted half-spread with an adverse-selection
component. Supporting reference:
**Cartea, Á., Donnelly, R., Jaimungal, S. — "Algorithmic
trading with model uncertainty."** *SIAM J. Financial
Math*, 8, 635–671 (2017).

Epic B's reference (`stat-arb-pairs-formulas.md`)
already pinned ch.11 (pairs) as the correct citation —
this doc pins ch.4 §4.3 as the AS citation. Both are
verified against the Cambridge frontmatter TOC.

---

## Sub-component #1 — Cont-Kukanov-Stoikov Order Flow Imbalance

### Goal

Given two consecutive top-of-book snapshots, compute a
signed L1 depth-change observation. Positive = net bid-
side pressure (price likely to rise); negative = net
ask-side pressure (price likely to fall).

Unlike "signed trade volume" (which our `TradeFlow` EWMA
already tracks), OFI captures *passive* depth changes:
a big limit buy posted at the touch counts as bid-side
pressure even though no trade happened.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `P_b` / `P_b'` | `Decimal` | Best bid price at `t-1` / `t` |
| `Q_b` / `Q_b'` | `Decimal` | Best bid qty at `t-1` / `t` |
| `P_a` / `P_a'` | `Decimal` | Best ask price at `t-1` / `t` |
| `Q_a` / `Q_a'` | `Decimal` | Best ask qty at `t-1` / `t` |
| `e_b` | `Decimal` | Bid-side contribution to OFI |
| `e_a` | `Decimal` | Ask-side contribution to OFI |
| `OFI` | `Decimal` | Net L1 order flow imbalance for this event |

### Formula — CKS 2014 eqs. (2)-(4)

The bid-side event contribution:

```text
         ⎧  Q_b'           if  P_b' > P_b    (bid moved up)
e_b[t] = ⎨  Q_b' − Q_b     if  P_b' = P_b    (bid unchanged, qty delta)
         ⎩  −Q_b           if  P_b' < P_b    (bid moved down)
```

The ask-side event contribution (sign inverted because
*less* ask depth = upward pressure):

```text
         ⎧  −Q_a'          if  P_a' < P_a    (ask moved down)
e_a[t] = ⎨  Q_a' − Q_a     if  P_a' = P_a    (ask unchanged, qty delta)
         ⎩  Q_a            if  P_a' > P_a    (ask moved up)
```

Final signed OFI is the sum of the two contributions —
but note the ask contribution is *subtracted* so positive
OFI consistently means upward pressure:

```text
OFI[t] = e_b[t] - e_a[t]
```

**Intuition.** A bid that moves up by one tick contributes
`+Q_b'` (all of the new bid qty — this was empty price
before, now it's bid). A bid unchanged in price but
growing in qty contributes `+(Q_b' − Q_b)` (just the
delta). A bid that moved down by one tick contributes
`−Q_b` (all of the old qty disappeared from that level).
Symmetric for asks, with the sign flipped in the final
sum.

### v1 simplifications

- **L1 only** — we track only the touch. CKS 2014 also
  defines a multi-level version but it needs an L2/L3
  feed that not every venue ships cleanly. v1 uses L1,
  stage-2 can extend.
- **Per-event, not per-bar.** The tracker fires one OFI
  observation per snapshot update. Downstream consumers
  (e.g. `MomentumSignals`) can EWMA the result if they
  want a smoothed signal.
- **No price-level normalisation.** CKS 2014 footnote 3
  suggests dividing OFI by average market depth to get
  a unitless ratio. v1 returns raw signed qty; callers
  who need a ratio can divide by `(Q_b + Q_a) / 2`.

### Implementation-ready pseudo-code

```rust
pub struct OfiTracker {
    prev: Option<(Decimal, Decimal, Decimal, Decimal)>,
}

impl OfiTracker {
    pub fn new() -> Self {
        Self { prev: None }
    }

    /// Fold one new L1 snapshot and emit the OFI observation
    /// relative to the previous snapshot. Returns `None` on the
    /// very first update (no previous state to diff against).
    pub fn update(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    ) -> Option<Decimal> {
        let (p_b, q_b, p_a, q_a) = self.prev?;
        let e_b = if bid_px > p_b {
            bid_qty
        } else if bid_px == p_b {
            bid_qty - q_b
        } else {
            -q_b
        };
        let e_a = if ask_px < p_a {
            -ask_qty
        } else if ask_px == p_a {
            ask_qty - q_a
        } else {
            q_a
        };
        self.prev = Some((bid_px, bid_qty, ask_px, ask_qty));
        Some(e_b - e_a)
    }

    pub fn seed(&mut self, bid_px: Decimal, bid_qty: Decimal,
                ask_px: Decimal, ask_qty: Decimal) {
        self.prev = Some((bid_px, bid_qty, ask_px, ask_qty));
    }
}
```

---

## Sub-component #2 — Learned Micro-price G-function

### Goal

The classic 1988 micro-price

```text
mp_classic = (Q_a · P_b + Q_b · P_a) / (Q_a + Q_b)
```

is the opposite-side-weighted fair value — already
shipped in `features::micro_price`. Stoikov 2018 shows
this is a *biased* estimator of the expected mid in a
few ticks because the book is not instantaneously
Markovian: the imbalance itself has autocorrelation and
the spread state carries extra information. The fix is
to learn the empirical function

```text
G : (imbalance, spread) → E[mid_{t+k} - mid_t | state_t]
```

and add it to the current mid:

```text
mp_learned = mid_t + G(imbalance, spread)
```

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `I` | `Decimal ∈ [-1, 1]` | L1 imbalance `(Q_b - Q_a) / (Q_b + Q_a)` |
| `S` | `Decimal` | Current bid-ask spread in ticks |
| `mid` | `Decimal` | `(P_b + P_a) / 2` |
| `k` | `usize` | Prediction horizon in ticks (default 10) |
| `n_I` | `usize` | Number of imbalance buckets (default 20) |
| `n_S` | `usize` | Number of spread buckets (default 5) |
| `G[i, s]` | `Decimal` | Fitted value at imbalance bucket `i`, spread bucket `s` |

### Histogram-fit formula — default v1 approach

For each historical `(I_t, S_t, mid_t)` observation and a
horizon `k`, compute the forward mid delta
`Δ_t = mid_{t+k} - mid_t`. Bucket `I_t` into one of
`n_I` equal-width bins on `[-1, 1]` (`bucket_I =
floor((I + 1) · n_I / 2)`) and `S_t` into one of `n_S`
bins using quantile-based edges computed offline. Each
bucket accumulates a running mean of `Δ`:

```text
G_raw[i, s] = (1 / count[i, s]) · Σ Δ[t where bucket(I_t)=i, bucket(S_t)=s]
```

Then clamp buckets with fewer than `MIN_BUCKET_SAMPLES`
(default 100) to zero to avoid noisy predictions:

```text
G[i, s] = G_raw[i, s]  if count[i, s] ≥ MIN_BUCKET_SAMPLES
G[i, s] = 0            otherwise
```

### Prediction

```text
predict(I, S):
    i = clamp(floor((I + 1) · n_I / 2), 0, n_I - 1)
    s = spread_bucket(S)
    return G[i, s]
```

The CLI tool `mm-learned-microprice-fit` reads a JSONL
book-event fixture, accumulates `(I, S, Δ)` triples, and
writes a TOML file with the fitted `G` matrix plus the
spread quantile edges.

### v1 simplifications

- **Histogram fit**, not iterative fixed-point. The
  Stoikov 2018 paper derives a Markov-chain fixed-point
  that converges to `G` without forward-looking data,
  but the histogram fit converges to the same answer in
  the limit of infinite data and is ~80 LoC simpler.
  Stage-2 upgrade path if sparse buckets surface.
- **Two-dimensional `G(I, S)`**, not higher-dimensional.
  Stoikov notes that adding imbalance-at-second-level
  or time-since-last-trade improves the fit but the
  dimensionality explodes. v1 ships the canonical 2D
  form.
- **Equal-width bins on imbalance**, quantile bins on
  spread. Imbalance is already bounded in `[-1, 1]` so
  equal-width works cleanly; spreads are unbounded on
  the right (stressed markets) so quantile bins give
  stable coverage.
- **No online update**. Fit is strictly offline —
  operators re-run the CLI on fresh tape periodically.

### Implementation-ready pseudo-code

```rust
pub struct LearnedMicroprice {
    g_matrix: Vec<Vec<Decimal>>,        // [n_I][n_S]
    spread_edges: Vec<Decimal>,         // length n_S - 1
    n_imbalance_buckets: usize,
    n_spread_buckets: usize,
}

impl LearnedMicroprice {
    pub fn from_toml(path: &Path) -> anyhow::Result<Self> { /* ... */ }

    pub fn predict(&self, imbalance: Decimal, spread: Decimal) -> Decimal {
        let i = imbalance_bucket(imbalance, self.n_imbalance_buckets);
        let s = spread_bucket(spread, &self.spread_edges);
        self.g_matrix[i][s]
    }

    /// For the fit CLI: fold one observation into the
    /// running histogram.
    pub fn accumulate(&mut self, imbalance: Decimal, spread: Decimal, delta_mid: Decimal);
    pub fn finalize(&mut self, min_samples: usize);
    pub fn to_toml(&self, path: &Path) -> anyhow::Result<()>;
}
```

---

## Sub-component #3 — Bulk Volume Classification

### Goal

Split a bar's total traded volume into *buy* and *sell*
fractions based on the price change over the bar,
without looking at individual trade `taker_side` flags.
Directly plugs into `VpinEstimator` — the feed the VPIN
paper actually used.

### Formula — Easley et al. 2012 eq. 4

For a bar with price change `ΔP` and total volume `V`:

```text
V_buy  = V · CDF_ν((ΔP - μ) / σ)
V_sell = V · (1 - CDF_ν((ΔP - μ) / σ))
       = V - V_buy
```

Where:

- `μ` = rolling mean of `ΔP` over the last `N` bars
- `σ` = rolling std of `ΔP` over the last `N` bars
- `CDF_ν` = Student-t CDF with `ν` degrees of freedom
  (EdP-O'H use `ν = 0.25` in the 2012 paper, which is
  heavy-tailed — crypto may need different, surfaced as
  a config knob)

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `bar_dp` | `Decimal` | Price change over the bar |
| `bar_volume` | `Decimal` | Total quote volume over the bar |
| `mu` | `Decimal` | Rolling mean of `bar_dp` |
| `sigma` | `Decimal` | Rolling std of `bar_dp` |
| `nu` | `Decimal` | Student-t degrees of freedom (config) |
| `z` | `Decimal` | `(bar_dp - mu) / sigma` |
| `cdf_z` | `Decimal` | `CDF_ν(z)` |

### Student-t CDF approximation

Full Student-t CDF is an incomplete beta function call,
which is painful in pure `Decimal`. v1 uses the
**Abramowitz-Stegun rational approximation** (§26.7.8)
good to ~5 decimal places on `ν ∈ [0.1, 30]` — more
than enough for the VPIN use case where the downstream
consumer tests only the rough magnitude (>0.3 or <0.3).

Full derivation + coefficient tables in the rust source;
the formula outline:

```text
If ν ≥ 30:
    cdf_z ≈ 1/2 + (1/2) · erf(z / √2)    (Normal approximation)

Else:
    x = ν / (ν + z²)
    cdf_z = 1 - (1/2) · I_x(ν/2, 1/2)    (regularised incomplete beta)
    if z < 0: cdf_z = 1 - cdf_z
```

v1 ships a minimal regularised-incomplete-beta
implementation via series expansion for `ν/2 ≤ 1` (our
default `ν = 0.25` case) — small, pure-`Decimal`, and
deterministic.

### v1 simplifications

- **Single `ν` value, operator-tuned.** No per-venue
  distribution fitting. Default `ν = 0.25` from the
  source paper; operators override in config after the
  first live run.
- **Rolling mean/std window fixed at `N = 50` bars**.
  Matches the default VPIN window of 50 buckets so the
  two estimators stay in phase.
- **Bar granularity = existing VPIN volume bucket.**
  The BVC feed runs at the same cadence as the
  existing `VpinEstimator::on_trade` path — one "bar"
  = one VPIN bucket. Operators pick volume-clock or
  time-clock bars via existing VPIN config.

### Implementation-ready pseudo-code

```rust
pub struct BvcClassifier {
    nu: Decimal,                    // Student-t ν
    window: VecDeque<Decimal>,      // recent bar Δprice values
    window_size: usize,
    sum: Decimal,
    sum_sq: Decimal,
}

impl BvcClassifier {
    pub fn new(nu: Decimal, window_size: usize) -> Self { /* ... */ }

    /// Classify one bar's volume into buy/sell fractions.
    /// Returns `None` until the window has at least ~10 bars.
    pub fn classify(&mut self, bar_dp: Decimal, bar_volume: Decimal)
        -> Option<(Decimal, Decimal)>
    {
        self.update_rolling_stats(bar_dp);
        let mu = self.mean()?;
        let sigma = self.std()?;
        if sigma.is_zero() { return None; }
        let z = (bar_dp - mu) / sigma;
        let cdf_z = student_t_cdf(z, self.nu);
        let buy = bar_volume * cdf_z;
        let sell = bar_volume - buy;
        Some((buy, sell))
    }
}

// VpinEstimator extension (no breakage to existing tick-rule path):
impl VpinEstimator {
    pub fn on_bvc_bar(
        &mut self,
        buy_vol: Decimal,
        sell_vol: Decimal,
    ) {
        // Skip the per-trade path entirely — feed the bucketiser
        // directly with the already-classified buy/sell volumes.
        self.current_buy_vol += buy_vol;
        self.current_sell_vol += sell_vol;
        self.current_total_vol += buy_vol + sell_vol;
        // ... same bucket finalisation as on_trade ...
    }
}
```

---

## Sub-component #4 — Cartea Adverse-Selection Closed-Form Spread

### Goal

Widen the quoted half-spread as a function of the
adverse-selection probability `ρ`. When `ρ = 0.5`
(flow is uninformed — 50/50 buy/sell), no adjustment.
When `ρ > 0.5` (more informed buys → adverse to a
market maker's long position), the ask widens and
the bid shifts down asymmetrically — but v1 uses the
symmetric simplification.

### Cartea-Jaimungal-Penalva ch.4 §4.3 formula

The closed-form optimal quoted half-spread with adverse
selection is (CJP 2015, eq. 4.20):

```text
δ*(t) = (1/γ) · ln(1 + γ/κ) + (1 − 2ρ) · σ · √(T − t)
```

Where:

- `γ` (gamma) — MM risk-aversion coefficient
  (dimensionless)
- `κ` (kappa) — order-arrival intensity decay constant
  (1/qty)
- `ρ` — probability that the next incoming trade is
  informed (adverse to the MM's inventory direction)
- `σ` — short-horizon volatility estimate
  (price / √time)
- `T − t` — time to horizon end (seconds)

When `ρ = 0.5`, the additive term vanishes and the
formula collapses to the classic Avellaneda-Stoikov
quoted spread. When `ρ > 0.5`, the additive term is
negative and the quoted spread **shrinks** on the side
the informed flow is hitting — wait, that's backwards.

**Sign convention (important, source of D-2 bugs if
unchecked).** CJP write the spread as
`(1 − 2ρ) · σ · √(T − t)`. `ρ > 0.5` makes this term
negative. Since it is *added* to the base spread, a
negative additive term *reduces* the spread on the side
the informed flow is most likely to attack — which is
the intended behaviour when you interpret `ρ` as
"probability the fill you just received was informed":
the MM shrinks the quoted spread ("gets out of the way")
to avoid getting run over. This matches CJP figure 4.6.

For v1 we interpret the term symmetrically — a single
`ρ` that widens or narrows *both* sides equally, pulled
from the existing `AdverseSelectionTracker` ex-post
measurement. Stage-2 introduces per-side `ρ_b` / `ρ_a`
for asymmetric flow.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `gamma` | `Decimal` | MM risk aversion |
| `kappa` | `Decimal` | Intensity decay |
| `sigma` | `Decimal` | Volatility estimate |
| `t_minus_t` | `Decimal` | Time to horizon end in seconds |
| `as_prob` | `Decimal ∈ [0, 1]` | Adverse-selection probability `ρ` |
| `base_half_spread` | `Decimal` | `(1/γ) · ln(1 + γ/κ)` — the wave-1 AS term |
| `as_component` | `Decimal` | `(1 − 2ρ) · σ · √(T − t)` |
| `quoted_half_spread` | `Decimal` | `max(0, base + as_component)` |

### v1 simplifications

- **Symmetric `ρ`** — one scalar, both sides widen or
  narrow together. Stage-2 extends.
- **Clamp at zero.** `ρ > 0.5` with large `σ · √(T−t)`
  could drive the quoted spread negative in principle.
  v1 clamps at `max(0, ...)` so the strategy never
  quotes through itself.
- **Short-horizon `T − t` is an engine-level config**
  (default 60 s, matches the refresh interval).
  Avellaneda-Stoikov already has a time-to-horizon
  input; we reuse it.
- **Source of `ρ`:** v1 reads the existing
  `AdverseSelectionTracker::adverse_selection_bps`
  output, converts bps → probability via a calibrated
  map (config-supplied logistic or piecewise linear).
  Default: `ρ = 0.5 + clip(as_bps / 20, -0.5, 0.5)`
  so `as_bps = 0` → `ρ = 0.5` (no effect), `as_bps = 10`
  → `ρ = 1.0` (maximal narrowing).

### Implementation-ready pseudo-code

```rust
pub fn quoted_half_spread(
    gamma: Decimal,
    kappa: Decimal,
    sigma: Decimal,
    t_minus_t: Decimal,
    as_prob: Decimal,
) -> Decimal {
    // Wave-1 Avellaneda-Stoikov base.
    let base = decimal_ln(Decimal::ONE + gamma / kappa) / gamma;
    // Cartea AS additive component.
    let as_component = (Decimal::ONE - Decimal::TWO * as_prob)
        * sigma
        * decimal_sqrt(t_minus_t);
    (base + as_component).max(Decimal::ZERO)
}

pub fn as_prob_from_bps(as_bps: Decimal) -> Decimal {
    // Piecewise linear map: as_bps in [-10, 10] → ρ in [0, 1].
    (dec!(0.5) + (as_bps / dec!(20)))
        .max(Decimal::ZERO)
        .min(Decimal::ONE)
}
```

`decimal_ln` is not in the `rust_decimal` crate — v1
adds a Newton-series implementation alongside the
existing `decimal_sqrt` helper. ~25 LoC, same family
as the sqrt helper.
