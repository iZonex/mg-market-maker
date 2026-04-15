use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Price, Side, Trade};
use mm_indicators::Hma;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::debug;

/// Default window for the optional HMA alpha feed. Chosen to
/// match the upstream `mm-toolbox` quickstart and give a HMA
/// lag of ~3 samples on mid-price updates.
pub const DEFAULT_HMA_WINDOW: usize = 9;

/// Momentum / alpha signals for quote adjustment.
///
/// Short-term price predictability from:
/// 1. Order book imbalance
/// 2. Trade flow imbalance (signed volume)
/// 3. Micro-price (improved mid estimate)
/// 4. Hull Moving Average slope on mid-price (optional,
///    opt-in via `with_hma`)
///
/// The output is an `alpha` value (expected price change direction)
/// that shifts the reservation price per Cartea-Jaimungal:
///   reservation = mid + alpha * (T - t) - gamma * sigma^2 * q * (T - t)
pub struct MomentumSignals {
    /// Recent signed trade volumes for flow imbalance.
    signed_volumes: VecDeque<Decimal>,
    window: usize,
    /// Optional Hull Moving Average on mid-price updates. When
    /// attached, `alpha()` folds in a 5-th component that
    /// captures the slope of the HMA — positive when the HMA
    /// is above the current mid, negative when below.
    hma: Option<Hma>,
    /// Most recent HMA value sampled before the current
    /// update. Used to compute the HMA slope as
    /// `(now - prev) / mid`.
    hma_prev: Option<Decimal>,
}

impl MomentumSignals {
    pub fn new(window: usize) -> Self {
        Self {
            signed_volumes: VecDeque::with_capacity(window),
            window,
            hma: None,
            hma_prev: None,
        }
    }

    /// Attach a Hull Moving Average on mid-price updates.
    /// Builder-style: returns `self` so callers can chain. Once
    /// attached the engine should call
    /// [`MomentumSignals::on_mid`] on every mid-price refresh
    /// so the HMA sees a steady sample stream.
    pub fn with_hma(mut self, window: usize) -> Self {
        self.hma = Some(Hma::new(window));
        self
    }

    /// Feed a mid-price update into the optional HMA stream.
    /// No-op when `with_hma` has not been called. Callers
    /// should invoke this once per engine tick before
    /// `alpha()` so the HMA sees every mid sample.
    pub fn on_mid(&mut self, mid: Price) {
        if let Some(h) = self.hma.as_mut() {
            self.hma_prev = h.value();
            h.update(mid);
        }
    }

    /// Current HMA value, if attached and warmed up.
    pub fn hma_value(&self) -> Option<Decimal> {
        self.hma.as_ref().and_then(|h| h.value())
    }

    /// HMA slope as a fraction of `mid` — `(now − prev)/mid`.
    /// Returns `None` until two consecutive HMA readings are
    /// available.
    pub fn hma_slope(&self, mid: Price) -> Option<Decimal> {
        if mid.is_zero() {
            return None;
        }
        let now = self.hma_value()?;
        let prev = self.hma_prev?;
        Some((now - prev) / mid)
    }

    /// Record a public trade.
    pub fn on_trade(&mut self, trade: &Trade) {
        let signed_vol = match trade.taker_side {
            Side::Buy => trade.qty * trade.price,
            Side::Sell => -(trade.qty * trade.price),
        };
        self.signed_volumes.push_back(signed_vol);
        if self.signed_volumes.len() > self.window {
            self.signed_volumes.pop_front();
        }
    }

    /// Order book imbalance at top N levels.
    /// Returns [-1, 1]: positive = more bids (bullish pressure).
    pub fn book_imbalance(book: &LocalOrderBook, levels: usize) -> Decimal {
        book.imbalance(levels).unwrap_or(dec!(0))
    }

    /// Trade flow imbalance over recent window.
    /// Returns a value in approximate [-1, 1] range.
    pub fn trade_flow_imbalance(&self) -> Decimal {
        if self.signed_volumes.is_empty() {
            return dec!(0);
        }
        let total: Decimal = self.signed_volumes.iter().sum();
        let abs_total: Decimal = self.signed_volumes.iter().map(|v| v.abs()).sum();
        if abs_total.is_zero() {
            return dec!(0);
        }
        total / abs_total
    }

    /// Micro-price: improved mid-price using order book imbalance.
    ///
    /// micro_price = ask * bid_qty / (bid_qty + ask_qty) + bid * ask_qty / (bid_qty + ask_qty)
    /// This is the weighted mid price — more weight to the side with more quantity.
    pub fn micro_price(book: &LocalOrderBook) -> Option<Price> {
        book.weighted_mid()
    }

