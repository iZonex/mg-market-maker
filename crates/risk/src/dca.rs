//! DCA / position-adjustment planner.
//!
//! Given a current position size, a target position size, and a time
//! budget, produce a schedule of child orders that linearly walks
//! from current to target. Unlike the existing TWAP executor (which
//! only unwinds to zero), this accepts an arbitrary target and
//! supports both *adding to* and *reducing* a position.
//!
//! The planner is pure sync: it turns inputs into a `Vec<DcaSlice>`.
//! The caller schedules the slices — this module does no I/O.
//!
//! ## Design
//!
//! - **Linear schedule** — the base case splits `(target - current)`
//!   into `n` equal chunks.
//! - **Size curve** — the caller can bias toward front-loading or
//!   back-loading via a [`SizeCurve`] enum: `Flat` (equal), `Linear`
//!   with a `slope` (front- or back-loaded), `Accelerated` (quadratic
//!   back-load).
//! - **Minimum slice** — planner will not emit a slice smaller than
//!   the venue lot size; instead it rounds up (or drops trailing
//!   residuals onto the final slice).
//! - **Reduce-only hint** — exposed to the caller so cancels and
//!   flattens can be tagged on the venue.

use std::time::Duration;

use rust_decimal::Decimal;

use crate::kill_switch::KillLevel;

/// Shape of the DCA schedule.
#[derive(Debug, Clone, Copy)]
pub enum SizeCurve {
    /// Equal-sized slices.
    Flat,
    /// Each slice is `base + slope × index`. `slope > 0` back-loads,
    /// `slope < 0` front-loads. Normalised so the total matches the
    /// requested delta.
    Linear { slope: Decimal },
    /// Quadratic back-load: slice `i` ∝ `i²`. Useful when urgency
    /// ramps with remaining exposure.
    Accelerated,
}

