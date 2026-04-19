use mm_common::config::MarketMakerConfig;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Fill, Price, ProductSpec, Qty, QuotePair, Side};
use rust_decimal::Decimal;

/// MM-2 — compact fill summary passed to `Strategy::on_fill`.
/// The full `Fill` is available on the engine side; strategies
/// usually only need the handful of fields that drive state
/// updates (depth from mid, side, size, timing, maker flag).
///
/// Computed by the engine once per fill and reused across every
/// strategy in the pool so each strategy doesn't have to redo
/// the `mid` lookup + depth arithmetic.
#[derive(Debug, Clone, Copy)]
pub struct FillObservation {
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    /// `|price - mid|` at the moment of the fill. Strategies
    /// that calibrate an arrival-rate curve against depth
    /// (GLFT, Cartea) read this directly.
    pub depth_from_mid: Decimal,
    /// Mid price at fill time.
    pub mid: Price,
    /// True for passive (maker) fills — always true on the
    /// PostOnly hot path, occasionally false on emergency
    /// take-outs (kill-switch, paired_unwind).
    pub is_maker: bool,
    /// Unix millis — for fill-timing memory (regret windows,
    /// inter-arrival distributions).
    pub ts_ms: i64,
}

impl FillObservation {
    /// Build from the raw `Fill` + live mid. `depth_from_mid`
    /// is absolute so downstream consumers don't have to
    /// branch on side for the usual |δ| use.
    pub fn from_fill(fill: &Fill, mid: Price) -> Self {
        let depth = (fill.price - mid).abs();
        Self {
            side: fill.side,
            price: fill.price,
            qty: fill.qty,
            depth_from_mid: depth,
            mid,
            is_maker: fill.is_maker,
            ts_ms: fill.timestamp.timestamp_millis(),
        }
    }
}

/// Context passed to the strategy on each tick.
#[derive(Clone)]
pub struct StrategyContext<'a> {
    pub book: &'a LocalOrderBook,
    pub product: &'a ProductSpec,
    pub config: &'a MarketMakerConfig,
    /// Current inventory in base asset (positive = long, negative = short).
    pub inventory: Decimal,
    /// Estimated volatility (σ).
    pub volatility: Decimal,
    /// Time remaining in the horizon as a fraction [0, 1].
    pub time_remaining: Decimal,
    /// Recent mid price for reference.
    pub mid_price: Price,
    /// Mid price on the hedge leg (basis-aware strategies shift
    /// reservation price toward this). `None` in single-connector
    /// mode or before the hedge book has seen its first update.
    pub ref_price: Option<Price>,
    /// Hedge-leg order book. Populated in dual-connector mode so
    /// strategies that need **real taker cost on the hedge** (as
    /// opposed to a single top-of-book scalar in `ref_price`) can
    /// walk the full depth via `features::market_impact`.
    /// `None` in single-connector mode.
    pub hedge_book: Option<&'a LocalOrderBook>,
    /// Expected-carry surcharge in basis points the strategy
    /// should bake into its reservation price when quoting an
    /// ask side that would require borrowing the base asset.
    /// Threaded by the engine from `BorrowManager::effective_carry_bps`
    /// — `None` (or `Some(zero)`) means borrow data is unavailable
    /// and the strategy reverts to the pre-P1.3 reservation
    /// formula. P1.3 stage-1.
    pub borrow_cost_bps: Option<Decimal>,
    /// Age of the hedge-leg order book in milliseconds, computed
    /// by the engine as `now_ms - hedge_book.last_update_ms`.
    /// Cross-venue basis strategies use this to stand down when
    /// the hedge feed pauses long enough that the reference
    /// price would be stale. `None` in single-connector mode or
    /// when the hedge book has not seen its first update yet.
    /// P1.4 stage-1.
    pub hedge_book_age_ms: Option<i64>,
    /// Epic D sub-component #4 — adverse-selection probability
    /// `ρ ∈ [0, 1]` threaded from
    /// `mm_risk::toxicity::AdverseSelectionTracker` through the
    /// engine's `refresh_quotes`. `Some(0.5)` is neutral and
    /// leaves the quoted spread unchanged; values > 0.5 narrow
    /// the spread (MM gets out of informed flow's way) and
    /// values < 0.5 widen it. `None` reverts to the pre-Epic-D
    /// spread formula.
    ///
    /// Both `AvellanedaStoikov` and `GlftStrategy` consume this
    /// (after Epic D stage-2). When the per-side fields below
    /// are also populated they take precedence; `as_prob` is
    /// the symmetric fallback.
    pub as_prob: Option<Decimal>,
    /// Epic D stage-3 — per-side asymmetric adverse-selection
    /// probability for the **bid** side. Threaded by the engine
    /// from
    /// `AdverseSelectionTracker::adverse_selection_bps_bid` via
    /// `cartea_spread::as_prob_from_bps`. When **both**
    /// [`Self::as_prob_bid`] and [`Self::as_prob_ask`] are
    /// `Some`, the strategy uses the per-side
    /// `quoted_half_spread_per_side` path and ignores
    /// [`Self::as_prob`]. When either is `None` the strategy
    /// falls back to the symmetric `as_prob` path. `None`
    /// preserves byte-identical pre-stage-3 behaviour.
    pub as_prob_bid: Option<Decimal>,
    /// Epic D stage-3 — per-side asymmetric adverse-selection
    /// probability for the **ask** side. See [`Self::as_prob_bid`].
    pub as_prob_ask: Option<Decimal>,
}

