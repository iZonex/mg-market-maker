use mm_common::config::MarketMakerConfig;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Price, ProductSpec, Qty, QuotePair};
use rust_decimal::Decimal;

/// Context passed to the strategy on each tick.
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
