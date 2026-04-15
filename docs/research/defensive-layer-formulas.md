# Defensive layer formulas — Epic F reference

> Transcription pass for Sprint F-1 of Epic F. Pins the
> formulas the dev sprints execute against so there is no
> "is this the right equation" rework in F-2 / F-3.

## Source attribution

Three primary references for v1:

[1] **Makarov, I., Schoar, A. — "Trading and Arbitrage in
Cryptocurrency Markets."** *Journal of Financial Economics*,
135(2), 293–319 (2020). The empirical anchor for the
binance-futures → spot lead-lag at the 200-800 ms
horizon. §4 documents the mean reversion to the
arbitrage-free price after a leader move.

[2] **Cartea, Á., Jaimungal, S., Penalva, J. — "Algorithmic
and High-Frequency Trading."** Cambridge University Press,
2015. **Chapter 10 §10.4 — Trading on News.** The Poisson-
jump risk-process formulation of news impact. v1 ships the
simpler state-machine approximation (operator-tunable regex
priority lists + cooldowns); CJP's full closed form is a
stage-2 upgrade.

[3] **Wintermute Trading — "Crypto market making in
practice"** (public blog post / Twitter Spaces archive,
2023). Wintermute's first-class news-retreat control is the
operational template Epic F's state machine reproduces.
Specifically the 3-tier classification (`Critical / High /
Low`) and the per-tier cooldown pattern come from their
public discussion.

---

## Sub-component #1 — Lead-lag guard

### Goal

Subscribe to a "leader" venue mid-price feed (typically
Binance Futures perpetual for crypto, since it leads spot
by 200-800 ms). When the leader makes a sharp move (return
> N · σ over a short window), push a soft-widen multiplier
into the autotuner's spread computation BEFORE the slower
follower venue's quotes get hit.

This is the **defensive** form of latency arbitrage — we
cannot race HFTs to update our quotes faster, but we can
retreat preemptively when the leader signals an incoming
move.

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `mid[t]` | `Decimal` | Leader-side mid price at time `t` |
| `r[t]` | `Decimal` | Log return `ln(mid[t] / mid[t-1])` |
| `μ` | `Decimal` | EWMA mean of `r` |
| `σ²` | `Decimal` | EWMA variance of `r` |
| `σ` | `Decimal` | `√σ²` |
| `z[t]` | `Decimal` | `(r[t] − μ) / σ` |
| `α` | `Decimal` | EWMA decay factor (Lambda) |
| `M` | `Decimal` | Output multiplier in `[1, max_mult]` |

### EWMA mean and variance

Standard exponentially-weighted moving statistics:

```text
μ[t]  = α · r[t]      + (1 − α) · μ[t−1]
σ²[t] = α · (r[t] − μ[t−1])² + (1 − α) · σ²[t−1]
σ[t]  = √σ²[t]
```

`α` is chosen via half-life: `α = 1 − 0.5^(1/N)` where
`N` is the desired half-life in update-event count. Default
`N = 20` events at a 250 ms tick → ~5 s half-life.

### z-score → multiplier mapping

Once the EWMA std is warm:

```text
z[t] = (r[t] − μ[t−1]) / σ[t−1]
```

The widen multiplier `M` is a piecewise-linear ramp on
`|z[t]|`:

```text
       ⎧  1.0                                     if  |z| < z_min
M[t] = ⎨  1 + (max_mult − 1) · (|z| − z_min) /    if  z_min ≤ |z| ≤ z_max
       ⎪                       (z_max − z_min)
       ⎩  max_mult                                if  |z| > z_max
```

Defaults: `z_min = 2.0`, `z_max = 4.0`, `max_mult = 3.0`.
At `|z| = 2`, no widening (signal is plausibly noise).
At `|z| = 4`, full triple-spread widening.

### Decay back to neutral

After a trigger fires, the multiplier should decay back to
1.0 if no fresh trigger lands. v1 uses the same EWMA
mechanism: each new mid update produces a fresh z-score
using the current EWMA, and the multiplier is computed
from that — no separate decay timer needed. As `r[t]`
returns to neutral, z drops, M drops, the guard
deactivates naturally.

### v1 simplifications

- **Single leader** — one feed, one `LeadLagGuard`. Stage-2
  can support multiple leaders with weighted aggregation.
- **Symmetric trigger** — the guard fires on `|z|`, not
  signed `z`. A sharp move in either direction widens both
  sides equally. Per-side asymmetric widening is stage-2.
