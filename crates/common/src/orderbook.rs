use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::types::{Price, PriceLevel, Qty};

/// Local mirror of the exchange order book, maintained via WebSocket updates.
#[derive(Debug, Clone)]
pub struct LocalOrderBook {
    pub symbol: String,
    /// Bids sorted descending (highest first).
    pub bids: BTreeMap<Price, Qty>,
    /// Asks sorted ascending (lowest first).
    pub asks: BTreeMap<Price, Qty>,
    pub sequence: u64,
    pub last_update_ms: i64,
}

impl LocalOrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            sequence: 0,
            last_update_ms: 0,
        }
    }

    /// Apply a full snapshot (replaces all levels).
    pub fn apply_snapshot(&mut self, bids: Vec<PriceLevel>, asks: Vec<PriceLevel>, seq: u64) {
        self.bids.clear();
        self.asks.clear();
        for level in bids {
            self.bids.insert(level.price, level.qty);
        }
        for level in asks {
            self.asks.insert(level.price, level.qty);
        }
        self.sequence = seq;
        self.last_update_ms = chrono::Utc::now().timestamp_millis();
    }

    /// Apply incremental update (qty=0 means remove level).
    pub fn apply_delta(&mut self, bids: Vec<PriceLevel>, asks: Vec<PriceLevel>, seq: u64) {
        if seq <= self.sequence {
            return; // stale update
        }
        for level in bids {
            if level.qty.is_zero() {
                self.bids.remove(&level.price);
            } else {
                self.bids.insert(level.price, level.qty);
            }
        }
        for level in asks {
            if level.qty.is_zero() {
                self.asks.remove(&level.price);
            } else {
                self.asks.insert(level.price, level.qty);
            }
        }
        self.sequence = seq;
        self.last_update_ms = chrono::Utc::now().timestamp_millis();
    }

    /// Best bid price.
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    /// Best ask price.
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// Mid price = (best_bid + best_ask) / 2.
    pub fn mid_price(&self) -> Option<Price> {
        let two = Decimal::from(2);
        Some((self.best_bid()? + self.best_ask()?) / two)
    }

    /// Spread in absolute terms.
    pub fn spread(&self) -> Option<Price> {
        Some(self.best_ask()? - self.best_bid()?)
    }

    /// Spread as a fraction of mid price.
    pub fn spread_bps(&self) -> Option<Decimal> {
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        let ten_k = Decimal::from(10_000);
        Some(self.spread()? / mid * ten_k)
    }

    /// Weighted mid price using top-of-book quantities.
    pub fn weighted_mid(&self) -> Option<Price> {
        let bid_price = self.best_bid()?;
        let ask_price = self.best_ask()?;
        let bid_qty = self.bids.get(&bid_price)?;
        let ask_qty = self.asks.get(&ask_price)?;
        let total = bid_qty + ask_qty;
        if total.is_zero() {
            return self.mid_price();
        }
        // Weighted mid: more weight to the side with less quantity (imbalance).
        Some((bid_price * ask_qty + ask_price * bid_qty) / total)
    }

    /// Total bid depth up to N levels.
    pub fn bid_depth(&self, levels: usize) -> Qty {
        self.bids.values().rev().take(levels).sum()
    }

    /// Total ask depth up to N levels.
    pub fn ask_depth(&self, levels: usize) -> Qty {
        self.asks.values().take(levels).sum()
    }

    /// Book imbalance = (bid_depth - ask_depth) / (bid_depth + ask_depth).
    /// Range: -1.0 (all asks) to +1.0 (all bids).
    pub fn imbalance(&self, levels: usize) -> Option<Decimal> {
        let bid_d = self.bid_depth(levels);
        let ask_d = self.ask_depth(levels);
        let total = bid_d + ask_d;
        if total.is_zero() {
            return None;
        }
        Some((bid_d - ask_d) / total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn level(price: &str, qty: &str) -> PriceLevel {
        PriceLevel {
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
        }
    }

    #[test]
    fn test_snapshot_and_mid() {
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![level("50000", "1.0"), level("49999", "2.0")],
            vec![level("50001", "1.5"), level("50002", "3.0")],
            1,
        );

        assert_eq!(book.best_bid(), Some(dec!(50000)));
        assert_eq!(book.best_ask(), Some(dec!(50001)));
        assert_eq!(book.mid_price(), Some(dec!(50000.5)));
    }

    #[test]
    fn test_delta_removes_level() {
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(vec![level("100", "5.0")], vec![level("101", "5.0")], 1);

        book.apply_delta(
            vec![level("100", "0")], // remove bid
            vec![],
            2,
        );

        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), Some(dec!(101)));
    }

    #[test]
    fn test_stale_delta_ignored() {
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(vec![level("100", "5.0")], vec![level("101", "5.0")], 5);

        book.apply_delta(vec![level("100", "0")], vec![], 3); // stale
        assert_eq!(book.best_bid(), Some(dec!(100))); // not removed
    }

    #[test]
    fn test_imbalance() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(vec![level("100", "10.0")], vec![level("101", "5.0")], 1);
        let imb = book.imbalance(5).unwrap();
        // (10 - 5) / (10 + 5) = 5/15 = 0.333...
        assert!(imb > dec!(0.33) && imb < dec!(0.34));
    }
}
