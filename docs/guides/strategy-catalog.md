# Strategy Catalog

Comprehensive reference for every strategy, signal, and parameter-
modulator in MG Market Maker. Use this document to pick a strategy,
understand what it does in live, debug why it is (or isn't) quoting,
and reason about interactions with the risk and execution layers.

All anchors reference files under `crates/strategy/src/`. Config
defaults come from `crates/common/src/config.rs` — cross-reference
[configuration-reference.md](configuration-reference.md) for the
full TOML surface.

## Table of Contents

1. [How to read this catalog](#how-to-read-this-catalog)
2. [Selection matrix — which strategy, when](#selection-matrix)
3. [Base quoting strategies](#base-quoting-strategies)
   - Avellaneda-Stoikov, GLFT, Grid, Basis
4. [Cross-venue & paired strategies](#cross-venue--paired-strategies)
   - CrossExchange, XEMM, FundingArb, StatArb, PairedUnwind
5. [Execution algorithms](#execution-algorithms)
   - ExecAlgorithm trait + TWAP, VWAP, POV, Iceberg; standalone TWAP
6. [Alpha signals](#alpha-signals)
   - Momentum, book imbalance, micro-price, OFI, learned microprice,
     Cartea AS spread, volatility
7. [Parameter modulators](#parameter-modulators)
   - Regime auto-tuner, Adaptive tuner, Inventory skew, Market
     resilience, PairClass templates, Hyperopt recalibrate
8. [Engine pipeline — how it all fits](#engine-pipeline)
9. [Config recipes per PairClass](#config-recipes-per-pairclass)
10. [Operational gotchas](#operational-gotchas)

---

## How to read this catalog

Each strategy / signal entry follows the same shape:

- **Purpose** — one sentence, the problem it solves.
- **When to use it** — situations where it's the right tool.
- **When NOT to use it** — explicit anti-patterns.
- **Core formula** — the actual math the code runs.
- **Config** — TOML fields + defaults (cross-ref to
  `configuration-reference.md`).
- **Public API** — the Rust entry points — useful if you're wiring a
  new strategy or writing tests.
- **Gotchas** — known edge cases, numbered so they can be referenced
  from issues.
- **Source** — file:line anchor.

"Sprint" / "Epic" references throughout point to the ROADMAP history.
"Stage N" means a multi-stage epic where only stage ≤ N shipped.

---

## Selection matrix

Start here when you're asked "which strategy should I use for $SYMBOL?".

### By PairClass

| PairClass | Primary | Alpha overlay | Risk posture | Notes |
|---|---|---|---|---|
| **MajorSpot** (BTC/ETH spot) | Avellaneda-Stoikov | Momentum + OFI | Moderate γ, tight spread, full inventory skew | Liquid, tight-spread — queue position matters most |
| **MajorPerp** (BTC/ETH perp) | GLFT | Momentum + OFI, learned MP if fitted | Moderate γ, basis/funding overlay when paired | FundingArbDriver for funding-rate capture |
| **AltSpot** | Avellaneda-Stoikov | Momentum | Higher γ, wider spread | Watch for sudden vol spikes; MarketResilience helps |
| **AltPerp** | GLFT | Momentum | Higher γ, basis guard | Funding can flip hard — forecasting overlay (roadmap) |
| **MemeSpot** | Grid | none | Very high γ, small size, very wide min_spread | Cheap to get run over; favour survival over PnL |
| **StableStable** (USDC/USDT) | Grid narrow | none | Low γ, very tight spread, small levels | Mean-reverting by construction; alpha rarely helps |

### By inventory posture

| Situation | Recommended action | Strategy tweak |
|---|---|---|
| Flat, target inventory | Normal quoting | Any base + standard γ |
| Long near `max_inventory` | Asymmetric quoting, bias toward sells | Urgency via `AdvancedInventoryManager` |
| Short near limit | Asymmetric quoting, bias toward buys | Urgency via `AdvancedInventoryManager` |
| Need to unwind within T seconds | Stop quoting, execute | TWAP / POV / Iceberg via `ExecAlgorithm` |
| Kill switch L4 on paired position | Matched-slice flatten preserving delta | `PairedUnwindExecutor` |

### By market regime (auto-detected by `RegimeDetector`)

| Regime | Recommended tweak | Driven by `AutoTuner` |
|---|---|---|
| Quiet | Tighter spreads, more aggressive | γ×0.8 |
| Trending | Wider spreads, inventory priority | γ×1.2, inventory skew emphasised |
| Volatile | Wider spreads, reduced size, fast refresh | γ×1.5, `order_size`×0.7, `refresh_interval_ms`→300 |
| MeanReverting | Tighter spreads, larger size | γ×0.9, `order_size`×1.2 |

### Decision tree quick-ref

```
Do you have a hedge leg on a different venue?
├── YES — is it funding arb?
│   ├── YES → FundingArb + FundingArbDriver
│   └── NO — is the hedge on a different venue entirely?
│       ├── YES → CrossExchange (single-leg) or XEMM (slippage-aware)
│       └── NO (same venue) → Basis
└── NO — is the symbol tradeable as a pairs trade?
    ├── YES → StatArb (cointegrated pair, Kalman hedge ratio)
    └── NO — is it high-volatility / meme / thin?
        ├── YES → Grid (survival mode)
        └── NO — tight spread / liquid?
            ├── YES → Avellaneda-Stoikov or GLFT (GLFT if calibrated)
            └── NO — want simple? → Grid
```

---

## Base quoting strategies

All four produce `Vec<QuotePair>` via `fn compute_quotes(&self, ctx:
&StrategyContext) -> Vec<QuotePair>` (the `Strategy` trait). Engine
calls once per `refresh_interval_ms` tick.

---

### Avellaneda-Stoikov

**Purpose.** Optimal market making per Avellaneda & Stoikov (2008).
Computes a reservation price shifted by inventory risk and a spread
that balances execution intensity against inventory penalty.

**When to use.** General-purpose, well-understood, easy to tune. Great
first pick for MajorSpot and AltSpot. Pairs well with momentum alpha.

**When NOT.** Exotic pairs with unstable κ (order arrival intensity) —
the closed-form is sensitive to κ miscalibration. Prefer GLFT once you
have enough fills to calibrate κ.

**Core formula.**

```
reservation_price = mid - q · γ · σ² · (T - t)
optimal_spread    = γ · σ² · (T - t) + (2/γ) · ln(1 + γ/κ)
```

- `q` — signed inventory (positive = long)
- `γ` — risk aversion (higher widens + de-skews inventory faster)
- `σ` — annualised volatility
- `T - t` — time remaining in strategy horizon
- `κ` — order arrival intensity (fills/sec at best bid/ask)

Epic D stage-2/3 adds a Cartea-Jaimungal adverse-selection term on top:

```
additional_widening = (1 − 2·ρ) · σ · √(T − t)
```

where `ρ ∈ [0, 1]` is the informed-flow probability; `as_prob_bid` /
`as_prob_ask` can be set independently for asymmetric widening.

**Config (TOML).**

```toml
[market_maker]
strategy = "avellaneda_stoikov"
gamma = 0.1                 # risk aversion
kappa = 1.5                 # order arrival intensity
sigma = 0.02                # annualised vol estimate (auto-recalc at live)
time_horizon_secs = 300     # strategy cycle window
num_levels = 3
order_size = 0.001
min_spread_bps = 5          # hard floor
max_distance_bps = 100      # outermost level cap
```

**Public API.**

```rust
impl Strategy for AvellanedaStoikov {
    fn name(&self) -> &str { "avellaneda-stoikov" }
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair>;
}
```

**Gotchas.**

1. Zero `γ` or `κ` falls back to `min_spread_bps × mid / 2` (divide-
   by-zero guard). If you see a strategy quoting only at the minimum
   spread regardless of config, check these values at startup.
2. Borrow cost shim (Epic P1.3): when `borrow_cost_bps` is active for
   the pair, reservation price shifts UP by `borrow_cost_bps × mid /
   10_000` to penalise short fills. Flat inventory strategies do NOT
   shift.
3. `as_prob = Some(0.5)` and `as_prob = None` produce identical output
   (widening = 0) — this is intentional; the neutral signal is 0.5.
4. At `T - t → 0` (end of horizon) the variance term collapses — the
   engine rewinds `t` every refresh, so this is only an issue if you
   stop ticking the strategy.

**Source.** `crates/strategy/src/avellaneda.rs`.

---

### GLFT (Guéant-Lehalle-Fernandez-Tapia)

**Purpose.** Closed-form optimal MM with execution risk and bounded
inventory constraints. More disciplined than Avellaneda under varying
fill intensity; calibrates its own `a` and `k` online from observed
fill depths.

**When to use.** MajorPerp and any pair where you have enough fill
history to calibrate the intensity curve. Better than Avellaneda when
fill distribution is bimodal (rarely-filled deep levels vs. often-hit
tops).

**When NOT.** Cold start with no fill history (first 50 fills the
strategy uses naive defaults and may over- or under-quote).

**Core formula.**

```
half_spread = C1 · σ
skew        = σ · C2
C1 = (1 / (ξ·δ)) · ln(1 + ξ·δ/k)
C2 = sqrt( (γ / (2·A·δ·k)) · (1 + ξ·δ/k)^(k/(ξ·δ)+1) )
```

where `ξ = γ`, `δ = 1`, `A` and `k` calibrated from live fill depths.
Quotes applied with time scaling:

```
bid = fair - (half_spread · t + skew · t · q + level_offset)
ask = fair + (half_spread · t - skew · t · q + level_offset)
```

**Config.** Same TOML surface as Avellaneda-Stoikov. GLFT internally
maintains an `IntensityCalibration` that gets recalibrated after every
50 fills:

```
k_new = 0.9 · k_old + 0.1 · (1 / mean_fill_depth)
```

**Public API.**

```rust
impl Strategy for GlftStrategy {
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair>;
}
impl GlftStrategy {
    pub fn on_fill_depth(&mut self, depth_from_mid: Decimal);
}
```

**Gotchas.**

1. Cold start: until 50 fills land, `k = 1.5`, `a = 1.0` (defaults).
   First hour of live trading may look jumpy.
2. Exponent/log helpers clamp arguments and iterate up to 30 times
   (Newton-style). On degenerate inputs the output is bounded but
   possibly imprecise.
3. Same Cartea AS widening as Avellaneda (per-side or symmetric).

**Source.** `crates/strategy/src/glft.rs`.

---

### Grid

**Purpose.** Symmetric grid: N levels at equal intervals around the
reservation price (mid with simple inventory skew), no spread model.

**When to use.** Survival in chaotic pairs (memes, thin alts). Also
StableStable pairs where the "alpha" is the grid itself
(mean-reversion within a narrow band).

**When NOT.** Trending markets — Grid does not re-center fast enough.
AutoTuner's regime detector will widen it but it's still fundamentally
non-directional.

**Core formula.**

```
base_spread = min_spread_bps / 10_000 · mid
half_spread = base_spread / 2
level_step  = base_spread                 # each level one spread apart
center      = mid - inventory · 5 bps · mid  # light linear skew

level_i_bid = center - (half_spread + i · level_step)
level_i_ask = center + (half_spread + i · level_step)
```

**Config.**

```toml
[market_maker]
strategy = "grid"
num_levels = 5
order_size = 0.001
min_spread_bps = 10   # both half-spread and level step
```

`gamma`, `kappa`, `sigma` are ignored by Grid.

**Gotchas.**

1. No risk model — Grid does not back off when mid becomes uncertain
   (stale book, wide spread). Rely on `circuit_breaker` + `stale_book`
   watchdog to pull orders.
2. Inventory skew is fixed at 5 bps per unit — small for majors, big
   for memes. For thin pairs consider adding `AdvancedInventoryManager`
   on top if you want quadratic skew.

**Source.** `crates/strategy/src/grid.rs`.

---

### Basis

**Purpose.** Spot ↔ perp market making with reservation price shifted
toward the hedge-leg mid. Turns a quote on the spot leg into a
conditional cross-product trade — the hedge happens via a separate
`HedgeExecutor` on the other leg, not via this strategy.

**When to use.** You're market-making the spot leg of a coin that has
a liquid perp on the same venue (same-venue basis) or a different
venue (cross-venue basis via `cross_venue(...)` constructor).

**When NOT.** No hedge leg configured — Basis returns empty quotes if
`ctx.ref_price` isn't threaded through. Also: basis wider than
`max_basis_bps` — strategy deliberately stops quoting.

**Core formula.**

```
reservation = spot_mid + shift · (perp_mid - spot_mid)     # 0 ≤ shift ≤ 1
basis_bps   = (perp_mid - spot_mid) / spot_mid · 10_000
```

- `shift = 0` → quote at spot mid (ignore perp)
- `shift = 0.5` → quote halfway
- `shift = 1` → quote at perp mid

If `|basis_bps| > max_basis_bps`, quotes are empty (basis is too wide
to risk).

Cross-venue mode additionally gates on hedge-book staleness:

```
if hedge_book_age_ms > max_hedge_staleness_ms → return []
```

**Config.**

```toml
[market_maker]
strategy = "basis"          # or "cross_venue_basis"
basis_shift = 0.5           # 0 = spot only, 1 = perp-tracking
cross_venue_basis_max_staleness_ms = 1500  # only for cross_venue_basis

[hedge]
# required; see configuration-reference.md
```

**Public API.**

```rust
impl BasisStrategy {
    pub fn new(shift: Decimal, max_basis_bps: Decimal) -> Self;
    pub fn cross_venue(shift, max_basis_bps, max_staleness_ms) -> Self;
    pub fn reservation_price(spot_mid, hedge_mid) -> Price;
    pub fn basis_bps(spot_mid, hedge_mid) -> Decimal;
    pub fn expected_cross_edge_bps(
        maker_side, maker_price, size, hedge_book
    ) -> Option<Decimal>;
}
```

**Gotchas.**

1. Same-venue mode (`cross_venue(..., None)`) does NOT gate on hedge
   staleness — assumes co-located mids.
2. Borrow-cost shim is active if configured (shift reservation up on
   short side).
3. Max-distance clamps are asymmetric — bid clamped to `mid -
   max_distance`, ask on high end is unclamped (historical quirk —
   flagged for review).

**Source.** `crates/strategy/src/basis.rs`.

---

## Cross-venue & paired strategies

These strategies involve two legs — either two venues or two symbols
on one venue — and need extra plumbing (hedge exchange, instrument
pair, staleness gates).

---

### CrossExchange

**Purpose.** Make on venue A (post limit), hedge on venue B (take on
fill). Quotes at prices that guarantee profit after both venues' fees
and a minimum edge.

**When to use.** You have deep liquidity on a taker-friendly venue (B)
and want to earn maker rebates on a separate venue (A) while staying
flat in economic exposure.

**When NOT.** Fees are tight and edge is thin — fee-cost alone can
kill the edge. Also: if the two venues have stale or divergent mids,
hedge leg may slip badly.

**Core formula.**

```
total_fees = hedge_taker_fee + |maker_fee|
fee_cost   = total_fees · hedge_mid
min_profit = min_profit_bps / 10_000 · hedge_mid

min_ask = hedge_mid + fee_cost + min_profit
max_bid = hedge_mid - fee_cost - min_profit

level_i_ask = min_ask + i · 2 bps · hedge_mid
level_i_bid = max_bid - i · 2 bps · hedge_mid
```

Epic D stage-3 AS widening is **one-sided and clamped at zero** —
informed flow can only widen the quote, never narrow the profit floor.

**Config.**

```toml
[market_maker]
strategy = "cross_exchange"
cross_exchange_min_profit_bps = 5   # minimum edge in bps

[hedge]
# required — the venue where we take
```

**Public API.**

```rust
impl CrossExchangeStrategy {
    pub fn new(min_profit_bps: Decimal) -> Self;
    pub fn set_hedge_mid(&mut self, mid: Price);
    pub fn set_fees(&mut self, maker_fee: Decimal, hedge_taker_fee: Decimal);
    pub fn effective_hedge_buy(price: Price) -> Price;   // price · (1 + taker_fee)
    pub fn effective_hedge_sell(price: Price) -> Price;  // price · (1 - taker_fee)
    pub fn compute_quotes(ctx) -> Vec<QuotePair>;
}
```

**Gotchas.**

1. Prefers `ctx.ref_price` (engine-threaded hedge mid); falls back to
   `self.hedge_mid` or returns empty.
2. AS widening clamps at zero — negative `(1 − 2·ρ)` components don't
   narrow quotes (prevents sub-floor).
3. Cross-venue fee rates must be kept in sync manually — if you bump
   VIP tier on the hedge venue, update `set_fees()` or the edge drifts.

**Source.** `crates/strategy/src/cross_exchange.rs`.

---

### XEMM (Cross-Exchange MM Executor)

**Purpose.** Stateless slippage-aware executor that validates the
hedge leg when a maker fill lands. Emits `XemmDecision::Hedge` or
`XemmDecision::RejectSlippage` per fill. **Library-complete; not
wired to live engine yet** — pending SOR inline dispatch stage-2.

**When to use.** Once stage-2 ships: as the hedge side of a
CrossExchange-style pair where you need hard slippage bands (e.g.,
institutional accounts with tight tolerance).

**When NOT.** Now — it's not in the live execution path.

**Core formula.**

```
# Maker buy (long fill) → we need to sell on hedge
adverse = maker_price - hedge_best_bid

# Maker sell (short fill) → we need to buy on hedge
adverse = hedge_best_ask - maker_price

adverse_bps = adverse / maker_price · 10_000

if adverse_bps > max_slippage_bps → RejectSlippage
else if edge < min_edge_bps       → Hedge with warning
else                              → Hedge
```

**Config (struct).**

```rust
pub struct XemmConfig {
    pub max_slippage_bps: Decimal,  // default 20 bps
    pub min_edge_bps:     Decimal,  // default 0
}
```

**Public API.**

```rust
impl XemmExecutor {
    pub fn new(config: XemmConfig) -> Self;
    pub fn on_maker_fill(
        &mut self,
        maker_side: Side,
        maker_qty: Qty,
        maker_price: Price,
        hedge_best_bid: Price,
        hedge_best_ask: Price,
    ) -> XemmDecision;
}
```

**Gotchas.**

1. Not wired to live — integration test coverage only.
2. Primary and hedge inventory tracked separately; caller must call
   `on_hedge_fill` after the hedge lands (otherwise internal state
   diverges from reality).

**Source.** `crates/strategy/src/xemm.rs`.

---

### FundingArb + FundingArbDriver

**Purpose.** Atomic spot ↔ perp pair for capturing funding rate. The
executor (`FundingArbExecutor`) handles one round (market-first hedge,
then maker-post on primary, compensate on maker failure). The driver
(`FundingArbDriver`) turns this into a periodic tick loop.

**When to use.** Funding rate on a perp is consistently positive
(longs paying shorts) — short the perp, long the spot, collect
funding. Or the reverse. Crypto funding regimes are regime-specific
and well-documented (8h ticks on Binance/Bybit, 1h on HyperLiquid).

**When NOT.** Funding signal is weak, or basis risk dominates —
calculate expected gain vs. basis volatility cost explicitly.

**Core flow.**

```
1. Place market-take on hedge leg (taker, faster confirmation)
   |
   ├── rejects → position flat, clean Err
   |
2. Place maker-post on primary leg (post-only GTC)
   |
   ├── rejects → delta-exposed, fire reverse market
   |             on hedge to flatten (compensating leg)
   |
3. Both legs live → PairDispatchOutcome::Ok
```

Failure taxonomy:

```
FailedLeg::Taker — safe clean failure (no exposure committed)
FailedLeg::Maker — pair break; compensating order attempted
```

**Config.**

```toml
[market_maker]
strategy = "funding_arb"

[funding_arb]
tick_interval_secs = 60        # FundingArbDriver tick
# FundingArbCfg from persistence crate:
# min_funding_rate_bps, max_position, max_basis_bps, ...

[hedge]
# required — the perp venue
```

**Public API.**

```rust
impl FundingArbExecutor {
    pub fn new(primary, hedge, pair: InstrumentPair) -> Self;
    pub fn enter(&self, signal: FundingSignal)
        -> Result<PairDispatchOutcome, PairLegError>;
    pub fn exit(...);  // stubbed in current range
}

impl FundingArbDriver {
    pub fn new(primary, hedge, pair, config, sink) -> Self;
    pub async fn run(self, shutdown_rx: watch::Receiver<bool>);
}
```

**Gotchas.**

1. Executor does NOT query funding rates or update engine state on
   fill — all deferred to the driver (Sprint H2 scope).
2. Maker-leg rejection fires a reverse market — if that fails too,
   expect the kill switch to escalate to L2 StopNewOrders.
3. Funding tick timing matters — driver should run at least 1 minute
   before funding-time epoch, otherwise it's "trading against the
   clock".

**Source.** `crates/strategy/src/funding_arb.rs`,
`crates/strategy/src/funding_arb_driver.rs`.

---

### StatArb

**Purpose.** Cointegrated pair trading. Regresses two symbol mids
(Engle-Granger ADF), maintains an online Kalman hedge ratio, computes
a z-scored spread, emits open/close events when the spread deviates
beyond entry/exit thresholds.

**When to use.** Two symbols with stable cointegration — e.g.
BTC/ETH, SOL/AVAX, mid-cap alts within a sector. Historical ADF test
must reject the null (residuals stationary).

**When NOT.** Cointegration has broken — runs of `StatArbDriver` in
paper mode should confirm the regression is stable before committing.
Shock events (exchange halt, contract migration) also break it.

**Core components (Epic B).**

- `EngleGrangerTest` (`cointegration.rs`) — ADF on residuals, emits
  `CointegrationResult` with `is_cointegrated: bool`, `hedge_ratio`,
  `p_value`.
- `KalmanHedgeRatio` (`kalman.rs`) — online Kalman filter updating
  hedge ratio per tick; tracks both β and its covariance.
- `ZScoreSignal` (`signal.rs`) — rolling z-score of residual spread,
  emits `SignalAction::Open/Close` on configured thresholds.
- `StatArbDriver` (`driver.rs`) — tick-interval state machine
  composing the three; emits `StatArbEvent` via `StatArbEventSink`.
- `StatArbScreener` (`screener.rs`) — offline tool for discovering
  cointegrated pairs from historical data.

**Config (driver).**

```rust
pub struct StatArbDriverConfig {
    pub tick_interval: Duration,        // default 60 s
    // z-score thresholds, window sizes, etc. from ZScoreConfig
}

pub struct StatArbPair {
    pub y_symbol: String,      // dependent (regressed)
    pub x_symbol: String,      // independent (hedge leg)
    pub strategy_class: String, // e.g. "stat_arb_BTCUSDT_ETHUSDT"
}
```

**Gotchas.**

1. Stage status: driver scaffolding + sink + tick loop landed in
   B-3. Engine wiring (`MarketMakerEngine::with_stat_arb_driver`),
   audit events, and real per-pair PnL dispatch land in B-4.
2. Advisory-only currently — driver does not dispatch leg orders yet.
   Audit events `StatArbEntered` / `StatArbExited` record intent.
3. Rolling z-score window ≈ 200 ticks by default — cold-start period
   should be paper-only.

**Source.** `crates/strategy/src/stat_arb/`.

---

### PairedUnwind

**Purpose.** Flattens both legs of a basis / funding-arb position in
matched slices, preserving delta-neutrality throughout. Used by kill-
switch L4 escalation (AD-11, Sprint J).

**When to use.** Automatically, by the kill switch, when an active
paired position needs to be closed. Not user-driven.

**Core formula.**

```
slice_pair  = (primary_qty / N, hedge_qty / N)   # N = num_slices
hedge_qty   = primary_qty · pair.multiplier

# After slice k:
open_delta = ((N - k) / N) · initial_delta         # proportional

residual_delta = primary_remaining - hedge_remaining / multiplier
```

**Config (executor constructor).**

```rust
PairedUnwindExecutor::new(
    pair: InstrumentPair,
    primary_side: Side,
    hedge_side: Side,
    primary_qty: Qty,
    duration_secs: u64,
    num_slices: u32,
    aggressiveness_bps: Decimal,   // 0 = at mid (aggressive)
)
```

**Public API.**

```rust
impl PairedUnwindExecutor {
    pub fn pair(&self) -> &InstrumentPair;
    pub fn active(&self) -> bool;
    pub fn progress(&self) -> Decimal;             // average of both legs
    pub fn is_complete(&self) -> bool;
    pub fn residual_delta(&self) -> Decimal;
    pub fn next_slice(&mut self, p_mid, h_mid, ctx) -> SlicePair;
    pub fn on_fill(&mut self, leg: Leg, qty: Qty);
}
```

**Gotchas.**

1. No compensating logic on unilateral fill failures — asymmetric
   progress is accepted; next slice continues. Operator is expected
   to watch `residual_delta` and intervene if it grows.
2. One level per side per slice, not multi-level. Intentional — we
   want fills, not shelf-filling.

**Source.** `crates/strategy/src/paired_unwind.rs`.

---

## Execution algorithms

Separate from quoting strategies. Used when the engine needs to
*execute* a pre-decided quantity (unwind on kill, scheduled
rebalance, large portfolio shift) rather than earn spread.

**Status: library-complete; not currently wired to the live engine**
— waiting on SOR inline dispatch stage-2. Exec algos today run via
hyperopt replays and manual dashboard-triggered paths.

### ExecAlgorithm trait

```rust
pub trait ExecAlgorithm {
    fn on_fill(&mut self, client_order_id: Uuid, price: Price, qty: Qty);
    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction>;
    fn filled(&self) -> Decimal;
    fn remaining(&self) -> Decimal;
    fn is_finished(&self) -> bool;
}

pub enum ExecAction {
    Place { client_order_id: Uuid, quote: Quote },
    Cancel { client_order_id: Uuid },
    Hold,
    Done,
}
```

### TWAP (exec_algo.rs)

**Formula.**

```
slice_qty = total_qty / num_slices
t_i       = duration · i / num_slices      # schedule
final_slice_qty = total_qty - slice_qty · (N - 1)   # residual catch
```

### TWAP (standalone twap.rs)

Similar but independently instantiable — used by kill switch L4
graceful flatten and by exec replays.

```rust
pub fn new(
    symbol, side,
    target_qty,
    duration_secs, num_slices,
    aggressiveness_bps,    // 0 = at mid, higher = more passive
) -> TwapExecutor;

pub fn next_slice(&mut self, mid_price: Price) -> Option<Quote>;
pub fn on_fill(&mut self, qty: Qty);
pub fn is_complete(&self) -> bool;
pub fn progress(&self) -> Decimal;
```

**Slice trigger.**

```
elapsed        = (now - started_at).secs()
expected_slice = (elapsed · num_slices) / duration_secs
if current_slice < expected_slice → emit next slice
```

### VWAP, POV, Iceberg

All live under `exec_algo.rs` as `ExecAlgorithm` impls. Signatures
present; wiring deferred. See source for signatures and defaults.

**Source.** `crates/strategy/src/exec_algo.rs`,
`crates/strategy/src/twap.rs`.

---

## Alpha signals

Signals don't quote on their own — they shift the *reservation price*
fed into quoting strategies (Avellaneda, GLFT, Basis, etc.). The
engine composes them via `MomentumSignals::alpha()`.

### MomentumSignals

**Purpose.** Composite alpha that shifts reservation price per
Cartea-Jaimungal. Built from 4–6 components depending on what's
attached.

**Components.**

1. **Order-book imbalance** (top-k): `(bid_qty − ask_qty) / (bid_qty
   + ask_qty)` ∈ [−1, 1]. Positive = upward pressure.
2. **Trade flow EWMA**: signed volume with half-life decay.
3. **Micro-price drift**: weighted mid, leads mid by O(ms).
4. **HMA slope** (optional): Hull Moving Average on mid; captures
   trend without lag.
5. **CKS OFI EWMA** (optional, Epic D #1): Cont-Kukanov-Stoikov order
   flow imbalance on L1 events.
6. **Learned microprice drift** (optional, Epic D #2): Stoikov 2018
   G-function fitted offline from history.

**Config.**

```toml
[market_maker]
momentum_enabled = true
momentum_window = 200           # rolling trade-flow window
momentum_ofi_enabled = false    # Epic D #1 opt-in
momentum_learned_microprice_path = "models/lmp_btcusdt.toml"  # Epic D #2
# per-pair override:
# momentum_learned_microprice_pair_paths = { "BTCUSDT" = "...", ... }

hma_enabled = true
hma_window = 9
```

**Public API.**

```rust
impl MomentumSignals {
    pub fn new(window: usize) -> Self;
    pub fn with_hma(window: usize) -> Self;
    pub fn with_ofi() -> Self;
    pub fn with_learned_microprice(model: LearnedMicroprice) -> Self;
    pub fn on_l1_snapshot(bid_px, bid_qty, ask_px, ask_qty);
    pub fn on_mid(mid: Price);
    pub fn alpha(&self) -> Decimal;
    pub fn ofi_ewma(&self) -> Option<Decimal>;
}
```

**Source.** `crates/strategy/src/momentum.rs`.

### Features (library functions)

| Function | What | When |
|---|---|---|
| `book_imbalance(bids, asks, k)` | Top-k imbalance ∈ [−1, 1] | General-purpose alpha |
| `book_imbalance_weighted(bids, asks, k)` | Linear decay weighting | Better when inner levels dominate |
| `TradeFlow::update + value` | EWMA signed volume | Trade-flow alpha |
| `micro_price(bids, asks)` | Level-1 micro-price | Leads mid |
| `micro_price_weighted(bids, asks, depth)` | Multi-level microprice | Smoother under thin L1 |
| `market_impact(levels, side, target_qty, ref)` | VWAP walker, signed bps slippage | Pre-trade impact estimate |

All lookahead-safe by construction — functions take book snapshots
at time `t` and return features at time `t`.

**Source.** `crates/strategy/src/features.rs`.

### CKS OFI (Cont-Kukanov-Stoikov)

**Purpose.** Signed L1 order-flow-imbalance from successive top-of-
book snapshots.

**Formula.**

```
e_b[t] = bid event (quantity change):
  +Q_b'      if price moved up      (new bid arrival)
   Q_b' − Q_b if price stayed       (quantity change)
  −Q_b       if price moved down    (bid lifted)

e_a[t] = ask event (signed):
  −Q_a'      if price moved down
   Q_a' − Q_a if price stayed
   Q_a       if price moved up

OFI[t] = e_b[t] − e_a[t]                   # positive = upward
```

EWMA-smoothed in `MomentumSignals` with `α = 0.07` (half-life ~10
events).

**Public API.**

```rust
impl OfiTracker {
    pub fn new() -> Self;
    pub fn seed(bid_px, bid_qty, ask_px, ask_qty);
    pub fn update(bid_px, bid_qty, ask_px, ask_qty) -> Option<Decimal>;
}
```

First call returns `None`; subsequent calls diff against prior.

**Source.** `crates/strategy/src/cks_ofi.rs`.

### Learned Microprice (Stoikov 2018)

**Purpose.** Offline-fitted G-function that predicts `mid_{t+k} − mid_t`
from current imbalance and spread. Loaded at startup from TOML.

**Formula.**

```
G(I, S) = E[ mid_{t+k} − mid_t | I_t = I, S_t = S ]
learned_mid = mid_t + G(I_t, S_t)
```

Bucketing:
- Imbalance: equal-width bins on [−1, 1], default 20 buckets.
- Spread: quantile bins, default 5 buckets.
- Under-sampled cells (< `min_bucket_samples`, default 100) clamp to 0.

**Workflow.**

```rust
let mut lmp = LearnedMicroprice::empty(config);
for tick in history {
    lmp.accumulate(tick.imbalance, tick.spread, tick.delta_mid);
}
lmp.finalize();                     // AFTER training; panics if called again
let toml = lmp.to_toml();           // save fitted model
// ... at runtime:
let lmp = LearnedMicroprice::from_toml(&toml)?;
let predicted = lmp.predict(current_imb, current_spread);
```

**Config.**

```rust
pub struct LearnedMicropriceConfig {
    pub n_imbalance_buckets: usize,  // default 20
    pub n_spread_buckets: usize,     // default 5
    pub min_bucket_samples: usize,   // default 100
}
```

**Source.** `crates/strategy/src/learned_microprice.rs`.

### Cartea AS Spread (adverse selection widening)

**Purpose.** Additional spread widening based on informed-flow
probability, per Cartea-Jaimungal-Penalva eq. 4.20.

**Formula.**

```
widening = (1 − 2·ρ) · σ · √(T − t)     # symmetric
# or per-side:
widening_bid = (1 − 2·ρ_bid) · σ · √(T − t)
widening_ask = (1 − 2·ρ_ask) · σ · √(T − t)
```

`ρ ∈ [0, 1]`: 0 = all uninformed (no widening), 0.5 = neutral (no
effect), 1 = all informed (maximum widening).

Wired into Avellaneda, GLFT, Basis, CrossExchange (CrossExchange
clamps at zero to prevent sub-floor quotes).

**Source.** `crates/strategy/src/cartea_spread.rs`.

### Volatility (EWMA realised)

**Purpose.** Online σ estimate used to feed the `sigma` parameter of
Avellaneda / GLFT when `sigma` in config is treated as a starting
prior.

**Formula.**

```
r_t    = (price_t − price_{t−1}) / price_{t−1}
σ²_t   = λ · σ²_{t−1} + (1−λ) · r_t²
σ_ann  = sqrt(σ²_t) · sqrt(365.25 · 86400 / tick_interval_secs)
```

Typical `λ = 0.94` (RiskMetrics). Bootstrap from sample variance once
>= 20 observations accumulated.

**Public API.**

```rust
impl VolatilityEstimator {
    pub fn new(lambda: Decimal, tick_interval_secs: Decimal) -> Self;
    pub fn update(&mut self, price: Price);
    pub fn volatility(&self) -> Option<Decimal>;
}
```

**Source.** `crates/strategy/src/volatility.rs`.

---

## Parameter modulators

Strategies take fixed config. Modulators are the layer that varies
those parameters at runtime based on observed state.

### Regime detector + AutoTuner

**Purpose.** Detect market regime (Quiet / Trending / Volatile /
MeanReverting) and multiplicatively adjust `γ`, `kappa`, `sigma`,
`order_size`, `refresh_interval_ms` per regime.

**Regime detection.** From rolling returns window:

- **Quiet**: low σ, low Hurst (random walk).
- **Trending**: low σ, high Hurst > 0.6.
- **Volatile**: high σ.
- **MeanReverting**: low σ, low Hurst < 0.4.

Uses `features::hurst_exponent` over 50–200 returns.

**Parameter shifts.** Per-regime multipliers applied on top of base
config:

| Regime | γ mult | order_size mult | refresh_interval_ms |
|---|---|---|---|
| Quiet | 0.8 | 1.0 | base |
| Trending | 1.2 | 0.8 | base |
| Volatile | 1.5 | 0.7 | 300 |
| MeanReverting | 0.9 | 1.2 | base |

**Source.** `crates/strategy/src/autotune.rs`.

### AdaptiveTuner (Epic 30 E30.4)

**Purpose.** Online feedback layer on top of AutoTuner. Observes fill
rate, inventory volatility, adverse selection, spread capture;
multiplies γ within bounded range.

**Config.**

```rust
pub struct AdaptiveConfig {
    pub target_fills_per_min: Decimal,   // default 5
    pub max_adj_per_min: Decimal,        // default 0.05 (5%)
    pub gamma_factor_min: Decimal,       // default 0.25 (4× tighter)
    pub gamma_factor_max: Decimal,       // default 4.0  (4× wider)
    pub inv_vol_threshold: Decimal,      // default 0.005
    pub adverse_bps_threshold: Decimal,  // default 5
    pub bucket_secs: u64,                // default 60
    pub window_buckets: usize,           // default 60 (1 hr)
}
```

**Adjustment reasons** (surfaced to dashboard):

- `NoOp` — inside tolerance or disabled
- `TightenForFills` — fill rate below target
- `WidenForInventory` — inventory vol EWMA > threshold
- `WidenForAdverse` — adverse selection > threshold
- `WidenForNegativeEdge` — spread capture < fees
- `RateLimited` — hit `max_adj_per_min` clamp
- `Clamped` — hit min/max bounds

**Opt-in.** `adaptive_enabled = true` in config (default `false`).

**Source.** `crates/strategy/src/adaptive.rs`.

### AdvancedInventoryManager

**Purpose.** Quadratic inventory skew, dynamic size scaling, urgency
unwinding when near limits.

**Formulas.**

```
q_frac = inventory / max_inventory
quadratic_skew = sign(q_frac) · q_frac²

# Size scaling per side:
if side increases |inventory|: scale = 1 − |q_frac|          # reduce
if side decreases |inventory|: scale = 1 + 0.5 · |q_frac|    # boost

# Urgency (after urgency_threshold breached for urgency_delay_secs):
urgency_level = min((elapsed_secs − delay_secs) / 60, 1)
urgency_adjustment = mid · 0.0005 · urgency_level   # up to 5 bps
# if long  → lower asks (fire sales)
# if short → raise bids (fire buys)
```

**Config.**

```rust
AdvancedInventoryManager::new(max_inventory)
    .with_urgency_threshold(0.7)     // 70% of max
    .with_urgency_delay_secs(60)
```

**Source.** `crates/strategy/src/inventory_skew.rs`.

### Market Resilience Calculator

**Purpose.** Event-driven shock detector. Score ∈ [0, 1] where 1 =
fully recovered, 0 = fragile (large trade just hit).

**Formula (weights).**

```
score = 0.3 · trade_shock_component
      + 0.1 · spread_recovery_component
      + 0.5 · depth_recovery_component
      + 0.1 · spread_magnitude_component
```

Shocks detected when:
- Trade size > trade_shock_sigma · rolling_std (default 2.0σ).
- Depth drops > depth_z_threshold · running_MAD (default 3.0σ).

Recovery tracked to `recovery_target` (default 90% of baseline).
Score decays linearly over `decay_window_ns` (default 5 s).

**Config.**

```toml
[market_maker]
market_resilience_enabled = true

# struct-level defaults (not TOML-exposed):
# shock_timeout_ns = 800_000_000     (800 ms)
# trade_shock_sigma = 2.0
# depth_z_threshold = 3.0
# warmup_samples = 200
# recovery_target = 0.9
# decay_window_ns = 5_000_000_000    (5 s)
```

**Public API.**

```rust
impl MarketResilienceCalculator {
    pub fn score(&self) -> Decimal;
    pub fn on_trade(&mut self, side: Side, qty: Qty, price: Price);
    pub fn on_book_update(&mut self, bids, asks, now_ns: i64);
}
```

**Source.** `crates/strategy/src/market_resilience.rs`.

### PairClass templates + classifier (Epic 30–31)

**Purpose.** At startup, classify each symbol into a `PairClass` based
on 24h volume + heuristics, then optionally apply the per-class
template to override config defaults.

**PairClass variants.**

- `MajorSpot`
- `AltSpot`
- `MemeSpot`
- `MajorPerp`
- `AltPerp`
- `StableStable`

**Template example** (`config/pair_class_templates.toml`):

```toml
[MajorSpot]
gamma = 0.08
kappa = 2.0
min_spread_bps = 3
order_size = 0.01
num_levels = 5

[MemeSpot]
gamma = 0.5
min_spread_bps = 20
order_size = 0.001
num_levels = 3
```

**Opt-in.** `apply_pair_class_template = true` in config.

### Hyperopt recalibrate flow (Epic 33)

**Purpose.** Run random-search hyperopt on recorded market data,
surface results as `PendingCalibration`, operator applies or discards.

**Endpoints.**

```
POST /api/admin/optimize/trigger    # kick off a run
GET  /api/admin/optimize/status     # active run status
GET  /api/admin/optimize/results    # completed runs
GET  /api/admin/optimize/pending    # awaiting sign-off
POST /api/admin/optimize/apply      # apply to live config
POST /api/admin/optimize/discard    # drop
```

**Loss functions available.** Sharpe, Sortino, Calmar, MaxDD.

**Source.** `crates/hyperopt/`, `crates/dashboard/src/state.rs`
(PendingCalibration state).

---

## Engine pipeline

How all of the above fits together per refresh tick.

```
┌───────────────────────────────────────────────────────────────────┐
│                    MarketMakerEngine::refresh_quotes              │
└───────────────────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────┐   (fresh? circuit-breaker ok?)
  │ BookKeeper     │   ├── NO → skip tick, log
  │ — LocalBook    │   └── YES → continue
  └────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Signals (on every mid / L1 snapshot)               │
  │  MomentumSignals.alpha()                           │
  │    ├── book_imbalance                              │
  │    ├── trade_flow EWMA                             │
  │    ├── micro_price / learned_micro_price           │
  │    ├── HMA slope                                   │
  │    ├── CKS OFI EWMA                                │
  │  MarketResilienceCalculator.score()                │
  │  VolatilityEstimator.volatility()                  │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Reservation price                                  │
  │   r = mid + alpha · (T−t) − q · γ · σ² · (T−t)     │
  │       ± cartea_spread_widening                     │
  │       ± borrow_cost_shim                           │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Parameter modulators                               │
  │   AutoTuner(regime) → γ, kappa, size, refresh_ms   │
  │   AdaptiveTuner     → γ multiplier                 │
  │   MarketResilience  → size + spread multiplier     │
  │   PairClass template (once at startup)             │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Strategy::compute_quotes()                         │
  │   AvellanedaStoikov / GLFT / Grid / Basis / …      │
  │   → Vec<QuotePair>                                 │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Inventory skew + urgency overlay                   │
  │   AdvancedInventoryManager.dynamic_size()          │
  │   AdvancedInventoryManager.apply_urgency()         │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Risk gates                                         │
  │   KillSwitch level → widen / stop / cancel         │
  │   CircuitBreaker (stale book, wide spread)         │
  │   VarGuard, LeadLagGuard, NewsRetreat              │
  │   StoplossGuard / CooldownPeriod                   │
  │   BalanceCache reserve                             │
  │   OrderToTradeRatio throttle                       │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Order manager (diffing)                            │
  │   cancel stale / place new / amend if eligible     │
  │   client_order_id tracking                         │
  └────────────────────────────────────────────────────┘
         │
         v
  ┌────────────────────────────────────────────────────┐
  │ Exchange connector                                 │
  │   Binance / Bybit / HyperLiquid / Custom           │
  │   Rate-limiter token bucket                        │
  │   Retry-after feedback from venue headers          │
  └────────────────────────────────────────────────────┘
```

On a **fill**:
- `Portfolio::on_fill` updates position + PnL (subject to the shared-
  mutex bottleneck — flagged in the performance audit).
- `FillReplay` appends to audit.
- `InventoryManager.on_fill` updates inventory.
- `AdvancedInventoryManager.tick` evaluates urgency.
- `Strategy::on_fill` (if implemented) feeds strategy-specific state
  (e.g. `GlftStrategy::on_fill_depth` for online calibration).
- Cross-product: `FundingArbDriver.on_hedge_fill`,
  `StatArbDriver.on_leg_fill`, `PairedUnwindExecutor.on_fill` as
  appropriate.

On a **kill-switch escalation**:
- L1 Widen: `min_spread_bps` multiplier × 2, `order_size` × 0.5.
- L2 StopNew: `refresh_quotes` no-op, existing orders honoured.
- L3 CancelAll: cancel all, inventory intact.
- L4 Flatten: `TwapExecutor` spins up to market-unwind inventory;
  paired positions go through `PairedUnwindExecutor`.
- L5 Disconnect: close WS, flush audit, exit.

---

## Config recipes per PairClass

Drop-in config snippets. Paste under `[market_maker]` and tune.

### MajorSpot (BTC/USDT, ETH/USDT on spot)

```toml
strategy = "avellaneda_stoikov"

gamma = 0.08
kappa = 2.0
sigma = 0.015              # overridden by live VolatilityEstimator
time_horizon_secs = 300
num_levels = 5
order_size = 0.01          # BTC units — 0.01 BTC ≈ $600 per level
min_spread_bps = 3
max_distance_bps = 50
refresh_interval_ms = 500

# Alpha
momentum_enabled = true
momentum_window = 200
momentum_ofi_enabled = true
hma_enabled = true
hma_window = 9

# Modulators
market_resilience_enabled = true
adaptive_enabled = true
apply_pair_class_template = true
```

### MajorPerp (BTCUSDT perp, ETHUSDT perp)

```toml
strategy = "glft"   # GLFT calibrates intensity from live fills

gamma = 0.08
kappa = 2.5
sigma = 0.02
time_horizon_secs = 300
num_levels = 5
order_size = 0.01
min_spread_bps = 3
max_distance_bps = 50
refresh_interval_ms = 500

momentum_enabled = true
momentum_ofi_enabled = true
hma_enabled = true
```

### AltSpot / AltPerp

```toml
strategy = "avellaneda_stoikov"   # or "glft" on perp

gamma = 0.2                # wider spread for less-liquid
kappa = 1.0
sigma = 0.05
time_horizon_secs = 300
num_levels = 3
order_size = 1             # adjust per asset's typical notional
min_spread_bps = 10
max_distance_bps = 200
refresh_interval_ms = 750

momentum_enabled = true
market_resilience_enabled = true
adaptive_enabled = true
```

### MemeSpot

```toml
strategy = "grid"

num_levels = 3
order_size = 0.001
min_spread_bps = 30        # wide — meme liquidity is thin and toxic
refresh_interval_ms = 1000

# No alpha — it's noise on memes
momentum_enabled = false

# Max safety
market_resilience_enabled = true

[kill_switch]
daily_loss_limit = 100     # cheaper daily cap on memes
```

### StableStable (USDC/USDT, BUSD/USDT)

```toml
strategy = "grid"

num_levels = 10            # many tight levels — this is the alpha
order_size = 100           # USDC is big — think notional, not base
min_spread_bps = 1         # as tight as the tick allows
refresh_interval_ms = 200

momentum_enabled = false
```

### CrossExchange (Binance → Bybit hedge, or similar)

```toml
strategy = "cross_exchange"
cross_exchange_min_profit_bps = 5

[hedge]
exchange_type = "bybit"    # the take leg
symbol = "BTCUSDT"
```

### FundingArb

```toml
strategy = "funding_arb"

[hedge]
exchange_type = "binance_futures"
symbol = "BTCUSDT"

[funding_arb]
tick_interval_secs = 60
# plus min_funding_rate_bps, max_position — see config ref
```

---

## Operational gotchas

### Strategy is not quoting — what to check

1. **BookKeeper reports stale** — `rx_ms` > `stale_book_timeout_secs`.
   Fresh WS feed? Check `Header` stale badge; if amber/red, problem
   is upstream of strategy.
2. **CircuitBreaker tripped** — spread too wide, drawdown limit hit,
   kill switch at L2+. Check `/api/status` and audit.
3. **Basis > max_basis_bps** — intentional no-quote on Basis /
   CrossExchange. Check hedge venue mid.
4. **Cross-venue staleness gate** — `hedge_book_age_ms` exceeds
   `cross_venue_basis_max_staleness_ms`. Check hedge WS health.
5. **Preflight fail in live** — clock-skew > 2 s, rate limit low,
   balance empty. Run `GET /api/v1/system/preflight`.
6. **Regime = Volatile + AdaptiveTuner widening** — quotes far from
   mid. Check `AdaptivePanel` for `gamma_factor` and `last_reason`.
7. **Inventory urgency** — near `max_inventory` for > 60 s. Quotes
   become asymmetric; one side widens. Visible in dashboard.

### Reading the AdaptivePanel

| Field | Meaning |
|---|---|
| `gamma_factor` | Current multiplier (0.25–4.0) |
| `gamma_target` | Target controller walks toward |
| `last_reason` | Most recent adjustment cause |
| `fill_rate_per_min` | Rolling fill rate (drives TightenForFills) |
| `inv_vol_ewma` | Inventory volatility EWMA |
| `adverse_bps` | Adverse-selection bps (drives WidenForAdverse) |

If `last_reason = RateLimited` for long periods — the controller
wants a bigger move than `max_adj_per_min` allows. Either raise the
cap or investigate a regime shift.

### When to run hyperopt

- **After a regime transition** — if the market has been in a new
  regime for 1+ week, prior calibration may be stale.
- **After adding a new symbol** — classify + template gets you close;
  hyperopt refines.
- **Never during live without review** — `apply` requires operator
  sign-off. The `PendingCalibration` state exists precisely for this.

Do not trust hyperopt results that show a Sharpe > 3 on < 1 week of
data. Random-search on short windows finds edges that don't
generalise.

### Debugging "why is my reservation price off-mid"

Compute the contributions in order:

```
base          = mid
+ alpha · (T−t)                          # momentum shift
− q · γ · σ² · (T−t)                     # inventory tilt
± basis_shift · (hedge_mid − mid)        # Basis / CrossExchange
± borrow_cost_shim                       # P1.3 short-penalty
± cartea_widening (in spread, not mid)   # AS widening
= reservation
```

Log each component at DEBUG level; the engine includes them in the
per-symbol state snapshot pushed to the dashboard.

---

## See also

- [Writing Strategies](writing-strategies.md) — implement a custom
  strategy, tests, what context you get.
- [Adaptive Calibration](adaptive-calibration.md) — deep dive on
  `AdaptiveTuner` and pair-class templates.
- [Configuration Reference](configuration-reference.md) — every TOML
  field + env var.
- [Architecture](architecture.md) — crate graph, engine event loop,
  persistence layout.
- [Operations](operations.md) — modes, troubleshooting, daily
  checklist, auth surface.
- [Competitor Gap Analysis](../research/competitor-gap-analysis-apr17.md)
  — what peers have we don't (STP, OCO, drop-copy, queue model, etc.).
