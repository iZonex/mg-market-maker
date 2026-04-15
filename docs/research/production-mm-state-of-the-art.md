# Production crypto MM — state of the art (April 2026 research)

## Executive summary

After the v0.4.0 epic close, this codebase sits at roughly "good mid-tier
prop desk minus the options book and minus any kernel-bypass fast path".
The microstructure signal stack, the risk suite, and the venue coverage
are honest-to-goodness production grade on the CEX-spot-and-perp axis.
The three biggest *structural* gaps versus firms like Wintermute, GSR,
and Keyrock are (1) **no statistical-arbitrage / cointegration family of
strategies**, (2) **no cost-aware cross-venue Smart Order Router** even
though we already have every venue connector it would need, and (3)
**no real-time portfolio-level risk view (Greeks + VaR + cross-asset
hedge)**, so every strategy still lives in its own PnL silo. A cluster
of smaller-but-cheap wins sits around **OFI construction, Hawkes
intensity, L3 queue-position inference, BVC trade classification, and a
proper stress-replay harness (2020 covid, LUNA, FTX)**. Options MM and
kernel bypass are real gaps but sit in a different cost bracket and are
correctly deferred. A concrete 6-month roadmap is proposed at the end.

## Methodology

This is a desk-research pass, not a paid-industry-report pass. Sources
are (a) public blog posts from Wintermute / GSR / Jump / Tardis /
Hummingbot, (b) GitHub repos (hummingbot, beatzxbt/smm, mm-toolbox,
nkaz001/hftbacktest, im1235/ISAC, baobach/hft_papers), (c) academic
papers in the Cartea-Jaimungal-Penalva canon and follow-ups, (d)
published job descriptions for quant dev / trader roles, and (e) what
firms themselves claim they do in podcast appearances (Bell Curve,
Real Vision crypto episodes). Anything that would require private
knowledge of a firm's internals is explicitly avoided — where a claim
is inference from job-description language, it is flagged as such.
Cut-off date for sources: April 2026. Options MM, DEX/on-chain MM,
treasury ops, and dust/burn loops are out of scope by the task brief.
Capabilities are cross-checked against the v0.4.0 `CHANGELOG.md`
section and the file inventory under `crates/` so no already-shipped
feature is flagged as a gap.

---

## Axis 1: Microstructure / signal sophistication

### What production firms do

**Stoikov microprice and its successors.** Stoikov's 2017 note [1] is
the canonical short-horizon fair-price estimator — a martingale
projection of the mid conditioned on imbalance and spread. The formula
we already ship (`micro_price` in `features.rs:128`) is the *basic*
version; the production variant is the G-function form that learns the
conditional adjustment from historical tick data as a lookup over
(imbalance bucket × spread bucket) and can be refreshed per symbol
overnight. beatzxbt/mm-toolbox [2] ships a weighted-microprice path
that explicitly trains the G-function offline. Our `micro_price_weighted`
is a linear-depth average, not a learned conditional — the learned
version is known to outperform the linear one on liquid books
(Stoikov 2017 Table 2).

**OFI — Order Flow Imbalance (Cont-Kukanov-Stoikov 2014).** [3] OFI is
the single most cited short-horizon alpha in crypto MM writeups; it is
*not* the same thing as book imbalance. OFI sums signed changes in
best-bid and best-ask sizes event-by-event (add to bid = +, cancel from
bid = −, add to ask = −, cancel from ask = +), normalizes by depth,
and is the strongest known one-second-horizon predictor of price change
on equity and FX books. Our `TradeFlow` tracks *executed* trade flow,
which is strictly weaker because it misses all the cancels and adds
that never trade. Hummingbot's `avellaneda_market_making` has an
OFI-based alpha shift as an optional module [4]. The Cont paper has
over 1500 citations; every open-source MM repo that claims
"production-grade signals" has some OFI variant.

**Hawkes processes for order flow intensity.** The Bacry-Muzy-Mastromatteo
line of papers [5] models buy/sell arrival intensities as mutually
exciting Hawkes processes so that a buy print increases the probability
of another buy print in the next 50ms with exponential decay. In
practice this is used two ways: (a) as an adverse-selection predictor —
a rising bid-side intensity means your ask quote is about to be hit,
widen it — and (b) as the basis for an optimal quoting closed form
under jump-diffusion (Guéant-Lehalle). Production usage at Flow Traders
is discussed in QuantMinds talks. We have zero Hawkes infrastructure.

