use std::collections::{BTreeMap, HashMap};

use mm_common::types::{Price, PriceLevel, Qty};
use rust_decimal::Decimal;

use crate::connector::VenueId;

/// Per-venue quantity at a price level.
#[derive(Debug, Clone, Default)]
pub struct VenueQty {
    pub quantities: HashMap<VenueId, Qty>,
}

impl VenueQty {
    pub fn total(&self) -> Qty {
        self.quantities.values().sum()
    }

    pub fn set(&mut self, venue: VenueId, qty: Qty) {
        if qty.is_zero() {
            self.quantities.remove(&venue);
        } else {
            self.quantities.insert(venue, qty);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.quantities.is_empty()
    }
}

/// Unified order book aggregating data from multiple venues.
///
/// Each price level tracks how much liquidity each venue provides,
/// so the Smart Order Router knows WHERE to send orders.
#[derive(Debug, Clone)]
pub struct UnifiedOrderBook {
    pub symbol: String,
    pub bids: BTreeMap<Price, VenueQty>,
    pub asks: BTreeMap<Price, VenueQty>,
}

impl UnifiedOrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    /// Apply a full snapshot from a single venue.
    pub fn apply_venue_snapshot(
        &mut self,
        venue: VenueId,
        bids: &[PriceLevel],
        asks: &[PriceLevel],
    ) {
        // Remove this venue from all existing levels.
        self.remove_venue_bids(venue);
        self.remove_venue_asks(venue);

        for level in bids {
            self.bids
                .entry(level.price)
                .or_default()
                .set(venue, level.qty);
        }
        for level in asks {
            self.asks
                .entry(level.price)
                .or_default()
                .set(venue, level.qty);
        }

        self.cleanup_empty();
    }

    /// Apply a delta update from a single venue.
    pub fn apply_venue_delta(&mut self, venue: VenueId, bids: &[PriceLevel], asks: &[PriceLevel]) {
        for level in bids {
            self.bids
                .entry(level.price)
                .or_default()
                .set(venue, level.qty);
        }
        for level in asks {
            self.asks
                .entry(level.price)
                .or_default()
                .set(venue, level.qty);
        }
        self.cleanup_empty();
    }

    /// Best aggregated bid.
    pub fn best_bid(&self) -> Option<(Price, Qty)> {
        self.bids.iter().next_back().map(|(p, vq)| (*p, vq.total()))
    }

    /// Best aggregated ask.
    pub fn best_ask(&self) -> Option<(Price, Qty)> {
        self.asks.iter().next().map(|(p, vq)| (*p, vq.total()))
    }

    /// Mid price from the unified book.
    pub fn mid_price(&self) -> Option<Price> {
        let (bid, _) = self.best_bid()?;
        let (ask, _) = self.best_ask()?;
        Some((bid + ask) / Decimal::from(2))
    }

    /// Get the best venue to buy at a given price (lowest ask).
    pub fn best_venue_for_buy(&self, max_price: Price) -> Option<(VenueId, Price, Qty)> {
        for (price, vq) in &self.asks {
            if *price > max_price {
                break;
            }
            // Return the venue with the most quantity at this level.
            if let Some((&venue, &qty)) = vq.quantities.iter().max_by_key(|(_, q)| *q) {
                return Some((venue, *price, qty));
            }
        }
        None
    }

    /// Get the best venue to sell at a given price (highest bid).
    pub fn best_venue_for_sell(&self, min_price: Price) -> Option<(VenueId, Price, Qty)> {
        for (price, vq) in self.bids.iter().rev() {
            if *price < min_price {
                break;
            }
            if let Some((&venue, &qty)) = vq.quantities.iter().max_by_key(|(_, q)| *q) {
                return Some((venue, *price, qty));
            }
        }
        None
    }

    /// Total depth available across all venues.
    pub fn total_bid_depth(&self, levels: usize) -> Qty {
        self.bids
            .values()
            .rev()
            .take(levels)
            .map(|vq| vq.total())
            .sum()
    }

    pub fn total_ask_depth(&self, levels: usize) -> Qty {
        self.asks.values().take(levels).map(|vq| vq.total()).sum()
    }

    fn remove_venue_bids(&mut self, venue: VenueId) {
        for vq in self.bids.values_mut() {
            vq.quantities.remove(&venue);
        }
    }

    fn remove_venue_asks(&mut self, venue: VenueId) {
        for vq in self.asks.values_mut() {
            vq.quantities.remove(&venue);
        }
    }

    fn cleanup_empty(&mut self) {
        self.bids.retain(|_, vq| !vq.is_empty());
        self.asks.retain(|_, vq| !vq.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn lvl(price: &str, qty: &str) -> PriceLevel {
        PriceLevel {
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
        }
    }

    #[test]
    fn test_multi_venue_aggregation() {
        let mut book = UnifiedOrderBook::new("BTCUSDT".into());

        // Venue A: bid at 50000 with qty 1.0
        book.apply_venue_snapshot(
            VenueId::Binance,
            &[lvl("50000", "1.0")],
            &[lvl("50002", "1.5")],
        );

        // Venue B: bid at 50000 with qty 0.5, bid at 50001 with qty 2.0
        book.apply_venue_snapshot(
            VenueId::Custom,
            &[lvl("50000", "0.5"), lvl("50001", "2.0")],
            &[lvl("50001", "1.0")],
        );

        // Best bid should be 50001 (only on Custom).
        let (best_bid, best_bid_qty) = book.best_bid().unwrap();
        assert_eq!(best_bid, dec!(50001));
        assert_eq!(best_bid_qty, dec!(2.0));

        // Best ask should be 50001 (on Custom).
        let (best_ask, _) = book.best_ask().unwrap();
        assert_eq!(best_ask, dec!(50001));

        // Total bid depth at price 50000 should be 1.5 (1.0 + 0.5).
        let vq = book.bids.get(&dec!(50000)).unwrap();
        assert_eq!(vq.total(), dec!(1.5));
    }

    #[test]
    fn test_venue_snapshot_replaces_old_data() {
        let mut book = UnifiedOrderBook::new("TEST".into());

        book.apply_venue_snapshot(VenueId::Binance, &[lvl("100", "5.0")], &[]);

        // New snapshot should replace.
        book.apply_venue_snapshot(VenueId::Binance, &[lvl("101", "3.0")], &[]);

        assert!(!book.bids.contains_key(&dec!(100))); // Old level gone.
        assert_eq!(book.bids.get(&dec!(101)).unwrap().total(), dec!(3.0));
    }
}