- **No staleness gate** — if the leader feed pauses, the
  EWMA std grows naturally and the |z| drops, so the
  guard self-deactivates. Stage-2 can add an explicit
  "leader feed pause > N seconds → freeze the multiplier
  at 1.0" gate if operators want it.
- **Decimal everywhere** — no f64 boundary needed because
  the math is just `+`, `−`, `·`, `÷`, and one `√` (the
  existing `volatility::decimal_sqrt` Newton helper).

### Implementation-ready pseudo-code

```rust
pub struct LeadLagGuardConfig {
    pub half_life_events: usize,   // default 20
    pub z_min: Decimal,             // default 2.0
    pub z_max: Decimal,             // default 4.0
    pub max_mult: Decimal,          // default 3.0
}

pub struct LeadLagGuard {
    config: LeadLagGuardConfig,
    alpha: Decimal,
    last_mid: Option<Decimal>,
    ewma_mean: Option<Decimal>,
    ewma_var: Option<Decimal>,
    last_multiplier: Decimal,
    last_z_abs: Decimal,
}

impl LeadLagGuard {
    pub fn new(config: LeadLagGuardConfig) -> Self {
        let alpha = compute_ewma_alpha(config.half_life_events);
        Self {
            config,
            alpha,
            last_mid: None,
            ewma_mean: None,
            ewma_var: None,
            last_multiplier: Decimal::ONE,
            last_z_abs: Decimal::ZERO,
        }
    }

    pub fn on_leader_mid(&mut self, mid: Decimal) {
        let prev = match self.last_mid {
            Some(p) => p,
            None => {
                self.last_mid = Some(mid);
                return;
            }
        };
        if prev.is_zero() {
            self.last_mid = Some(mid);
            return;
        }
        // Approximate log return as (mid - prev) / prev
        // for small moves — avoids decimal_ln in the hot
        // path. Accuracy is fine within 1% for |return| < 5%.
        let r = (mid - prev) / prev;
        self.update_ewma(r);
        self.last_mid = Some(mid);
        self.recompute_multiplier(r);
    }

    fn recompute_multiplier(&mut self, r: Decimal) {
        let (Some(mean), Some(var)) = (self.ewma_mean, self.ewma_var) else {
            self.last_multiplier = Decimal::ONE;
            return;
        };
        if var <= Decimal::ZERO {
            self.last_multiplier = Decimal::ONE;
            return;
        }
        let std = decimal_sqrt(var);
        if std.is_zero() {
            self.last_multiplier = Decimal::ONE;
            return;
        }
        let z_abs = ((r - mean) / std).abs();
        self.last_z_abs = z_abs;
        self.last_multiplier = ramp(
            z_abs,
            self.config.z_min,
            self.config.z_max,
            self.config.max_mult,
        );
    }

    pub fn current_multiplier(&self) -> Decimal { self.last_multiplier }
    pub fn current_z_abs(&self) -> Decimal { self.last_z_abs }
    pub fn is_active(&self) -> bool { self.last_multiplier > Decimal::ONE }
}
```

---

## Sub-component #2 — News retreat state machine

### Goal

Trip a defensive flag on a high-priority news headline so
the quoter widens or pulls in advance of the price move
that historically follows the headline. The retreat decays
on a per-class cooldown so a single old headline does not
hold the bot offline forever.

### State diagram

```text
                    ┌──────────────┐
                    │   Normal     │ ◄─── (cooldown expiry)
                    └──────┬───────┘
              Low headline │
                ┌──────────┼──────────┐
                ▼          ▼          ▼
            ┌───────┐  ┌────────┐  ┌──────────┐
            │  Low  │  │  High  │  │ Critical │
            └───┬───┘  └────┬───┘  └────┬─────┘
                │           │           │
            cooldown=0  cooldown=5m  cooldown=30m
```

Class promotion: a `High` headline arriving while in
`Low` state promotes to `High` and resets the High cooldown.
A `Critical` headline always promotes (or resets the
Critical cooldown). A `Low` headline arriving in `High` or
`Critical` state is a no-op (state cannot be demoted by
fresh headlines, only by cooldown expiry).

### Variable names

| Symbol | Type | Meaning |
|---|---|---|
| `state` | `NewsRetreatState` | Current state machine value |
| `entered_at_ms` | `i64` | Wall-clock of the most recent state entry |
| `cooldown_ms[s]` | per-state `i64` | Per-class cooldown in milliseconds |
| `priority(text)` | `&str → NewsClass` | Regex-matched classification |
| `M(state)` | `state → Decimal` | Spread multiplier per state |

### Multiplier table