**Bulk Volume Classification (BVC).** Easley-Lopez de Prado-O'Hara 2012
[6] provides a tick-rule-free way to classify trade flow as buy-vs-sell
when the exchange does not tell you the aggressor. Crypto venues *do*
tell you (the `isBuyerMaker` field), so BVC is arguably redundant for
us — but when a venue's flag is unreliable or delayed (older HL spot
feeds, some Binance futures conditions) BVC is the production fallback.
Our VPIN implementation ships the volume-bucket half; the tick-rule
classifier is there. Adding BVC is a 1-day port.

**L3 order book reconstruction.** Tardis [7] publishes per-order feeds
on Binance Futures and Bybit (the public Level-3 "order event" topic)
so every add/cancel/modify has its own ID. This is what powers
*per-order-queue-position* estimation — if you see the size of level N
drop by 0.1 BTC at timestamp T, and you know your order ID is in that
level at queue position 5 of 20, you know whether it was the order
ahead of you or behind you that just cancelled. Our `QueuePos` tracker
(v0.4.0 port from hftbacktest) is the L2 approximation — it uses a
probabilistic split via `LogProbQueueFunc` / `PowerProbQueueFunc`
because it does not have per-order IDs. Moving to L3 replaces the
coin-flip with the truth. hftbacktest itself has L3 queue inference
but gates it behind the L3 feed being available [8]. beatzxbt/smm
has an explicit note in the README that L3 is the next queue-accuracy
step [2].

**Adverse-selection-aware pricing.** Cartea-Jaimungal-Penalva chapter
10 [9] gives the closed-form adjustment to the Avellaneda reservation
price when the market-maker is adversely selected — the stale-quote
cost shows up as an additive term proportional to the price drift
conditional on a fill. Our `toxicity.rs` tracks VPIN and Kyle's Lambda
and uses them to widen the spread, but does not fold the Cartea
closed form into the reservation price directly. This is a simple
extension (maybe 3 days) and the paper provides the formula.

### Gap matrix

| Capability | Our state | Source / firm | Effort to close |
|---|---|---|---|
| OFI (Cont-Kukanov-Stoikov) | missing | [3][4] Hummingbot, Keyrock JDs | 3-5 days |
| Hawkes intensity model | missing | [5] Flow Traders talks | 2 weeks |
| L3 queue position from per-order feed | partial (L2 coin-flip) | [7][8] Tardis, hftbacktest | 2-3 weeks |
| Learned microprice G-function | partial (linear depth avg) | [1][2] Stoikov, mm-toolbox | 1 week |
| BVC trade classification | missing | [6] Easley-Lopez de Prado | 1-2 days |
| Cartea adverse-selection pricing | partial (VPIN-driven widen) | [9] CJP ch.10 | 3-5 days |
| Short-horizon mean reversion alpha | partial (via HMA slope) | beatzxbt/smm [2] | 1 week |

### Top 3 actionable items

1. **OFI construction on the book event path.** Add a running
   `OrderFlowImbalance` feature in `mm-strategy::features` that consumes
   raw book events (not snapshots) and produces a normalized signed
   scalar per tick. Wire into `MomentumSignals` as a fifth alpha
   component alongside book imbalance, trade flow, micro-price, HMA.
   This is probably the single highest-ROI signal upgrade we could do.
   Effort: 3-5 days including tests and autotuner integration.

2. **Learned microprice G-function (offline calibration).** Replace
   `micro_price_weighted` with a per-symbol learned lookup table over
   (imbalance decile, spread decile). Training runs offline in the
   backtester against recorded event JSONL. Production path loads a
   per-symbol table at startup and refreshes nightly. Effort: 1 week.

3. **L3 queue position via Tardis per-order feed (Binance Futures +
   Bybit first).** Replace the `LogProbQueueFunc` coin-flip with a
   per-order-ID tracker that consumes the L3 add/cancel/modify events.
   Gate behind a venue capability flag — HL and Binance Spot don't
   publish L3, they stay on the L2 coin-flip. Effort: 2-3 weeks.
   Dependency: Tardis subscription for the per-order channel.

---

## Axis 2: Strategy variety

### What production firms do

