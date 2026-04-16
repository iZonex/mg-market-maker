use mm_common::orderbook::LocalOrderBook;
use mm_exchange_core::events::MarketEvent;
use tracing::debug;

/// Maintains a local order book from WebSocket events.
pub struct BookKeeper {
    pub book: LocalOrderBook,
    /// Tracks when the book was last updated for stale detection.
    last_update: Option<std::time::Instant>,
}

impl BookKeeper {
    pub fn new(symbol: &str) -> Self {
        Self {
            book: LocalOrderBook::new(symbol.to_string()),
            last_update: None,
        }
    }

    /// When was the book last updated? `None` if never.
    pub fn last_update_at(&self) -> Option<std::time::Instant> {
        self.last_update
    }

    /// Process a market event and update the local book.
    /// Returns true if the book was updated.
    pub fn on_event(&mut self, event: &MarketEvent) -> bool {
        let updated = match event {
            MarketEvent::BookSnapshot {
                bids,
                asks,
                sequence,
                ..
            } => {
                self.book
                    .apply_snapshot(bids.clone(), asks.clone(), *sequence);
                debug!(
                    seq = sequence,
                    bids = self.book.bids.len(),
                    asks = self.book.asks.len(),
                    "book snapshot applied"
                );
                true
            }
            MarketEvent::BookDelta {
                bids,
                asks,
                sequence,
                ..
            } => {
                self.book.apply_delta(bids.clone(), asks.clone(), *sequence);
                true
            }
            _ => false,
        };
        if updated {
            self.last_update = Some(std::time::Instant::now());
        }
        updated
    }

    pub fn is_ready(&self) -> bool {
        self.book.best_bid().is_some() && self.book.best_ask().is_some()
    }
}
