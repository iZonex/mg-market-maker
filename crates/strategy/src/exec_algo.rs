//! Execution algorithm framework.
//!
//! Generalises the existing `twap.rs` module into a plug-in
//! `ExecAlgorithm` trait: every algorithm is a **pure synchronous
//! state machine** that turns the passage of time and market ticks
//! into child-order decisions. The engine drives them via `tick(now,
//! ctx)` and reads the emitted `ExecAction`s.
//!
//! Shipped algorithms:
//!
//! - [`TwapAlgo`] — time-weighted splitting over a fixed window
//! - [`VwapAlgo`] — volume-weighted splitting over a profile
//! - [`PovAlgo`] — percent-of-volume participation
//! - [`IcebergAlgo`] — show a small slice, refill as it fills
//!
//! The algorithms emit `ExecAction::Place { quote }` and
//! `ExecAction::Cancel { id }` — they own no I/O and no clock. The
//! caller (engine) is responsible for turning those into venue
//! requests via the existing connector layer.
//!
//! ## Integration status
//!
//! All four algorithms are library-complete with their own unit
//! test suites and are re-exported from the `mm_strategy` crate
//! root for external consumers. They are not currently driven by
//! the live engine's hot path because the shipping
//! `FundingArbExecutor` / `BasisStrategy` / `PairedUnwindExecutor`
//! use synchronous market-take entry to keep both legs of a
//! cross-product pair filled atomically — slicing via TWAP during
//! entry would leak basis during the slice window. The algorithms
//! are the right tool for:
//!
//! 1. Offline / replay tuning through `mm-hyperopt` — already
//!    compatible with the `Simulator`'s tick model.
//! 2. Stage-2 of the SOR inline dispatch (ROADMAP.md Epic A,
//!    item "Real leg-execution dispatch via ExecAlgorithm::TwapAlgo
//!    on entry") once per-venue fill observation is plumbed into
//!    the `PairDispatch` loop.
//! 3. Operator-triggered manual execution from the dashboard — a
//!    future ops endpoint `POST /api/v1/ops/twap/{symbol}` can
//!    instantiate a `TwapAlgo` and slice a user-supplied quantity.
//!
//! Treat the code here as **ready-to-wire**, not dead. The hooks
//! exist intentionally — they're held back until the atomic-
//! dispatch invariants above are preserved by the caller.

use std::time::{Duration, Instant};

use mm_common::types::{OrderId, Quote, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

/// Context provided to every `tick` call.
#[derive(Debug, Clone, Copy)]
pub struct ExecContext {
    pub now: Instant,
    /// Current best bid from the order book.
    pub best_bid: Decimal,
    /// Current best ask from the order book.
    pub best_ask: Decimal,
    /// Market volume observed in the last tick window. Used by the
    /// POV and VWAP algorithms; ignored by TWAP/Iceberg.
    pub recent_volume: Decimal,
    /// Venue lot size for rounding slice qty.
    pub lot_size: Decimal,
}

/// What an algorithm wants the engine to do this tick.
///
/// `PartialEq` is not derived because `mm_common::Quote` does not
/// implement it — tests match via `matches!` and destructuring.
#[derive(Debug, Clone)]
pub enum ExecAction {
    /// Place a new child order with the given quote.
    Place {
        client_order_id: OrderId,
        quote: Quote,
    },
    /// Cancel a previously-placed child order.
    Cancel { client_order_id: OrderId },
    /// Nothing to do on this tick.
    Hold,
    /// The algorithm has reached its target and should be removed.
    Done,
}

/// Shared interface for all execution algorithms.
pub trait ExecAlgorithm: Send {
    /// Report that a previously-emitted child order filled for `qty`
    /// at `price`. The algorithm updates its internal progress
    /// counters before the next `tick`.
    fn on_fill(&mut self, client_order_id: OrderId, price: Decimal, qty: Decimal);

    /// Advance the algorithm. Emits zero, one, or many actions for
    /// this tick.
    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction>;

    /// Cumulative base-asset quantity already filled.
    fn filled(&self) -> Decimal;

    /// Total base-asset quantity still to fill.
    fn remaining(&self) -> Decimal;

    /// `true` once the algorithm reached its target or timed out.
    fn is_finished(&self) -> bool;
}

// ---------------------------------------------------------------------------
// TWAP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TwapConfig {
    pub side: Side,
    pub total_qty: Decimal,
    pub duration: Duration,
    pub num_slices: usize,
}

