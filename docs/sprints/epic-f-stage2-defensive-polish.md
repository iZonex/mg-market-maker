# Epic F — Stage-2 Defensive-layer polish

**Status:** in progress (Track 4 of four parallel stage-2 tracks)
**Owner:** defensive layer
**Depends on:** Epic F stage-1 (closed) — `docs/sprints/epic-f-defensive-layer.md`

## Scope

Stage-1 shipped both defensive predictive controls behind corners the
orchestrator explicitly flagged for stage-2 polish:

1. `NewsRetreatStateMachine` used **case-insensitive substring keyword
   matching** because the workspace had no `regex` dependency. v1 is
   operationally correct for the canonical headline set (SEC / hack /
   FOMC / CPI / exploit) but cannot express word boundaries, wildcards,
   or alternation.
2. `LeadLagGuard` supported a **single leader only**. Operators who
   want to watch Binance futures AND Bybit perpetuals AND OKX perps
   at the same time had to pick one — no aggregation. Multi-leader
   weighted aggregation was explicitly deferred.

This track resolves both follow-ups without touching the engine-side
wiring or any other Epic F sub-components.

## Audit findings (before code)

### `crates/risk/src/news_retreat.rs`
- 508 LoC, 14 tests all green
- `NewsRetreatStateMachine::new(NewsRetreatConfig) -> Self` pre-lowercases
  the three keyword lists once and stores them in private `*_lc: Vec<String>`
  caches
- `on_headline` lowercases `text` once, then calls `classify` which runs
  `critical → high → low` priority ladder with `String::contains`
- Nothing outside the risk crate constructs `NewsRetreatStateMachine`
  except the engine's `crates/engine/src/market_maker.rs` test suite
  (4 call sites, `fixture_news_config()`). Engine-side production wiring
  accepts a pre-constructed machine via `.with_news_retreat(sm)` so
  operators call `NewsRetreatStateMachine::new(cfg).unwrap()` themselves
  once `new` becomes fallible — the production wiring needs no changes
- Test-side engine callers (Track 1's territory) WILL need `.unwrap()`
  added; that's Track 1's merge responsibility, not this track's.

### `crates/risk/src/lead_lag_guard.rs`
- 474 LoC, 14 tests all green
- Single-leader `LeadLagGuard`: EWMA mean/variance of per-update returns,
  piecewise-linear ramp on `|z|`, config has `half_life_events`, `z_min`,
  `z_max`, `max_mult`
- Public API we must leave byte-identical: `new`, `on_leader_mid`,
  `current_multiplier`, `current_z_abs`, `is_active`, `reset`, `obs_count`
- Stage-2 adds a NEW sibling struct `MultiLeaderLeadLagGuard` that
  composes one `LeadLagGuard` per leader-id, with per-leader weights
- Stage-1 already exports the single-leader guard from `risk::lib`;
  the new struct sits in the same module, same `pub use`

## Open questions (pre-resolved)

| Question | Resolution |
|---|---|
| Regex case-sensitivity | Bake `(?i)` inline flag into every compiled regex so operators configure raw patterns and never think about case. Matches v1 substring behaviour. |
| Breaking change to `new` | `NewsRetreatStateMachine::new` becomes fallible — returns `anyhow::Result<Self>`. Additive breaking change. The only real production caller (engine `with_news_retreat`) is a passthrough builder method that takes a pre-constructed machine. Operator code adds `.unwrap()` / `?`. Engine-side test call sites in Track 1's files will need `.unwrap()` added by Track 1 at merge time. |
| Multi-leader aggregation rule | **Weight-scaled max**, not average. Formula: `M_agg = max over L of (w_L * (M_L - 1) + 1)`. Rationale: defensive controls take the LOUDEST leader — averaging would let N quiet leaders dilute one shocked leader. The `(M - 1)` shift ensures weight scales the *additional* widening, so `weight 0.5` halves the widening headroom (not the multiplier outright). Multiplier floored at 1.0. |
| Multi-leader API shape | `register_leader(id, weight)` / `unregister_leader(id)` — operators can add/remove leaders at runtime without rebuilding the guard. Weight clamped to `>= 0` (0 mutes a leader without removing it; no upper bound). |
| Module layout | `MultiLeaderLeadLagGuard` lives in `lead_lag_guard.rs` alongside the single-leader struct. Existing single-leader code stays byte-identical. |

## Definition of Done

| Item | Target |
|---|---|
| `news_retreat.rs` uses `regex::Regex` priority lists | done |
| `NewsRetreatStateMachine::new` returns `anyhow::Result<Self>` | done |
| `(?i)` baked in for case-insensitivity | done |
| ≥4 new regex tests (word boundary, alternation, wildcard, malformed) | done |
| Total ≥18 tests in `news_retreat::tests` | done |
| `MultiLeaderLeadLagGuard` added to `lead_lag_guard.rs` | done |
| Single-leader `LeadLagGuard` byte-identical | done |
| ≥10 new multi-leader tests | done |
| `crates/risk/Cargo.toml` gains `regex = { workspace = true }` | done |
| `cargo test -p mm-risk` green | done |
| `cargo clippy -p mm-risk --all-targets -- -D warnings` clean | done |
| `cargo fmt -p mm-risk` clean | done |

## Non-goals

- No new audit event types (stage-1's `news_retreat.v1` / `lead_lag_guard.v1`
  stay as-is)
- No changes to engine wiring / builder methods — Track 1 / the
  orchestrator handle the `.unwrap()` add for engine-side test fixtures
  as a trivial merge
- No changes to other risk modules
- No ROADMAP / CHANGELOG / CLAUDE / memory updates in this track — those
  happen in the Epic F stage-2 closure commit

## Rollback

- `news_retreat.rs`: revert to substring matching is a 10-line diff;
  keyword config is still `Vec<String>`
- `lead_lag_guard.rs`: multi-leader struct is additive; deleting it
  restores the v1 surface
- `regex` dep is already in the workspace root (for strategy /
  listing-sniper); removing it from `mm-risk/Cargo.toml` is 1 line
