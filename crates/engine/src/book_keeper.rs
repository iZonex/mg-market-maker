use mm_common::orderbook::LocalOrderBook;
use mm_exchange_client::ws::WsEvent;
use tracing::debug;

/// Maintains a local order book from WebSocket events.
pub struct BookKeeper {
    pub book: LocalOrderBook,
}

impl BookKeeper {
    pub fn new(symbol: &str) -> Self {
        Self {
            book: LocalOrderBook::new(symbol.to_string()),
        }
    }

    /// Process a WebSocket event and update the local book.
    /// Returns true if the book was updated.
    pub fn on_event(&mut self, event: &WsEvent) -> bool {
        match event {
            WsEvent::BookSnapshot {
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
            WsEvent::BookDelta {
                bids,
                asks,
                sequence,
                ..
            } => {
                self.book.apply_delta(bids.clone(), asks.clone(), *sequence);
                true
            }
            _ => false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.book.best_bid().is_some() && self.book.best_ask().is_some()
    }
}
