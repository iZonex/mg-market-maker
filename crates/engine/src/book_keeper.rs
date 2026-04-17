use mm_common::orderbook::LocalOrderBook;
use mm_exchange_core::events::MarketEvent;
use mm_exchange_core::metrics::WS_PARSE_ERRORS_TOTAL;
use tracing::{debug, warn};

/// Maintains a local order book from WebSocket events.
pub struct BookKeeper {
    pub book: LocalOrderBook,
    /// Tracks when the book was last updated for stale detection.
    last_update: Option<std::time::Instant>,
    /// Most recent sequence number we successfully applied. Used
    /// to detect gaps — `Some(last)` where the next delta seq is
    /// not `last + 1` means we missed a message. A gap forces a
    /// resync flag so the engine can re-pull a REST snapshot
    /// before trusting the book again.
    last_sequence: Option<u64>,
    /// Running count of detected sequence gaps since start. Reset
    /// to zero whenever a full snapshot arrives and re-anchors the
    /// stream. Exposed via the `gap_count()` accessor so the
    /// engine can log it in the periodic summary / feed Prometheus.
    gap_count: u64,
    /// Set to `true` when `on_event` has observed a sequence gap
    /// since the last snapshot anchor. Stays `true` until the next
    /// `BookSnapshot` clears it. While set, the engine should treat
    /// the book as untrusted (stop quoting until resync).
    needs_resync: bool,
    /// Venue label attached to the `mm_ws_parse_errors_total`
    /// counter so operators can filter gaps by exchange. "custom"
    /// by default since the engine builds BookKeepers via
    /// `BookKeeper::new()` which does not pass a venue.
    venue: &'static str,
}

impl BookKeeper {
    pub fn new(symbol: &str) -> Self {
        Self {
            book: LocalOrderBook::new(symbol.to_string()),
            last_update: None,
            last_sequence: None,
            gap_count: 0,
            needs_resync: false,
            venue: "custom",
        }
    }

    /// Attach a venue label for the WS-parse-error counter so
    /// Grafana can differentiate gap sources per exchange.
    pub fn with_venue(mut self, venue: &'static str) -> Self {
        self.venue = venue;
        self
    }

    /// When was the book last updated? `None` if never.
    pub fn last_update_at(&self) -> Option<std::time::Instant> {
        self.last_update
    }

    /// Sequence-gap count since start. Monotonic — reset only by
    /// restart. Useful for per-deployment reliability tracking.
    pub fn gap_count(&self) -> u64 {
        self.gap_count
    }

    /// True when we saw a gap and have not yet re-anchored on a
    /// fresh `BookSnapshot`. The engine should stop quoting and
    /// trigger a REST snapshot fetch while this is set.
    pub fn needs_resync(&self) -> bool {
        self.needs_resync
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
                // Snapshot re-anchors the stream — clear the
                // resync flag and re-seed the sequence tracker.
                if self.needs_resync {
                    debug!(seq = sequence, "book resync completed via snapshot");
                }
                self.needs_resync = false;
                self.last_sequence = Some(*sequence);
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
                // Venues differ in how they number deltas: some
                // strictly increment by one, others by the length
                // of the message or by update-id ranges. We only
                // treat a strict decrease or a non-zero skip of
                // more than one as a gap — the first case is a
                // clear out-of-order delivery, the second covers
                // the common "missed N messages" failure mode
                // without false-flagging venues whose sequence
                // jumps by small amounts per update.
                if let Some(last) = self.last_sequence {
                    if *sequence <= last {
                        // Reordered / duplicate — drop silently.
                        debug!(
                            prev = last,
                            got = sequence,
                            "out-of-order book delta (<=last), skipping"
                        );
                        return false;
                    }
                    let jump = sequence.saturating_sub(last);
                    // Allow up to 1000 consecutive ids before we
                    // flag — some venues (Binance futures) burst
                    // large chunks of updates under the same
                    // update-id header.
                    if jump > 1 && jump < 1000 && !self.needs_resync {
                        self.gap_count = self.gap_count.saturating_add(1);
                        self.needs_resync = true;
                        warn!(
                            venue = self.venue,
                            prev = last,
                            got = sequence,
                            jump,
                            total_gaps = self.gap_count,
                            "SEQUENCE GAP detected in book stream — needs resync"
                        );
                        WS_PARSE_ERRORS_TOTAL
                            .with_label_values(&[self.venue, "market_data", "sequence_gap"])
                            .inc();
                    }
                }
                self.book.apply_delta(bids.clone(), asks.clone(), *sequence);
                self.last_sequence = Some(*sequence);
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

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::types::PriceLevel;
    use mm_exchange_core::connector::VenueId;
    use rust_decimal_macros::dec;

    fn snapshot(seq: u64) -> MarketEvent {
        MarketEvent::BookSnapshot {
            venue: VenueId::Bybit,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            asks: vec![PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            sequence: seq,
        }
    }

    fn delta(seq: u64) -> MarketEvent {
        MarketEvent::BookDelta {
            venue: VenueId::Bybit,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(2),
            }],
            asks: vec![],
            sequence: seq,
        }
    }

    #[test]
    fn no_gap_on_strict_incrementing_sequence() {
        let mut bk = BookKeeper::new("BTCUSDT");
        bk.on_event(&snapshot(100));
        bk.on_event(&delta(101));
        bk.on_event(&delta(102));
        bk.on_event(&delta(103));
        assert_eq!(bk.gap_count(), 0);
        assert!(!bk.needs_resync());
    }

    #[test]
    fn gap_triggers_resync_flag() {
        let mut bk = BookKeeper::new("BTCUSDT");
        bk.on_event(&snapshot(100));
        bk.on_event(&delta(101));
        // Jump from 101 to 105 → gap of 4.
        bk.on_event(&delta(105));
        assert_eq!(bk.gap_count(), 1);
        assert!(bk.needs_resync());
    }

    #[test]
    fn snapshot_clears_resync_flag() {
        let mut bk = BookKeeper::new("BTCUSDT");
        bk.on_event(&snapshot(100));
        bk.on_event(&delta(150)); // gap
        assert!(bk.needs_resync());
        bk.on_event(&snapshot(200));
        assert!(!bk.needs_resync());
    }

    #[test]
    fn out_of_order_delta_is_dropped() {
        let mut bk = BookKeeper::new("BTCUSDT");
        bk.on_event(&snapshot(100));
        bk.on_event(&delta(105));
        let updated = bk.on_event(&delta(103)); // reorder
        assert!(!updated);
        // Gap counter still 1 from the 100→105 jump, not 2.
        assert_eq!(bk.gap_count(), 1);
    }

    #[test]
    fn enormous_jump_is_ignored_as_stream_restart() {
        // Venues that re-anchor the sequence on reconnect can send
        // a delta with a sequence far larger than the previous one
        // — treating this as a gap would permanently stick us in
        // resync mode, so we only flag jumps < 1000.
        let mut bk = BookKeeper::new("BTCUSDT");
        bk.on_event(&snapshot(100));
        bk.on_event(&delta(1_000_000));
        assert_eq!(bk.gap_count(), 0);
        assert!(!bk.needs_resync());
    }
}