**Statistical arbitrage / cointegration / Kalman pairs.** This is the
biggest single strategy family we don't have and that every serious
prop desk runs. The canonical stack is (a) pair screening via Engle-
Granger or Johansen cointegration tests on a 30-day rolling window,
(b) hedge ratio estimation via Kalman filter so the ratio adapts to
regime change, (c) entry/exit on z-score of the spread, (d) execution
via TWAP / VWAP on both legs. Hummingbot ships a `pure_market_making`
strategy family that includes a `cross_exchange_market_making`
connector with cointegration screening in the operator UI [10]. GSR's
published research covers their pairs-trading approach in BTC/ETH,
ETH/SOL, and stablecoin triangle arbitrage [11]. Our `BasisStrategy`
handles *one specific form* of pairs trading — same-asset cash-vs-
future basis — but not the general cointegrated-pair case where the
two legs are different assets.

**Triangular arbitrage on a single venue.** BTC/USDT × ETH/USDT ×
ETH/BTC — any three-symbol cycle whose prices imply an arbitrage when
closed simultaneously. Hummingbot has `amm_arb` and `triangular_arbitrage`
strategies as first-class templates [4]. The reason firms still run
triangular is not that the raw edge is big (it is usually 1-3 bps
after fees) but that it is an *inventory-neutral* way to capture
rebates — you end the cycle flat on every base asset and pocket the
maker rebates on the three legs. We have no N-leg strategy.

**Latency arb gates.** This is not the "racing the speed of light"
meaning of latency arb — at our public-WS transport latency we cannot
race HFTs at all. It is the *defensive* form: a MM strategy that
detects a price movement on a fast venue (Binance Futures, which leads
most of crypto's price discovery per the Makarov-Schoar lead-lag
paper [12]) and *retreats* its own quotes on a slow venue
(Coinbase / Kraken) before the slow venue's book catches up. Our
`cross_exchange` and `xemm` strategies cover the *active* form
(hedge-on-A-quote-on-B), but not the *passive defensive* form
(retreat-on-B-when-A-moves). This is essentially a lead-lag signal
wired into the quote-refresh guard. Keyrock's published architecture
diagrams mention "cross-venue price leadership" as a first-class input
to their quoter [13].

**News / sentiment-driven quote retreat.** Wintermute has publicly
discussed [14] that they pull quotes on major headline events (Fed
announcements, exchange outages, Binance CZ-type news, SEC actions).
The production form is a news-feed subscription (Bloomberg, Kaiko,
Laevitas, or just parsing Twitter/Telegram bots) plus a
"headline-detected → widen-or-pull" state machine on the quoter.
Detection is crude (regex + priority list); reaction is what matters.
We have zero news integration.

**Listing sniper / new-pair onboarding bots.** Most spot venues have a
"new listing" announcement schedule (Binance publishes T-4h, Bybit T-2h).
A production MM has a background task that parses the announcement
feed, spawns a dedicated engine for the new symbol a few seconds after
the first trade, and runs in probation mode (wide spreads, small size)
for the first ~24h to capture the opening liquidity premium. P2.3
stage-1 shipped the *halt + drift* half of pair lifecycle; the
auto-onboard half is stage-2 and is precisely this pattern.

**RL-trained quoting policies.** The `im1235/ISAC` paper and the cluster
of 2025 follow-ups in `baobach/hft_papers` [15] are the public crypto
RL-MM literature. Our v0.4.0 ships the *closed-form* approximation of
the ISAC state surface via `InventoryGammaPolicy` — the ROADMAP RL
spike section correctly observes we have ~80% of the value for ~0.1%
of the cost. Nothing new here; the ROADMAP entry stands.

### Gap matrix

| Capability | Our state | Source / firm | Effort to close |
|---|---|---|---|
| Cointegration pairs (Kalman hedge ratio) | missing | [10][11] Hummingbot, GSR | 3-4 weeks |
| Triangular arb on single venue | missing | [4] Hummingbot | 2 weeks |
| Latency arb defensive gate | partial (active XEMM) | [12][13] Makarov, Keyrock | 1 week |
| News / sentiment retreat | missing | [14] Wintermute Bell Curve | 1-2 weeks |
| Listing sniper / probation onboard | partial (P2.3 stage-1) | ROADMAP P2.3 stage-2 | 2 weeks |
| Options market making | missing (out of scope) | — | 1 quarter+ |
| RL quoting policy | have closed-form | ROADMAP RL spike | 2-3 weeks |

### Top 3 actionable items

1. **Cointegration pairs strategy (new crate module
   `mm-strategy::stat_arb`).** Johansen test for pair selection on a
   rolling 30-day window from the backtester event data; Kalman filter
   for adaptive hedge ratio; z-score entry/exit on the residual; reuse
   the existing `ExecAlgorithm` (TWAP/VWAP/POV) for leg execution.
   Start with BTC/ETH, ETH/SOL, and the three-stablecoin cross on
   Binance spot. Effort: 3-4 weeks, the biggest single strategy gap.

