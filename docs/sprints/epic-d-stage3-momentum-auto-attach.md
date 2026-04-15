# Epic D stage-3 — engine-side OFI + learned-MP auto-attach

> Closes the second Epic D stage-2 deferral the closure note
> tracked: "OFI and learned-MP consumers are wired into
> `MomentumSignals` as opt-in builder knobs but the engine's
> default `MomentumSignals::new(...).with_hma(...)`
> construction does not yet attach them — operators enable
> per config in stage-2." Stage-3 ships the config knobs and
> the engine wiring.

## Why this stage-3

Epic D stage-1 shipped the wave-2 signals (CKS OFI +
Stoikov learned microprice). Stage-2 added them as opt-in
builders on `MomentumSignals` but the engine constructor
never called those builders — operators had to manually
wire them in their own engine subclasses. Stage-3 closes
that gap with two new `MarketMakerConfig` knobs that the
engine reads at construction time.

## What ships

### #1 New config knobs (`mm-common::config`)

`MarketMakerConfig` gains two new optional fields:

```rust
/// Epic D stage-3 — enable the Cont-Kukanov-Stoikov L1
/// Order Flow Imbalance signal as a fifth alpha component.
#[serde(default)]
pub momentum_ofi_enabled: bool,

/// Epic D stage-3 — optional path to a finalized
/// LearnedMicroprice TOML file produced by the
/// mm-learned-microprice-fit offline CLI binary.
#[serde(default)]
pub momentum_learned_microprice_path: Option<String>,
```

Both default to off (`false` / `None`) so operators who
tuned the wave-1 alpha weights see byte-identical
behaviour.

### #2 Engine `MomentumSignals` construction

`MarketMakerEngine::new` extends the existing
`with_hma`-conditional construction with two new
conditional builder calls:

- When `momentum_ofi_enabled = true`, calls
  `MomentumSignals::with_ofi()` to attach a fresh
  `OfiTracker`.
- When `momentum_learned_microprice_path = Some(path)`,
  calls `LearnedMicroprice::from_toml(path)` and on success
  attaches the model via `with_learned_microprice(model)`.
  **On load failure the engine logs a warning and
  continues without the signal — never panics.**

### #3 Engine `handle_ws_event` L1 feed

The book-event handler in `handle_ws_event` already calls
`momentum.on_mid(mid)` for HMA. Stage-3 adds a new
`momentum.on_l1_snapshot(bid_px, bid_qty, ask_px, ask_qty)`
call right after, reading the freshly-applied snapshot
directly from `book_keeper.book`. The call is a no-op when
`with_ofi()` was not called at construction time, so it's
free for operators who haven't enabled OFI.

### #4 Tests

- `momentum_ofi_disabled_keeps_ewma_unset` — pin the
  default-off path, verify EWMA stays `None` on a stream of
  book events
- `momentum_ofi_enabled_populates_ewma_from_book_events` —
  pin the default-on path, verify EWMA goes positive after
  growing-bid-depth snapshots
- `momentum_learned_microprice_missing_path_does_not_panic`
  — pin the load-failure recovery path

## Definition of done

- ✅ Two new config knobs with `serde(default)` for backward
  compat
- ✅ Engine constructor reads them and conditionally
  attaches OFI / learned MP
- ✅ `handle_ws_event` feeds L1 snapshot every book event
- ✅ Load-failure path logs warning and continues without
  panic
- ✅ 3 new engine integration tests
- ✅ All 7 test fixtures updated with new field defaults
  (engine integration tests, strategy bench, avellaneda,
  glft, basis, cross_exchange, simulator)
- ✅ `cargo test --workspace` green (1011 → 1014)
- ✅ `cargo clippy --workspace --all-targets -- -D warnings`
  clean
- ✅ `cargo fmt --all --check` clean
- ✅ Single epic-stage-3 commit

## Open questions resolved

1. **OFI feed cadence — every book event vs sampled?**
   Every event. The OFI signal is most predictive at the
   per-event horizon (Cont-Kukanov-Stoikov 2014 §3.2). The
   `on_l1_snapshot` call is constant-time so there's no
   throughput risk.

2. **Learned MP load failure handling — error vs warn?**
   Warn + continue. The learned MP is a soft alpha
   enhancement, not a correctness requirement. An invalid
   TOML file should not block the engine from starting —
   operators see the warning in logs and re-fit / re-deploy.

3. **`momentum_ofi_enabled` default — true or false?**
   `false`. Operators who tuned wave-1 alpha weights for
   the existing 5-component split (book / flow / micro /
   HMA / scaling) might see different alpha output if OFI
   is auto-attached. Backward compat wins; flip in config
   per pair after A/B comparison.

4. **`momentum_learned_microprice_path` is a relative or
   absolute path?** Relative paths are resolved against the
   working directory `mm-server` is launched from — same
   convention as `MM_CONFIG`. Operators typically use
   absolute paths in production deployments.

## Stage-3+ follow-ups (not in this push)

- Per-pair learned MP models (currently the engine loads
  one file system-wide; multi-symbol deployments need
  per-pair model files keyed on symbol)
- Online learned MP fit (currently strictly offline)
- OFI weight tuning per pair (currently the wave-1
  `MomentumSignals::alpha` rebalances weights uniformly
  when an optional signal is attached)
- Dashboard / Prometheus exposure of `momentum.ofi_ewma()`
  and `momentum.learned_microprice_drift()` for operator
  monitoring
