use std::collections::BTreeMap;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::types::{Price, PriceLevel, Qty, Side};

/// MM-1 — Result of a simulated book sweep. Returned by
/// [`LocalOrderBook::sweep_vwap`]. `impact_bps` is the cost of
/// taking `target_qty` right now: the absolute delta between
/// the achievable VWAP and current mid, always non-negative for
/// the side we're taking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepResult {
    pub side: Side,
    /// Size the caller asked us to fill.
    pub target_qty: Qty,
    /// Qty the book actually covers. Equals `target_qty` when
    /// `fully_filled`; smaller otherwise.
    pub filled_qty: Qty,
    /// Total quote notional `sum(price * qty)` over the filled
    /// portion.
    pub notional: Decimal,
    /// Volume-weighted average price across the filled portion.
    pub vwap: Price,
    /// `|vwap - mid| / mid * 10_000`, clamped to 0 on the
    /// favourable side. Cost in bps.
    pub impact_bps: Decimal,
    /// True when the book was deep enough to cover `target_qty`.
    pub fully_filled: bool,
}

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

    /// MM-1 — Book-sweep VWAP estimate.
    ///
    /// Simulates walking from the top of `side` (buying sweeps
    /// asks ascending; selling sweeps bids descending) and
    /// filling as much of `target_qty` as the book can support.
    /// Returns the filled qty, the notional consumed / received,
    /// the VWAP over the filled portion, and the absolute
    /// adverse-side impact in bps vs current mid.
    ///
    /// `impact_bps` is always non-negative — it's the cost, not
    /// a signed delta. A buy that sweeps a thin ask book gets a
    /// VWAP above mid → positive impact; a sell through a thin
    /// bid book gets a VWAP below mid → positive impact. Call
    /// sites treat it as "how many bps did taking this size
    /// actually cost us right now".
    ///
    /// Returns `None` when the book side is empty or mid price
    /// is zero / missing. Partial fill (book shallower than
    /// `target_qty`) returns `Some` with `fully_filled = false`.
    pub fn sweep_vwap(&self, side: Side, target_qty: Qty) -> Option<SweepResult> {
        if target_qty <= Decimal::ZERO {
            return None;
        }
        let mid = self.mid_price()?;
        if mid <= Decimal::ZERO {
            return None;
        }
        let mut remaining = target_qty;
        let mut filled = Decimal::ZERO;
        let mut notional = Decimal::ZERO;
        // Walk from best: buying sweeps asks ascending; selling
        // sweeps bids descending.
        let levels: Box<dyn Iterator<Item = (&Price, &Qty)>> = match side {
            Side::Buy => Box::new(self.asks.iter()),
            Side::Sell => Box::new(self.bids.iter().rev()),
        };
        for (price, qty) in levels {
            if remaining <= Decimal::ZERO {
                break;
            }
            let take = if *qty >= remaining { remaining } else { *qty };
            filled += take;
            notional += take * price;
            remaining -= take;
        }
        if filled <= Decimal::ZERO {
            return None;
        }
        let vwap = notional / filled;
        // Absolute adverse-side impact in bps of mid. Always
        // non-negative for the side we're taking.
        let signed = match side {
            Side::Buy => vwap - mid,
            Side::Sell => mid - vwap,
        };
        let ten_k = Decimal::from(10_000);
        let impact_bps = (signed / mid * ten_k).max(Decimal::ZERO);
        Some(SweepResult {
            side,
            target_qty,
            filled_qty: filled,
            notional,
            vwap,
            impact_bps,
            fully_filled: remaining <= Decimal::ZERO,
        })
    }

    /// Convenience — just the `(vwap, impact_bps)` pair. For
    /// callers that only need the cost estimate, not the full
    /// fill breakdown.
    pub fn impact_bps(&self, side: Side, size: Qty) -> Option<(Price, Decimal)> {
        let r = self.sweep_vwap(side, size)?;
        Some((r.vwap, r.impact_bps))
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

    // ── MM-1: sweep_vwap + impact_bps ─────────────────────

    /// Buy sweep that fits entirely on the first ask level:
    /// VWAP is that level's price, impact is the one-tick spread
    /// half-ish (best_ask - mid).
    #[test]
    fn sweep_vwap_buy_within_first_level() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(
            vec![level("99", "5.0")],
            vec![level("101", "5.0"), level("102", "5.0")],
            1,
        );
        let r = book.sweep_vwap(Side::Buy, dec!(2)).unwrap();
        assert!(r.fully_filled);
        assert_eq!(r.filled_qty, dec!(2));
        assert_eq!(r.vwap, dec!(101));
        // mid = 100, vwap = 101 → 1/100 * 10_000 = 100 bps.
        assert_eq!(r.impact_bps, dec!(100));
    }

    /// Buy sweep that crosses two ask levels: VWAP is the
    /// notional-weighted mean.
    #[test]
    fn sweep_vwap_buy_crosses_levels() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(
            vec![level("99", "5.0")],
            vec![level("101", "1.0"), level("103", "4.0")],
            1,
        );
        let r = book.sweep_vwap(Side::Buy, dec!(3)).unwrap();
        assert!(r.fully_filled);
        // Notional = 101 * 1 + 103 * 2 = 307. VWAP = 307 / 3 ≈ 102.333…
        assert_eq!(r.filled_qty, dec!(3));
        assert_eq!(r.notional, dec!(307));
        assert!(r.vwap > dec!(102.33) && r.vwap < dec!(102.34));
    }

    /// Sell sweep descends the bid side best-first.
    #[test]
    fn sweep_vwap_sell_descends_bids() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(
            vec![level("99", "1.0"), level("98", "4.0")],
            vec![level("101", "5.0")],
            1,
        );
        let r = book.sweep_vwap(Side::Sell, dec!(3)).unwrap();
        assert!(r.fully_filled);
        // Notional = 99 * 1 + 98 * 2 = 295. VWAP = 98.333…
        assert_eq!(r.filled_qty, dec!(3));
        assert!(r.vwap > dec!(98.33) && r.vwap < dec!(98.34));
        // mid = 100, vwap = 98.333 → (100 - 98.333)/100 * 10_000
        // ≈ 166.67 bps.
        assert!(r.impact_bps > dec!(166) && r.impact_bps < dec!(167));
    }

    /// Partial fill — book is shallower than target.
    #[test]
    fn sweep_vwap_partial_fill_returns_available() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(
            vec![level("99", "5.0")],
            vec![level("101", "1.0"), level("102", "1.0")],
            1,
        );
        let r = book.sweep_vwap(Side::Buy, dec!(10)).unwrap();
        assert!(!r.fully_filled);
        assert_eq!(r.filled_qty, dec!(2));
    }

    /// Empty side or zero target → None.
    #[test]
    fn sweep_vwap_edge_cases_return_none() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(vec![level("99", "5.0")], vec![], 1);
        // Empty ask side.
        assert!(book.sweep_vwap(Side::Buy, dec!(1)).is_none());
        // Zero target.
        book.apply_snapshot(vec![level("99", "5.0")], vec![level("101", "5.0")], 2);
        assert!(book.sweep_vwap(Side::Buy, dec!(0)).is_none());
        // Negative target.
        assert!(book.sweep_vwap(Side::Buy, dec!(-1)).is_none());
    }

    /// `impact_bps` is just the `(vwap, impact)` pair from a
    /// full sweep — sanity-check they agree.
    #[test]
    fn impact_bps_agrees_with_sweep_vwap() {
        let mut book = LocalOrderBook::new("T".into());
        book.apply_snapshot(
            vec![level("99", "5.0")],
            vec![level("101", "1.0"), level("103", "4.0")],
            1,
        );
        let full = book.sweep_vwap(Side::Buy, dec!(3)).unwrap();
        let (vwap, impact) = book.impact_bps(Side::Buy, dec!(3)).unwrap();
        assert_eq!(vwap, full.vwap);
        assert_eq!(impact, full.impact_bps);
    }
}