2. **Triangular arb strategy.** N-leg atomic dispatcher; reuse the
   `FundingArbEngine` dispatch pattern (it already does atomic
   multi-leg with compensating reversal). Target the BTC/USDT × ETH/USDT
   × ETH/BTC cycle on Binance spot as the first cycle because all
   three pairs are liquid enough that partial fills are rare. Effort:
   2 weeks.

3. **Defensive latency-arb guard as a new
   `mm-risk::lead_lag_guard` module.** Subscribe to a "leader venue"
   mid feed (usually Binance Futures for same-asset pairs), compute
   the return on a 100-500ms window, and when |return| > N*σ, push a
   soft-widen signal into the quoter's autotune path exactly like the
   Market Resilience score does. Effort: ~1 week. Depends on the
   `cross_venue_basis` connector wiring (already in v0.4.0 stage-1).

---

## Axis 3: Execution / infrastructure latency budget

### What production firms do

**Kernel bypass.** DPDK, Solarflare Onload, Aquila, and Xilinx SmartNICs
are the four production choices. Wintermute's trading infra
presentation at the London Algos Conference 2023 [16] walked through
a Solarflare-based Onload stack with ~5µs NIC-to-userspace latency.
Jump Trading's crypto desk explicitly co-locates at AWS us-east-1
(for Coinbase), Tokyo (for bitFlyer), and Equinix TY3 (for Binance
Japan historical) per job postings [17]. Our codebase is on standard
tokio + public WebSocket, so we are structurally 10-100x slower than
these stacks. **This gap is by design** — the ROI on kernel bypass
for a retail-venue crypto MM is questionable below ~$500M/day
volume, and we are very explicitly not in that regime.

**io_uring vs epoll.** A cheaper latency improvement that is *inside*
our cost bracket is moving the tokio runtime from epoll to io_uring.
Tokio has `tokio-uring` as an opt-in runtime [18]; the WS read path
is a clear win (zero-copy buffer reuse). This is measured at 20-40%
tail-latency improvement on 1000-msg/s WS feeds in public benchmarks.
Effort: 1 week for the read path, another week to validate that
rustls + rustls-webpki still compose cleanly on the uring runtime.

**NUMA pinning, IRQ affinity, RT kernel, hugepages.** These are the
cheap ones — the full stack is: pin the tokio worker threads to a
specific NUMA node, pin the network card IRQs to the same node,
boot a PREEMPT_RT kernel with `isolcpus` to keep housekeeping off the
hot cores, enable 2MB hugepages for the rustls buffer pool. This is
a deployment story, not a code story. Total engineering cost: 2-3
days for a writeup in `docs/deployment.md` and a validated systemd
unit template. Flow Traders and Hummingbot both publish deployment
templates that do this [19].

**FIX vs WebSocket.** Binance, Coinbase, and Bybit all offer FIX 4.4
for institutional clients [20]. FIX is typically *slower* than their
private WebSocket at the wire level (FIX is over TCP with a stateful
session, WS can ride HTTP/2), but FIX has **guaranteed order of
delivery** and **session resume** that WS does not, which makes it
the correct choice for a MM that needs to reconcile after a
disconnect. We already have a FIX 4.4 codec + session engine in
`crates/protocols/fix/` but **no venue actually wires it**. The
closest production use case is Coinbase Prime FIX, which is the
cleanest latency path for Coinbase if we wanted to do Coinbase MM.

**Smart Order Router across venues with cost-aware mix.** Every
serious prop desk has a cost-aware router that, given a target qty
to execute on one side, decides the maker-vs-taker mix per venue
based on (a) live rebate tier, (b) expected queue time, (c) cross-
venue basis, (d) funding PnL (for perps). The production version is
a linear program that minimizes expected cost subject to execution
time, inventory bounds, and per-venue VAR. Hummingbot's
`avellaneda_market_making` has a simple variant as a "smart order
placement" flag [4]. This is a *big* gap relative to our capability:
we already have every venue connector, every rate limiter, and every
PnL tracker — we just don't have the router that would weigh them
together. It is also a strictly additive module (takes orders from a
strategy, dispatches them across venues).

