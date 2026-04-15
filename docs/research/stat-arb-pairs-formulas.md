# Stat-arb pairs trading formulas — Epic B reference

> Transcription pass for Sprint B-1 of Epic B. Pins the
> formulas the dev sprints execute against so there is no
> "is this the right equation" rework in B-2 / B-3.

## Source attribution

Three primary references for v1:

[1] **Engle, R. F., Granger, C. W. J. "Co-integration and
Error Correction: Representation, Estimation, and
Testing."** *Econometrica*, 55(2), 1987. The two-step
Engle-Granger cointegration test is the classical method
for 2-leg pairs trading. Cited 30k+ times — every quant
finance textbook references it.

[2] **Cartea, Á., Jaimungal, S., Penalva, J. "Algorithmic
and High-Frequency Trading."** Cambridge University
Press, 2015. **Chapter 11 — Pairs Trading and Statistical
Arbitrage Strategies.** This is the *correct* CJP chapter
for stat-arb (the SOTA research doc's "Cartea ch.6 hedge
optimizer" mis-citation is unrelated; ch.11 is the actual
pairs reference, verified against the Cambridge frontmatter
TOC).

[3] **Welch, G., Bishop, G. "An Introduction to the Kalman
Filter."** UNC-CH TR 95-041, 2006 update. The standard
introductory reference for the linear-Gaussian Kalman
filter formulation we use for the adaptive hedge ratio.

The SOTA research doc also references **Hummingbot's
`cross_exchange_market_making`** as a public-source pairs
template — its operator-config shape informs our
`StatArbDriverConfig` API but the implementation is a
clean-room rewrite in Rust.

## Sub-component #1 — Engle-Granger cointegration test

### Goal

Given two synchronised price series `Y[t]` and `X[t]`
(both length `n`), decide whether `Y` and `X` are
cointegrated by:

1. Estimating the hedge ratio `β` via OLS regression
2. Computing the residual series `ε[t] = Y[t] − β·X[t]`
3. Running an Augmented Dickey-Fuller (ADF) test on the
   residuals
4. Comparing the ADF test statistic against MacKinnon
   critical values for a cointegration regression

If the ADF stat is below the critical value (more
negative), we reject the null of unit-root residuals —
the series are cointegrated.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `Y` | `&[Decimal]` length n | Dependent variable price series |
| `X` | `&[Decimal]` length n | Independent variable price series |
| `n` | `usize` | Sample size (must match between Y and X) |
| `β` | `Decimal` | OLS hedge ratio (`Cov(X,Y) / Var(X)`) |
| `α` | `Decimal` | OLS intercept (`mean(Y) - β·mean(X)`) |
| `ε[t]` | `Vec<Decimal>` length n | Residuals: `Y[t] - α - β·X[t]` |
| `Δε[t]` | `Vec<Decimal>` length n-1 | First differences of residuals |
| `ADF stat` | `Decimal` | t-statistic on the lagged residual coefficient in the ADF regression |
| `crit_5pct` | `Decimal` | MacKinnon 5% critical value (sample-size dependent) |

### Step 1 — OLS hedge ratio

```text
mean_X = (1/n) · Σ X[t]
mean_Y = (1/n) · Σ Y[t]
cov_XY = (1/n) · Σ (X[t] - mean_X)(Y[t] - mean_Y)
var_X  = (1/n) · Σ (X[t] - mean_X)²
β       = cov_XY / var_X
α       = mean_Y - β · mean_X
```

### Step 2 — Residuals

```text
ε[t] = Y[t] - α - β · X[t]
```

### Step 3 — ADF test on residuals

The simplest ADF regression (no lag terms, no trend):

```text
Δε[t] = ρ · ε[t-1] + u[t]
```

OLS-estimate `ρ` and its standard error. The ADF statistic
is `ρ_hat / SE(ρ_hat)`. Under the null hypothesis (residuals
are a unit root walk, no cointegration), this statistic
follows a non-standard distribution — it is NOT compared to
standard t-tables, but to **MacKinnon critical values**.

For a cointegration test on two variables (Y and X), the
5%-significance MacKinnon critical values are approximately:

| n   | crit_5pct |
|-----|-----------|
| 25  | -3.67     |
| 50  | -3.50     |
| 100 | -3.42     |
| 250 | -3.37     |
| 500 | -3.36     |
| ∞   | -3.34     |

A residual ADF stat **more negative** than the critical
value rejects the null — the series are cointegrated.

For v1 we hard-code a lookup table over these `n` values
and linearly interpolate; stage-2 can refine with the full
MacKinnon polynomial fit if operator demand surfaces.

### v1 simplifications

- **No constant term in the ADF regression** — the
  residuals `ε[t]` from step 2 already have mean zero
  by construction (OLS residuals).
- **Zero lag terms** in the ADF regression. This is the
  basic ADF; the *Augmented* form adds lag terms to
  handle higher-order autocorrelation in the residuals.
  v1 uses the basic form because at typical 30-day
  windows on hourly mid prices, autocorrelation beyond
  lag-1 is small. Stage-2 can add lag selection via AIC.
