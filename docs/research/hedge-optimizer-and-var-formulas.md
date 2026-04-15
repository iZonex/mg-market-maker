# Hedge optimizer + VaR formulas — Epic C reference

> Transcription pass for Sprint C-1 of Epic C. Pins the exact
> formulas the dev sprints execute against with our variable
> names, NOT the academic ones, so there is zero
> "is this the right equation" rework in C-3.

## Source-attribution correction

The SOTA research doc
(`docs/research/production-mm-state-of-the-art.md`) cites
**"Cartea-Jaimungal-Penalva ch.6 for the hedge optimizer
closed form"** and **"ch.7 for VaR / drawdown"**. This is
wrong — the real table of contents (verified against
Cambridge University Press frontmatter [1]) is:

| Chapter | Topic |
|---|---|
| 6-7 | Optimal execution with continuous trading (Almgren-Chriss family) |
| 8 | Optimal execution with limit + market orders |
| 9 | Targeting volume (VWAP/TWAP) |
| **10** | **Market making** (Avellaneda-Stoikov + adverse-selection) |
| **11** | **Pairs trading and statistical arbitrage** |
| 12 | Order imbalance |

Corrected reference map for Epic C:

- **Hedge optimizer** — not in CJP at all. Canonical source
  is **Markowitz 1952 "Portfolio Selection"** [2] plus the
  **Merton 1972** closed-form extension [3]. The mean-variance
  optimization we need is pre-Cartea by ~60 years.
- **VaR / drawdown** — standard **parametric Gaussian VaR**
  from **Jorion "Value at Risk"** [4] and the **RiskMetrics
  technical document** [5]. CJP chapters 6-7 give us the
  *risk-adjusted execution* angle (Almgren-Chriss mean-variance
  penalty), which is a related but different formalism.
- **Adverse-selection-aware pricing** (for Epic D axis 1,
  not Epic C) — CJP **chapter 10** §10.4 has the real
  closed-form adjustment. The research doc's citation was
  right about the chapter number here.
- **Cointegration + Kalman pairs** (for Epic B, not Epic C) —
  CJP **chapter 11**. Correct when we get to Epic B.

## Sub-component #3 — Cross-asset hedge optimizer

### Goal