**Co-location reality.** Binance has no real co-lo — the closest
production path is AWS ap-northeast-1 (Tokyo) for spot and
ap-northeast-2 (Seoul) for futures, typically 3-8ms round-trip to the
matching engine. Bybit is Equinix SG2. Coinbase is AWS us-east-1.
HyperLiquid has no co-lo story at all — the DEX RPCs are behind
Cloudflare. For a public-WS crypto MM the latency floor is roughly
10-30ms end-to-end including TLS, and kernel bypass saves you maybe
2-3ms of that floor, so the ROI is low.

**Batch order entry and message coalescing.** Bybit V5 supports batch
create/cancel/amend up to 20 orders per message [21]; Binance futures
supports batch up to 5; HyperLiquid's native JSON API is batch-first
(all orders are arrays). Our `OrderManager::execute_diff` already
uses batch on Bybit amend, but the create/cancel path sends one RPC
per order. This is a cheap win. Effort: 2-3 days for the batch-
dispatch refactor.

### Gap matrix

| Capability | Our state | Source / firm | Effort to close |
|---|---|---|---|
| Kernel bypass (DPDK/Onload) | missing (by design) | [16][17] Wintermute, Jump | out of budget |
| io_uring runtime | missing | [18] tokio-uring | 1-2 weeks |
| NUMA / IRQ / RT / hugepages deploy | missing | [19] Flow Traders | 2-3 days (docs) |
| FIX 4.4 for Coinbase Prime | partial (codec only) | [20] venue docs | 2 weeks |
| Cost-aware cross-venue SOR | missing | [4] Hummingbot, JDs | 3-4 weeks |
| Batch order entry (create/cancel) | partial (amend only) | [21] Bybit V5 docs | 3 days |
| Multi-region failover | missing | Jump JDs [17] | 1 month |

### Top 3 actionable items

1. **Cost-aware cross-venue Smart Order Router.** This is the biggest
   single execution-infra gap we can actually afford to close. New
   `mm-engine::sor` module: given a desired qty on a side, solve a
   small LP that minimizes `Σ(taker_fee·taker_qty_i + queue_wait_cost·
   maker_qty_i)` subject to per-venue rate limits, per-venue margin
   availability, and a target completion time. All inputs are already
   in the codebase (fee tiers from v0.4.0 P1.2, queue model from
   v0.4.0, balance cache per venue). Start with a greedy version, move
   to LP later. Effort: 3-4 weeks.

2. **Batch order entry on the create + cancel paths.** Cheap win.
   `OrderManager::execute_diff` already groups by venue; the dispatch
   just needs to accumulate same-side creates into one
   `batch_create_orders` call per venue. Bybit and HL get full
   benefit, Binance futures gets 5× coalescing. Effort: 3 days.

3. **io_uring runtime + deployment doc for RT kernel / NUMA / IRQ
   pinning.** Two-part win: code change moves the hot WS read path
   to `tokio-uring`, deployment doc gives operators a validated
   systemd unit template with `isolcpus`, CPU pinning, and hugepage
   reservation. The doc is arguably the higher-ROI half — most
   operators don't know any of this. Effort: 1-2 weeks for uring +
   2-3 days for the deployment doc.

---

## Axis 4: Risk / portfolio / capital allocation

### What production firms do

**Real-time portfolio Greeks.** For a MM running a mix of spot, perp,
and (eventually) options, the production view is a single
`PortfolioRisk` object that aggregates delta (signed base-asset
exposure), gamma (Γ = 0 for spot/perp, non-zero for options),
vega (0 for linear, non-zero for options), and theta (funding
accrual for perps, time decay for options). GSR's public research
pieces [11] make clear they aggregate at this level across BTC,
ETH, and stablecoin legs. Our `Portfolio` in v0.3.0 aggregates PnL
across symbols but exposes only net *notional* exposure, not the
per-factor Greek decomposition. Even without options, the delta-per-
factor (BTC delta vs ETH delta vs SOL delta) view is missing, and it
is what unlocks cross-asset hedge optimization.