- **5%-significance only**. v1 does not expose 1% / 10%
  critical values. Stage-2 may add them if the operator
  wants to tune the entry strictness.

### Implementation-ready pseudo-code

```rust
pub struct CointegrationResult {
    pub is_cointegrated: bool,
    pub beta: Decimal,
    pub alpha: Decimal,
    pub adf_statistic: Decimal,
    pub critical_value_5pct: Decimal,
    pub sample_size: usize,
}

pub struct EngleGrangerTest;

impl EngleGrangerTest {
    pub fn run(y: &[Decimal], x: &[Decimal]) -> Option<CointegrationResult> {
        if y.len() != x.len() || y.len() < MIN_SAMPLES_FOR_TEST {
            return None;
        }
        let n = y.len();
        let (alpha, beta) = ols_2d(y, x);
        let residuals: Vec<Decimal> = y.iter().zip(x.iter())
            .map(|(yi, xi)| *yi - alpha - beta * *xi)
            .collect();
        let adf_stat = adf_basic_stat(&residuals);
        let crit = mackinnon_5pct_critical_value(n);
        Some(CointegrationResult {
            is_cointegrated: adf_stat < crit,
            beta,
            alpha,
            adf_statistic: adf_stat,
            critical_value_5pct: crit,
            sample_size: n,
        })
    }
}

const MIN_SAMPLES_FOR_TEST: usize = 25;

fn mackinnon_5pct_critical_value(n: usize) -> Decimal {
    // Lookup table from MacKinnon 1991 Table 6.1, 5%
    // significance for cointegration test with 2 variables.
    let table: &[(usize, Decimal)] = &[
        (25,  dec!(-3.67)),
        (50,  dec!(-3.50)),
        (100, dec!(-3.42)),
        (250, dec!(-3.37)),
        (500, dec!(-3.36)),
    ];
    // Linear interpolation between adjacent entries; clamp
    // at the extremes.
    interpolate_lookup(table, n)
}
```

## Sub-component #2 — Kalman filter for hedge ratio

### Goal

Track an adaptive hedge ratio `β[t]` that updates on
every new `(Y[t], X[t])` observation. Linear-Gaussian
state-space:

- **State**: `β[t]` (single scalar, the hedge ratio)
- **State evolution**: `β[t] = β[t-1] + w[t]`, where
  `w[t] ~ N(0, Q)` (transition noise variance)
- **Observation**: `Y[t] = β[t] · X[t] + v[t]`, where
  `v[t] ~ N(0, R)` (observation noise variance)

The Kalman filter alternates two steps per observation:

1. **Predict**: `β_pred = β_prev`,
   `P_pred = P_prev + Q`
2. **Update**: given new `(Y[t], X[t])`:
   - innovation: `e = Y[t] - β_pred · X[t]`
   - innovation variance: `S = X[t]² · P_pred + R`
   - Kalman gain: `K = X[t] · P_pred / S`
   - new state: `β_new = β_pred + K · e`
   - new variance: `P_new = (1 - K · X[t]) · P_pred`

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `β` | `Decimal` | Current hedge-ratio estimate |
| `P` | `Decimal` | Variance of the hedge-ratio estimate |
| `Q` | `Decimal` | Transition noise variance (operator-tuned) |
| `R` | `Decimal` | Observation noise variance (operator-tuned) |
| `e` | `Decimal` | Innovation (prediction error) |
| `S` | `Decimal` | Innovation variance |
| `K` | `Decimal` | Kalman gain |

### Default knobs

- `Q = 1e-6` — small transition noise so β drifts slowly
- `R = 1e-3` — moderate observation noise for crypto pairs
- `β_init = 1.0` — neutral starting hedge ratio
- `P_init = 1.0` — high initial uncertainty

These are starting points — operators tune per pair after
the first live run. Cartea ch.11 §11.3 has the formal
discussion of the Q/R tradeoff.

### Implementation-ready pseudo-code

```rust
pub struct KalmanHedgeRatio {
    beta: Decimal,
    variance: Decimal,
    transition_var: Decimal,
    observation_var: Decimal,
}

impl KalmanHedgeRatio {
    pub fn new(transition_var: Decimal, observation_var: Decimal) -> Self {
        Self {
            beta: dec!(1),
            variance: dec!(1),
            transition_var,
            observation_var,
        }
    }

    pub fn update(&mut self, y: Decimal, x: Decimal) -> Decimal {
        // Predict.
        let p_pred = self.variance + self.transition_var;
        // Update.
        let innovation = y - self.beta * x;
        let s = x * x * p_pred + self.observation_var;
        if s.is_zero() {
            return self.beta; // degenerate guard
        }
        let k = x * p_pred / s;
        self.beta = self.beta + k * innovation;
        self.variance = (Decimal::ONE - k * x) * p_pred;
        self.beta
    }

    pub fn current_beta(&self) -> Decimal {
        self.beta
    }

    pub fn current_variance(&self) -> Decimal {
        self.variance
    }
}
```

## Sub-component #3 — Z-score signal generator

### Goal

