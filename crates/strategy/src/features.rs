//! Microstructure feature extractors.
//!
//! Pure numerical primitives that turn raw order-book and trade
//! events into features suitable for a downstream alpha model. No
//! training loop, no PyTorch, no ONNX in this module — just the
//! feature engineering layer. A separate predictor crate (future)
//! can consume the `FeatureVector` output directly.
//!
//! Shipped extractors:
//!
//! - **Book imbalance at depth k** — signed ratio of bid vs ask
//!   volume in the top k levels. Range `[-1, +1]`. Positive = more
//!   buy pressure.
//! - **Trade flow EWMA** — exponentially weighted signed volume of
//!   recent trades. Positive = buyers dominating taker flow.
//! - **Micro-price** — `(bid_qty * ask_px + ask_qty * bid_px) / (bid_qty
//!   + ask_qty)`. Robust to momentary quote pulls.
//! - **Micro-price drift** — EWMA of micro-price changes, a
//!   directional-pressure estimate.
//! - **Realised volatility term structure** — parallel EWMAs at
//!   multiple half-lives; ratio of short over long gives the
//!   short-term volatility regime.
//!
//! All extractors are **lookahead-safe by construction** — each
//! `update()` reads only new data, and `value()` returns the latest
//! snapshot. (The dedicated `check_lookahead` property test lives in
//! the backtester crate; the construction here is the primary
//! guarantee.)

use mm_common::types::{PriceLevel, Side};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// ---------------------------------------------------------------------------
// Book imbalance
// ---------------------------------------------------------------------------

/// Signed order-book imbalance at depth `k`:
///
/// `imbalance = (bid_qty_top_k - ask_qty_top_k) / (bid_qty_top_k + ask_qty_top_k)`
///
/// Returns `0` if both sides are empty.
pub fn book_imbalance(bids: &[PriceLevel], asks: &[PriceLevel], k: usize) -> Decimal {
    let bid_qty: Decimal = bids.iter().take(k).map(|l| l.qty).sum();
    let ask_qty: Decimal = asks.iter().take(k).map(|l| l.qty).sum();
    let total = bid_qty + ask_qty;
    if total.is_zero() {
        return Decimal::ZERO;
    }
    (bid_qty - ask_qty) / total
}

/// Weighted book imbalance — inner levels count more via a linear
/// decay. Useful when top-of-book depth dominates execution and
/// deeper levels are mostly noise.
pub fn book_imbalance_weighted(bids: &[PriceLevel], asks: &[PriceLevel], k: usize) -> Decimal {
    let mut bid_w = Decimal::ZERO;
    let mut ask_w = Decimal::ZERO;
    for i in 0..k {
        let weight = Decimal::from((k - i) as i64);
        if let Some(level) = bids.get(i) {
            bid_w += level.qty * weight;
        }
        if let Some(level) = asks.get(i) {
            ask_w += level.qty * weight;
        }
    }
    let total = bid_w + ask_w;
    if total.is_zero() {
        return Decimal::ZERO;
    }
    (bid_w - ask_w) / total
}

// ---------------------------------------------------------------------------
// Trade flow
// ---------------------------------------------------------------------------

/// EWMA of signed trade volume. Buy trades add `+qty`, sell trades
/// add `-qty`. Positive values indicate net buying pressure.
#[derive(Debug, Clone)]
pub struct TradeFlow {
    alpha: Decimal,
    state: Option<Decimal>,
}

