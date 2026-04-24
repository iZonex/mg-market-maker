# Contributing

Thank you for your interest in contributing to MG Market Maker. This document covers the coding style, testing expectations, commit conventions, and review checklist we apply before merging.

## Getting Started

1. **Fork** the repository
2. **Clone** your fork: `git clone https://github.com/YOUR_USERNAME/market-maker.git`
3. **Create a branch**: `git checkout -b feat/your-feature`
4. **Install Rust**: `rustup default stable` (minimum: edition 2021)
5. **Build**: `cargo build`
6. **Test**: `cargo test --workspace`
7. **Frontend** (if touching UI): `cd frontend && npm install && npm run dev`

## Development Workflow

### Before submitting a PR

```bash
# Must all pass — CI enforces these exact commands
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check

# Frontend (only if frontend touched)
cd frontend && npm run build              # must build without errors
./scripts/lint-design-system.sh           # enforces design-system invariants
```

The design-system linter catches: new `.btn {}` CSS rules outside
`primitives/Button.svelte`, inline `class="btn"` usages, hex/rgba
literals in components (should use `tokens.css`), brand-string
hardcodes outside `branding.js`, duplicate chip/pill/tone utility
CSS outside `utilities.css`, and hand-rolled `.modal-backdrop`
chrome. Every new PR runs it; see
**[docs/guides/frontend-style-guide.md](docs/guides/frontend-style-guide.md)**
for the rules + available primitives.

### Code Style — Rust

- **Edition 2021** — use `let … else`, `if let`-chains, `async fn` in traits
- **`rust_decimal::Decimal`** for all money/price/quantity — never `f64`
- **Meaningful names** — `inventory_manager` not `im`, `mid_price` not `mp`
- **Tests alongside code** — inline `#[cfg(test)] mod tests { ... }` is the default. For large test modules (>200 LOC or more than ~10 tests), extract to a sibling file at `<module>/tests.rs` with a bare `#[cfg(test)] mod tests;` declaration — keeps the production file readable.
- **Tracing for logging** — `info!`, `warn!`, `error!` from the `tracing` crate; never `println!`
- **No `unwrap()` on Results that represent externalities** — propagate `?` or handle the error explicitly
- **No backward-compat shims** — when the shape of something changes, change every call site. We don't maintain deprecated APIs internally.

### Code Style — Svelte / frontend

- **Svelte 5 runes** — `$state`, `$derived`, `$effect`, `$props`. No legacy `$:` reactivity blocks in new code.
- **`$derived.by(...)` for multi-step computations** — keeps the dependency graph explicit
- **Component granularity** — if a modal > ~200 LOC of template or holds >5 state roots, extract it to a sibling component
- **CSS scoped to component** — no global styles except design tokens in `src/app.css`
- **Svelte-flow for the strategy canvas** — custom node types go in `src/lib/components/Strategy*.svelte`

## Adding a New Rust Strategy

1. Create `crates/strategy/src/your_strategy.rs`
2. Implement the `Strategy` trait:
   ```rust
   impl Strategy for YourStrategy {
       fn name(&self) -> &str { "your-strategy" }
       fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> { ... }
   }
   ```
3. Export from `crates/strategy/src/lib.rs`
4. Add variant to `StrategyType` enum in `crates/common/src/config.rs`
5. Wire into `run_symbol()` in `crates/server/src/main.rs`
6. Add tests in the sibling `crates/strategy/src/your_strategy/tests.rs`
7. Document in `docs/guides/strategy-catalog.md` with formula + config + gotchas

## Adding a Graph Node

The preferred way to add strategy logic is as a graph node — users compose them through the dashboard palette without recompiling.

1. Pick the right module under `crates/strategy-graph/src/nodes/`:
   - `sources.rs` — source nodes (zero input ports, produced by engine)
   - `math.rs` — arithmetic
   - `stats.rs` — running statistics
   - `logic.rs` — boolean logic
   - `indicators.rs` — technical indicators (SMA, EMA, etc.)
   - `strategies.rs` — composite strategy nodes
   - `plan.rs` — execution algorithms
   - `exec.rs` — exec composites
   - `sinks.rs` — terminals (sink actions)
2. Implement `NodeKind`:
   ```rust
   impl NodeKind for MyNode {
       fn kind(&self) -> &'static str { "Category.MyNode" }
       fn input_ports(&self) -> &[Port] { &INPUT_PORTS }
       fn output_ports(&self) -> &[Port] { &OUTPUT_PORTS }
       fn config_schema(&self) -> Vec<ConfigField> { ... }  // drives UI form
       fn from_config(cfg: &Value) -> Option<Self> { ... }
       fn evaluate(&self, ctx: &EvalCtx, inputs: &[Value], state: &mut NodeState)
           -> Result<Vec<Value>> { ... }
   }
   ```
3. Register in `crates/strategy-graph/src/catalog.rs` — add a match arm in the builder + metadata in the catalog listing
4. Add tests in the node's sibling test module or inline
5. If the node is a pentest exploit, mark `restricted: true` in the catalog entry so it's gated by `MM_ALLOW_RESTRICTED=yes-pentest-mode`
6. Write a template that uses the node in `crates/strategy-graph/templates/` (optional, but helpful for discovery)

## Adding a New Exchange Connector

