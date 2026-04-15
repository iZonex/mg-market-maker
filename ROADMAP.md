# Roadmap

Ideas that are not in-flight and not part of an open epic, but we
have explicitly decided are worth revisiting. Each entry records the
motivation, the minimum-risk path, the estimated scope, and the
pre-conditions under which it becomes worth picking up.

This is not a commitment list. Things land here when we have
decided *not* to do them now and want to avoid re-running the same
analysis later.

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
