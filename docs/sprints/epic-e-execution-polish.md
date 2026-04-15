# Epic E — Execution polish

> Sprint plan for the sixth epic in the SOTA gap closure
> sequence. The user reordered as **F → E** so the
> defensive surface landed first. Epics C, A, B, D, F all
> closed stage-1 in Apr 2026. Epic E ships the execution-
> infra polish items that improve tail latency and
> operational deployment without touching the alpha or
> defensive stacks.

## Why this epic

Wave-1 execution wired correct semantics: order diffing,
amend-when-supported, balance reservation, kill switch
escalation, audit trail. The remaining execution gaps from
the April 2026 SOTA pass are pure latency / operational
polish — nothing here changes captured edge per fill,
but together they shave 20-40% off tail latency and turn
a working bot into one a prop desk would actually deploy
on dedicated hardware.

The ROADMAP's Epic E originally bundled four sub-components:

1. **Batch order entry** — wire `OrderManager::execute_diff`
   to `place_orders_batch` / `cancel_orders_batch` instead
   of the per-order loop. Effort: ~3 days.
2. **io_uring runtime** for the WS read path. Effort: ~1-2
   weeks (invasive — requires `tokio-uring` dep, rustls
   validation, kernel-version gate).
3. **NUMA / IRQ / RT-kernel / hugepages deployment guide**
   + validated systemd unit template. Effort: ~2-3 days.
4. **Coinbase Prime FIX 4.4** — new venue connector on top
   of the existing FIX 4.4 codec. Effort: ~2 weeks.

Sub-components #2 and #4 are **deferred to stage-2** — both
are 1-2 week individual sub-epics that don't fit a 4-sprint
"polish" budget. Stage-1 ships #1 + #3, the two genuinely
small-and-high-ROI items.

## Scope (2 sub-components stage-1, 2 deferred)

| # | Component | New module / extension | Why |
|---|---|---|---|
| 1 | Batch order entry | `mm-engine::order_manager::execute_diff` extension | Wire to existing `place_orders_batch` / `cancel_orders_batch`. ~50 LoC net change, ≥10 unit tests |
| 2 | ~~io_uring runtime~~ | ~~`mm-server` runtime swap~~ | **Stage-2.** Invasive runtime change, needs rustls validation, Linux 5.6+ gate, benchmark validation |
| 3 | NUMA / RT-kernel / hugepages deploy guide | `docs/deployment.md` extension + `deploy/systemd/mm.service` template | Pure docs + one new file. Highest-ROI deployment win because operators rarely know any of this |
| 4 | ~~Coinbase Prime FIX~~ | ~~new `mm-exchange-coinbase-prime` crate~~ | **Stage-2.** New venue connector + auth + order types — full venue bring-up |

## Pre-conditions

- ✅ `ExchangeConnector::place_orders_batch` /
  `cancel_orders_batch` trait methods (since v0.1.0)
- ✅ Real batch endpoints on Bybit V5
  (`/v5/order/create-batch` + `/v5/order/cancel-batch`,
  max 20), Binance futures (`/fapi/v1/batchOrders`, max 5),
  HyperLiquid (bulk action, max 20). Binance spot's trait
  method is a sequential loop fallback — wiring is correct
  but the venue does not benefit.
- ✅ `OrderManager::execute_diff` already issues amend +
  cancel + place in the right order; only the inner per-
  order loop needs to swap to the batch call.
- ✅ `docs/deployment.md` already exists (207 lines) with
  the basic `RUST_LOG`, `MM_*` env var, and config-loader
  story. Sprint E-3 extends rather than rewrites.
- ✅ Existing `mm.service` systemd template would be
  helpful but does NOT exist yet — Sprint E-3 ships it.

## Total effort

**4 sprints × 1 week = 4 weeks** matching prior epic
cadence, even though the literal scope is smaller (2
sub-components vs 4). The slack covers the e2e wiring +
docs polish + commit discipline.

- **E-1** Planning + Study (no code) — audit existing
  `OrderManager`, connector batch APIs, and the existing
  deployment doc; pin the chunking + partial-fail
  behavior; write the sprint plan + open questions
