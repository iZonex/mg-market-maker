# Multi-Venue Architecture — Levels 2 + 3

Design doc for the next two architecture layers of the strategy
graph: cross-engine data bus (Level 2) and true multi-venue trading
with a global portfolio view (Level 3). The current build is Level 1
— each engine is single-symbol, single-venue, single-product; graphs
read only from their owning engine.

## Current state (Level 1)

```
┌──────────────────┐          ┌──────────────────┐
│ engine BTCUSDT   │          │ engine ETHUSDT   │
│  bybit spot      │          │  binance perp    │
│  ├─ book_keeper  │          │  ├─ book_keeper  │
│  ├─ balance_cache│          │  ├─ balance_cache│
│  └─ graph ←──────┼── owns   │  └─ graph ←──────┼── owns
└──────────────────┘          └──────────────────┘
                (no shared data path between engines)
```

- A graph scope of `Symbol(BTCUSDT)` matches exactly one engine.
- `Book.L1` as a source node reads only from *that* engine's book.
- `hedge_book` is the one backdoor for cross-venue price reference,
  but it's single-direction + read-only (strategy consumes, no
  feedback).
- `Strategy.*` nodes use the engine's compiled strategy instance —
  placement routes only through the engine's own order manager.
- No cross-engine balance view.

This is enough for single-symbol MM (Avellaneda-Stoikov against
one pair on one venue) but breaks for anything involving:

1. **Triangular arb** between three venues.
2. **Spot ↔ perp basis carry** (spot on Binance, perp on Bybit).
3. **Cross-market surveillance** — MarkingClose, FakeLiquidity
   across books, CancelOnReaction correlation.
4. **Synchronised portfolio balance** — long BTC spot + short BTC
   perp = neutral net delta; current engines don't see the sum.

## Level 2 — Cross-engine data bus

### Goal

One canonical hub where every engine publishes its data (book
snapshots, trade tape, funding, balances) keyed by
`(venue, symbol, product)`, and where every graph can subscribe to
any combination via parameterised source nodes.

### Shape

```rust
// crates/dashboard/src/data_bus.rs (new)

pub type StreamKey = (String /*venue*/, String /*symbol*/, ProductType);

pub struct DataBus {
    // L1 snapshot + spread_bps, 1 entry per stream key.
    pub books_l1: Arc<RwLock<HashMap<StreamKey, BookL1Snapshot>>>,
    // Top-N levels per side (N=20 by default).
    pub books_l2: Arc<RwLock<HashMap<StreamKey, BookL2Snapshot>>>,
    // Rolling public-trade tape, 60 s window.
    pub trades:   Arc<RwLock<HashMap<StreamKey, VecDeque<TradeTick>>>>,
    // Funding rate + next funding timestamp per perp stream.
    pub funding:  Arc<RwLock<HashMap<StreamKey, FundingRate>>>,
    // Per-venue wallet balances, keyed on (venue, asset).
    pub balances: Arc<RwLock<HashMap<(String, String), Balance>>>,
}
```

### Publisher side (engine)

Every engine gains an `Option<Arc<DataBus>>`; when set, the engine
pushes its slice on every `refresh_quotes` (or a dedicated push
task). Cheap — a write lock on a small hashmap.

### Consumer side (graph source nodes)

New parameterised source nodes. JSON config carries the
`(venue, symbol, product)` tuple; the engine's source-overlay
looks up the key in the bus and injects values.

```
Book.L1(venue="bybit",   symbol="BTCUSDT", product="spot")
Book.L1(venue="binance", symbol="BTCUSDT", product="perp")
Book.L2(venue=…, symbol=…, product=…, depth=10)
Trade.Tape(venue=…, symbol=…, product=…, window_secs=60)
Balance(venue=…, asset="USDT")
Funding(venue=…, symbol=…)
```

Backwards compat: the old `Book.L1` / `Book.Trades` sources (no
params) remain, interpreted as "current engine's venue / symbol /
product". Existing templates (`major-spot-basic` etc.) keep working
untouched; new templates opt into the parameterised form.

### Migration of existing nodes

| Today | After Level 2 |
|---|---|
| `Book.L1` | alias for `Book.L1(venue=SELF, symbol=SELF, product=SELF)` |
| `Volatility.Realised` | stays — it's a local estimator, not a raw feed |
| `Toxicity.VPIN` | stays — per-engine estimator |
| `Sentiment.Rate` | stays — per-asset, not per-venue |
| `Strategy.*` | stays (pool instance per node; see Level 3) |

No node gets removed, only new parameterised variants added.

## Level 3 — Multi-venue trading + global portfolio

### Goal

One graph places orders across multiple venues, tracks balance /
delta globally, and can author cross-venue strategies without
spinning up a second engine.

### VenueQuote

Today `SinkAction::Quotes(Vec<GraphQuote>)` implicitly ships to
the engine's own venue. Level 3 adds:

```rust
pub struct VenueQuote {
    pub venue: String,
    pub symbol: String,
    pub product: ProductType,
    pub side: Side,
    pub price: Decimal,
    pub qty: Decimal,
}

pub enum SinkAction {
    ...
    Quotes(Vec<GraphQuote>),              // legacy single-venue
    VenueQuotes(Vec<VenueQuote>),         // new multi-venue
}
```

