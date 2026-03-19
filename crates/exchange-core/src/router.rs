use mm_common::types::{Price, Qty, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::connector::VenueId;
use crate::unified_book::UnifiedOrderBook;

/// A routed child order — part of a larger parent order split across venues.
#[derive(Debug, Clone)]
pub struct RoutedOrder {
    pub venue: VenueId,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
}

/// Smart Order Router — splits orders across venues for best execution.
///
/// Considers: effective price (including fees), available liquidity,
/// and venue reliability.
pub struct SmartOrderRouter {
    /// Fee structure per venue.
    fees: std::collections::HashMap<VenueId, VenueFees>,
}

#[derive(Debug, Clone)]
pub struct VenueFees {
    pub maker_fee: Decimal,
    pub taker_fee: Decimal,
}

impl SmartOrderRouter {
    pub fn new() -> Self {
        Self {
            fees: std::collections::HashMap::new(),
        }
    }

    pub fn set_venue_fees(&mut self, venue: VenueId, fees: VenueFees) {
        self.fees.insert(venue, fees);
    }

    /// Route a market buy across venues for best execution.
    ///
    /// Greedily fills from the cheapest effective ask price.
    pub fn route_buy(&self, book: &UnifiedOrderBook, total_qty: Qty) -> Vec<RoutedOrder> {
        let mut remaining = total_qty;
        let mut orders = Vec::new();

        // Collect all ask levels with venue attribution, sorted by effective price.
        let mut levels: Vec<(Price, VenueId, Qty)> = Vec::new();
        for (price, vq) in &book.asks {
            for (&venue, &qty) in &vq.quantities {
                let effective = self.effective_buy_price(*price, venue);
                levels.push((effective, venue, qty.min(remaining)));
            }
        }
        levels.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, venue, available) in levels {
            if remaining.is_zero() {
                break;
            }
            let fill_qty = available.min(remaining);
            if fill_qty > dec!(0) {
                orders.push(RoutedOrder {
                    venue,
                    side: Side::Buy,
                    price: dec!(0), // Market order — use available price.
                    qty: fill_qty,
                });
                remaining -= fill_qty;
            }
        }

        debug!(
            total = %total_qty,
            filled = %(total_qty - remaining),
            venues = orders.len(),
            "SOR: routed buy"
        );
        orders
    }

    /// Route a market sell across venues for best execution.
    pub fn route_sell(&self, book: &UnifiedOrderBook, total_qty: Qty) -> Vec<RoutedOrder> {
        let mut remaining = total_qty;
        let mut orders = Vec::new();

        let mut levels: Vec<(Price, VenueId, Qty)> = Vec::new();
        for (price, vq) in book.bids.iter().rev() {
            for (&venue, &qty) in &vq.quantities {
                let effective = self.effective_sell_price(*price, venue);
                levels.push((effective, venue, qty.min(remaining)));
            }
        }
        // Sort descending (best effective sell price first).
        levels.sort_by(|a, b| b.0.cmp(&a.0));

        for (_, venue, available) in levels {
            if remaining.is_zero() {
                break;
            }
            let fill_qty = available.min(remaining);
            if fill_qty > dec!(0) {
                orders.push(RoutedOrder {
                    venue,
                    side: Side::Sell,
                    price: dec!(0),
                    qty: fill_qty,
                });
                remaining -= fill_qty;
            }
        }

        debug!(
            total = %total_qty,
            filled = %(total_qty - remaining),
            venues = orders.len(),
            "SOR: routed sell"
        );
        orders
    }

    /// Effective buy price = ask_price * (1 + taker_fee).
    fn effective_buy_price(&self, ask_price: Price, venue: VenueId) -> Price {
        let fee = self
            .fees
            .get(&venue)
            .map(|f| f.taker_fee)
            .unwrap_or(dec!(0.002));
        ask_price * (dec!(1) + fee)
    }

    /// Effective sell price = bid_price * (1 - taker_fee).
    fn effective_sell_price(&self, bid_price: Price, venue: VenueId) -> Price {
        let fee = self
            .fees
            .get(&venue)
            .map(|f| f.taker_fee)
            .unwrap_or(dec!(0.002));
        bid_price * (dec!(1) - fee)
    }
}

impl Default for SmartOrderRouter {
    fn default() -> Self {
        Self::new()
    }
}