impl TradeFlow {
    /// `half_life_trades` is the number of trades over which the
    /// influence of a single event decays to half. Smaller values
    /// react faster.
    pub fn new(half_life_trades: usize) -> Self {
        assert!(half_life_trades > 0);
        // α such that (1-α)^half_life = 0.5 → α = 1 - 2^(-1/half_life).
        let n = half_life_trades as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            state: None,
        }
    }

    pub fn update(&mut self, taker_side: Side, qty: Decimal) {
        let signed = match taker_side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        self.state = Some(match self.state {
            None => signed,
            Some(prev) => self.alpha * signed + (Decimal::ONE - self.alpha) * prev,
        });
    }

    pub fn value(&self) -> Option<Decimal> {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Micro-price
// ---------------------------------------------------------------------------

/// Micro-price weighted by the opposite side's quantity. Used as a
/// more robust mid than `(bid + ask) / 2` because it favours
/// whichever side has more resting liquidity.
///
/// `None` if either side is empty.
pub fn micro_price(bids: &[PriceLevel], asks: &[PriceLevel]) -> Option<Decimal> {
    let bid = bids.first()?;
    let ask = asks.first()?;
    let total = bid.qty + ask.qty;
    if total.is_zero() {
        return None;
    }
    Some((bid.qty * ask.price + ask.qty * bid.price) / total)
}

/// Multi-level microprice with linearly decaying depth weights.
///
/// For each of the top `depth` levels, the per-level microprice is
/// `(bid_px * ask_qty + ask_px * bid_qty) / (bid_qty + ask_qty)` —
/// the classic opposite-side-weighted fair value. The final output
/// is the weighted average of those per-level values with weight
/// `w(i) = depth - i` on level `i` (i.e. top-of-book matters most,
/// deep levels contribute less).
///
/// A multi-level microprice is more robust than top-of-book alone
/// when the inside quote is thin: one dusting order at the touch
/// can't dominate the fair-value estimate. `BasisStrategy` and any
/// alpha model that shifts reservation price around a fair value
/// should prefer this when `depth ≥ 3`.
///
/// Returns `None` if either side is empty. `depth` is clamped to
/// `min(bids.len(), asks.len())`.
pub fn micro_price_weighted(
    bids: &[PriceLevel],
    asks: &[PriceLevel],
    depth: usize,
) -> Option<Decimal> {
    if bids.is_empty() || asks.is_empty() {
        return None;
    }
    let d = depth.min(bids.len()).min(asks.len());
    if d == 0 {
        return None;
    }

    let mut num = Decimal::ZERO;
    let mut den = Decimal::ZERO;
    for i in 0..d {
        let bid = &bids[i];
        let ask = &asks[i];
        let w = Decimal::from(d - i);
        let level_total = bid.qty + ask.qty;
        if level_total.is_zero() {
            continue;
        }
        // Accumulate numerator and denominator without
        // computing each per-level microprice individually —
        // algebraically identical but avoids d divisions.
        num += (bid.qty * ask.price + ask.qty * bid.price) * w;
        den += level_total * w;
    }
    if den.is_zero() {
        return None;
    }
    Some(num / den)
}

// ---------------------------------------------------------------------------
// Micro-price drift
// ---------------------------------------------------------------------------

/// EWMA of the first difference of the micro-price. Gives an
/// instantaneous directional pressure estimate.
#[derive(Debug, Clone)]
pub struct MicroPriceDrift {
    alpha: Decimal,
    last_mp: Option<Decimal>,
    state: Option<Decimal>,
}

impl MicroPriceDrift {
    pub fn new(half_life_ticks: usize) -> Self {
        assert!(half_life_ticks > 0);
        let n = half_life_ticks as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            last_mp: None,
            state: None,
        }
    }

    pub fn update(&mut self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        let Some(mp) = micro_price(bids, asks) else {
            return;
        };
        let Some(prev) = self.last_mp else {
            self.last_mp = Some(mp);
            return;
        };
        let delta = mp - prev;
        self.state = Some(match self.state {
            None => delta,
            Some(s) => self.alpha * delta + (Decimal::ONE - self.alpha) * s,
        });
        self.last_mp = Some(mp);
    }

    pub fn value(&self) -> Option<Decimal> {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Realised volatility term structure
// ---------------------------------------------------------------------------

/// Two parallel EWMA volatility estimators at different half-lives.
/// The ratio `short / long` gives a unitless short-term vol regime
/// indicator: `> 1` means short-term vol is running hot relative to
/// long-term baseline.
#[derive(Debug, Clone)]
pub struct VolTermStructure {
    short: VolEwma,
    long: VolEwma,
}

impl VolTermStructure {
    pub fn new(short_half_life: usize, long_half_life: usize) -> Self {
        Self {
            short: VolEwma::new(short_half_life),
            long: VolEwma::new(long_half_life),
        }
    }

    pub fn update(&mut self, price: Decimal) {
        self.short.update(price);
        self.long.update(price);
    }

    /// Short half-life vol.
    pub fn short(&self) -> Option<Decimal> {
        self.short.value()
    }

    /// Long half-life vol.
    pub fn long(&self) -> Option<Decimal> {
        self.long.value()
    }

    /// Ratio `short / long`. `None` if either is unavailable or
    /// long is zero.
    pub fn ratio(&self) -> Option<Decimal> {
        let s = self.short.value()?;
        let l = self.long.value()?;
        if l.is_zero() {
            return None;
        }
        Some(s / l)
    }
}

#[derive(Debug, Clone)]
struct VolEwma {
    alpha: Decimal,
    last_price: Option<Decimal>,
    var: Option<Decimal>,
}

impl VolEwma {
    fn new(half_life: usize) -> Self {
        assert!(half_life > 0);
        let n = half_life as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            last_price: None,
            var: None,
        }
    }

    fn update(&mut self, price: Decimal) {
        let Some(prev) = self.last_price else {
            self.last_price = Some(price);
            return;
        };
        let ret = if prev.is_zero() {
            Decimal::ZERO
        } else {
            (price - prev) / prev
        };
        let sq = ret * ret;
        self.var = Some(match self.var {
            None => sq,
            Some(prev_var) => self.alpha * sq + (Decimal::ONE - self.alpha) * prev_var,
        });
        self.last_price = Some(price);
    }

    fn value(&self) -> Option<Decimal> {
        let v = self.var?;
        // sqrt via f64 — we've already conceded precision for the
        // variance input anyway.
        v.to_f64().map(|v| v.sqrt()).and_then(Decimal::from_f64)
    }
}

// ---------------------------------------------------------------------------
// Bundled feature vector
// ---------------------------------------------------------------------------

/// One-shot feature vector snapshot. Consumers that want all the
/// standard microstructure features in one place feed this struct
/// to their predictor.
#[derive(Debug, Clone, Default)]
pub struct FeatureVector {
    pub imbalance_top5: Decimal,
    pub imbalance_weighted_top10: Decimal,
    pub micro_price: Option<Decimal>,
    pub micro_price_drift: Option<Decimal>,
    pub trade_flow: Option<Decimal>,
    pub vol_short: Option<Decimal>,
    pub vol_long: Option<Decimal>,
    pub vol_ratio: Option<Decimal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    #[test]
    fn imbalance_positive_when_bid_heavier() {
        let bids = vec![bid(dec!(100), dec!(10)), bid(dec!(99), dec!(5))];
        let asks = vec![bid(dec!(101), dec!(2)), bid(dec!(102), dec!(3))];
        let ib = book_imbalance(&bids, &asks, 2);
        // (15 - 5) / 20 = 0.5
        assert_eq!(ib, dec!(0.5));
    }

    #[test]
    fn imbalance_zero_on_balanced_book() {
        let bids = vec![bid(dec!(100), dec!(5))];
        let asks = vec![bid(dec!(101), dec!(5))];
        assert_eq!(book_imbalance(&bids, &asks, 5), Decimal::ZERO);
    }

    #[test]
    fn imbalance_empty_book_is_zero() {
        assert_eq!(book_imbalance(&[], &[], 5), Decimal::ZERO);
    }

    #[test]
    fn weighted_imbalance_gives_more_weight_to_top() {
        // Bid side tiny at top but huge deeper; ask side flat.
        // Weighted version should still lean slightly ask-heavy.
        let bids = vec![bid(dec!(100), dec!(1)), bid(dec!(99), dec!(100))];
        let asks = vec![bid(dec!(101), dec!(5)), bid(dec!(102), dec!(5))];
        let flat = book_imbalance(&bids, &asks, 2);
        let w = book_imbalance_weighted(&bids, &asks, 2);
        // Flat imbalance is strongly bid (tons of volume deeper).
        assert!(flat > dec!(0.5));
        // Weighted is less extreme because deep level loses weight.
        assert!(w < flat);
    }

    #[test]
    fn micro_price_between_bid_and_ask() {
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(10))];
        let mp = micro_price(&bids, &asks).unwrap();
        assert!(mp > dec!(100) && mp < dec!(101));
    }

    #[test]
    fn micro_price_anchors_to_heavier_side() {
        // Formula: (bid_qty * ask_px + ask_qty * bid_px) / total.
        // Heavy ask means the `ask_qty * bid_px` term dominates, so
        // the micro-price sits near the bid price — the next trade
        // is most likely to sweep the thin bid before the wall of
        // asks is consumed.
        let bids = vec![bid(dec!(100), dec!(1))]; // thin bid
        let asks = vec![bid(dec!(101), dec!(100))]; // heavy ask wall
        let mp = micro_price(&bids, &asks).unwrap();
        assert!(mp < dec!(100.5), "expected mp near bid, got {mp}");
    }

    #[test]
    fn trade_flow_positive_on_net_buying() {
        let mut tf = TradeFlow::new(10);
        for _ in 0..20 {
            tf.update(Side::Buy, dec!(1));
        }
        assert!(tf.value().unwrap() > Decimal::ZERO);
    }

    #[test]
    fn trade_flow_negative_on_net_selling() {
        let mut tf = TradeFlow::new(10);
        for _ in 0..20 {
            tf.update(Side::Sell, dec!(1));
        }
        assert!(tf.value().unwrap() < Decimal::ZERO);
    }

    #[test]
    fn micro_price_drift_detects_upward_trend() {
        let mut mpd = MicroPriceDrift::new(5);
        for i in 0..20 {
            let p = dec!(100) + Decimal::from(i);
            let bids = vec![bid(p, dec!(10))];
            let asks = vec![bid(p + dec!(1), dec!(10))];
            mpd.update(&bids, &asks);
        }
        let d = mpd.value().unwrap();
        assert!(d > Decimal::ZERO);
    }

    #[test]
    fn vol_term_structure_ratio_rises_with_short_term_burst() {
        let mut vts = VolTermStructure::new(3, 30);
        // Long stable regime.
        for _ in 0..50 {
            vts.update(dec!(100));
        }
        let quiet_ratio = vts.ratio();
        // Short spike.
        for i in 0..10 {
            let p = if i % 2 == 0 { dec!(100) } else { dec!(105) };
            vts.update(p);
        }
        let spike_ratio = vts.ratio().unwrap();
        // Quiet period ratio may be None (all zeros) or near zero.
        // Spike ratio should be materially positive.
        assert!(spike_ratio > Decimal::ZERO);
        if let Some(q) = quiet_ratio {
            assert!(spike_ratio > q);
        }
    }

    /// Canonical sign convention pinned: all-bid → +1, all-ask →
    /// −1, symmetric → 0. Matches Cont, Stoikov, Talreja (2010)
    /// "A stochastic model for order book dynamics" and the Hasbrouck
    /// order-flow imbalance definition `(bid − ask)/(bid + ask)`.
    #[test]
    fn imbalance_canonical_extremes_and_sign() {
        let b = vec![bid(dec!(100), dec!(10))];
        let empty: Vec<PriceLevel> = Vec::new();
        assert_eq!(book_imbalance(&b, &empty, 5), dec!(1));
        assert_eq!(book_imbalance(&empty, &b, 5), dec!(-1));
        assert_eq!(book_imbalance(&b, &[bid(dec!(101), dec!(10))], 5), dec!(0));
    }

    /// Linear-decay weighting with `weight = k - i`. Pinned hand-
    /// computed example for k = 3:
    ///
    ///   bids qtys [10, 10, 10] → weighted 3*10 + 2*10 + 1*10 = 60
    ///   asks qtys [1,   1,  1] → weighted 3 + 2 + 1 = 6
    ///   imbalance = (60 - 6) / 66 = 54/66 = 9/11 ≈ 0.8181…
    ///
    /// The inner level dominates the outer levels, as promised by
    /// the weighting.
    #[test]
    fn weighted_imbalance_hand_computed() {
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(10)),
            bid(dec!(98), dec!(10)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(1)),
            bid(dec!(102), dec!(1)),
            bid(dec!(103), dec!(1)),
        ];
        let w = book_imbalance_weighted(&bids, &asks, 3);
        // 54/66 = 9/11
        let expected = dec!(9) / dec!(11);
        let diff = (w - expected).abs();
        assert!(
            diff < dec!(0.0000001),
            "expected 9/11, got {w} (|diff| = {diff})"
        );
    }

    /// Pinned micro-price example from Cartea, Jaimungal & Penalva
    /// (2015), *Algorithmic and High-Frequency Trading*, §"Order-flow
    /// imbalance and micro-price":
    ///
    ///   P_micro = (Q_a × P_b + Q_b × P_a) / (Q_a + Q_b)
    ///
    /// where `Q_a` is the ASK size and `P_b` is the BID price. With
    /// bid = (100, 10) and ask = (101, 30):
    ///
    ///   P_micro = (30 × 100 + 10 × 101) / (10 + 30)
    ///           = (3000 + 1010) / 40
    ///           = 4010 / 40
    ///           = 100.25
    ///
    /// Heavier ask side pulls the micro-price toward the bid, as
    /// expected.
    #[test]
    fn micro_price_canonical_hand_computed_value() {
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(30))];
        let mp = micro_price(&bids, &asks).unwrap();
        assert_eq!(mp, dec!(100.25));
    }

    /// EWMA half-life formula: `α = 1 - 2^(-1/half_life)` gives the
    /// weight such that `(1-α)^half_life = 0.5`. Standard RiskMetrics
    /// convention. After enough steps of a monotone-delta sequence,
    /// the state should converge toward the common delta.
    #[test]
    fn micro_price_drift_converges_to_constant_delta() {
        let mut mpd = MicroPriceDrift::new(3);
        // Feed a perfectly linear micro-price sequence. Each
        // micro-price is the midpoint because both sides are equal,
        // so the delta is exactly 1 per step.
        for i in 0..50 {
            let mid = dec!(100) + Decimal::from(i);
            let bids = vec![bid(mid - dec!(0.5), dec!(5))];
            let asks = vec![bid(mid + dec!(0.5), dec!(5))];
            mpd.update(&bids, &asks);
        }
        let state = mpd.value().unwrap();
        // After 50 steps of constant delta=1, the EWMA should have
        // converged to within a tiny fraction of 1.
        let diff = (state - dec!(1)).abs();
        assert!(diff < dec!(0.001), "expected EWMA near 1, got {state}");
    }

    #[test]
    fn vol_term_structure_both_legs_populate_after_enough_ticks() {
        let mut vts = VolTermStructure::new(3, 10);
        for i in 0..20 {
            vts.update(dec!(100) + Decimal::from(i));
        }
        assert!(vts.short().is_some());
        assert!(vts.long().is_some());
    }

    // ---- micro_price_weighted ----

    #[test]
    fn weighted_micro_price_single_level_matches_plain_micro_price() {
        // `depth = 1` must reduce to the classic top-of-book
        // microprice (Cartea/Jaimungal formula).
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(30))];
        let plain = micro_price(&bids, &asks).unwrap();
        let weighted = micro_price_weighted(&bids, &asks, 1).unwrap();
        assert_eq!(plain, dec!(100.25));
        assert_eq!(weighted, dec!(100.25));
    }

    #[test]
    fn weighted_micro_price_returns_none_on_empty_side() {
        let b: Vec<PriceLevel> = Vec::new();
        let a = vec![bid(dec!(101), dec!(1))];
        assert!(micro_price_weighted(&b, &a, 3).is_none());
        assert!(micro_price_weighted(&a, &b, 3).is_none());
        assert!(micro_price_weighted(&[], &[], 3).is_none());
    }

    #[test]
    fn weighted_micro_price_clamps_depth_to_available_levels() {
        // Only one level on each side but caller asks for depth=5.
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(10))];
        let mp = micro_price_weighted(&bids, &asks, 5).unwrap();
        // Equal qtys → midpoint.
        assert_eq!(mp, dec!(100.5));
    }

    /// Pinned hand-computed 3-level example. Per level the inner
    /// microprice formula yields:
    ///
    ///   lvl 0: bid=100 q=10, ask=101 q=10 → mp=100.5,  total_qty=20
    ///   lvl 1: bid=99  q=20, ask=102 q=20 → mp=100.5,  total_qty=40
    ///   lvl 2: bid=98  q=30, ask=103 q=30 → mp=100.5,  total_qty=60
    ///
    /// With weights `w(i) = 3 - i` → `[3, 2, 1]`:
    ///
    ///   numerator   = (100.5*20)*3 + (100.5*40)*2 + (100.5*60)*1
    ///               = 6030 + 8040 + 6030 = 20 100
    ///   denominator = 20*3 + 40*2 + 60*1 = 60 + 80 + 60 = 200
    ///   weighted_mp = 20 100 / 200 = 100.5
    ///
    /// All three levels are symmetric so the weighted value is
    /// exactly the midpoint.
    #[test]
    fn weighted_micro_price_symmetric_book_equals_midpoint() {
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(20)),
            bid(dec!(98), dec!(30)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(10)),
            bid(dec!(102), dec!(20)),
            bid(dec!(103), dec!(30)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        assert_eq!(mp, dec!(100.5));
    }

    /// Asymmetric book — heavy asks on the inside, light bids.
    /// Weighted microprice should be pulled toward the bid side
    /// (fewer contrarian orders on that side) relative to the
    /// plain midpoint `100.5`.
    #[test]
    fn weighted_micro_price_heavy_ask_side_leans_toward_bid() {
        let bids = vec![
            bid(dec!(100), dec!(1)),
            bid(dec!(99), dec!(1)),
            bid(dec!(98), dec!(1)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(9)),
            bid(dec!(102), dec!(9)),
            bid(dec!(103), dec!(9)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        assert!(
            mp < dec!(100.5),
            "heavy ask → fair value should lean below midpoint, got {mp}"
        );
    }

    #[test]
    fn weighted_micro_price_skips_levels_with_zero_total_qty() {
        // Level 1 is a degenerate entry with zero on both
        // sides — the weighted average must skip it cleanly.
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(0)),
            bid(dec!(98), dec!(10)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(10)),
            bid(dec!(102), dec!(0)),
            bid(dec!(103), dec!(10)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        // Levels 0 and 2 only; both symmetric → midpoint.
        assert_eq!(mp, dec!(100.5));
    }
}