```text
state     | multiplier | should_stop_new_orders
----------|------------|------------------------
Normal    | 1.0        | false
Low       | 1.0        | false   (alert only, no quote impact)
High      | 2.0        | false
Critical  | 3.0        | true    (kill switch L2)
```

### Classification

Operator config supplies three regex lists (one per class).
The `on_headline(text)` function tries each list in
priority order (Critical → High → Low) and returns the
first hit. If no list matches, the result is `None` and
the headline is logged but does not transition state.

### v1 simplifications

- **Operator-tuned regex lists** — no NLP, no ML
  classification. The 3-class scheme is what Wintermute
  and GSR publicly run.
- **Caller-supplied feed** — v1 does NOT depend on any
  HTTP service. Operators wire their own feed source
  (Telegram bot, file tail, paid Tiingo adapter) and
  call `on_headline(text)` for each item.
- **No headline deduplication** — v1 fires the same
  state machine on every call. Stage-2 can add a hash-set
  dedupe within the cooldown window.
- **Single instance per process** — multi-engine
  deployments share one state machine via
  `Arc<Mutex<NewsRetreatStateMachine>>` (same pattern as
  the asset-class kill switch from the production-spot
  gap closure epic).

### Implementation-ready pseudo-code

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRetreatState { Normal, Low, High, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsClass { Low, High, Critical }

pub struct NewsRetreatConfig {
    pub critical_patterns: Vec<String>,  // regex strings
    pub high_patterns: Vec<String>,
    pub low_patterns: Vec<String>,
    pub critical_cooldown_ms: i64,       // default 30 * 60_000
    pub high_cooldown_ms: i64,           // default 5 * 60_000
    pub low_cooldown_ms: i64,            // default 0
}

pub struct NewsRetreatStateMachine {
    config: NewsRetreatConfig,
    critical_re: Vec<Regex>,
    high_re: Vec<Regex>,
    low_re: Vec<Regex>,
    state: NewsRetreatState,
    entered_at_ms: i64,
}

impl NewsRetreatStateMachine {
    pub fn new(config: NewsRetreatConfig) -> Result<Self> { /* compile regexes */ }

    pub fn on_headline(&mut self, text: &str, now_ms: i64) -> NewsRetreatTransition {
        let class = self.classify(text);
        match class {
            None => NewsRetreatTransition::NoMatch,
            Some(cls) => self.maybe_promote(cls, now_ms),
        }
    }

    pub fn current_state(&mut self, now_ms: i64) -> NewsRetreatState {
        // Apply cooldown expiry lazily on read.
        let elapsed = now_ms - self.entered_at_ms;
        let cooldown = match self.state {
            NewsRetreatState::Normal => return NewsRetreatState::Normal,
            NewsRetreatState::Low => self.config.low_cooldown_ms,
            NewsRetreatState::High => self.config.high_cooldown_ms,
            NewsRetreatState::Critical => self.config.critical_cooldown_ms,
        };
        if elapsed >= cooldown {
            self.state = NewsRetreatState::Normal;
        }
        self.state
    }

    pub fn current_multiplier(&mut self, now_ms: i64) -> Decimal {
        match self.current_state(now_ms) {
            NewsRetreatState::Normal | NewsRetreatState::Low => dec!(1),
            NewsRetreatState::High => dec!(2),
            NewsRetreatState::Critical => dec!(3),
        }
    }

    pub fn should_stop_new_orders(&mut self, now_ms: i64) -> bool {
        matches!(self.current_state(now_ms), NewsRetreatState::Critical)
    }
}
```

The regex compile happens once in `new()`. v1 takes a
`regex` dependency from the workspace (already in use by
other crates — `mm-server` log filtering uses it).

---

## Sub-component #3 — Listing sniper (DEFERRED)

The listing sniper would discover *new* venue listings
(symbols not in the engine's universe at startup) and
auto-spawn a probation engine that runs at wide spreads
and small size for the first ~24h to capture the opening
liquidity premium.

**Why deferred to stage-2:** the sniper needs a venue-level
`list_symbols` API on the `ExchangeConnector` trait. None
of the four current venue connectors (Binance, Bybit,
HyperLiquid, custom `mm-exchange-client`) expose this — the
trait only has `get_product_spec(symbol)` which requires
the symbol to already be known. Adding `list_symbols`
across 4 venue adapters + the per-venue REST endpoint
plumbing (`/api/v3/exchangeInfo`, `/v5/market/instruments-info`,
HyperLiquid's `meta` API, the custom client) is a
multi-venue sub-epic on its own — easily another 1.5 weeks
of work.

**Stage-2 follow-up tracked in ROADMAP:** see the Epic F
closure note.