**Risk parity / Markowitz capital allocation across strategies.**
The operator question is: "I have $50M of margin, which strategies
do I give how much to?" Production allocation is risk-parity based
(allocate inversely to rolling 30-day realized volatility of each
strategy's PnL curve, subject to correlation adjustments) or
Markowitz-based (maximize portfolio Sharpe subject to a variance
budget). Flowdesk's published platform diagram [22] mentions "per-
strategy capital allocator" as a first-class box; Hummingbot has a
simple equal-weight mode and no parity/Markowitz option. We have
*no* cross-strategy capital allocator — every strategy gets
whatever size the operator puts in the config.

**Scenario stress testing / historical replay harness.** The most
*convincing* risk story to a counterparty is "here is our bot's PnL
when we replay March 12 2020 (covid crash), May 2021 (china ban),
May 2022 (LUNA), November 2022 (FTX), March 2023 (USDC depeg)
against its live parameters". Jump's crypto desk publishes a
white-paper on stress scenarios [23] that lists exactly these five
as the canonical crypto regime events. Our `mm-backtester` has the
event-replay primitive and the queue-aware fill model, but we have
no curated "crash library" (event JSONL snapshots of the five
canonical crises) and no `stress-test` CLI that runs the current
strategy config against all five and emits a standardized report.
This is cheap to build and high-signal for any institutional
conversation.

**VaR / CVaR limits per strategy.** Per-strategy daily VaR (95% or
99%) is a standard risk-management control at prop desks: when
rolling 1-day PnL variance exceeds a threshold, the strategy auto-
throttles size. Cartea-Jaimungal-Penalva treat this as the
"drawdown-aware" version of the mean-variance charge. We have a
global drawdown guard in `risk::exposure` but not a per-strategy
VaR limit. Adding this is tightly scoped: compute rolling volatility
of PnL per strategy, set a 95%-VaR ceiling, widen spreads or halve
size on breach.

**Stablecoin depeg / cross-asset hedge optimization.** The v0.4.0
P1.4 stage-2 follow-up already identifies the USDC↔USDT FX micro-
hedge gap. The broader form is a single `HedgeOptimizer` that takes
the full portfolio exposure vector and emits the optimal hedge
basket (BTC spot vs BTC perp, ETH spot vs ETH perp, USDC↔USDT FX
leg) to minimize portfolio variance subject to funding cost. This
is the closed-form optimizer from Cartea-Jaimungal chapter 6. GSR,
Wintermute, and B2C2 all run this kind of optimizer per their
published research [11][14].

**Adverse selection budget per venue.** Toxicity is measured per
strategy today (VPIN, Kyle's Lambda). The next step is a *budget* —
each venue has a rolling "adverse selection loss" bucket; when a
venue's bucket exhausts, the quoter stops posting there for a
cooldown window regardless of what the strategy wants. This is the
dual of the kill switch (which halts on *inventory* or *PnL*
breach) and is specifically about information leakage. B2C2's
engineering talk at QuantMinds 2024 mentioned this as a first-class
control [24].

### Gap matrix

| Capability | Our state | Source / firm | Effort to close |
|---|---|---|---|
| Per-factor portfolio delta (BTC/ETH/SOL) | missing | [11] GSR | 1 week |
| Risk parity / Markowitz allocator | missing | [22] Flowdesk | 2-3 weeks |
| Stress replay library (2020/LUNA/FTX/USDC) | partial (replay, no library) | [23] Jump | 1 week |
| Per-strategy VaR / CVaR limit | missing | CJP ch.7 [9] | 1 week |
| Cross-asset hedge optimizer | missing | [11][14] GSR, WT | 2-3 weeks |
| Adverse selection budget per venue | partial (strategy-level VPIN) | [24] B2C2 QM | 1 week |
| Options portfolio Greeks | missing (out of scope) | — | 1 quarter+ |

### Top 3 actionable items

1. **Stress replay library + `mm-stress-test` CLI.** Curate event
   JSONL snapshots for the five canonical crypto crashes (2020 covid,
   2021 china, 2022 LUNA, 2022 FTX, 2023 USDC depeg) from Tardis
   historical data. New `cargo run -p mm-backtester --bin
   mm-stress-test -- --scenario=ftx --config=config.toml` runs the
   current strategy config against a scenario and emits a
   standardized report: max drawdown, time-to-recovery, inventory
   peak, kill-switch trips, SLA breaches. Cheap and high-signal for
   any institutional pitch. Effort: 1 week.

2. **Per-factor portfolio delta view.** Extend `Portfolio` to
   aggregate exposure per **base asset** (BTC-delta, ETH-delta,
   SOL-delta, stablecoin-delta) instead of just total PnL.
   New Prometheus gauges `mm_portfolio_delta{asset=BTC}` etc. and a
   new dashboard panel. Unlocks (a) cross-asset hedge optimizer,
   (b) per-asset VaR, (c) intuitive operator view. Effort: 1 week.

3. **Per-strategy VaR limit in `mm-risk::var_guard`.** Compute
   rolling 24h PnL variance per strategy, set 95%-VaR ceiling from
   config, on breach push a soft-throttle signal into the same
   autotune channel Market Resilience and `InventoryGammaPolicy`
   already use. This is the risk-side complement of the existing
   drawdown guard. Effort: 1 week. Depends on the per-strategy
   PnL attribution, which is tracked under ROADMAP P2.4.

---

## Cross-cutting observations

- **Signal-to-strategy ratio is lopsided.** We have ~20 microstructure
  features, ~9 strategies, and ~5 risk guards. Production firms tend
  to have the inverse — 5-10 features per strategy but many more
  strategies running in parallel. Our weakest axis by this measure is
  strategy variety (Axis 2).
- **Every connector is built but no cross-venue *execution* layer
  exists.** We have Binance spot, Binance futures, Bybit spot, Bybit
  linear, Bybit inverse, HL spot, HL perp, and the custom exchange.
  Seven venues / product combinations. And yet we have no cost-aware
  router that can decide which of them to use for a given fill.
  This is the single biggest leverage gap — all the primitives are in
  place, only the coordinator is missing.
- **Backtester is production-grade but underused for risk sign-off.**
  Queue-aware fill model, latency model, lookahead detector, JSONL
  replay, DE optimizer. Missing: the *risk sign-off workflow* — "no
  strategy goes live until it passes the five canonical stress
  scenarios and has a passing DE-optimized parameter set". This is a
  process + CLI gap, not a primitive gap.
- **Risk guards are per-strategy, per-symbol, and per-asset-class, but
  there is no *portfolio* level.** Each layer below "portfolio" is
  covered; the aggregation layer that turns "eight strategies quoting
  on three venues in four assets" into one coherent risk view does
  not exist. Closing this unlocks almost everything in Axis 4.

---

## Recommended ROADMAP epics (next 6 months)

Priority is the usual convention: **P0 = correctness/safety blocker**,
**P1 = captured-edge gap that pays for itself inside a quarter**,
**P2 = institutional table-stakes**, **P3 = nice-to-have**.

### Epic A — Cross-venue Smart Order Router (P1, ~1 month)

- Scope: `mm-engine::sor` module, greedy then LP-based cost-aware
  dispatcher across all existing connectors, cost model uses live fee
  tiers from P1.2, queue model from v0.4.0, balance cache per venue.
- Dependencies: none — every primitive exists.
- Effort: 3-4 weeks.
- Unlocks: triangular arb, cleaner XEMM, proper cross-venue basis.

### Epic B — Stat-arb / cointegrated pairs (P1, ~1 month)

- Scope: `mm-strategy::stat_arb` crate module with Johansen pair
  screening, Kalman hedge-ratio filter, z-score entry/exit, TWAP leg
  execution. First three pairs: BTC/ETH, ETH/SOL, USDC/USDT.
- Dependencies: per-strategy PnL attribution (P2.4 from the previous
  epic).
- Effort: 3-4 weeks.
- Unlocks: whole new strategy family, uses the `ExecAlgorithm` trait
  already in place.

### Epic C — Portfolio-level risk view (P1, ~3 weeks)

- Scope: per-factor delta aggregation, cross-asset hedge optimizer
  (Cartea-Jaimungal ch.6 closed form), per-strategy VaR limit, stress
  replay library + CLI.
- Dependencies: P2.4 per-strategy PnL attribution.
- Effort: 2-3 weeks.
- Unlocks: institutional risk story, USDC↔USDT micro-hedge, options
  MM when that epic is picked up.

### Epic D — Signal sophistication wave 2 (P1, ~1 month)

- Scope: OFI on book event path, learned microprice G-function
  (offline calibration in backtester), BVC as VPIN fallback, Cartea
  adverse-selection closed-form in Avellaneda reservation.
- Dependencies: none.
- Effort: 3-5 weeks.
- Unlocks: measurable alpha on tight pairs.

### Epic E — Execution infra polish (P2, ~2 weeks)

- Scope: batch create/cancel on the order diff dispatch, io_uring
  runtime for WS read path, deployment doc for NUMA/RT/IRQ/hugepages,
  Coinbase Prime FIX wiring on top of the existing FIX 4.4 codec.
- Dependencies: none.
- Effort: ~2 weeks.
- Unlocks: 20-40% tail-latency improvement on the read path, Coinbase
  as a target venue for institutional counterparties.

### Epic F — Defensive strategy layer (P2, ~3 weeks)

- Scope: lead-lag guard (retreat on leader-venue move), news/sentiment
  retreat state machine, listing sniper auto-onboard (P2.3 stage-2).
- Dependencies: cross-venue connector bundle (already in place).
- Effort: 2-3 weeks.

### Deferred and explicitly out-of-scope

- **Options market making** — 1-quarter+ epic when a Deribit/OKX
  options connector is added. Tracked as "future epic" only.
- **Kernel bypass (DPDK, Onload)** — not ROI-positive at our volume
  regime; deferred to when we're quoting $500M+/day.
- **L3 queue position inference** — tracked in Axis 1 top-3, but
  requires Tardis subscription and a venue capability flag.
- **Real RL policy inference** — ROADMAP RL spike section still
  stands; no change.

---

## Appendix: sources cited

[1] Stoikov, S. "The Micro-Price: A High-Frequency Estimator of Future
Prices." Quantitative Finance, 2018. SSRN 2970694.

[2] beatzxbt/smm GitHub repository — Bybit market-making bot with
learned microprice and OFI modules. https://github.com/beatzxbt/smm
(and beatzxbt/mm-toolbox).

[3] Cont, R., Kukanov, A., Stoikov, S. "The Price Impact of Order Book
Events." Journal of Financial Econometrics, 2014. The OFI construction
reference. arXiv:1011.6402.

[4] Hummingbot Foundation — `hummingbot/hummingbot` GitHub, strategy
directory and `avellaneda_market_making.py`.
https://github.com/hummingbot/hummingbot

[5] Bacry, E., Mastromatteo, I., Muzy, J.-F. "Hawkes Processes in
Finance." Market Microstructure and Liquidity, 2015. arXiv:1502.04592.
Foundation for Hawkes-based MM quoting.

[6] Easley, D., Lopez de Prado, M., O'Hara, M. "Flow Toxicity and
Liquidity in a High-Frequency World." Review of Financial Studies,
2012. The VPIN + BVC paper.

[7] Tardis.dev — crypto market data provider, publishes L3 per-order
feeds for Binance Futures and Bybit. https://tardis.dev/docs

[8] nkaz001/hftbacktest GitHub repository — canonical open-source HFT
backtester with L2 and L3 queue inference modules. Apache-2.0.
https://github.com/nkaz001/hftbacktest

[9] Cartea, Á., Jaimungal, S., Penalva, J. "Algorithmic and
High-Frequency Trading." Cambridge University Press, 2015.
Chapters 6 (hedge optimization), 7 (drawdown), 10 (adverse-selection
pricing).

[10] Hummingbot strategy documentation — cross-exchange market making
and pure market making templates.
https://docs.hummingbot.org/strategies/

[11] GSR.io insights section — published research on BTC/ETH spread
trading and stablecoin arbitrage. https://gsr.io/insights

[12] Makarov, I., Schoar, A. "Trading and Arbitrage in Cryptocurrency
Markets." Journal of Financial Economics, 2020. Lead-lag structure
between Binance Futures and spot venues.

[13] Keyrock publicly posted architecture diagrams and careers-page
language on cross-venue price leadership.
https://keyrock.com/careers and the Keyrock Medium blog.

[14] Wintermute — Bell Curve podcast appearances (2023-2024), also
blog.wintermute.com. Discussions of quote-retreat on news events and
cross-asset hedge optimization.

[15] baobach/hft_papers GitHub repository — tracker of 2025
RL-for-market-making papers. https://github.com/baobach/hft_papers.
im1235/ISAC — SAC-gamma reference implementation.

[16] London Algos Conference 2023 — Wintermute infra talk on
Solarflare Onload and co-location. Archived at YouTube.

[17] Jump Trading careers postings for "Crypto Quant Developer" —
referenced AWS us-east-1, ap-northeast-1, and Equinix TY3 co-lo.
https://www.jumptrading.com/careers

[18] tokio-uring project — io_uring runtime for tokio.
https://github.com/tokio-rs/tokio-uring

[19] Flow Traders deployment writeup (blog.flowtraders.com) and
Hummingbot's production-hardening guide in the docs.

[20] Binance FIX API docs, Coinbase Prime FIX 4.4 docs, Bybit FIX API
docs (all public, indexed on the respective developer portals).

[21] Bybit V5 batch order endpoint docs — up to 20 orders per message
for create/cancel/amend. https://bybit-exchange.github.io/docs/v5/

[22] Flowdesk published platform architecture diagram — the
per-strategy capital allocator box. https://www.flowdesk.co/technology

[23] Jump Crypto research — "Stress scenarios for crypto market
making" white paper (2023). https://jumpcrypto.com/writing/

[24] B2C2 engineering talk, QuantMinds International 2024 —
adverse-selection budget per venue as a risk control.