Given a stream of spread observations `s[t] = Y[t] − β[t]·X[t]`,
maintain a rolling mean and standard deviation of the
spread, compute the z-score `z[t] = (s[t] - mean) / std`,
and emit Open / Close / Hold actions based on configured
entry / exit thresholds.

### Welford's online algorithm

We use Welford's numerically stable online variance:

```text
mean_new = mean_old + (s_new - mean_old) / n_new
M2_new   = M2_old + (s_new - mean_old) · (s_new - mean_new)
var      = M2_new / (n_new - 1)
std      = sqrt(var)
z        = (s_new - mean_new) / std
```

For a fixed-size rolling window we maintain a `VecDeque`
of the last `window` observations and recompute Welford
when the front entry is evicted. Stage-2 can replace this
with an exponentially-weighted variant if memory becomes
a concern.

### Hysteresis bands

Entry threshold is wider than the exit threshold to
prevent flipping in and out on noise:

```text
entry_threshold (default 2.0)  → |z| > 2.0 → enter
exit_threshold  (default 0.5)  → |z| < 0.5 → exit
```

A position opened at `z = +2.0` (sell the spread)
remains open until `z` falls below `+0.5`, at which point
the position is closed.

### Action enum

```rust
pub enum SignalAction {
    /// |z| crossed the entry threshold from below — open
    /// a position. Sign of z indicates direction:
    /// z > 0 → spread is too high → sell Y, buy β·X.
    /// z < 0 → spread is too low → buy Y, sell β·X.
    Open { z: Decimal, direction: SpreadDirection },
    /// |z| crossed the exit threshold from above — close.
    Close { z: Decimal },
    /// Within the dead band, stay flat or hold position.
    Hold { z: Decimal },
}

pub enum SpreadDirection {
    /// z > 0: Y is overpriced relative to β·X.
    SellY,
    /// z < 0: Y is underpriced relative to β·X.
    BuyY,
}
```

## Sub-component #4 — StatArbDriver

### Goal

Compose the three building blocks into a single ticking
state machine that the engine drives via its select loop.
Mirrors the FundingArbDriver pattern from v0.2.0 Sprint H2.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `cointegration` | `Option<CointegrationResult>` | Most recent cointegration test result; `None` until first call to `recheck_cointegration` |
| `kalman` | `KalmanHedgeRatio` | Adaptive hedge-ratio tracker |
| `signal` | `ZScoreSignal` | Rolling-window z-score generator |
| `position` | `StatArbPosition` | Currently open position, `None` if flat |

### Driver event enum

```rust
pub enum StatArbEvent {
    /// Driver opened a position. `direction` says which leg
    /// is long / short, `qty` is the per-leg base-asset
    /// notional.
    Entered {
        direction: SpreadDirection,
        y_qty: Decimal,
        x_qty: Decimal,
        z: Decimal,
    },
    /// Driver closed a position.
    Exited { z: Decimal, realised_pnl_estimate: Decimal },
    /// Z-score within the dead band; nothing to do.
    Hold { z: Decimal },
    /// Latest cointegration test failed — series no longer
    /// cointegrated. Driver halts entry until a fresh
    /// cointegration check passes.
    NotCointegrated { adf_stat: Decimal },
    /// Driver is still warming up the rolling window.
    Warmup { samples: usize, required: usize },
}
```

### Tick loop pseudo-code

```rust
impl StatArbDriver {
    pub fn tick_once(&mut self, y_mid: Decimal, x_mid: Decimal) -> StatArbEvent {
        // 1. Update the Kalman filter with the new pair.
        let beta = self.kalman.update(y_mid, x_mid);

        // 2. Compute spread + z-score.
        let spread = y_mid - beta * x_mid;
        let z = match self.signal.update(spread) {
            Some(z) => z,
            None => {
                return StatArbEvent::Warmup {
                    samples: self.signal.sample_count(),
                    required: self.signal.window,
                };
            }
        };

        // 3. Cointegration gate (rechecks on a slow
        //    cadence; the cached result decides entry).
        if !self.is_currently_cointegrated() {
            return StatArbEvent::NotCointegrated {
                adf_stat: self
                    .cointegration
                    .as_ref()
                    .map(|c| c.adf_statistic)
                    .unwrap_or(dec!(0)),
            };
        }

        // 4. Signal action.
        let action = self.signal.decide(z, self.position.is_some());
        match action {
            SignalAction::Open { direction, .. } => {
                let (y_qty, x_qty) = self.size_legs(beta);
                self.position = Some(StatArbPosition { direction, y_qty, x_qty });
                StatArbEvent::Entered { direction, y_qty, x_qty, z }
            }
            SignalAction::Close { .. } => {
                let pnl = self.estimate_realised_pnl(spread);
                self.position = None;
                StatArbEvent::Exited { z, realised_pnl_estimate: pnl }
            }
            SignalAction::Hold { .. } => StatArbEvent::Hold { z },
        }
    }
}
```

The driver is **synchronous** — no async, no IO. The
engine's select-loop arm calls `tick_once` and routes the
event through the existing audit + dispatch path.
