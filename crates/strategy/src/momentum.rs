use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Price, Side, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::debug;

/// Momentum / alpha signals for quote adjustment.
///
/// Short-term price predictability from:
/// 1. Order book imbalance
/// 2. Trade flow imbalance (signed volume)
/// 3. Micro-price (improved mid estimate)
///
/// The output is an `alpha` value (expected price change direction)
/// that shifts the reservation price per Cartea-Jaimungal:
///   reservation = mid + alpha * (T - t) - gamma * sigma^2 * q * (T - t)
pub struct MomentumSignals {
    /// Recent signed trade volumes for flow imbalance.
    signed_volumes: VecDeque<Decimal>,
    window: usize,
}

impl MomentumSignals {
    pub fn new(window: usize) -> Self {
        Self {
            signed_volumes: VecDeque::with_capacity(window),
            window,
        }
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

        // Component 1: Book imbalance (weight 0.4).
        let book_imb = Self::book_imbalance(book, 5);

        // Component 2: Trade flow (weight 0.4).
        let flow_imb = self.trade_flow_imbalance();

        // Component 3: Micro-price deviation (weight 0.2).
        let micro_dev = Self::micro_price(book)
            .map(|mp| (mp - mid) / mid)
            .unwrap_or(dec!(0));

        let alpha = book_imb * dec!(0.4) + flow_imb * dec!(0.4) + micro_dev * dec!(0.2);

        // Scale: raw alpha is in [-1, 1], scale to a small fraction of price.
        // This determines how aggressive the momentum adjustment is.
        let scaled = alpha * dec!(0.0001); // 1 bps max shift per unit of signal.

        debug!(
            book_imbalance = %book_imb,
            trade_flow = %flow_imb,
            micro_dev = %micro_dev,
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
}
