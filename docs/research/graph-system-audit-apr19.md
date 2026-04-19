# Graph system audit — 2026-04-19

Systematic walk of `crates/strategy-graph/` + the engine's
integration in `crates/engine/src/market_maker.rs` after Batch
A–E closed the TODO.md bands. Goal: surface every "half-wired"
node, stale comment, or port-type mismatch so a follow-up pass
has concrete targets.

**Catalog totals (as of this commit):** 104 node kinds, 7
sinks, ~47 source kinds with engine overlays, 10 bundled
templates.

---

## P0 — fixed this pass

### Book.L1 bid_qty / ask_qty ports

Ports `bid_qty` and `ask_qty` on `Book.L1` used to emit
`Missing` unconditionally (`market_maker.rs:2379`). The common
orderbook already exposed `best_bid_qty()` + `best_ask_qty()`,
just nobody had threaded them into the source overlay. Plumbed
in this pass — the comment on the old code was stale by at
least two epics. Any graph consuming `Book.L1.bid_qty` now
sees a real number.

---

## P1 — deferred, documented

### Per-node Strategy.* config override (γ / κ / σ)

Every `Strategy.Avellaneda | GLFT | Grid | Basis | CrossExchange`
source node can carry per-node config in the graph JSON
(`{gamma: 0.5, kappa: 1.2, ...}`), but the engine source
overlay at `market_maker.rs:3472–3505` reads
`last_strategy_quotes_per_node` without consulting the node's
config. The comment there reads "per-node `γ`/`κ`/`σ` override
lands in Phase 5" — Phase 5 of Epic H never shipped.

**Scope:** per-node strategy instance pool with independent
config. Design is well-understood; implementation is a 100-200
line refactor of the strategy_pool wiring. Not blocking current
deployments (single-engine graphs work fine).

### Config schema coverage for `Risk.*`

`Risk.ToxicityWiden`, `Risk.InventoryUrgency`,
`Risk.CircuitBreaker` each parse a non-trivial config blob in
`from_config` but have no `config_schema()`, so the UI falls
back to a free-form JSON textarea. Operators working off
defaults don't notice; anyone editing in-place does. Adding
schemas is a mechanical follow-up, same shape as the exec
schemas landed this pass.

### Sentiment scope resolution

`Sentiment.Rate` / `Sentiment.Score` resolve to the engine's
own asset only on a `Scope::Symbol` graph (market_maker.rs:
2410–2417). Global / AssetClass / Client graphs emit
`Missing`. Intentional at the moment (no per-engine asset tag
on non-symbol scopes), but undocumented behaviour trips
operators who author a global sentiment gate. Needs either a
new config field on the source node (explicit asset = "BTC") or
clear wording in the catalog palette hint.

---

## P2 — polish / drift

### Source overlay registry

47 source kinds are hard-wired in the `tick_strategy_graph`
switch statement. If a new source is added to `catalog.rs`'s
`build()` but forgotten in `market_maker.rs`, the node exists
and compiles but emits `Missing` forever. A
catalog-vs-engine-coverage test would catch this at CI — low
priority now because every existing source is wired, but worth
adding before the next source batch.

### Stale audit-era comments

Updated `market_maker.rs:3647` ("degenerate dispatcher") to
describe the actual 3.A + 3.B dispatch flow. The
"advisory-only" Stat-Arb docstring at 472–475 was refreshed
earlier in Batch C to reflect the MV-4 naked-leg safety net.

A sweep for `// TODO` / `// FIXME` / `stub` / `advisory-only`
in strategy-graph + engine surfaced no other incorrect
markers.

### Template test coverage

- `every_bundled_template_parses` walks all 10 templates
  including pentests — ✓.
- `every_safe_template_compiles` skips pentests (they require
  `MM_ALLOW_RESTRICTED=yes-pentest-mode`) — ✓.
- `pentest_templates_refused_without_env` confirms the gate
  fires — ✓.

Missing: an env-gated compile test for pentest templates. Needs
`serial_test` or similar since `std::env::set_var` is unsafe
under Rust 2024 and races parallel tests. Not blocking (the
parse test catches JSON drift; the compile gate catches
production-mode misuse).

---

## Fixed in this pass

- **Book.L1.bid_qty / Book.L1.ask_qty** (engine overlay now
  reads `book.best_bid_qty()` / `book.best_ask_qty()`).
- **`Exec.{Twap,Vwap,Pov,Iceberg}Config` schemas** — every
  Exec.*Config node now declares a `config_schema()` with
  appropriate Number/Integer/Text widgets + bounds. The
  frontend renders proper forms instead of free-form JSON.
- **Stale comment at market_maker.rs:3647** — refreshed to
  describe the real 3.A + 3.B VenueQuotes dispatch.

## Open work, tracked in TODO.md

- Per-node Strategy.* config override (P1).
- Risk.* config schemas (P2).
- Sentiment scope hint (P2).
- Catalog-vs-engine coverage test (P2).