/// Trait for market-making strategies.
pub trait Strategy: Send + Sync {
    /// Compute the desired quotes given the current market state.
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair>;

    /// Name of the strategy for logging.
    fn name(&self) -> &str;

    /// Optional pre-tick hook — engine threads per-symbol session
    /// timing (seconds to next session boundary). Default no-op.
    /// `MarkStrategy` uses this to gate its close-window activity.
    fn on_session_tick(&self, _seconds_to_boundary: i64) {}

    /// MM-2 — called **every** tick before [`Self::compute_quotes`],
    /// with the same context. Lets a strategy advance its own state
    /// (timers, plan FSMs, regret decay) without being coupled to
    /// the quote-computation path. Default no-op so legacy
    /// stateless strategies keep their current behaviour.
    ///
    /// Interior mutability required (trait is `Send + Sync`; engine
    /// owns strategies behind `Box<dyn Strategy>` and
    /// `strategy_pool: HashMap<NodeId, Box<dyn Strategy>>`). Use
    /// `Mutex`, `AtomicU64`, etc. — see `GlftStrategy` for the
    /// canonical pattern.
    fn on_tick(&self, _ctx: &StrategyContext) {}

    /// MM-2 — called after every successful fill on a symbol this
    /// strategy participates in. `obs` is the pre-computed compact
    /// view the engine builds via [`FillObservation::from_fill`].
    /// Default no-op.
    ///
    /// Strategies that calibrate arrival-rate curves (GLFT, Cartea
    /// adverse-selection) or track fill-timing regret update their
    /// private state here. Must not block — the engine calls this
    /// synchronously on the event-loop thread.
    fn on_fill(&self, _obs: &FillObservation) {}

    /// S5.4 — snapshot of the strategy's live calibration state,
    /// if any. Returns `None` for stateless strategies (grid,
    /// basis, Avellaneda). `GlftStrategy` overrides to surface
    /// the current fitted `(A, k)` + sample count so operators
    /// can watch calibration convergence on the dashboard
    /// without cracking open Prometheus. Cheap (mutex-guarded
    /// read) and safe to poll on the engine's minute tick.
    fn calibration_state(&self) -> Option<CalibrationState> {
        None
    }

    /// S5.4 — ask the strategy to recalibrate *if it is due* —
    /// i.e. enough time has passed since the last retune AND
    /// enough samples have accumulated. Cadence is
    /// strategy-specific (`GlftStrategy` uses a 30-second floor
    /// with the existing ≥50 sample gate). Strategies that
    /// calibrate only on fills override this to become a no-op
    /// when nothing changed; default is no-op.
    fn recalibrate_if_due(&self, _now_ms: i64) {}
}

/// S5.4 — snapshot of a strategy's live calibration state for
/// the dashboard. The concrete numbers are strategy-specific
/// (GLFT publishes fitted intensity `a` + decay `k`); the
/// dashboard renders them side by side with sample count.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationState {
    /// Name of the calibrating strategy (`"glft"`, etc.).
    pub strategy: String,
    /// Fitted intensity `a` from ln(λ) = ln(A) - k·δ.
    pub a: Decimal,
    /// Fitted decay `k` from ln(λ) = ln(A) - k·δ.
    pub k: Decimal,
    /// Number of fill-depth samples backing the current fit.
    /// `< 50` means the strategy is still seeded with the
    /// constructor defaults — the panel should call this out.
    pub samples: usize,
    /// `now_ms` at the last successful recalibration, or
    /// `None` if the strategy has not yet crossed its sample
    /// threshold.
    pub last_recalibrated_ms: Option<i64>,
}

/// Helper: clamp a price to [mid - max_dist, mid + max_dist].
pub fn clamp_price(price: Price, mid: Price, max_distance: Price) -> Price {
    let lo = mid - max_distance;
    let hi = mid + max_distance;
    price.max(lo).min(hi)
}

/// Convert bps to a fraction (e.g., 10 bps → 0.001).
pub fn bps_to_frac(bps: Decimal) -> Decimal {
    bps / Decimal::from(10_000)
}

/// Ensure a quote meets the product's min notional.
pub fn ensure_min_qty(price: Price, min_qty: Qty, product: &ProductSpec) -> Qty {
    let min_for_notional = if price.is_zero() {
        min_qty
    } else {
        product.round_qty(product.min_notional / price) + product.lot_size
    };
    min_qty.max(min_for_notional)
}