pub struct TwapAlgo {
    config: TwapConfig,
    start: Option<Instant>,
    slice_qty: Decimal,
    slices_sent: usize,
    filled: Decimal,
    pending: Vec<OrderId>,
}

impl TwapAlgo {
    pub fn new(config: TwapConfig, lot_size: Decimal) -> Self {
        assert!(config.num_slices > 0);
        let raw = config.total_qty / Decimal::from(config.num_slices);
        let slice_qty = (raw / lot_size).floor() * lot_size;
        Self {
            config,
            start: None,
            slice_qty,
            slices_sent: 0,
            filled: Decimal::ZERO,
            pending: Vec::new(),
        }
    }

    fn next_slice_offset(&self) -> Duration {
        self.config.duration / self.config.num_slices as u32 * self.slices_sent as u32
    }

    fn target_qty_for_slice(&self, i: usize) -> Decimal {
        if i + 1 == self.config.num_slices {
            // Absorb any lot-size rounding residual on the final slice.
            self.config.total_qty - self.slice_qty * Decimal::from(self.config.num_slices - 1)
        } else {
            self.slice_qty
        }
    }
}

impl ExecAlgorithm for TwapAlgo {
    fn on_fill(&mut self, _cloid: OrderId, _price: Decimal, qty: Decimal) {
        self.filled += qty;
    }

    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction> {
        let start = *self.start.get_or_insert(ctx.now);
        if self.slices_sent >= self.config.num_slices {
            if self.filled >= self.config.total_qty {
                return vec![ExecAction::Done];
            }
            return vec![ExecAction::Hold];
        }

        let elapsed = ctx.now.saturating_duration_since(start);
        if elapsed < self.next_slice_offset() {
            return vec![ExecAction::Hold];
        }

        let qty = self.target_qty_for_slice(self.slices_sent);
        if qty <= Decimal::ZERO {
            self.slices_sent += 1;
            return vec![ExecAction::Hold];
        }
        let price = match self.config.side {
            Side::Buy => ctx.best_ask,
            Side::Sell => ctx.best_bid,
        };
        let cloid = Uuid::new_v4();
        self.pending.push(cloid);
        self.slices_sent += 1;
        vec![ExecAction::Place {
            client_order_id: cloid,
            quote: Quote {
                side: self.config.side,
                price,
                qty,
            },
        }]
    }

    fn filled(&self) -> Decimal {
        self.filled
    }

    fn remaining(&self) -> Decimal {
        (self.config.total_qty - self.filled).max(Decimal::ZERO)
    }

    fn is_finished(&self) -> bool {
        self.slices_sent >= self.config.num_slices && self.filled >= self.config.total_qty
    }
}

// ---------------------------------------------------------------------------
// VWAP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct VwapConfig {
    pub side: Side,
    pub total_qty: Decimal,
    /// Expected volume profile across the window. The algorithm
    /// matches this distribution as closely as slippage allows.
    /// Weights are normalised internally; units don't matter.
    pub volume_profile: Vec<Decimal>,
    pub slice_interval: Duration,
}

pub struct VwapAlgo {
    config: VwapConfig,
    start: Option<Instant>,
    slices_sent: usize,
    filled: Decimal,
    normalised: Vec<Decimal>,
}

impl VwapAlgo {
    pub fn new(config: VwapConfig) -> Self {
        assert!(!config.volume_profile.is_empty(), "empty volume profile");
        let total: Decimal = config.volume_profile.iter().copied().sum();
        let normalised: Vec<Decimal> = if total > Decimal::ZERO {
            config.volume_profile.iter().map(|w| *w / total).collect()
        } else {
            vec![
                Decimal::ONE / Decimal::from(config.volume_profile.len());
                config.volume_profile.len()
            ]
        };
        Self {
            config,
            start: None,
            slices_sent: 0,
            filled: Decimal::ZERO,
            normalised,
        }
    }
}