1. Create `crates/exchange/<venue>/`
2. Implement `ExchangeConnector` trait (see `crates/exchange/core/src/connector.rs`)
3. Handle authentication (HMAC / EIP-712 / FIX logon), WebSocket reconnect, REST rate-limit backoff, 429 handling
4. Set `VenueCapabilities` flags — only flip `supports_ws_trading` / `supports_fix` / `supports_amend` / `supports_funding_rate` true when the code path is actually wired; set `max_batch_size` / `max_order_rate` to the venue's real limits
5. Reuse shared protocol layers in `crates/protocols/` when applicable (`ws_rpc` for id-correlated WS, `fix` for FIX 4.4)
6. Add to workspace in root `Cargo.toml`
7. Add a capability-audit test (each exchange crate has one) — ensures capability flags match reality
8. Write venue-specific docs in `docs/protocols/<venue>.md` — endpoint, auth scheme, rate limits, error codes, reconnect semantics
9. Update `docs/guides/adding-exchange.md` if the 8-step guide drifts

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add OKX exchange connector
fix: correct VPIN bucket overflow on low volume
perf: optimize GLFT C2 computation
refactor: split dashboard/client_api.rs by endpoint family
docs: add graph-authoring.md
test: add integration tests for kill switch
chore: bump rust_decimal to 1.34
```

Scoped commits are encouraged: `feat(sor): ...` or `fix(strategy-graph): ...`.

**Don't squash unrelated changes** — one commit per logical change. Reviewers should be able to see the pieces.

**Co-author trailer when AI-assisted:**
```
Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

## Testing Philosophy

- **Unit tests ≠ done.** Unit-green has historically missed 10-15 silent bugs per large change. Run `scripts/distributed-smoke.sh` before claiming a feature complete.
- **Integration tests for every new connector** with a mock server in the crate.
- **Paper-smoke before live.** The `MM_MODE=paper` run with real feed is the last gate; even if unit tests pass, paper mode catches fill-simulation edge cases.
- **No mocking the database in risk-path tests** — integration tests hit a real DB. Mocks have masked migration bugs.
- **Deterministic time** — use `ctx.now_ms()` / `chrono::DateTime<Utc>` injection, not `SystemTime::now()` directly. Tests with wall-clock flake.

## Pre-commit hooks (recommended)

Wire your own via `.git/hooks/pre-commit`:

```bash
#!/usr/bin/env bash
set -e
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib --workspace
```

This catches the three CI blockers before you push.

## Review Checklist (what reviewers look for)

- [ ] **No f64 for money.** If a Decimal was converted to f64, reviewer asks why.
- [ ] **No `.unwrap()` in hot paths.** Panics in the tick loop are outages.
- [ ] **New public fn has a doc comment.** Internal helpers get a one-liner; public API gets the full "why".
- [ ] **New config fields have defaults + are threaded through the default struct builder + every test fixture.** A missing default breaks 30 test initializers.
- [ ] **Tests for the bug being fixed.** `fix:` commits without a regression test get pushed back.
- [ ] **No debug-only `println!`.** Use `tracing::debug!`.
- [ ] **Frontend: `npm run build` passes.** CI has had gaps here — check locally.
- [ ] **CLAUDE.md + README stats.** If you added a crate or a major subsystem, update the counts.
- [ ] **Security-sensitive changes audited.** Vault, auth, MiCA audit — two reviewers.
- [ ] **New metrics documented in `docs/guides/metrics-glossary.md`.**

## Architecture Decisions

### Why Rust?

Market making is latency-sensitive. Rust gives us:
- Zero-cost abstractions
- No GC pauses
- Memory safety without runtime overhead
- `Decimal` arithmetic without floating-point errors

### Why `rust_decimal` instead of `f64`?

Financial arithmetic requires exact decimal representation. `0.1 + 0.2 == 0.3` must be true. With `f64`, it isn't. A single rounding error compounded over millions of operations leads to real money loss.

### Why Avellaneda-Stoikov / GLFT?

These are the mathematically optimal solutions to the market making problem under specific assumptions. They provide a rigorous framework rather than ad-hoc heuristics. The GLFT model adds practical execution risk to the theoretical A-S framework.

### Why the strategy-graph layer?

Graph-authored strategies let operators compose signals and rules without recompiling. The engine still runs the Rust hot path — the graph is a **policy layer** that dispatches to existing primitives. This has two wins: non-Rust authors can customise, and the same deployment binary serves every client's tailored policy.

### Why one abstraction per protocol pattern (`protocols/`)?

When two venues share a transport pattern (id-correlated WebSocket request/response, FIX 4.4 session), the pattern lives once in `crates/protocols/*`. Venue crates are thin adapters mapping the shared abstraction onto the venue-specific shape. `VenueCapabilities::supports_ws_trading` / `supports_fix` flags are never set unless the code path is actually wired — covered by a capability-audit test in each exchange crate.

## Questions?

Open an issue with the `question` label, or check:
- **[CLAUDE.md](CLAUDE.md)** — architecture overview + key design principles
- **[docs/guides/architecture.md](docs/guides/architecture.md)** — crate graph, data flow
- **[docs/guides/writing-strategies.md](docs/guides/writing-strategies.md)** — Rust strategy trait
- **[docs/guides/graph-authoring.md](docs/guides/graph-authoring.md)** — graph node authoring
- **[docs/guides/adding-exchange.md](docs/guides/adding-exchange.md)** — connector walkthrough
