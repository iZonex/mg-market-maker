use std::collections::BTreeMap;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

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

    /// Qty at the best bid price.
    pub fn best_bid_qty(&self) -> Option<Qty> {
        self.bids.iter().next_back().map(|(_, q)| *q)
    }

    /// Qty at the best ask price.
    pub fn best_ask_qty(&self) -> Option<Qty> {
        self.asks.iter().next().map(|(_, q)| *q)
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

    /// Bid depth within `pct` percent of mid price, in quote asset (qty * price).
    pub fn bid_depth_within_pct_quote(&self, pct: Decimal) -> Decimal {
        let mid = match self.mid_price() {
            Some(m) if !m.is_zero() => m,
            _ => return dec!(0),
        };
        let hundred = Decimal::from(100);
        let lower_bound = mid * (Decimal::ONE - pct / hundred);
        self.bids
            .iter()
            .rev()
            .take_while(|(&price, _)| price >= lower_bound)
            .map(|(price, qty)| price * qty)
            .sum()
    }

    /// Ask depth within `pct` percent of mid price, in quote asset (qty * price).
    pub fn ask_depth_within_pct_quote(&self, pct: Decimal) -> Decimal {
        let mid = match self.mid_price() {
            Some(m) if !m.is_zero() => m,
            _ => return dec!(0),
        };
        let hundred = Decimal::from(100);
        let upper_bound = mid * (Decimal::ONE + pct / hundred);
        self.asks
            .iter()
            .take_while(|(&price, _)| price <= upper_bound)
            .map(|(price, qty)| price * qty)
            .sum()
    }

    /// Top-`n` bid levels as an **ordered vector** (best first).
    /// Used by feature extractors and the Market Resilience
    /// detector, which expect a best-first slice rather than
    /// the internal `BTreeMap`.
    pub fn top_bids(&self, n: usize) -> Vec<PriceLevel> {
        self.bids
            .iter()
            .rev()
            .take(n)
            .map(|(price, qty)| PriceLevel {
                price: *price,
                qty: *qty,
            })
            .collect()
    }

    /// Top-`n` ask levels as an **ordered vector** (best first).
    pub fn top_asks(&self, n: usize) -> Vec<PriceLevel> {
        self.asks
            .iter()
            .take(n)
            .map(|(price, qty)| PriceLevel {
                price: *price,
                qty: *qty,
            })
            .collect()
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

    #[test]
    fn test_bid_depth_within_pct_quote() {
        let mut book = LocalOrderBook::new("TEST".into());
        // Mid = (100+102)/2 = 101. 1% of 101 = 1.01 → lower bound = 99.99.
        // Bid at 100 (within 1%), bid at 98 (outside 1%).
        book.apply_snapshot(
            vec![level("100", "2.0"), level("98", "5.0")],
            vec![level("102", "1.0")],
            1,
        );
        let depth_1pct = book.bid_depth_within_pct_quote(dec!(1));
        // Only bid at 100 is within 1% of mid (101). Depth = 100 * 2 = 200.
        assert_eq!(depth_1pct, dec!(200));

        // 5% → lower bound = 101 * 0.95 = 95.95. Both bids are within.
        let depth_5pct = book.bid_depth_within_pct_quote(dec!(5));
        assert_eq!(depth_5pct, dec!(200) + dec!(490)); // 100*2 + 98*5
    }

    #[test]
    fn test_ask_depth_within_pct_quote() {
        let mut book = LocalOrderBook::new("TEST".into());
        // Mid = (100+102)/2 = 101. 1% of 101 = 1.01 → upper bound = 102.01.
        book.apply_snapshot(
            vec![level("100", "2.0")],
            vec![level("102", "3.0"), level("105", "1.0")],
            1,
        );
        let depth_1pct = book.ask_depth_within_pct_quote(dec!(1));
        // Ask at 102 is within 1% of 101. Depth = 102 * 3 = 306.
        assert_eq!(depth_1pct, dec!(306));

        // 5% → upper bound = 101 * 1.05 = 106.05. Both asks within.
        let depth_5pct = book.ask_depth_within_pct_quote(dec!(5));
        assert_eq!(depth_5pct, dec!(306) + dec!(105)); // 102*3 + 105*1
    }

    #[test]
    fn test_depth_within_pct_empty_book() {
        let book = LocalOrderBook::new("TEST".into());
        assert_eq!(book.bid_depth_within_pct_quote(dec!(1)), dec!(0));
        assert_eq!(book.ask_depth_within_pct_quote(dec!(1)), dec!(0));
    }

    /// Pins the bps scale so future refactors don't silently
    /// downshift the output to a fraction. A 99/101 book at
    /// mid=100 is a 2% spread → 200 bps.
    #[test]
    fn test_spread_bps_scale() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(vec![level("99", "1.0")], vec![level("101", "1.0")], 1);
        assert_eq!(book.mid_price(), Some(dec!(100)));
        assert_eq!(book.spread(), Some(dec!(2)));
        assert_eq!(book.spread_bps(), Some(dec!(200)));
    }

    /// Tight one-tick spread on a BTC-sized price (the real
    /// Binance BTCUSDT case) — 0.01/77000 * 10000 ≈ 0.001298.
    /// Confirms the formula still lands in bps units and not
    /// in a fraction.
    #[test]
    fn test_spread_bps_tight_btc_book() {
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![level("77000.00", "1.0")],
            vec![level("77000.01", "1.0")],
            1,
        );
        let bps = book.spread_bps().unwrap();
        // Spread = 0.01, mid = 77000.005, bps = 0.01/77000.005*10000
        // ≈ 0.001298940861516.
        assert!(bps > dec!(0.001) && bps < dec!(0.002));
    }
}
