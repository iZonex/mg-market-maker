# Roadmap

Ideas that are not in-flight and not part of an open epic, but we
have explicitly decided are worth revisiting. Each entry records the
motivation, the minimum-risk path, the estimated scope, and the
pre-conditions under which it becomes worth picking up.

This is not a commitment list. Things land here when we have
decided *not* to do them now and want to avoid re-running the same
analysis later.

---

## Production crypto MM SOTA gap epics (2026-04-15 research pass)

Six new epics surfaced by the desk-research pass against
publicly-known production prop desks (Wintermute, GSR, Jump,
Flow Traders, Cumberland, Flowdesk, Keyrock, B2C2, Hummingbot
ecosystem). Full source citations and per-axis gap matrices
live in `docs/research/production-mm-state-of-the-art.md`.

The previous `production-spot-MM gap closure` epic (now CLOSED
as v0.4.0) closed eight P0/P1/P2 items that were pure
operational hardening on top of the v0.2.0 spot+cross-product
foundation. This new cluster is structurally different: each
epic adds either a new strategy family, a new aggregation
layer, or a new execution coordinator. Most depend on
primitives we *already have* — the gap is the missing
coordinator on top, not missing infrastructure underneath.

The recommended sequencing is **C → A → B → D → E → F**
because (a) Epic C unlocks the per-strategy PnL view that
Epic B's stat-arb depends on, (b) Epic A's SOR is the cleanest
single execution win and unlocks both XEMM and triangular arb,
(c) Epic B is the biggest *strategy-variety* gap and pays for
itself the fastest in $/eng-week, (d) D / E / F are
incrementally additive once A-C land.

### Epic A — Cross-venue Smart Order Router ✅ CLOSED stage-1 (Apr 2026)

**Status.** All 4 stage-1 sub-components shipped over 4
one-week sprints (A-1 planning/study, A-2 cost model +
venue state aggregator, A-3 greedy router + engine hook,
A-4 audit + metrics + docs). Full breakdown in CHANGELOG
`[Unreleased]` and in
`docs/sprints/epic-a-cross-venue-sor.md`.

**Stage-2 follow-ups (tracked as next epic):**

- LP solver for the constrained quadratic variant of
  the cost minimisation (`good_lp` or `clarabel`)
- Inline dispatch via an `ExecAlgorithm` — stage-1 is
  strictly advisory, stage-2 adds auto-routing
- Real trade-rate estimator wired into the queue-wait
  cost (v1 uses a fixed `queue_wait_bps_per_sec`
  constant)
- Full `B` matrix cross-beta routing across assets
  (diagonal-β only in v1)
- Multi-symbol snapshot per venue (one symbol per
  venue in v1)
- Auto-refresh of venue seeds from the P1.2 fee-tier
  refresh task
- Server-side composition of a multi-venue
  `ConnectorBundle` for operator's production config
- `mm-route` CLI dry-runner for calibration

---

### Epic A — Cross-venue Smart Order Router (P1, ~1 month) — ORIGINAL SCOPE

**Why.** Every venue connector is built (Binance spot+futures,
Bybit spot/linear/inverse, HL spot+perp, custom) and every
primitive a router needs (live fee tiers from P1.2, queue
model from v0.4.0, balance cache per venue, rate limiters)
is in place — but no coordinator decides where to route a
given fill. This is the single biggest leverage gap in the
codebase.

**Scope.** New `mm-engine::sor` module. Given a desired
quantity on one side, solve a small linear program that
minimizes
`Σ(taker_fee · taker_qty_i + queue_wait_cost · maker_qty_i)`
subject to per-venue rate limits, per-venue margin
availability, and a target completion time. Start with a
greedy version and move to a real LP solver later.

**Effort.** 3-4 weeks.

**Pre-conditions.** None — all primitives already exist.

**Unlocks.** Triangular arb (Epic B sub-item), cleaner XEMM,
proper cross-venue basis dispatch, multi-venue stat-arb leg
execution.

### Epic B — Stat-arb / cointegrated pairs ✅ CLOSED stage-1 (Apr 2026)

**Status.** All 4 stage-1 sub-components shipped over 4
one-week sprints (B-1 planning/study, B-2 cointegration +
Kalman, B-3 z-score signal + driver scaffolding, B-4 engine
wiring + audit + docs). Full breakdown in CHANGELOG
`[Unreleased]` and `docs/sprints/epic-b-stat-arb-pairs.md`.