- **E-2** Batch order entry (sub-component #1)
- **E-3** Deployment guide extension + systemd template
  (sub-component #3)
- **E-4** Engine integration smoke + 1-2 e2e tests +
  CHANGELOG / CLAUDE / ROADMAP / memory + single epic
  commit

---

## Sprint E-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before code
lands. End the sprint with a per-sub-component design
note plus a resolved open-question list.

### Phase 1 — Planning

- [ ] Walk every relevant existing primitive
  (`OrderManager::execute_diff`, `OrderManager::diff_orders`,
  `ExchangeConnector::place_orders_batch` /
  `cancel_orders_batch`, `VenueCapabilities::max_batch_size`,
  per-venue batch endpoint impls) and write a
  field-by-field delta of what batch order entry needs
- [ ] Pin the public API for the two stage-1 sub-components
- [ ] Define DoD per sub-component
- [ ] Decide chunking strategy: chunk by `max_batch_size`,
  fall back to per-order on partial failure
- [ ] Decide which deploy levers to document (NUMA pinning,
  IRQ steering, RT-kernel, hugepages, file descriptor
  limits, swap, OOM killer)

### Phase 2 — Study

- [ ] Audit `crates/exchange/binance/src/connector.rs`
  `place_orders_batch` impl — Binance spot does NOT have a
  real batch endpoint and falls back to a sequential loop.
  Wiring is correct anyway because the trait API is
  uniform; Binance spot just doesn't benefit.
- [ ] Audit `crates/exchange/binance/src/futures.rs`
  `place_orders_batch` impl — real `/fapi/v1/batchOrders`,
  max 5 orders per call, falls back to per-order for
  larger requests.
- [ ] Audit `crates/exchange/bybit/src/connector.rs` —
  real batch via `/v5/order/create-batch` and
  `/v5/order/cancel-batch`, max 20.
- [ ] Audit `crates/exchange/hyperliquid/src/connector.rs`
  — real batch via the bulk action, max 20.
- [ ] Read public io_uring + tokio-uring benchmarks to
  understand the deferred sub-component's benefit and risk
  surface for the stage-2 follow-up.
- [ ] Read Linux RT-kernel + hugepages + IRQ steering
  documentation (`PREEMPT_RT`, `irqbalance`,
  `transparent_hugepage`, `cpuset`) to draft the
  deployment guide content.

### Open questions to resolve

1. **Chunking — engine-side or connector-side?** The
   per-venue `place_orders_batch` impls already handle
   chunking via fall-back-to-loop when `orders.len() >
   max_batch_size`. **Default: engine sends the full list
   in one call**, lets the connector chunk internally.
   Cleaner separation of concerns. Stage-2 can introduce
   engine-level chunking if a venue surfaces a hard
   request-size limit.

2. **Partial batch failure handling.** Bybit's
   `/v5/order/create-batch` can return a mix of success +
   failure entries. **Default: trust the connector to
   return one OrderId per input order** (in input order),
   with `OrderId::nil()` placeholders for failed entries
   that the engine post-processes via `track_order` skip
   logic. Stage-2 can refine if a venue's partial-fail
   shape differs.

3. **Cancel batch fallback.** `cancel_orders_batch` returns
   `Result<(), Error>` (no per-order outcome). On a batch
   failure the engine **falls back to per-order
   `cancel_order`** for the same set, mirroring the existing
   amend → cancel + place fallback pattern in
   `execute_diff`. Default: fall back unconditionally on
   any batch error.

4. **Min batch size.** For a single-order placement,
   calling `place_orders_batch(&[one_order])` adds JSON
   overhead with no benefit. **Default: threshold
   `MIN_BATCH_SIZE = 2`** — single-order placements still
   go through the per-order `place_order` path. Bench
   later in stage-2 if needed.

5. **Deployment guide scope.** The ROADMAP lists NUMA, IRQ,
   RT-kernel, hugepages. **Default: document all four**
   plus file descriptor ulimits, swap disable, and OOM
   score adjustment. Ship one validated systemd unit
   template at `deploy/systemd/mm.service` that bundles
   the cgroup pinning + LimitNOFILE + ProtectSystem hardening.

6. **Systemd template — opinionated or generic?** Two
   choices: a one-size-fits-all template that operators
   tweak, or a parameterised template with placeholder
   variables. **Default: opinionated single-template with
   `${PLACEHOLDER}` markers** that operators substitute
   via `envsubst` or `sed`. Simpler to ship, simpler to
   document.

7. **io_uring deferral.** The runtime change requires:
   (a) adding `tokio-uring` dep, (b) replacing tokio's
   work-stealing scheduler with the io-uring single-
   threaded runtime in the WS read path, (c) validating
   that rustls works under tokio-uring (the existing
   `tokio-tungstenite` + rustls combo may need adapter
   work), (d) Linux kernel ≥ 5.6 gate. **Default: defer
   to stage-2.** Tracked in ROADMAP closure note.

8. **Coinbase Prime FIX deferral.** A new venue connector
   needs auth, order types, message routing, error mapping,
   per-venue rate limiting, and integration with the
   existing `mm-portfolio` / `mm-risk` pipelines.
   **Default: defer to stage-2.** Tracked as a follow-up.

### Deliverables

- Audit findings inline at the bottom of this sprint
  doc (same shape as Epic C / A / B / D / F Sprint 1
  audit sections)
- Per-sub-component public API sketch
- All 8 open questions resolved with defaults or
  explicit "decide in Sprint E-2"

### DoD

- Every stage-1 sub-component has a public API sketch,
  a tests list, and a "files touched" estimate
- The next 3 sprints can execute without further open
  rounds
- io_uring + Coinbase Prime FIX deferrals are documented
  in the sprint doc + ROADMAP so they don't get dropped

### Audit findings — existing primitives we reuse

Phase-2 audit done against the live tree on 2026-04-15.

#### A — `OrderManager::execute_diff` (`crates/engine/src/order_manager.rs`)

Already has the right shape: `diff_orders` produces a
`Plan { to_amend, to_cancel, to_place }`, then
`execute_diff` issues amends → cancels → places in that
order. The per-order loops at the bottom are the only
thing batch order entry needs to change:

```rust
// Cancel stale orders.
for order_id in &plan.to_cancel {
    connector.cancel_order(symbol, *order_id).await ...
}
// Place new orders.
for quote in &plan.to_place {
    connector.place_order(&order).await ...
}
```

Replace each loop with: chunk by `max_batch_size`,
call the batch method, on error fall back to the
per-order loop. ~20 LoC per call site.

**Verdict:** no structural changes. Pure inner-loop swap.

#### B — Per-venue batch endpoint coverage

| Venue | `place_orders_batch` | `cancel_orders_batch` | `max_batch_size` |
|---|---|---|---|
| Bybit V5 | real `/v5/order/create-batch` | real `/v5/order/cancel-batch` | 20 |
| Binance futures | real `/fapi/v1/batchOrders` | per-order loop | 5 |
| HyperLiquid | real bulk action | real bulk action | 20 |
| Binance spot | per-order loop fallback | per-order loop fallback | 5 |
| Custom client | per-order loop fallback | per-order loop fallback | (default) |

**Verdict:** Bybit V5 + HL get full benefit (5-20× round-
trip reduction on big diffs). Binance futures gets up to
5× coalescing. Binance spot + custom client are
no-benefit but no-regression. The trait API is uniform so
the engine wiring is the same regardless.

#### C — `VenueCapabilities::max_batch_size`

Already populated for every venue (5, 5, 20, 20). The
engine reads `connector.capabilities().max_batch_size` on
every diff to pick the chunk size. No new connector trait
method needed.

#### D — `docs/deployment.md` (207 lines)

Existing content covers: RUST_LOG, env-var secrets,
config file loading, telegram setup, systemd basics
(but not a complete unit file), Docker, paper mode. NUMA
/ IRQ / RT-kernel / hugepages / fd limits / swap are
**not** mentioned. Sprint E-3 appends a new "High-perf
deployment" section after the existing systemd snippet.

**Verdict:** extension, not rewrite. Append + add one
new file (`deploy/systemd/mm.service`).

### Open questions — resolved

All 8 open questions resolved against the defaults above:

1. **Chunking** → ✅ connector-side, engine sends the full
   list in one call.
2. **Partial batch failure** → ✅ trust connector to
   return per-input-order outcomes.
3. **Cancel batch fallback** → ✅ unconditional per-order
   fallback on any batch error.
4. **Min batch size** → ✅ `MIN_BATCH_SIZE = 2`,
   single-order calls stay per-order.
5. **Deployment guide scope** → ✅ NUMA / IRQ / RT-kernel
   / hugepages / fd limits / swap / OOM score, all in one
   new section.
6. **Systemd template** → ✅ opinionated single template
   with `${PLACEHOLDER}` markers.
7. **io_uring deferral** → ✅ explicitly stage-2. Tracked
   in ROADMAP closure note.
8. **Coinbase Prime FIX deferral** → ✅ explicitly
   stage-2. Tracked in ROADMAP closure note.

### Per-sub-component API surface — pinned

#### #1 Batch order entry — `mm_engine::order_manager`

No new public API surface. The change is internal to
`OrderManager::execute_diff`:

```rust
// New private helper (placed alongside execute_diff):
async fn place_quotes_batched(
    &mut self,
    symbol: &str,
    quotes: &[Quote],
    product: &ProductSpec,
    connector: &Arc<dyn ExchangeConnector>,
) -> usize;  // returns count of successful placements

async fn cancel_orders_batched(
    &mut self,
    symbol: &str,
    order_ids: &[OrderId],
    connector: &Arc<dyn ExchangeConnector>,
) -> usize;  // returns count of successful cancels

const MIN_BATCH_SIZE: usize = 2;
```

Files touched:
- `crates/engine/src/order_manager.rs` — internal
  refactor of `execute_diff` + 2 new private helpers,
  ~80 LoC net.

Tests list (≥10):
- single-order place stays on per-order path
- 2-order place uses batch call
- 5-order place chunks correctly on Binance-futures-style
  connector (max=5)
- 21-order place against a max=20 venue chunks into 20+1
- partial batch failure returns the success count and
  falls back per-order for the failed slice
- batch returning fewer ids than inputs is handled
- empty-list place is a no-op
- single-order cancel stays on per-order path
- 5-order cancel uses batch call
- batch cancel error falls back to per-order
- internal `LiveOrder` tracking matches batch-vs-per-
  order path output (no double-tracking)
- existing `execute_diff` tests stay green

#### #3 Deployment guide — `docs/deployment.md` + `deploy/systemd/mm.service`

No code surface. The deliverables are pure markdown +
one systemd unit file:

- New "High-performance deployment" section in
  `docs/deployment.md` covering:
  - File descriptor limits (`ulimit -n`, `LimitNOFILE`)
  - Swap disable (`swapoff -a`, `vm.swappiness=0`)
  - OOM score adjustment (`OOMScoreAdjust=-500`)
  - NUMA pinning (`numactl --cpunodebind`, cpuset)
  - IRQ steering (`irqbalance` disable, manual smp_affinity)
  - Transparent hugepages (`/sys/kernel/mm/transparent_hugepage/enabled`)
  - PREEMPT_RT kernel notes (when to use, how to verify)
- New file `deploy/systemd/mm.service` — a complete
  validated systemd unit template with cgroup pinning,
  ProtectSystem, NoNewPrivileges, MemoryAccounting, etc.

Tests list: N/A (pure docs). The systemd template is
hand-validated by the sprint reviewer.

### Per-sub-component DoD — pinned

| # | Component | Files | LoC (est) | Tests | DoD |
|---|---|---|---|---|---|
| 1 | Batch order entry | `order_manager.rs` | ~80 net | ≥10 | Single-order stays per-order, 2+ uses batch, partial-fail falls back, internal tracking parity |
| 3 | Deployment guide | `docs/deployment.md` + `deploy/systemd/mm.service` | ~250 lines docs + ~60 lines systemd | N/A | All 7 deploy levers documented; systemd template validated by reviewer |

**Epic total**: ~80 LoC code + ~310 lines docs across 3
files, ≥10 unit tests, plus 1 end-to-end test in Sprint
E-4 verifying the engine's `refresh_quotes → execute_diff`
path actually issues a batch call when ≥ 2 quotes are
diffed.

---

## Sprint E-2 — Batch order entry (week 2)

**Goal.** Land sub-component **#1** as the high-ROI
execution polish win.

### Phase 3 — Collection

- [ ] Build a synthetic `MockConnector` extension that
  records both `place_order` and `place_orders_batch`
  call sites separately so tests can verify the engine
  picked the batch path
- [ ] Build per-venue fixtures with different
  `max_batch_size` values (5, 20) to verify the chunking
  logic handles both

### Phase 4a — Dev

- [ ] **Sub-component #1** — `mm-engine::order_manager`:
  - Add `MIN_BATCH_SIZE` constant
  - Add `place_quotes_batched` private helper
  - Add `cancel_orders_batched` private helper
  - Replace per-order loops in `execute_diff` with calls
    to the new helpers
  - Preserve the existing `LiveOrder` tracking semantics
- [ ] ≥10 unit tests

### Deliverables

- `crates/engine/src/order_manager.rs` extension
- `crates/engine/src/test_support.rs` — extend
  `MockConnector` with batch-path counters
- ≥10 tests
- Workspace test + fmt + clippy green

---

## Sprint E-3 — Deployment guide + systemd template (week 3)

**Goal.** Land sub-component **#3** as the operational
deployment polish win.

### Phase 4b — Dev

- [ ] **Sub-component #3** — `docs/deployment.md` extension:
  - New "High-performance deployment" section
  - 7 sub-sections (one per deploy lever)
  - Each sub-section: what + why + how + verification
- [ ] **Sub-component #3 cont.** — `deploy/systemd/mm.service`:
  - Complete unit file with `${PLACEHOLDER}` markers for
    user / group / install path / config path
  - Hardening: `ProtectSystem=strict`, `NoNewPrivileges`,
    `MemoryAccounting`, `CPUAccounting`, `LimitNOFILE`
  - Cgroup pinning via `CPUAffinity` / `MemoryNUMA`
- [ ] Inline shell snippets the operator runs to verify
  each lever (e.g. `cat /sys/kernel/mm/transparent_hugepage/enabled`)

### Deliverables

- `docs/deployment.md` extended (~+250 lines)
- `deploy/systemd/mm.service` new file (~60 lines)
- Hand-reviewed for accuracy
- Workspace test + fmt + clippy green (no code changes)

---

## Sprint E-4 — Engine integration + audit + commit (week 4)

**Goal.** Engine-side smoke tests, end-to-end batch path
verification, close the epic with the standard CHANGELOG
+ CLAUDE + ROADMAP + memory updates and a single commit.

### Phase 4c — Dev

- [ ] No new engine fields needed — batch entry is a
  pure swap of an internal `OrderManager` loop. Engine
  wiring is "do nothing, the diff path picks it up
  automatically."
- [ ] One end-to-end test in `mm-engine`: build a
  `MarketMakerEngine` with a `MockConnector` whose
  `max_batch_size = 20`, call `refresh_quotes`, assert
  the connector saw a single `place_orders_batch` call
  and zero `place_order` calls when the diff has ≥ 2
  new quotes.

### Phase 5 — Testing

- [ ] One e2e test verifying the engine routes through
  the batch path on a multi-quote diff
- [ ] Existing engine integration tests stay green

### Phase 6 — Documentation

- [ ] CHANGELOG entry following the Epic A / B / C / D
  / F shape
- [ ] CLAUDE.md: bump stats line, mention batch order
  entry in the engine module list comment
- [ ] ROADMAP.md: mark Epic E as DONE stage-1 (2/4),
  list stage-2 follow-ups (io_uring, Coinbase Prime FIX)
- [ ] Memory: extend `reference_sota_research.md` with
  Epic E closure notes

### Deliverables

- 1 end-to-end test
- CHANGELOG, CLAUDE, ROADMAP, memory all updated
- Single epic commit without CC-Anthropic line

---

## Definition of done — whole epic

- Both stage-1 sub-components (batch order entry + deploy
  guide) shipped
- io_uring and Coinbase Prime FIX explicitly deferred and
  tracked
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per commit discipline
- `OrderManager::execute_diff` issues batch calls when
  ≥ 2 orders + venue supports them
- `docs/deployment.md` has a complete "High-performance
  deployment" section
- `deploy/systemd/mm.service` exists and is reviewer-
  validated
- CHANGELOG, CLAUDE, ROADMAP, memory all updated

## Risks and open questions

- **Binance spot regression risk.** Binance spot's
  `place_orders_batch` is a sequential per-order loop
  that may have subtly different error semantics than a
  direct loop in `execute_diff` (e.g. error type wrapping,
  `OrderId::nil()` placeholders). v1 mitigation: keep
  the per-order code path live for `MIN_BATCH_SIZE = 1`
  and assert no behavior change on Binance spot.
- **Partial batch failure behavior.** Real venues return
  partial-success batches. v1 mitigation: connector
  contract is "return one OrderId per input order, in
  order, `OrderId::nil()` for failed". The engine post-
  processes by skipping nil placeholders. Stage-2 may
  need a richer per-order error type if operators want
  per-order failure reasons.
- **Systemd template assumptions.** The template assumes
  the operator runs on Linux with systemd. Macs and
  non-systemd Linux distros need separate guidance —
  v1 mitigation: the deployment doc explicitly says
  "systemd-based Linux" and points non-systemd users at
  the existing Docker deployment path.
- **io_uring stage-2 unblocks.** Tracked in ROADMAP. The
  rustls-under-tokio-uring story is the long pole.
- **Coinbase Prime FIX stage-2 unblocks.** Tracked in
  ROADMAP. Coinbase Prime API access + sandbox
  credentials are operator-side prerequisites.

## Sprint cadence rules

- **One week per sprint.** Friday EOD = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint E-1.** Planning + study only.
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of E-4.
