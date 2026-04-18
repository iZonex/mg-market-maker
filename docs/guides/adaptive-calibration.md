# Adaptive Calibration (Epic 30)

Operator guide for the online `AdaptiveTuner` + pair-class
template machinery that ship in Epic 30.

## What it is

A slow-moving, closed-loop γ controller that sits **on top of**
the existing `AutoTuner` regime multiplier. It watches rolling
fill rate, realised spread capture, and adverse selection on a
one-minute cadence and nudges γ up or down by ≤ 5 % per bucket.

It is **off by default**. Existing deployments see no change until
an operator sets `market_maker.adaptive_enabled = true`.

## Multiplier stack

When adaptive is enabled, γ in every refresh tick is:

```
γ_effective = γ_base (from config / pair-class template)
            × regime_mult   (AutoTuner — 0.6 … 3.0, FAST)
            × adaptive_mult (AdaptiveTuner — 0.25 … 4.0, SLOW)
            × portfolio_risk_mult
            × kill_switch_spread
```

Each multiplier is bounded and logged separately. See
`crates/engine/src/market_maker.rs` around `refresh_quotes` for
the exact compose order.

## When does it nudge?

Checked on every one-minute bucket rollover:

| Rule | Trigger | Action |
|------|---------|--------|
| WidenForAdverse | avg adverse bps > threshold (default 5 bps) | γ × 1.10 |
| WidenForNegativeEdge | realised spread capture < fees | γ × 1.05 |
| WidenForInventory | inventory-vol EWMA > threshold | γ × 1.03 |
| TightenForFills | fill rate < target/2 AND edge positive | γ × 0.95 |

The `AdjustmentReason` is published to the dashboard so operators
can see which rule fired.

## Rate limit & bounds

- **Per-minute move:** max ±5 % from the previous bucket's factor.
- **Absolute bounds:** `[0.25, 4.0]`. Cannot widen beyond 4× base
  or tighten below 0.25× base regardless of rules.
- **Manual override:** operator can set a floor/ceiling via
  `AdaptiveTuner::set_manual_bounds(...)` that further tightens
  the absolute bounds.
- If a proposed step would blow past these, it's clamped and the
  reason is recorded as `RateLimited` or `Clamped`.

## Pair-class templates

Each symbol is tagged with a `PairClass` at startup via
`mm_common::classify_symbol(spec, daily_volume_usd, is_perp)`:

| Class | Examples | Default γ | Default min_spread_bps |
|-------|----------|-----------|------------------------|
| MajorSpot | BTCUSDT, ETHUSDT | 0.05 | 2 |
| MajorPerp | BTCUSDT-PERP, ETHUSDT-PERP | 0.08 | 3 |
| AltSpot | AVAXUSDT, LINKUSDT | 0.15 | 8 |
| AltPerp | (alt perps) | 0.20 | 10 |
| MemeSpot | DOGEUSDT, SHIBUSDT, unknown thin pairs | 0.40 | 30 |
| StableStable | USDCUSDT, FDUSDUSDT | 0.01 | 0.5 |

Templates live under `config/pair-classes/*.toml`. Loader:
`crates/server/src/pair_template.rs`. Precedence when a template
is applied:

```
AppConfig::default()  →  class template  →  user venue config  →  env vars
```

Values set later win. Template files only override fields they
explicitly mention; absent fields leave the caller's config
intact.

## Enabling

**Opt-in per deployment.** Add to your venue config:

```toml
[market_maker]
adaptive_enabled = true
```

That's all. The engine constructs an `AdaptiveTuner` with default
`AdaptiveConfig` and routes fills / inventory / adverse readings
through its `on_*` methods. Disabled tuner returns γ factor `1.0`,
so runs stay byte-identical to pre-Epic-30 behaviour unless the
flag is set.

## Dashboard

`SymbolState::adaptive_state` publishes:
- `pair_class` — string tag
- `enabled` — true/false
- `gamma_factor` — current multiplier (1.0 = neutral)
- `last_reason` — last adjustment tag (`tighten_for_fills`, `widen_for_adverse`, `clamped`, …)

Consumed by the future UI panel. Backend data already flows.

## When NOT to enable

- **Brand-new symbols:** the tuner needs ≥ 10–20 minutes of flow
  before its rolling stats stabilise. Quote conservatively with
  manual γ for the first half hour, then flip on.
- **Very tight instruments** (StableStable at < 1 bps): single-bps
  moves round to zero under Decimal quantisation — the tuner's
  feedback signal is dominated by measurement noise. Stick with
  static γ from the template.
- **Kill-switch escalated:** the engine stops quoting at level ≥ 3
  anyway, so the tuner has no data to learn from. Re-enable
  after reset.

## Disabling in a hurry

Set `market_maker.adaptive_enabled = false` in the config and hit
the admin `/api/admin/config/{symbol}` endpoint (the standard
`ConfigOverride` path). No restart needed — the override applies
on the next tick. The tuner's state (current γ factor) is wiped
on disable.

## Related files

- `crates/strategy/src/adaptive.rs` — the tuner itself + 10 unit tests
- `crates/common/src/pair_class.rs` — classifier (13 tests, 2 proptests)
- `config/pair-classes/*.toml` — six class templates (6 tests)
- `crates/server/src/pair_template.rs` — merge helper
- `crates/dashboard/src/state.rs` — `AdaptiveStateSnapshot`
- `crates/engine/src/market_maker.rs` — wiring (γ compose site + fill hooks)
- `docs/research/adaptive-calibration.md` — design doc

## Not yet wired (follow-ups)

1. `PairClass` classifier → `SymbolState.adaptive_state.pair_class`
   currently reports `"unclassified"`. Hooking `classify_symbol`
   at engine startup (after `get_product_spec`) is a small next
   patch.
2. Hyperopt "re-calibrate now" admin endpoint (planned E30.5, deferred).
3. Frontend Svelte panel rendering the γ-stack breakdown. The
   backend data is published; the UI component is not yet shipped.