#[derive(Debug, Clone)]
pub struct DcaRequest {
    pub current: Decimal,
    pub target: Decimal,
    pub num_slices: usize,
    pub interval: Duration,
    pub lot_size: Decimal,
    pub curve: SizeCurve,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DcaSlice {
    pub offset: Duration,
    /// Signed: positive = buy, negative = sell.
    pub delta: Decimal,
    /// Reduce-only is `true` when the slice moves the position
    /// toward zero (strictly smaller magnitude).
    pub reduce_only: bool,
}

/// Build the slice schedule for a DCA request.
pub fn plan(req: &DcaRequest) -> Vec<DcaSlice> {
    assert!(req.num_slices > 0, "num_slices must be > 0");
    assert!(req.lot_size > Decimal::ZERO, "lot_size must be > 0");

    let total_delta = req.target - req.current;
    if total_delta.is_zero() {
        return Vec::new();
    }

    let sign = if total_delta > Decimal::ZERO {
        Decimal::ONE
    } else {
        Decimal::NEGATIVE_ONE
    };
    let total_abs = total_delta.abs();

    // Raw weights per slice, before normalisation.
    let weights: Vec<Decimal> = match req.curve {
        SizeCurve::Flat => vec![Decimal::ONE; req.num_slices],
        SizeCurve::Linear { slope } => (0..req.num_slices)
            .map(|i| Decimal::ONE + slope * Decimal::from(i))
            .map(|w| w.max(Decimal::ZERO))
            .collect(),
        SizeCurve::Accelerated => (0..req.num_slices)
            .map(|i| {
                let i = Decimal::from(i + 1);
                i * i
            })
            .collect(),
    };
    let weight_sum: Decimal = weights.iter().copied().sum();
    if weight_sum <= Decimal::ZERO {
        return Vec::new();
    }

    // Distribute magnitude according to weights and round each slice
    // down to lot_size. Accumulate the residual and drop it onto the
    // final slice so the schedule sums exactly to total_abs.
    //
    // `reduce_only` is evaluated against the *running* simulated
    // position, not the original `current`. A long→short flip marks
    // the slices that bring the position to zero as reduce-only and
    // the slices that open the new short as not-reduce-only.
    let mut slices: Vec<DcaSlice> = Vec::with_capacity(req.num_slices);
    let mut accounted = Decimal::ZERO;
    let mut running = req.current;
    for (i, w) in weights.iter().enumerate() {
        let raw = total_abs * *w / weight_sum;
        let rounded = (raw / req.lot_size).floor() * req.lot_size;
        let is_last = i + 1 == req.num_slices;
        let size = if is_last {
            total_abs - accounted
        } else {
            rounded
        };
        accounted += size;
        if size <= Decimal::ZERO {
            continue;
        }
        let delta = sign * size;
        let reduce_only = slice_reduces_running_position(running, delta);
        running += delta;
        slices.push(DcaSlice {
            offset: req.interval * i as u32,
            delta,
            reduce_only,
        });
    }

    slices
}

fn slice_reduces_running_position(running: Decimal, delta: Decimal) -> bool {
    if running.is_zero() {
        return false;
    }
    // Opposite sign?
    let opposite = (running > Decimal::ZERO) != (delta > Decimal::ZERO);
    if !opposite {
        return false;
    }
    // Does the slice stay on the same side of zero? If the new
    // running position crosses zero (or lands exactly at zero) the
    // slice is still reduce-only; if it lands on the *other* side
    // with a larger magnitude than the step needed to reach zero,
    // the slice cannot be reduce-only on the venue because it would
    // open a fresh position.
    running.abs() >= delta.abs()
}

/// Choose sensible defaults based on the current kill level.
///
/// - Normal mode: flat 10-slice schedule
/// - WidenSpreads: flat 6-slice, slightly faster
/// - StopNewOrders: NOT called (the planner is only invoked for
///   unwinds at this level)
/// - CancelAll: accelerated 5-slice (quadratic back-load)
/// - FlattenAll: accelerated 3-slice (aggressive)
/// - Disconnect: single slice (immediate)
pub fn defaults_for_level(level: KillLevel) -> (usize, SizeCurve) {
    match level {
        KillLevel::Normal => (10, SizeCurve::Flat),
        KillLevel::WidenSpreads => (6, SizeCurve::Flat),
        KillLevel::StopNewOrders => (5, SizeCurve::Flat),
        KillLevel::CancelAll => (5, SizeCurve::Accelerated),
        KillLevel::FlattenAll => (3, SizeCurve::Accelerated),
        KillLevel::Disconnect => (1, SizeCurve::Flat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn req_flat(current: Decimal, target: Decimal, n: usize) -> DcaRequest {
        DcaRequest {
            current,
            target,
            num_slices: n,
            interval: Duration::from_secs(1),
            lot_size: dec!(0.001),
            curve: SizeCurve::Flat,
        }
    }

    #[test]
    fn zero_delta_yields_empty_schedule() {
        let slices = plan(&req_flat(dec!(1), dec!(1), 5));
        assert!(slices.is_empty());
    }

    #[test]
    fn flat_schedule_sums_to_exact_delta() {
        let slices = plan(&req_flat(dec!(0), dec!(1), 10));
        let total: Decimal = slices.iter().map(|s| s.delta).sum();
        assert_eq!(total, dec!(1));
        assert_eq!(slices.len(), 10);
    }

    #[test]
    fn reducing_position_to_zero_is_reduce_only() {
        let slices = plan(&req_flat(dec!(10), dec!(0), 5));
        assert!(slices.iter().all(|s| s.reduce_only));
        let total: Decimal = slices.iter().map(|s| s.delta).sum();
        assert_eq!(total, dec!(-10));
    }

    #[test]
    fn adding_to_position_is_not_reduce_only() {
        let slices = plan(&req_flat(dec!(10), dec!(20), 5));
        assert!(slices.iter().all(|s| !s.reduce_only));
    }

    #[test]
    fn flipping_long_to_short_is_not_reduce_only() {
        // Going from +5 to -5 means the second half overshoots zero;
        // the schedule is not reduce-only because the net effect
        // opens a short position.
        let slices = plan(&req_flat(dec!(5), dec!(-5), 10));
        let total: Decimal = slices.iter().map(|s| s.delta).sum();
        assert_eq!(total, dec!(-10));
        // At least one slice should NOT be reduce-only because the
        // cumulative delta eventually flips past zero.
        assert!(slices.iter().any(|s| !s.reduce_only));
    }

    #[test]
    fn offsets_are_evenly_spaced() {
        let slices = plan(&req_flat(dec!(0), dec!(1), 5));
        for (i, s) in slices.iter().enumerate() {
            assert_eq!(s.offset, Duration::from_secs(i as u64));
        }
    }

    #[test]
    fn linear_back_load_produces_growing_slices() {
        let req = DcaRequest {
            current: dec!(0),
            target: dec!(10),
            num_slices: 5,
            interval: Duration::from_secs(1),
            lot_size: dec!(0.001),
            curve: SizeCurve::Linear { slope: dec!(0.5) },
        };
        let slices = plan(&req);
        // Slices should be monotonically increasing in size (back-load).
        for w in slices.windows(2) {
            assert!(w[1].delta.abs() >= w[0].delta.abs());
        }
        // Total still exact.
        let total: Decimal = slices.iter().map(|s| s.delta).sum();
        assert_eq!(total, dec!(10));
    }

    #[test]
    fn accelerated_curve_front_small_back_large() {
        let req = DcaRequest {
            current: dec!(0),
            target: dec!(10),
            num_slices: 5,
            interval: Duration::from_secs(1),
            lot_size: dec!(0.001),
            curve: SizeCurve::Accelerated,
        };
        let slices = plan(&req);
        let first = slices.first().unwrap().delta.abs();
        let last = slices.last().unwrap().delta.abs();
        assert!(last > first);
    }

    #[test]
    fn lot_size_rounding_drops_residual_on_last_slice() {
        // total = 1.0, 3 slices, lot_size = 0.001
        // equal weights: each 1/3, rounded down to 0.333
        // residual 1.0 - 3*0.333 = 0.001 → last slice = 0.334
        let req = DcaRequest {
            current: dec!(0),
            target: dec!(1),
            num_slices: 3,
            interval: Duration::from_secs(1),
            lot_size: dec!(0.001),
            curve: SizeCurve::Flat,
        };
        let slices = plan(&req);
        let total: Decimal = slices.iter().map(|s| s.delta).sum();
        assert_eq!(total, dec!(1));
    }

    #[test]
    fn defaults_for_level_scale_with_urgency() {
        let (n_normal, _) = defaults_for_level(KillLevel::Normal);
        let (n_flatten, _) = defaults_for_level(KillLevel::FlattenAll);
        assert!(n_normal > n_flatten);
    }
}