impl ExecAlgorithm for VwapAlgo {
    fn on_fill(&mut self, _cloid: OrderId, _price: Decimal, qty: Decimal) {
        self.filled += qty;
    }

    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction> {
        let start = *self.start.get_or_insert(ctx.now);
        if self.slices_sent >= self.config.volume_profile.len() {
            return vec![if self.filled >= self.config.total_qty {
                ExecAction::Done
            } else {
                ExecAction::Hold
            }];
        }
        let elapsed = ctx.now.saturating_duration_since(start);
        let need = self.config.slice_interval * self.slices_sent as u32;
        if elapsed < need {
            return vec![ExecAction::Hold];
        }

        let weight = self.normalised[self.slices_sent];
        let raw = self.config.total_qty * weight;
        let slice_qty = (raw / ctx.lot_size).floor() * ctx.lot_size;
        self.slices_sent += 1;
        if slice_qty <= Decimal::ZERO {
            return vec![ExecAction::Hold];
        }

        let price = match self.config.side {
            Side::Buy => ctx.best_ask,
            Side::Sell => ctx.best_bid,
        };
        vec![ExecAction::Place {
            client_order_id: Uuid::new_v4(),
            quote: Quote {
                side: self.config.side,
                price,
                qty: slice_qty,
            },
        }]
    }

    fn filled(&self) -> Decimal {
        self.filled
    }

    fn remaining(&self) -> Decimal {
        (self.config.total_qty - self.filled).max(Decimal::ZERO)
    }

    fn is_finished(&self) -> bool {
        self.slices_sent >= self.config.volume_profile.len() && self.filled >= self.config.total_qty
    }
}

// ---------------------------------------------------------------------------
// POV (percent of volume)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PovConfig {
    pub side: Side,
    pub total_qty: Decimal,
    /// Target participation rate, in `[0, 1]`.
    pub participation: Decimal,
}

pub struct PovAlgo {
    config: PovConfig,
    filled: Decimal,
}

impl PovAlgo {
    pub fn new(config: PovConfig) -> Self {
        assert!(config.participation >= Decimal::ZERO && config.participation <= dec!(1));
        Self {
            config,
            filled: Decimal::ZERO,
        }
    }
}

impl ExecAlgorithm for PovAlgo {
    fn on_fill(&mut self, _cloid: OrderId, _price: Decimal, qty: Decimal) {
        self.filled += qty;
    }

    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction> {
        if self.filled >= self.config.total_qty {
            return vec![ExecAction::Done];
        }
        // Size this slice as `participation × recent_volume`, clamped
        // by remaining quantity and the venue lot size.
        let raw = ctx.recent_volume * self.config.participation;
        let capped = raw.min(self.config.total_qty - self.filled);
        let slice_qty = (capped / ctx.lot_size).floor() * ctx.lot_size;
        if slice_qty <= Decimal::ZERO {
            return vec![ExecAction::Hold];
        }
        let price = match self.config.side {
            Side::Buy => ctx.best_ask,
            Side::Sell => ctx.best_bid,
        };
        vec![ExecAction::Place {
            client_order_id: Uuid::new_v4(),
            quote: Quote {
                side: self.config.side,
                price,
                qty: slice_qty,
            },
        }]
    }

    fn filled(&self) -> Decimal {
        self.filled
    }

    fn remaining(&self) -> Decimal {
        (self.config.total_qty - self.filled).max(Decimal::ZERO)
    }

    fn is_finished(&self) -> bool {
        self.filled >= self.config.total_qty
    }
}

// ---------------------------------------------------------------------------
// Iceberg
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IcebergConfig {
    pub side: Side,
    pub total_qty: Decimal,
    pub display_qty: Decimal,
    /// Limit price for all child orders.
    pub limit_price: Decimal,
}

pub struct IcebergAlgo {
    config: IcebergConfig,
    filled: Decimal,
    active: Option<(OrderId, Decimal)>,
}

impl IcebergAlgo {
    pub fn new(config: IcebergConfig) -> Self {
        Self {
            config,
            filled: Decimal::ZERO,
            active: None,
        }
    }
}

impl ExecAlgorithm for IcebergAlgo {
    fn on_fill(&mut self, cloid: OrderId, _price: Decimal, qty: Decimal) {
        self.filled += qty;
        if let Some((id, remaining)) = &mut self.active {
            if *id == cloid {
                *remaining -= qty;
                if *remaining <= Decimal::ZERO {
                    self.active = None;
                }
            }
        }
    }

    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction> {
        if self.filled >= self.config.total_qty {
            return vec![ExecAction::Done];
        }
        if self.active.is_some() {
            return vec![ExecAction::Hold];
        }
        let remaining = self.config.total_qty - self.filled;
        let raw = remaining.min(self.config.display_qty);
        let slice_qty = (raw / ctx.lot_size).floor() * ctx.lot_size;
        if slice_qty <= Decimal::ZERO {
            return vec![ExecAction::Hold];
        }
        let cloid = Uuid::new_v4();
        self.active = Some((cloid, slice_qty));
        vec![ExecAction::Place {
            client_order_id: cloid,
            quote: Quote {
                side: self.config.side,
                price: self.config.limit_price,
                qty: slice_qty,
            },
        }]
    }