Given a portfolio exposure vector (per-factor delta from
sub-component #1) and a universe of hedge instruments, emit
the hedge basket that minimizes residual portfolio variance
subject to a funding-cost penalty. Input is a
`Vec<(Asset, Delta)>`, output is a
`Vec<(HedgeInstrument, Qty)>`.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `x` | `Vec<Decimal>` of length K | Current exposure vector — one entry per factor (BTC, ETH, SOL, USDC, …) |
| `h` | `Vec<Decimal>` of length M | Hedge instrument sizes (the thing we solve for) |
| `B` | `K×M` matrix | Beta matrix: `B[k][m]` = exposure of 1 unit of hedge instrument `m` to factor `k`. For a BTC-perp hedging BTC-delta, `B["BTC"]["BTC_PERP"] = 1`. Cross-beta (e.g. ETH-perp partially hedging BTC via correlation) deferred to v2. |
| `Σ` | `K×K` matrix | Covariance matrix of factor returns. v1 uses diagonal only — per-factor variance from a rolling 30-day mid-price window. |
| `f` | `Vec<Decimal>` of length M | Funding cost per hedge instrument in bps per holding interval. Perps charge funding, spot doesn't. |
| `λ` | `Decimal` | Funding-cost penalty coefficient. Operator-tuned. Default `1` = equal weight with variance. |
| `h_max` | `Decimal` | Per-instrument position cap (from `config.risk.max_inventory`). Hard constraint. |

### Closed-form solution (Markowitz 1952 → Merton 1972 form)

**Unconstrained minimum-variance hedge:**

```text
min_h  (x + B·h)^T · Σ · (x + B·h)
```

Setting `∂/∂h = 0`:

```text
2·B^T·Σ·(x + B·h) = 0
B^T·Σ·B·h = -B^T·Σ·x
h* = -(B^T·Σ·B)^(-1) · B^T·Σ·x
```

**With funding-cost L1 penalty** (our extension, the trivial
shrinkage version — not the full LASSO):

```text
min_h  (x + B·h)^T · Σ · (x + B·h) + λ·f^T·|h|
```

For the diagonal β case (`B = I` — each hedge instrument
hedges exactly one factor one-for-one) the solution collapses
to a per-factor loop:

```text
for each factor k:
    h_unconstrained[k] = -x[k]
    κ[k] = 1 / Σ_diag[k]           # inverse variance
    shrinkage = λ · f[k] · κ[k]
    |h_shrunk[k]| = max(0, |h_unconstrained[k]| - shrinkage)
    h_shrunk[k] = sign(h_unconstrained[k]) · |h_shrunk[k]|
    h_final[k] = sign(h_shrunk[k]) · min(|h_shrunk[k]|, h_max)
```

This is a **one-loop-over-K-factors computation** in pure
`Decimal` math. No matrix inversion needed for v1.

### v1 scope (what we actually ship in Sprint C-3)

1. **Diagonal β only** (`B = I`). MM desks do not use
   cross-beta hedging at v1 because the estimation error on
   off-diagonal β swamps the variance reduction.
2. **Per-factor variance only** (diagonal Σ). Off-diagonal
   correlations are deferred — same noisy-estimate argument.
3. **L1 funding-cost shrinkage** as above.
4. **Hard cap** from `config.risk.max_inventory`.
5. **No LP solver, no nalgebra dependency, no matrix ops.**
   Pure `Decimal` loops, ~80 LoC excluding tests.

### v2 stage-2 (deferred, tracked in ROADMAP follow-up)

- Full `B` matrix with off-diagonal β from linear regression
- Full `Σ` matrix with off-diagonal correlations
- `good_lp` or `clarabel` LP solver for the constrained LASSO
- Per-instrument tick/lot rounding via `ProductSpec`

### Implementation-ready pseudo-code

```rust
pub struct HedgeInstrument {
    pub symbol: String,           // "BTC-PERP"
    pub factor: String,           // "BTC"  — which factor it hedges
    pub mark_price: Decimal,      // for qty→notional conversion
    pub funding_bps: Decimal,     // per holding interval
    pub position_cap: Decimal,    // from config.risk.max_inventory
}

pub struct HedgeBasket {
    pub entries: Vec<(String, Decimal)>,  // (symbol, qty), signed
}

impl HedgeOptimizer {
    pub fn optimize(
        &self,
        exposure: &[(String, Decimal)],        // [(factor, delta)]
        universe: &[HedgeInstrument],
        factor_variances: &HashMap<String, Decimal>,
    ) -> HedgeBasket {
        let mut entries = Vec::new();
        for instrument in universe {
            let delta = exposure
                .iter()
                .find(|(f, _)| f == &instrument.factor)
                .map(|(_, d)| *d)
                .unwrap_or(Decimal::ZERO);
            if delta.is_zero() {
                continue;
            }
            let variance = factor_variances
                .get(&instrument.factor)
                .copied()
                .unwrap_or(Decimal::ONE);
            let kappa = if variance.is_zero() {
                Decimal::ZERO
            } else {
                Decimal::ONE / variance
            };
            // Unconstrained diagonal solution.
            let h_unconstrained = -delta;
            // L1 shrinkage.
            let shrinkage = self.funding_penalty * instrument.funding_bps * kappa;
            let mag = (h_unconstrained.abs() - shrinkage).max(Decimal::ZERO);
            let h_shrunk = if h_unconstrained.is_sign_positive() { mag } else { -mag };
            // Hard cap.
            let capped = if h_shrunk.abs() > instrument.position_cap {
                if h_shrunk.is_sign_positive() {
                    instrument.position_cap
                } else {
                    -instrument.position_cap
                }
            } else {
                h_shrunk
            };
            if !capped.is_zero() {
                entries.push((instrument.symbol.clone(), capped));
            }
        }
        HedgeBasket { entries }
    }
}
```

### Test matrix for Sprint C-3

| Test | Input | Expected |
|---|---|---|
| `flat_exposure_emits_empty_basket` | All deltas = 0 | Empty basket |
| `single_asset_trivial_hedge` | `BTC: +0.5`, universe has BTC-PERP | `-0.5 BTC-PERP` |
| `funding_cost_zero_reproduces_unconstrained` | `λ = 0` | Exactly `-x` per factor |
| `funding_cost_large_shrinks_to_zero` | `λ = 1000` | Empty basket — penalty dominates |
| `funding_cost_intermediate_partial_hedge` | Mid shrinkage, expected smaller-mag hedge | `|h_final| < |x|` |
| `hard_cap_clamps_oversized_hedge` | Delta = +10, position_cap = 2 | `-2 BTC-PERP` (clamped) |
| `missing_factor_in_universe_skipped` | ETH delta present, no ETH-PERP in universe | ETH leg absent from basket, no panic |
| `asymmetric_long_short_mix` | BTC +1, ETH -0.5 | BTC hedge is short, ETH hedge is long |
| `variance_zero_kappa_handled` | Variance = 0 for a factor | Shrinkage = 0 (no divide-by-zero), trivial hedge applied |
| `property_hedge_never_exceeds_cap` | Random property test: 100 random exposure vectors | `|h[m]| ≤ position_cap[m]` for all m |

## Sub-component #4 — Per-strategy VaR guard

### Goal

Rolling per-strategy PnL variance → parametric Gaussian
confidence interval → soft throttle multiplier. On breach,
push a multiplier into the engine's existing autotune path
alongside Market Resilience, InventoryGammaPolicy, and the
kill switch.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `P` | `VecDeque<Decimal>` per strategy class | Ring-buffered PnL samples over the rolling window |
| `W` | `Duration` | Rolling window length. Default `24h`. |
| `N` | `usize` | Sample count inside the window |
| `μ` | `Decimal` | Sample mean of `P` |
| `σ²` | `Decimal` | Sample variance of `P` |
| `σ` | `Decimal` | Sample std dev = `sqrt(σ²)` |
| `z_95` | `Decimal` | Standard normal quantile at 95% — `1.645` |
| `z_99` | `Decimal` | Standard normal quantile at 99% — `2.326` |
| `VaR_95` | `Decimal` | 95%-VaR = `μ - z_95·σ` (signed, negative = loss) |
| `VaR_99` | `Decimal` | 99%-VaR = `μ - z_99·σ` |
| `VaR_limit_95` | `Decimal` | Operator-configured 95%-VaR floor |
| `VaR_limit_99` | `Decimal` | Operator-configured 99%-VaR floor |

### Parametric Gaussian VaR formula (RiskMetrics standard) [5]

```text
μ   = (1/N) · Σ P_i
σ²  = (1/(N-1)) · Σ (P_i - μ)²
σ   = sqrt(σ²)

VaR_95 = μ - 1.645·σ
VaR_99 = μ - 2.326·σ
```

The z-scores are frozen constants from the standard normal
inverse CDF:

- `1.645` = `Φ⁻¹(0.95)` (one-sided)
- `2.326` = `Φ⁻¹(0.99)` (one-sided)

No `erf_inv` needed at runtime — they are compile-time
`dec!(1.645)` / `dec!(2.326)`.

### Throttle policy (our composition, not from any paper)

```text
if VaR_99 < VaR_limit_99:
    throttle = 0.0     # hard halt for this strategy class
elif VaR_95 < VaR_limit_95:
    throttle = 0.5     # halve size
else:
    throttle = 1.0     # no throttle
```

**Warmup.** The guard requires at least `min_samples` samples
before it will return anything other than `1.0`. Default
`min_samples = 30` — below that the Gaussian estimate is too
noisy to throttle on.

**Composition with the kill switch + MR + IGP.** The engine
computes the effective size multiplier as the **min** of all
contributing multipliers (max-restrictive wins, same shape
as today):

```text
effective_size_mult = min(
    kill_switch.size_multiplier(),
    market_resilience_mult,
    inventory_gamma_mult,
    var_guard.throttle(strategy_class),
)
```

This is the decision pinned in Sprint C-1 open question #4.
The VaR guard is strictly additive — it never relaxes a
pre-existing throttle.

### PnL-sampling cadence decision

Two options for how often we push a PnL sample:

1. **Every fill** — deterministic but creates a wildly
   inconsistent sample count per strategy (a high-churn
   strategy has 10× more samples than a low-churn one,
   skewing the variance estimate).
2. **Every N seconds** (say `N = 60`) — normalises the
   sample cadence so every strategy's Gaussian estimate has
   the same statistical weight.

**Decision: option 2, 60 s sample cadence.** `VarGuard::tick`
is called from the engine's existing 1 s `sla_interval` arm,
gates on `tick_count % 60 == 0`, and pushes
`pnl_tracker.attribution.total_pnl() - prev_total_pnl`
into the ring buffer. Ring buffer holds 24 h × 60 samples =
**1440 entries per strategy class** — same size as the P2.2
presence-bucket array.

### v1 scope (Sprint C-3)

- `HashMap<&'static str, VecDeque<Decimal>>` ring
  buffer per strategy class
- Constant z-scores for 95% / 99%
- `μ` / `σ²` computed on every throttle query (~O(1440)
  per call, cheap)
- Throttle policy as above
- New `AuditEventType::VarGuardThrottleApplied`
- New config fields `var_guard_enabled`,
  `var_guard_limit_95_usdt`, `var_guard_limit_99_usdt`

### v2 stage-2 (deferred)

- Exponentially-weighted moving variance (EWMA) for faster
  regime adaptation
- Historical-simulation VaR (empirical quantile, no Gaussian
  assumption) as a cross-check
- CVaR / expected-shortfall computation alongside VaR

### Test matrix for Sprint C-3

| Test | Input | Expected |
|---|---|---|
| `warmup_returns_one` | Fewer than 30 samples | Throttle = 1.0 |
| `zero_variance_returns_one` | 30 identical samples | σ = 0, VaR = μ, no breach → throttle 1.0 |
| `negative_drift_breaches_95_pct` | Mean = -100, σ = 20, limit_95 = -120, VaR_95 = -132.9 | Breach → throttle 0.5 |
| `severe_drift_breaches_99_pct` | Mean = -500, σ = 50, limit_99 = -600, VaR_99 = -616.3 | Breach → throttle 0.0 |
| `multi_strategy_isolation` | Strategy A in breach, B clean | Throttle(A) = 0.5, Throttle(B) = 1.0 |
| `rolling_window_evicts_stale_samples` | 1441 samples → window drops first | Buffer size caps at 1440 |
| `composition_with_kill_switch` | VaR throttle 0.5, kill switch throttle 0.0 | Effective = min(0.5, 0.0) = 0.0 |
| `composition_preserves_min_restriction` | VaR throttle 1.0, MR 0.3 | Effective = 0.3 (MR wins) |

## Bibliography

[1] Cartea, Á., Jaimungal, S., Penalva, J. "Algorithmic and
High-Frequency Trading." Cambridge University Press, 2015.
Frontmatter PDF:
`https://assets.cambridge.org/97811070/91146/frontmatter/9781107091146_frontmatter.pdf`.
Verified TOC shows MM is Ch 10, pairs trading is Ch 11,
execution is Ch 6-8. The SOTA research-doc attribution of
"Ch 6 hedge optimizer" was wrong.

[2] Markowitz, H. "Portfolio Selection." Journal of Finance,
7(1), 1952. The origin of mean-variance optimization.
Reprinted in every portfolio theory textbook.

[3] Merton, R. C. "An Analytic Derivation of the Efficient
Portfolio Frontier." Journal of Financial and Quantitative
Analysis, 7(4), 1972. Closed-form Markowitz frontier — the
formula transcribed here is its direct application to the
hedge-basket optimization problem.

[4] Jorion, P. "Value at Risk: The New Benchmark for Managing
Financial Risk." McGraw-Hill, 3rd ed. 2006. The canonical
VaR textbook. Parametric Gaussian VaR is in Ch 5.

[5] RiskMetrics Technical Document, 4th edition. J.P. Morgan,
1996. The industry reference for parametric VaR under the
normal assumption. The `1.645` / `2.326` z-scores we freeze
as constants are the one-sided standard-normal quantiles.

[6] Almgren, R., Chriss, N. "Optimal execution of portfolio
transactions." Journal of Risk, 3, 2001. The mean-variance
execution penalty that CJP ch. 6-7 formalizes. Not directly
used in Epic C but informs the "risk-adjusted PnL scoring"
framing in the VaR guard decision.