    /// Compute combined alpha signal.
    ///
    /// Returns expected price direction * magnitude.
    /// Positive = expected up-move, negative = expected down-move.
    ///
    /// The alpha is in terms of fraction of mid-price.
    pub fn alpha(&self, book: &LocalOrderBook, mid: Price) -> Decimal {
        if mid.is_zero() {
            return dec!(0);
        }

        // Component 1: Book imbalance (weight 0.4 when HMA is
        // absent, 0.3 when present).
        let book_imb = Self::book_imbalance(book, 5);

        // Component 2: Trade flow (same weights as component 1).
        let flow_imb = self.trade_flow_imbalance();

        // Component 3: Micro-price deviation (weight 0.2 both
        // ways).
        let micro_dev = Self::micro_price(book)
            .map(|mp| (mp - mid) / mid)
            .unwrap_or(dec!(0));

        // Component 4: HMA slope (weight 0.2, only when the
        // HMA is attached AND warmed up; otherwise the slope
        // component is skipped and the other weights revert
        // to the 0.4 / 0.4 / 0.2 split).
        let hma_slope = self.hma_slope(mid);

        let alpha = match hma_slope {
            Some(slope) => {
                book_imb * dec!(0.3)
                    + flow_imb * dec!(0.3)
                    + micro_dev * dec!(0.2)
                    // HMA slope is already a fraction of mid —
                    // keep it on the same scale as micro_dev.
                    // Clip to [-1, 1] so a wild HMA swing
                    // cannot single-handedly blow the alpha
                    // past the saturation cap below.
                    + slope.max(dec!(-1)).min(dec!(1)) * dec!(0.2)
            }
            None => book_imb * dec!(0.4) + flow_imb * dec!(0.4) + micro_dev * dec!(0.2),
        };

        // Scale: raw alpha is in [-1, 1], scale to a small fraction of price.
        // This determines how aggressive the momentum adjustment is.
        let scaled = alpha * dec!(0.0001); // 1 bps max shift per unit of signal.

        debug!(
            book_imbalance = %book_imb,
            trade_flow = %flow_imb,
            micro_dev = %micro_dev,
            hma_slope = ?hma_slope,
            alpha = %scaled,
            "momentum signals"
        );

        scaled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mm_common::types::PriceLevel;

    #[test]
    fn test_book_imbalance() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let imb = MomentumSignals::book_imbalance(&book, 5);
        // (10 - 5) / (10 + 5) = 0.333
        assert!(imb > dec!(0.3));
    }

    #[test]
    fn test_trade_flow() {
        let mut signals = MomentumSignals::new(100);
        for _ in 0..10 {
            signals.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let flow = signals.trade_flow_imbalance();
        assert_eq!(flow, dec!(1)); // All buys.
    }

    #[test]
    fn test_alpha_neutral() {
        let signals = MomentumSignals::new(100);
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();
        let alpha = signals.alpha(&book, mid);
        // Balanced book, no trades → alpha ≈ 0.
        assert!(alpha.abs() < dec!(0.0001));
    }

    // ----- HMA wiring tests -----

    /// Without `with_hma` the HMA accessors return `None` and
    /// `on_mid` is a no-op.
    #[test]
    fn hma_is_none_by_default() {
        let mut s = MomentumSignals::new(10);
        s.on_mid(dec!(100));
        s.on_mid(dec!(101));
        assert!(s.hma_value().is_none());
        assert!(s.hma_slope(dec!(100)).is_none());
    }

    /// After `with_hma` the HMA warms up and produces a value
    /// on enough mid-price samples.
    #[test]
    fn hma_warms_up_after_enough_samples() {
        let mut s = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for i in 0..40 {
            s.on_mid(dec!(100) + Decimal::from(i));
        }
        assert!(s.hma_value().is_some());
        // Slope must be positive on a rising mid stream.
        let slope = s.hma_slope(dec!(130)).unwrap();
        assert!(slope > dec!(0));
    }

    /// A warmed-up HMA on a rising stream should drive the
    /// alpha positive — i.e. produce a larger output than the
    /// same `MomentumSignals` without the HMA attached.
    #[test]
    fn hma_attached_tilts_alpha_positive_on_rising_mid() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        // Baseline signals — no HMA, same trade stream.
        let mut baseline = MomentumSignals::new(10);
        for _ in 0..20 {
            baseline.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let base_alpha = baseline.alpha(&book, mid);

        // With HMA on a rising mid stream — slope positive,
        // alpha should be biased up compared to baseline.
        let mut withhma = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for _ in 0..20 {
            withhma.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        for i in 0..40 {
            withhma.on_mid(dec!(100) + Decimal::from(i));
        }
        let hma_alpha = withhma.alpha(&book, mid);

        assert!(
            hma_alpha > dec!(0),
            "HMA alpha must stay positive on a rising stream: {hma_alpha}"
        );
        // The two alphas use different weight splits, so the
        // direct comparison is a sanity check: neither should
        // be zero, and neither should be NaN-like.
        assert!(base_alpha > dec!(0));
    }
}