    fn filled(&self) -> Decimal {
        self.filled
    }

    fn remaining(&self) -> Decimal {
        (self.config.total_qty - self.filled).max(Decimal::ZERO)
    }

    fn is_finished(&self) -> bool {
        self.filled >= self.config.total_qty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(best_bid: Decimal, best_ask: Decimal, volume: Decimal) -> ExecContext {
        ExecContext {
            now: Instant::now(),
            best_bid,
            best_ask,
            recent_volume: volume,
            lot_size: dec!(0.001),
        }
    }

    fn take_place(actions: &[ExecAction]) -> Option<(OrderId, Quote)> {
        actions.iter().find_map(|a| match a {
            ExecAction::Place {
                client_order_id,
                quote,
            } => Some((*client_order_id, quote.clone())),
            _ => None,
        })
    }

    // --- TWAP ---

    #[test]
    fn twap_schedules_num_slices() {
        let cfg = TwapConfig {
            side: Side::Sell,
            total_qty: dec!(10),
            duration: Duration::from_secs(10),
            num_slices: 5,
        };
        let mut algo = TwapAlgo::new(cfg, dec!(0.001));
        let start = Instant::now();
        let mut places = 0;
        for i in 0..6 {
            let mut c = ctx(dec!(100), dec!(101), dec!(0));
            c.now = start + Duration::from_secs(i * 2);
            let actions = algo.tick(c);
            if take_place(&actions).is_some() {
                places += 1;
            }
        }
        assert_eq!(places, 5);
    }

    #[test]
    fn twap_holds_before_next_slice_time() {
        let cfg = TwapConfig {
            side: Side::Buy,
            total_qty: dec!(1),
            duration: Duration::from_secs(10),
            num_slices: 2,
        };
        let mut algo = TwapAlgo::new(cfg, dec!(0.001));
        let start = Instant::now();
        let mut c = ctx(dec!(100), dec!(101), dec!(0));
        c.now = start;
        let first = algo.tick(c);
        assert!(take_place(&first).is_some());
        // Immediately again — should hold.
        c.now = start + Duration::from_millis(10);
        let second = algo.tick(c);
        assert!(second.iter().all(|a| matches!(a, ExecAction::Hold)));
    }

    #[test]
    fn twap_sum_matches_total() {
        let cfg = TwapConfig {
            side: Side::Sell,
            total_qty: dec!(10),
            duration: Duration::from_secs(5),
            num_slices: 3,
        };
        let mut algo = TwapAlgo::new(cfg, dec!(0.001));
        let start = Instant::now();
        let mut total_sent = Decimal::ZERO;
        for i in 0..3 {
            let mut c = ctx(dec!(100), dec!(101), dec!(0));
            c.now = start + Duration::from_secs(i * 2);
            if let Some((_, q)) = take_place(&algo.tick(c)) {
                total_sent += q.qty;
            }
        }
        assert_eq!(total_sent, dec!(10));
    }

    #[test]
    fn twap_marks_done_when_filled() {
        let cfg = TwapConfig {
            side: Side::Buy,
            total_qty: dec!(1),
            duration: Duration::from_secs(2),
            num_slices: 1,
        };
        let mut algo = TwapAlgo::new(cfg, dec!(0.001));
        let start = Instant::now();
        let mut c = ctx(dec!(100), dec!(101), dec!(0));
        c.now = start;
        let actions = algo.tick(c);
        let (cloid, q) = take_place(&actions).unwrap();
        algo.on_fill(cloid, q.price, q.qty);
        c.now = start + Duration::from_secs(5);
        let next = algo.tick(c);
        assert!(next.iter().any(|a| matches!(a, ExecAction::Done)));
        assert!(algo.is_finished());
    }

    // --- VWAP ---

    #[test]
    fn vwap_distributes_by_profile() {
        let cfg = VwapConfig {
            side: Side::Buy,
            total_qty: dec!(10),
            volume_profile: vec![dec!(1), dec!(2), dec!(1)],
            slice_interval: Duration::from_secs(1),
        };
        let mut algo = VwapAlgo::new(cfg);
        let start = Instant::now();
        let mut sizes = Vec::new();
        for i in 0..3 {
            let mut c = ctx(dec!(100), dec!(101), dec!(0));
            c.now = start + Duration::from_secs(i);
            if let Some((_, q)) = take_place(&algo.tick(c)) {
                sizes.push(q.qty);
            }
        }
        assert_eq!(sizes.len(), 3);
        // Middle slice should be larger than edges.
        assert!(sizes[1] > sizes[0]);
        assert!(sizes[1] > sizes[2]);
    }

    // --- POV ---

    #[test]
    fn pov_sizes_by_recent_volume() {
        let cfg = PovConfig {
            side: Side::Sell,
            total_qty: dec!(100),
            participation: dec!(0.1),
        };
        let mut algo = PovAlgo::new(cfg);
        let c = ctx(dec!(100), dec!(101), dec!(50));
        // 0.1 * 50 = 5, rounded to 0.001 = 5.
        let (_, q) = take_place(&algo.tick(c)).unwrap();
        assert_eq!(q.qty, dec!(5));
    }

    #[test]
    fn pov_holds_when_no_volume() {
        let cfg = PovConfig {
            side: Side::Sell,
            total_qty: dec!(100),
            participation: dec!(0.1),
        };
        let mut algo = PovAlgo::new(cfg);
        let c = ctx(dec!(100), dec!(101), dec!(0));
        let actions = algo.tick(c);
        assert!(actions.iter().any(|a| matches!(a, ExecAction::Hold)));
    }

    #[test]
    fn pov_finishes_after_total_filled() {
        let cfg = PovConfig {
            side: Side::Sell,
            total_qty: dec!(10),
            participation: dec!(0.5),
        };
        let mut algo = PovAlgo::new(cfg);
        let c = ctx(dec!(100), dec!(101), dec!(100));
        let (cloid, q) = take_place(&algo.tick(c)).unwrap();
        algo.on_fill(cloid, q.price, q.qty);
        let next = algo.tick(c);
        assert!(next.iter().any(|a| matches!(a, ExecAction::Done)));
    }

    // --- Iceberg ---

    #[test]
    fn iceberg_only_one_active_slice_at_a_time() {
        let cfg = IcebergConfig {
            side: Side::Buy,
            total_qty: dec!(10),
            display_qty: dec!(1),
            limit_price: dec!(100),
        };
        let mut algo = IcebergAlgo::new(cfg);
        let c = ctx(dec!(99), dec!(101), dec!(0));
        let first = algo.tick(c);
        assert!(take_place(&first).is_some());
        // Next tick without a fill → hold (active slot occupied).
        let second = algo.tick(c);
        assert!(second.iter().any(|a| matches!(a, ExecAction::Hold)));
    }

    #[test]
    fn iceberg_refills_after_fill() {
        let cfg = IcebergConfig {
            side: Side::Buy,
            total_qty: dec!(10),
            display_qty: dec!(1),
            limit_price: dec!(100),
        };
        let mut algo = IcebergAlgo::new(cfg);
        let c = ctx(dec!(99), dec!(101), dec!(0));
        let (cloid, q) = take_place(&algo.tick(c)).unwrap();
        algo.on_fill(cloid, q.price, q.qty);
        let refill = algo.tick(c);
        let (next_cloid, _) = take_place(&refill).unwrap();
        assert_ne!(next_cloid, cloid);
    }

    #[test]
    fn iceberg_marks_done_when_total_filled() {
        let cfg = IcebergConfig {
            side: Side::Sell,
            total_qty: dec!(3),
            display_qty: dec!(1),
            limit_price: dec!(100),
        };
        let mut algo = IcebergAlgo::new(cfg);
        let c = ctx(dec!(100), dec!(101), dec!(0));
        for _ in 0..3 {
            if let Some((cloid, q)) = take_place(&algo.tick(c)) {
                algo.on_fill(cloid, q.price, q.qty);
            }
        }
        let tail = algo.tick(c);
        assert!(tail.iter().any(|a| matches!(a, ExecAction::Done)));
        assert_eq!(algo.filled(), dec!(3));
    }
}
