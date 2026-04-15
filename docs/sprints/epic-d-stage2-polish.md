# Epic D — Signal Wave 2 / Stage-2 Polish

Follow-up track wrapping three deferrals carried over from the Epic D
stage-1 closure (see `docs/sprints/epic-d-signal-wave-2.md` §Closure).
Stage-2 runs in parallel with Stat-arb/SOR, Listing Sniper, and
Defensive Layer tracks — strict file-ownership rules apply.

## Scope

| # | Sub-component | Files | Status |
|---|---|---|---|
| 2A | LearnedMicroprice TOML round-trip + offline CLI fit binary | `learned_microprice.rs`, `bin/mm_learned_microprice_fit.rs`, `Cargo.toml` | planned |
| 2B | GLFT integration of Cartea AS spread | `glft.rs` | planned |
| 2C | Per-side asymmetric `ρ_b` / `ρ_a` closed-form spread | `cartea_spread.rs` | planned (pure-function only — see §Design) |

## Audit — what's currently in tree (Apr 2026)

### `learned_microprice.rs`

- Stoikov 2018 G-function, two-pass `with_spread_edges` /
  `accumulate_with_edges` path already lands.
- `finalize` clamps under-sampled buckets to zero and discards
  `spread_samples` memory. Idempotent.
- **No serde on the struct.** No TOML/JSON path — this is the gap
  stage-2 closes.
- No CLI binary. `docs/sprints/epic-d-signal-wave-2.md` lists the
  CLI + TOML persistence as deferred.
- 14 unit tests already in `tests` module.

### `cartea_spread.rs`

- `quoted_half_spread(γ, κ, σ, T-t, ρ) → Decimal` is symmetric —
  only one `ρ` input for both bid and ask sides.
- Output clamps at zero.
- `decimal_ln` helper lives in this module (not promoted to a shared
  helper — stage-2 leaves that structural question alone).
- `as_prob_from_bps` is the wave-1 mapping used by Avellaneda.
- 14 unit tests already in `tests` module.

### `glft.rs`

- Wave-1 C1/C2 closed-form spread already computes `half_spread` and
  `skew`. No Cartea AS additive component yet.
- `StrategyContext::as_prob` field exists (set by the engine) — GLFT
  just ignores it in wave 1.
- `test_ctx` helper in `tests` already builds the full context with
  `as_prob: None`.
- 3 tests.

### Context construction sites

Confirmed via grep of `StrategyContext {` inside `crates/strategy/src`:
`avellaneda.rs`, `basis.rs`, `cross_exchange.rs`, `glft.rs` and
`benches/strategy_bench.rs`. `stat_arb/driver.rs` does NOT construct
`StrategyContext` (it dispatches via pair-side hedging without the
strategy trait). `crates/engine/src/market_maker.rs` is the canonical
engine-side construction site and is owned by Track 1.

## Design — why per-side `ρ` ships as a pure function only

The original stage-2 plan wanted to thread `as_prob_bid` /
`as_prob_ask` through `StrategyContext`. Adding struct fields to
`StrategyContext` requires updating every construction site, including
`crates/engine/src/market_maker.rs`, which Track 1 owns this stage.
Any touch there would merge-conflict.

**Decision:** ship the per-side closed-form as a pure function
`quoted_half_spread_per_side(γ, κ, σ, T-t, ρ_b, ρ_a) → (δ_b, δ_a)`
inside `cartea_spread.rs` only. No `StrategyContext` changes. Any
future caller (stage-3 wiring, offline calibration tooling, research
notebook) can import the pure function directly. The symmetric
wave-1 `quoted_half_spread` path is unchanged, so Avellaneda / GLFT
consumers keep the single-`ρ` contract.

Wiring per-side `ρ` through `StrategyContext` is a **stage-3**
follow-up that will coordinate with Track 1's changes on
`market_maker.rs`.

## Per-sub-component DoD

### 2A — LearnedMicroprice TOML + CLI

- `#[derive(Serialize, Deserialize)]` on `LearnedMicropriceConfig`
  and `LearnedMicroprice` (with `#[serde(skip)]` on the transient
  `spread_samples` field).
- `LearnedMicroprice::from_toml(path) -> anyhow::Result<Self>`
- `LearnedMicroprice::to_toml(&self, path) -> anyhow::Result<()>`
- ≥4 new unit tests: empty model round-trip, finalized fit
  round-trip, spread-edges round-trip, prediction parity
  post-roundtrip.
- New binary `crates/strategy/src/bin/mm_learned_microprice_fit.rs`:
  - stdlib-only CLI arg parsing (`std::env::args`) — no new deps.
  - JSONL input schema: one minimal
    `{ ts, bid, bid_qty, ask, ask_qty, mid }` record per line
    (CLI-local struct, no coupling to `mm-backtester`).
  - Two-pass fit: first pass collects spread + delta-mid samples to
    compute quantile edges externally; second pass calls
    `with_spread_edges` + `accumulate_with_edges`.
  - `to_toml` at the end.
  - One in-file smoke test building a synthetic in-memory tape.
- `toml = { workspace = true }` added to `crates/strategy/Cargo.toml`
  (workspace dep already exists — 0.8).

### 2B — GLFT Cartea AS spread

- Inside `GlftStrategy::compute_quotes`, after the wave-1 `half_spread`
  is clamped at `min_spread / 2`, additively widen by
  `(1 − 2ρ) · σ · √(T − t)` when `ctx.as_prob.is_some()`. Re-clamp at
  `min_spread / 2` floor.
- When `ctx.as_prob == None` or `ctx.as_prob == Some(0.5)`, produce
  byte-identical output to the wave-1 path. Verified by two
  identity tests.
- ≥4 new tests: `None` identity, `Some(0.5)` identity, `Some(0)`
  widening, `Some(1)` narrowing (or clamp firing).

### 2C — Per-side `cartea_spread::quoted_half_spread_per_side`

- Signature:
  `fn quoted_half_spread_per_side(γ, κ, σ, T-t, ρ_b, ρ_a) -> (Decimal, Decimal)`
- Formula: `base = (1/γ) · ln(1 + γ/κ)`, then
  `δ_b = (base + (1 − 2ρ_b)·σ·√(T-t)).max(0)`,
  `δ_a = (base + (1 − 2ρ_a)·σ·√(T-t)).max(0)`
- ≥6 tests: symmetric collapse to `quoted_half_spread`, bid-widen,
  ask-widen, mixed, both-clamped-zero, monotonicity in each side.

## Out of scope — tracked as stage-3 deferrals

- Wiring `as_prob_bid` / `as_prob_ask` through `StrategyContext`
  (Track 1 coordination required on `market_maker.rs`).
- JSON persistence for `LearnedMicroprice` (TOML only for stage-2 —
  JSON adds no calibration value).
- Auto-fit driver inside the engine that reloads the TOML on a timer.
- Promoting `decimal_ln` to a shared crate helper.

## Validation

- `cargo test -p mm-strategy`
- `cargo clippy -p mm-strategy --all-targets -- -D warnings`
- `cargo fmt -p mm-strategy`
- `cargo build -p mm-strategy --bin mm-learned-microprice-fit`

No workspace-wide build/test from this track — parallel agents would
race.