### MultiVenueOrderRouter

Lives on DashboardState, holds a handle to every engine:

```rust
pub struct MultiVenueOrderRouter {
    // engine handle per (venue, symbol) — product derived from engine.config
    engines: HashMap<(String, String), EngineHandle>,
}

impl MultiVenueOrderRouter {
    pub fn route(&self, vq: VenueQuote) -> Result<()>;
}
```

A `VenueQuotes` sink dispatches each entry to the correct engine's
`order_manager`. Misses (no engine for that venue/symbol) surface
as a surveillance-grade audit event so an op catches misrouted
strategies instead of silently dropping orders.

### PortfolioBalanceTracker

```rust
pub struct PortfolioBalanceTracker {
    // per (venue, asset) → raw wallet balance
    balances: HashMap<(String, String), Balance>,
    // net delta per asset, aggregated across venues + products
    // (spot long + perp short ⇒ 0 net)
    net_delta: HashMap<String, Decimal>,
}
```

Refreshed from `DataBus.balances` + per-engine `inventory_manager`.

### Cross-venue composite strategies

Extend the Strategy pool machinery from Phase 5 with params that
make sense across venues:

- `Strategy.BasisArb(spot_venue, perp_venue, symbol)` — spot-perp
  basis carry. Output `VenueQuotes` for both legs.
- `Strategy.TriangularArb(v1, v2, v3, a/b, b/c, a/c)` — triangular
  arb across three venues or three pairs.
- `Strategy.FundingCollector(perp_venue, symbol, threshold_bps)` —
  maker-post spot, taker-take perp on funding window approach.

All restricted: false (these are the *honest* strategies; exploit
strategies keep the `Strategy.{Spoof,Layer,...}` namespace).

### Atomicity

`SinkAction::AtomicBundle { maker: VenueQuote, hedge: VenueQuote, timeout_ms }`
— router places the maker, then waits up to `timeout_ms` for the
hedge to ack; on hedge failure, cancels the maker. Same pattern as
`paired_unwind` but driven by the graph.

### Scope semantics

| Scope | Engine routing | Source access |
|---|---|---|
| `Symbol(BTCUSDT)` | exactly one engine per symbol | SELF-engine + DataBus if referenced |
| `Global` | all engines that match the graph's sinks' venues | DataBus for everything |
| `AssetClass(...)` | engines whose pair_class matches | DataBus |
| `Client(...)` | engines owned by that client | DataBus |

No breaking change: existing Symbol-scoped graphs route exactly as
today.

## Phased rollout

| Phase | Scope | Files touched |
|---|---|---|
| **2.A — Data bus skeleton** | `DataBus` struct + registration on DashboardState + engine push task (no graph-side consumption yet) | `crates/dashboard/src/data_bus.rs`, `state.rs`, `market_maker.rs` |
| **2.B — Parameterised source nodes** | `Book.L1(v,s,p)`, `Book.L2`, `Trade.Tape`, `Balance`, `Funding` — reads `DataBus` | `strategy-graph/src/nodes/sources.rs`, engine source overlay |
| **2.C — Template migration** | Update bundled templates to use explicit venue parameterisation where helpful | `templates/*.json` |
| **3.A — VenueQuote + SinkAction::VenueQuotes** | New variant + router skeleton (routes to one venue for now, degenerate case) | `strategy-graph/src/evaluator.rs`, `dashboard` |
| **3.B — MultiVenueOrderRouter** | Real multi-engine dispatch, misroute audit | `crates/dashboard`, `crates/engine` |
| **3.C — PortfolioBalanceTracker** | Global balance + net_delta aggregator, surfaced as `Balance` + `NetDelta` source nodes | `crates/risk` |
| **3.D — Cross-venue composites** | `Strategy.BasisArb`, `Strategy.FundingCollector`, `Strategy.TriangularArb` | `crates/strategy` + pool wiring |
| **3.E — Atomic bundles** | `Out.AtomicBundle` with maker/hedge timeout rollback | evaluator + router |

Each phase is a stand-alone commit; nothing between phases breaks
existing graphs.

## Open questions

1. **Per-venue API-rate budgets.** DataBus writes on every engine
   tick = ~2 writes/sec/engine. Trivial. If we ever grow to 100
   engines we'll want a batched flush, not a write per tick.
2. **Leader/follower semantics** for `VenueQuote`. Right now the
   bid on venue A is placed regardless of venue B state; a real
   basis-arb wants the hedge first. Phase 3.E atomic bundles solve
   this but design docs for the queue-position trade-offs belong
   here before we ship.
3. **Fee-aware routing.** Two venues can quote the same symbol; the
   router should prefer the cheaper fee-schedule venue when the
   graph is indifferent. Phase 3.B initial routing is kind-key only.
4. **Cross-venue cancel propagation.** If Binance cancels our maker,
   do we cancel the Bybit hedge automatically? Phase 3.E.

## Links

- Existing: `crates/exchange/core/src/sor.rs`, `crates/risk/src/hedge_optimizer.rs`, `crates/strategy/src/{cross_exchange,xemm,basis,funding_arb}.rs`
- Compliance audit trail extension will track which venue each sink
  fired on — update `docs/research/complince.md` §11 when Phase 3.B
  lands.