**What shipped.**
- `mm-strategy::stat_arb::cointegration` — Engle-Granger
  2-leg test (OLS → residuals → basic ADF → MacKinnon 5%
  lookup). v1 uses 2-leg Engle-Granger, NOT multivariate
  Johansen (which is deferred to stage-2 if 3+ asset
  cointegration vectors surface).
- `mm-strategy::stat_arb::kalman` — scalar linear-Gaussian
  Kalman filter for adaptive β, `with_initial_beta` warm
  start from the Engle-Granger OLS β.
- `mm-strategy::stat_arb::signal` — rolling-window z-score
  with Welford-equivalent running sums, two-level
  hysteresis, `SignalAction { Open, Close, Hold }`.
- `mm-strategy::stat_arb::driver` — `StatArbDriver` state
  machine composing the three primitives. Standalone
  tokio-task pattern mirroring `FundingArbDriver`.
- `MarketMakerEngine::with_stat_arb_driver` builder +
  `stat_arb_interval` select-loop arm + `handle_stat_arb_event`
  routing to new `StatArbEntered` / `StatArbExited` audit
  event types.

**Stage-1 is advisory-only** (same pattern as Epic A's SOR):
the driver runs its state machine and emits events, but
does NOT dispatch leg orders. Audit trail records intent;
operators sign off before stage-2 wires inline dispatch.

**Stage-2 follow-ups (tracked as next epic):**

- Real leg-execution dispatch via `ExecAlgorithm::TwapAlgo`
  on entry + `OrderManager::execute_unwind_slice` on exit.
- Per-pair PnL bucket wiring once real fills flow through
  the driver (the Portfolio infra is already in place from
  Epic C — it's a pure call-site change).
- Background pair-screener (v1 ships the screener as an
  offline CLI helper only).
- Multivariate Johansen cointegration for 3+ asset
  cointegration vectors.
- MacKinnon 1991 polynomial fit instead of the v1 lookup
  table if operators run non-standard sample sizes.
- ADF with lag selection via AIC (v1 is basic ADF, no
  lag terms).

**Pre-conditions (satisfied).** Per-strategy PnL attribution
from Epic C.

### Epic C — Portfolio-level risk view ✅ CLOSED stage-1 (Apr 2026)

**Status.** All 5 stage-1 sub-components shipped over 4
one-week sprints (C-1 planning/study, C-2 per-factor delta
+ per-strategy labeling + stress scaffolding, C-3 hedge
optimizer + VaR guard, C-4 stress runner + CLI + docs).
Full breakdown in CHANGELOG `[Unreleased]` and in
`docs/sprints/epic-c-portfolio-risk-view.md`.

**Stage-2 follow-ups (tracked as next epic):**

- Real historical Tardis replay for the five scenarios
  (v1 uses deterministic synthetic shock profiles)
- Cross-beta hedging (off-diagonal β from rolling
  regression) in the hedge optimizer
- Off-diagonal factor covariance estimation
- LP solver (`good_lp` / `clarabel`) for the constrained
  LASSO variant of the hedge optimization
- EWMA variance in the VaR guard for faster regime
  adaptation
- Historical-simulation VaR as a cross-check against the
  parametric Gaussian formula
- CVaR / expected-shortfall alongside VaR
- Full-engine stress integration test — Sprint C-4 runs
  the stress path through the synthetic runner only; the
  full `MarketMakerEngine` end-to-end drive is deferred

---

### Epic C — Portfolio-level risk view (P1, ~3 weeks) — ORIGINAL SCOPE

**Why.** Risk guards are per-strategy, per-symbol, and
per-asset-class (P2.1), but there is no *portfolio* level.
The aggregation layer that turns "eight strategies quoting
on three venues in four assets" into one coherent risk view
does not exist. This is what unlocks risk-parity capital
allocation, cross-asset hedge optimization, and a credible
institutional risk story.

**Scope.**
- **Per-factor delta aggregation.** Extend `Portfolio` to
  aggregate exposure per **base asset** (BTC-delta, ETH-delta,
  SOL-delta, stablecoin-delta) instead of just total PnL.
  New Prometheus gauges `mm_portfolio_delta{asset=BTC}` etc.
- **Cross-asset hedge optimizer** (Cartea-Jaimungal ch.6
  closed form). Takes the full portfolio exposure vector,
  emits the optimal hedge basket (BTC spot vs perp, ETH spot
  vs perp, USDC↔USDT FX leg) to minimize portfolio variance
  subject to funding cost.
- **Per-strategy VaR limit.** `mm-risk::var_guard` computes
  rolling 24h PnL variance per strategy, sets a 95%-VaR
  ceiling from config, on breach pushes a soft-throttle
  signal into the autotune channel exactly like Market
  Resilience does today.
- **Stress replay library.** Curate event JSONL snapshots
  for the five canonical crypto crashes (2020 covid, 2021
  China ban, 2022 LUNA, 2022 FTX, 2023 USDC depeg) from
  Tardis historical data. New
  `cargo run -p mm-backtester --bin mm-stress-test --
  --scenario=ftx --config=config.toml` runs the current
  strategy config against a scenario and emits a
  standardized report (max DD, time-to-recovery, inventory
  peak, kill-switch trips, SLA breaches).

**Effort.** 2-3 weeks.

**Pre-conditions.** Per-strategy PnL attribution (still
deferred from the v0.2.0 epic — this is one of the dangling
ends).

**Unlocks.** Institutional risk story, USDC↔USDT micro-hedge
(P1.4 stage-2), options MM when that epic gets picked up.

### Epic D — Signal sophistication wave 2 (P1, ~1 month)

**Why.** The microstructure stack ships ~20 features but
several of the highest-cited production-MM signals are still
missing or only have a weaker proxy. Each one is small to
build and most have a pure-function shape that drops into the
existing `MomentumSignals` autotune path with no plumbing.

**Scope.**
- **OFI on the book event path** (Cont-Kukanov-Stoikov 2014).
  Add `OrderFlowImbalance` in `mm-strategy::features`. Sums
  signed changes in best-bid / best-ask sizes event-by-event,
  normalizes by depth. Wire as a fifth alpha component in
  `MomentumSignals` alongside book imbalance, trade flow,
  micro-price, HMA. Probably the single highest-ROI signal
  upgrade we can make. Effort: 3-5 days.
- **Learned microprice G-function** (Stoikov 2017 §3).
  Replace `micro_price_weighted` with a per-symbol learned
  lookup table over (imbalance decile × spread decile).
  Training runs offline in the backtester against recorded
  event JSONL; production path loads the table at startup
  and refreshes nightly. Effort: 1 week.
- **BVC trade classification** (Easley-Lopez de Prado 2012).
  Cheap port for the venues that don't expose `isBuyerMaker`
  reliably. Effort: 1-2 days.
- **Cartea adverse-selection closed form** in
  Avellaneda reservation. Currently we use VPIN /
  Kyle's Lambda to widen spread; the production form is the
  closed-form additive term in Cartea-Jaimungal ch.10.
  Effort: 3-5 days.

**Effort.** 3-5 weeks total.

**Pre-conditions.** None.

### Epic E — Execution infra polish (P2, ~2 weeks)

**Why.** Three small wins that improve tail latency and add a
new venue path. None of these is in the kernel-bypass cost
bracket — that one is correctly deferred.

**Scope.**
- **Batch order entry on create + cancel paths.**
  `OrderManager::execute_diff` already groups by venue; the
  dispatch just needs to accumulate same-side creates into
  one `batch_create_orders` call per venue. Bybit V5 + HL get
  full benefit (up to 20 orders/msg), Binance futures gets
  5× coalescing. Effort: 3 days.
- **io_uring runtime for the WS read path.** Move tokio's
  worker threads to `tokio-uring` for the hot WS read loop.
  Public benchmarks measure 20-40% tail-latency improvement
  on 1000-msg/s feeds. Effort: 1-2 weeks for code +
  rustls validation.
- **NUMA / IRQ / RT-kernel / hugepages deployment guide** in
  `docs/deployment.md` plus a validated systemd unit
  template. This is a deployment story, not a code change,
  but it is the highest-ROI piece because most operators do
  not know any of this. Effort: 2-3 days.
- **Coinbase Prime FIX 4.4 wiring** on top of the existing
  FIX 4.4 codec we already have in `crates/protocols/fix/`.
  No venue currently uses it. Coinbase Prime FIX is the
  cleanest latency path to Coinbase for institutional
  counterparties. Effort: 2 weeks.

**Effort.** ~2 weeks for the first three; +2 weeks for
Coinbase Prime FIX.

**Pre-conditions.** None.

### Epic F — Defensive strategy layer (P2, ~3 weeks)

**Why.** Three additive defensive features that improve
adverse-selection performance without touching the alpha
stack. All three are small, independent, and low-risk.

**Scope.**
- **Lead-lag guard.** New `mm-risk::lead_lag_guard` module.
  Subscribe to a "leader venue" mid feed (usually Binance
  Futures for same-asset pairs per the Makarov-Schoar paper),
  compute the return on a 100-500ms window, and when
  `|return| > N·σ`, push a soft-widen signal into the
  quoter's autotune path. The defensive form of latency arb
  — we cannot race HFTs but we can retreat before they hit
  us. Effort: 1 week.
- **News / sentiment retreat state machine.** Background
  task subscribes to a news feed (Kaiko, Laevitas, or just
  a Telegram/Twitter scraper for crypto-priority headlines)
  with a regex priority list. On a high-priority headline,
  flips a `news_retreat` flag that the quoter consults to
  widen or pull. Wintermute publicly discusses this as a
  first-class control. Effort: 1-2 weeks.
- **Listing sniper / probation onboard** (P2.3 stage-2).
  Background task parses each venue's "new listing"
  announcement schedule, spawns a dedicated engine for the
  new symbol a few seconds after the first trade, runs in
  probation mode (wide spreads, small size) for the first
  ~24h to capture the opening liquidity premium. P2.3
  stage-1 shipped halt + drift detection; this is the
  auto-onboard half. Effort: 2 weeks.

**Effort.** 2-3 weeks.

**Pre-conditions.** Cross-venue connector bundle (already in
place since v0.2.0 Sprint G).

### Epic-level summary

| Epic | Priority | Effort | Depends on | Unlocks |
|------|---------|--------|------------|---------|
| A — Cross-venue SOR | P1 | ~1 mo | none | ✅ stage-1 closed Apr 2026 |
| B — Stat-arb pairs | P1 | ~1 mo | Epic C (per-strat PnL) | ✅ stage-1 closed Apr 2026 |
| C — Portfolio risk view | P1 | ~3 wk | per-strat PnL | ✅ stage-1 closed Apr 2026 |
| D — Signal wave 2 | P1 | ~1 mo | none | tighter spreads on liquid pairs |
| E — Execution polish | P2 | ~2 wk | none | tail latency, Coinbase Prime |
| F — Defensive layer | P2 | ~3 wk | cross-venue bundle | adverse-selection survival |

**Total epic effort:** ~14-18 weeks if run sequentially, ~10
weeks with two engineers parallelizing on independent epics
(A+D, then B+C, then E+F).

### Explicit out-of-scope from this research pass

- **Options market making** — 1-quarter+ epic when a Deribit
  or OKX options connector lands. Tracked as future epic only.
- **Kernel bypass (DPDK, Solarflare Onload, Aquila)** — not
  ROI-positive at our public-WS / sub-$500M-day volume
  regime. Deferred until that volume threshold.
- **L3 queue position via per-order feeds** — surfaced in
  Axis 1 of the research doc but requires a Tardis
  subscription for the per-order channel; tracked as a
  follow-up to Epic D rather than as its own epic.
- **Real RL policy inference** — the existing
  `RL-driven γ / spread policy` section below stands. The
  closed-form `InventoryGammaPolicy` from v0.4.0 captures
  ~80% of the value at ~0.1% of the cost per the original
  ISAC analysis.

---

## Production-spot-MM gap closure epic (2026-04-15 research pass)

**Why it is here, not in-flight.** The 10-sprint spot-and-cross-
product epic closed at v0.2.0 with the venue × product matrix
covered (Binance spot/futures, Bybit spot/linear/inverse, HL
perp/spot) and the cross-product plumbing in place (dual
connector bundle, basis strategy, funding-arb executor, paired-
unwind, portfolio aggregator). A follow-up gap-analysis pass in
April 2026 cross-referenced the internal audit against how
production prop shops actually run spot MM (Hummingbot, Keyrock,
GSR, Flowdesk writeups, Cartea-Jaimungal-Penalva ch. 10). The
audit surfaced **eight concrete gaps** that are the difference
between "v0.2.0 demo-quality spot MM" and "production spot MM
that a prop desk would trust". They are listed below in priority
order and tracked as sub-items under one epic so the effort can
be planned as a single multi-sprint push.

**Priority order** is set by impact on captured edge and by
risk to inventory correctness. P0 items are **correctness
blockers for live spot MM** — a bot running without them is
silently leaking fills and inventory drift. P1 items each pay
for themselves in captured spread or rebate edge within a
month. P2 items are table-stakes for a paid MM agreement or
institutional report.

### P0 — live-spot-MM correctness blockers

#### P0.1 — Wire Binance listen-key user-data stream into the engine

**Gap.** `crates/exchange/binance/src/user_stream.rs` is a
complete module with listen-key lifecycle (POST to obtain, PUT
every 30 min, DELETE on shutdown), spot + futures variants via
`UserStreamProduct`, and `executionReport` /
`ORDER_TRADE_UPDATE` / `outboundAccountPosition` /
`ACCOUNT_UPDATE` parsers that emit `MarketEvent::Fill` +
`MarketEvent::BalanceUpdate`. **Nothing in `mm-server::main` or
`mm-engine::market_maker` calls `user_stream::start`.** The
engine's `handle_ws_event::BalanceUpdate` branch (line 865) is
dead code in production — the channel that feeds it is never
opened. Fills that arrive out-of-band (REST fallback, partial
fills after the WS-API envelope, manual UI orders, RFQ/OTC
trades) never reach `InventoryManager`. `reconciliation.rs::
reconcile_balances` runs every 60 s, so worst-case inventory
drift is one reconcile cycle. For a spot MM quoting tight
spreads, that is already a production-blocking gap.

**Scope.** Add `start_user_stream(...)` call to the per-symbol
run loop in `mm-server::main.rs` for Binance venues. Route the
spawned task's channel into the same `MarketEvent` stream the
public subscribe task writes to. Add Bybit V5 (wssocket private
stream) and HyperLiquid spot (post-auth WS subscriptions)
equivalents — both venues have their own lifecycle, Bybit uses
`topic: "order"` / `"wallet"` / `"execution"` on a signed WS
connection, HL uses post-handshake `{"type":"subscribe","subscription":{"type":"userEvents","user":"0x..."}}`.

**Estimated effort.** 3-5 days for Binance (module already
built, just call site + supervision + reconnect). Another 3-5
days each for Bybit and HL (new modules). Total ~2 weeks.

**Pre-conditions.** None — this is a production blocker, not
deferred.

#### P0.2 — Inventory-vs-wallet reconciliation closes drift on missed fills

**Gap.** `InventoryManager` tracks net position via
`on_fill(fill)` callbacks. Spot ground truth is the wallet, not
the fill stream. A missed fill (dropped WS message, listen-key
gap, restart) drifts `InventoryManager.inventory()` from reality
permanently. `reconciliation::reconcile_balances` fetches
wallet balances but does not compare them against
`inventory_manager.inventory()` or correct drift — it only
checks `BalanceCache` consistency.

**Scope.** Extend `reconcile_balances` to: (1) snapshot
`InventoryManager.inventory()` per asset, (2) compare against
wallet balance delta since last reconcile, (3) alert on
mismatch > tick-level noise, (4) optionally force-correct
inventory when drift > configurable threshold (manual opt-in
flag — auto-correct is scary). Add audit event
`InventoryDriftDetected` with before/after deltas.

**Estimated effort.** 3-4 days.

**Pre-conditions.** P0.1 must land first or this just catches
the symptom of P0.1 without fixing the cause.

### P1 — captured-edge gaps that pay for themselves inside a month

#### P1.1 — Amend (soft cancel-replace) preserving queue priority

**Gap.** `ExchangeConnector::amend_order` is in the trait
(`crates/exchange/core/src/connector.rs:202`), Binance futures
/ Bybit V5 / HL all implement it, **but `mm-engine` never calls
it.** `OrderManager`'s order diff is always cancel-and-place.
Every quote refresh loses queue priority. Tardis measured this
at **2–5 bps of captured spread** in tight markets — a
meaningful fraction of spread capture for a liquid pair.

**Scope.** In `OrderManager::diff_and_sync`, when the only
change on a live order is a price tweak inside `amend_epsilon`
ticks and the size is unchanged, issue `amend_order` instead of
cancel+place. Preserve queue priority on the venue. Bybit V5
supports batch amend up to 20 per message — exploit it in
`batch_amend_orders`. Binance spot has `order.cancelReplace`
which is not a true amend — leave spot on cancel-place; amend
is primarily a perp win anyway.

**Estimated effort.** 4-6 days (OrderManager logic is
non-trivial; the condition matrix for "safe to amend" has
edge cases — PostOnly violation risk, exchange-side amend
rejection semantics).

**Pre-conditions.** None.

#### P1.2 — Dynamic fee-tier + rebate-aware quoter

**Gap.** `ProductSpec.maker_fee` / `taker_fee` are populated
once from venue defaults at startup and never refreshed.
Production MMs poll `GET /sapi/v1/asset/tradeFee` (Binance),
`/v5/account/fee-rate` (Bybit), etc. on a schedule — a month-end
VIP tier crossing immediately tightens captured edge. The
current design cannot react to it until restart.

**Scope.** New `mm-exchange-core::connector::ExchangeConnector::
fetch_fee_tiers()` trait method (default
`NotSupported`). Per-venue implementations. `BalanceCache` gains
a `fee_tier_snapshot: FeeTierInfo` field refreshed every N
minutes from a background task. `PnlTracker` separates **rebate
income** from **fee paid** (maker rebate is a negative fee at
VIP 9 — conflating them kills attribution). New Prometheus
gauges `mm_maker_fee_bps`, `mm_taker_fee_bps`,
`mm_rebate_income_30d`.

**Estimated effort.** 6-8 days.

**Pre-conditions.** None.

#### P1.3 — Borrow-to-short integration for spot ask-side quoting

**Gap.** A spot MM with zero base inventory cannot quote the
ask side — the venue would reject the order as over-balance.
Production spot MMs auto-borrow via `POST /sapi/v1/margin/loan`
at quote time, auto-repay on the opposing fill, and add the
borrow rate as a carry cost to the reservation price.
`BalanceCache` has no concept of borrowed vs owned base. The
system effectively operates at half capacity on every pair
where we start flat.

**Scope.** New `mm-risk::borrow` module: per-asset borrow state
machine, min/max borrow, repayment on opposing fill. Wire to
`BasisStrategy` / `AvellanedaStoikov` reservation price as a
`borrow_cost_bps` input. Wallet topology already separates
`Margin` and `Spot` via `WalletType` — reuse. Pre-borrow at
strategy start (small buffer), repay on shutdown.

**Estimated effort.** ~2 weeks. Non-trivial because borrow
semantics differ sharply per venue and borrow rate is a moving
target.

**Pre-conditions.** P0.1 (listen-key) must land first — borrow
state reconciliation relies on fill events arriving in real
time, not via 60 s reconcile.

#### P1.4 — Cross-venue basis (spot A × perp B)

**Gap.** `BasisStrategy` + `FundingArbExecutor` are currently
same-venue only. The memory-index epic followup notes explicit
decision to defer cross-venue basis until same-venue validates.
Wintermute / GSR routinely quote BTC/USDT on Coinbase hedged on
Binance / Bybit perps — the basis-desk edge lives in cross-
venue spreads, not same-venue ones. Missing capability =
missing a whole class of trade.

**Scope.** `ConnectorBundle` already supports dual connectors,
so the plumbing exists. New `CrossVenueBasisStrategy` in
`mm-strategy` that reads hedge mid from a venue ≠ primary.
Synchronized mark-price feeds, per-venue funding PnL accrual,
**settlement-currency FX micro-hedge** (USDC↔USDT is silently
5-10 bps of leakage if ignored). Audit trail gets
`CrossVenueBasisEntered` / `Exited` events.

**Estimated effort.** ~3 weeks. The FX micro-hedge leg is the
hard part — needs a third connector for the FX leg or
periodic swap on a conversion venue.

**Pre-conditions.** P0.1 + P0.2 + P1.2.

### P2 — operational / compliance table-stakes

#### P2.1 — Per-asset-class / per-symbol kill switch tiers

**Gap.** `mm-risk::kill_switch::KillSwitch` is a global state
machine: L1 WidenSpreads → L5 Disconnect. Real MM ops want
"halt all ETH-family pairs" without touching BTC (e.g. stETH
depeg, Ronin bridge incident, single-venue outage on one
asset). Current architecture can only flip the global switch.

**Scope.** Introduce `KillSwitchMap: HashMap<AssetClass,
KillSwitch>` with `AssetClass` derived from symbol prefix or
config tag. `tick_second` per-symbol checks the asset-class
switch before the global one. New config
`kill_switch.asset_classes = [{name, symbols, limits}]`. The
hard escalation levels (L3 CancelAll, L4 FlattenAll) still
respect global state — only soft levels branch per asset class.

**Estimated effort.** ~1 week.

#### P2.2 — SLA presence tracker (per pair per minute)

**Gap.** `mm-risk::sla::SlaTracker` tracks uptime, spread
compliance, requote cadence — but at a coarse aggregation. Paid
MM agreements require **X % presence at Y bps for Z hours per
day per pair**, audited per minute. Current tracker cannot
produce that breakdown — breach auditing means rebate clawback
in a real agreement.

**Scope.** New `PerPairPresenceBucket` — 1440 per-minute
buckets per pair per day rolling. `SlaTracker` gains
`record_presence(pair, minute, spread, two_sided)`. Daily
report in `mm-dashboard::client_api::report::daily` adds a
per-pair presence table. Prometheus gauges by label.

**Estimated effort.** ~1 week.

#### P2.3 — Pair lifecycle automation (discovery + probation + halt)

**Gap.** `ProductSpec` is fetched once per connector at
startup. New listings, delistings, trading-status transitions
(PRE_TRADING, HALT, BREAK), tick/lot updates — all require a
restart to pick up. For venues listing 10+ new pairs per week
this is a manual operational burden. Halt handling is
particularly dangerous: venues sometimes send fills *after* a
halt, and the MM has no state to reject them.

**Scope.** Background task per connector polls
`get_product_spec` / `exchange_info` / equivalent every N
minutes, diffs against the in-memory map, fires events
(`PairListed`, `PairDelisted`, `PairHalted`,
`PairTickLotChanged`). A new `PairLifecycleManager` in
`mm-engine` owns the map and routes events: halt → cancel +
pause per symbol, listing → optional auto-onboard in
probation mode (wider spreads, smaller size, 7-day
observation window).

**Estimated effort.** ~1.5 weeks.

#### P2.4 — Full PnL attribution (spread / inventory / rebate / funding / borrow / FX)

**Gap.** `mm-risk::pnl::PnlTracker` currently tracks `spread /
inventory / rebate / fees`. Missing:
- **Funding PnL** (perp funding payments accrued per 8h)
- **Borrow cost** (margin interest, once P1.3 lands)
- **FX leg PnL** (once P1.4 lands — USDC/USDT slippage)
- **Per-venue / per-pair breakdown** (currently just global)

Without this breakdown you cannot tell a winning strategy from
a losing one carried by rebates.

**Scope.** Extend `PnlAttribution` with `funding_pnl`,
`borrow_cost`, `fx_pnl`. Per-venue / per-pair labels on
existing Prometheus gauges. Daily report section for PnL
attribution pie chart.

**Estimated effort.** ~1 week.

**Pre-conditions.** P1.2 (rebate separation), P1.3 (borrow),
P1.4 (FX) for the new fields to have inputs.

### Rejected as out-of-scope (documented so we do not re-run the analysis)

- **Treasury ops** — spot↔margin↔futures transfers, on-chain
  withdrawals/deposits, cross-venue sweeps. Not a quoter's job;
  lives in a separate "treasury service" process.
- **Options market making** — `VenueProduct::Option` is
  reserved but the surface (IV, vega, delta hedging, portfolio
  margin) is a different product. Separate epic when we add a
  Deribit / Okx options connector.
- **Dust consolidation / fee-token burn loops** — scheduled
  jobs that live in the treasury layer, not here.
- **On-chain MM / DEX integration** — different latency model
  and execution paradigm.

### Epic-level scope summary

| Item | Priority | Effort | Blocks |
|------|---------|--------|--------|
| P0.1 listen-key wiring | P0 | 2 weeks | P0.2, P1.3 |
| P0.2 inventory reconcile | P0 | 3-4 days | — |
| P1.1 amend order diff | P1 | 4-6 days | — |
| P1.2 fee tier + rebate | P1 | 6-8 days | P2.4 |
| P1.3 borrow-to-short | P1 | ~2 weeks | P2.4 |
| P1.4 cross-venue basis | P1 | ~3 weeks | P2.4 |
| P2.1 per-asset-class kill | P2 | ~1 week | — |
| P2.2 SLA presence bucket | P2 | ~1 week | — |
| P2.3 pair lifecycle | P2 | ~1.5 weeks | — |
| P2.4 full PnL attribution | P2 | ~1 week | P1.2 + P1.3 + P1.4 |

**Total epic effort:** ~12-14 weeks if run sequentially,
~8-10 weeks if P1 items are parallelised by multiple engineers.

---

## RL-driven γ / spread policy (SAC, PyTorch-trained)

**Source of inspiration.** `github.com/im1235/ISAC` — Soft Actor
Critic agent that learns a γ-control policy on an
Avellaneda-Stoikov inventory strategy, trained on 2000 simulated
price paths. Also the RL-MM cluster of papers tracked in
`github.com/baobach/hft_papers` (Oct–Nov 2025): RL-based market
making, adverse-selection-aware meta-order MM, DRL on orderbook
imbalance, etc.

**What we already took from ISAC.** The closed-form approximation
of the state surface the SAC agent converges to, landed in this
commit cycle:

- `mm_strategy::autotune::InventoryGammaPolicy` — analytical
  `γ_mult(|q|, t_remaining)` with the same shape SAC produces after
  ~2000 training paths, no GPU / training loop.
- `mm_strategy::autotune::inventory_risk_penalty` — the
  `0.5·|q|·σ·√dt` mean-variance charge used by the ISAC reward
  function.

This gets us ~80 % of ISAC's value for ~0.1 % of the engineering
cost. The remaining gap is that our policy surface is *fixed* —
it can't adapt to a regime the authors didn't manually encode.

**Why we are not doing the full port now.**

- A real Rust SAC port needs: a tensor crate (`candle` / `burn` /
  `tch`), replay buffer, actor/critic nets, training loop, reward
  shaping, exploration schedule, evaluation harness, distribution-
  shift monitoring, weight checkpointing. Realistic scope:
  **3–6 months**, not a cherry-pick.
- RL market making in production is notoriously brittle: online
  distribution shift, reward hacking, exploration cost paid in
  real PnL. The closed-form path ships.
- The ISAC paper's contribution to us is the *state and reward
  formulation*, not the learner. Both are already encoded.

**Minimum-risk path (if we decide to pick this up).** Do *not*
port the learner to Rust. Instead:

1. Train offline in Python (PyTorch / Stable-Baselines3 / CleanRL)
   against replays from `mm_backtester`. Our existing JSONL event
   recorder already produces the training data.
2. Export policy weights to ONNX or safetensors.
3. Add an inference-only crate — `mm-strategy-rl` — built on
   `candle` or `burn`. No training in Rust. The crate loads the
   exported weights and exposes a `policy(state) -> action` fn.
4. Wire the action through the same `AutoTuner::update_policy_state`
   channel `InventoryGammaPolicy` uses today, so RL is just
   "another policy" that can be A/B-compared against the closed-
   form one behind a config flag.

Estimated spike size via this path: **2–3 weeks**, not 3–6 months.
The tradeoff is that weights are frozen between training runs —
no online learning, no exploration in prod. Which is the *point*:
it keeps RL out of the hot path and out of the PnL attribution
story.

**Pre-conditions for picking this up.**

- Closed-form `InventoryGammaPolicy` is running in live and we can
  point to a systematic error it makes that RL could plausibly
  fix. Without a documented failure mode, there is no baseline to
  beat.
- All current open debts landed: dependabot alerts cleared, the
  3 deferrals from the spot-cross epic closed, reconciliation
  + audit trail stable for at least one month of live trading.
- At least one full month of backtester replay data captured on
  a representative set of symbols (training set).

**Open questions to answer before spiking.**

- State: do we feed the full `features/` vector (imbalance, trade
  flow, micro-price, VPIN, Kyle's Lambda, realized vol) or a
  projected subset? Larger state → slower convergence, richer
  policy.
- Action: does RL output a γ multiplier (drop-in replacement for
  `InventoryGammaPolicy::multiplier`) or a full `(bid_spread,
  ask_spread, size)` triple? The former composes cleanly with
  the regime / toxicity multipliers we already have; the latter
  is more powerful but bypasses the safety net.
- Reward: raw PnL, or PnL − `inventory_risk_penalty`, or
  Sharpe-like with running variance? ISAC uses penalty-subtracted
  reward; we should start there.
- Evaluation: which loss in `mm_hyperopt::loss_fn` do we grade
  RL variants against? Needs to match what we already use to
  tune the closed-form policy, or comparisons become apples-to-
  oranges.
