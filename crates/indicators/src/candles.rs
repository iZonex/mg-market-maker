//! Trade-aggregated candles with **non-time** bucketing modes.
//!
//! Ported from `beatzxbt/mm-toolbox`'s `candles` module (MIT).
//! The Python version is Numba-JIT'd and uses a pre-allocated
//! 2-D ring buffer of `f64`; the Rust version uses a `VecDeque`
//! of a `Candle` struct with `Decimal` fields — money safety
//! first, the candle pipeline is not on the critical path.
//!
//! Three aggregation modes are provided, each matching the
//! upstream semantics:
//!
//! - [`TickCandles`]: closes a candle after every N trades
//!   regardless of wall-clock time. Normalises for burst
//!   activity — a 10-tick candle in a quiet minute may span
//!   30 seconds, in a busy one 300 ms.
//! - [`VolumeCandles`]: closes a candle after every N base
//!   asset units traded, splitting the straddling trade so the
//!   bucket fills exactly. The surplus volume carries into the
//!   next candle as a fresh trade — so a single huge trade
//!   can emit several candles in one call.
//! - [`MultiTriggerCandles`]: closes on whichever of
//!   `(max_duration_ms, max_ticks, max_volume)` fires first.
//!   Useful when you want volume-normalised candles but cap the
//!   worst-case latency of a candle close during a dead market.
//!
//! Each aggregator holds a [`VecDeque`] of completed candles
//! plus the in-progress candle's state. `completed()` returns a
//! slice snapshot of closed candles; `current()` returns the
//! in-progress state (or `None` before the first trade).
//! Downstream consumers (alpha signal generators, feature
//! extractors) should call `update(...)` on every public trade
//! and then read `last_closed()` to pick up freshly-closed
//! candles since the previous tick.

use std::collections::VecDeque;

use rust_decimal::Decimal;

/// Side of a trade as seen by the taker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// One aggregated candle, populated as trades are pushed in and
/// closed on a mode-specific trigger.
#[derive(Debug, Clone, PartialEq)]
pub struct Candle {
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub buy_volume: Decimal,
    pub sell_volume: Decimal,
    pub vwap: Decimal,
    pub total_trades: u64,
    pub open_ts_ms: i64,
    pub close_ts_ms: i64,
}

impl Candle {
    /// Total volume (buy + sell) in base asset.
    pub fn volume(&self) -> Decimal {
        self.buy_volume + self.sell_volume
    }

    /// Signed net flow (buy − sell) in base asset. Positive =
    /// buyers dominated the candle.
    pub fn net_flow(&self) -> Decimal {
        self.buy_volume - self.sell_volume
    }
}

/// Private aggregation state used by every concrete aggregator.
/// Invariants:
/// - `open`, `high`, `low`, `close`, `vwap` are meaningful only
///   after the first trade has been absorbed.
/// - `cum_price_qty` / `cum_qty` are the running VWAP
///   numerator/denominator; reset on each candle close.
#[derive(Debug, Clone)]
struct PartialCandle {
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    buy_volume: Decimal,
    sell_volume: Decimal,
    cum_price_qty: Decimal,
    cum_qty: Decimal,
    total_trades: u64,
    open_ts_ms: i64,
    close_ts_ms: i64,
    started: bool,
}

impl PartialCandle {
    fn new() -> Self {
        Self {
            open: Decimal::ZERO,
            high: Decimal::ZERO,
            low: Decimal::ZERO,
            close: Decimal::ZERO,
            buy_volume: Decimal::ZERO,
            sell_volume: Decimal::ZERO,
            cum_price_qty: Decimal::ZERO,
            cum_qty: Decimal::ZERO,
            total_trades: 0,
            open_ts_ms: 0,
            close_ts_ms: 0,
            started: false,
        }
    }

    /// Absorb a single trade into the partial. The caller is
    /// responsible for triggering `close_into` when the
    /// mode-specific bucket threshold has been crossed.
    fn absorb(&mut self, ts_ms: i64, side: TradeSide, price: Decimal, qty: Decimal) {
        if qty <= Decimal::ZERO {
            return;
        }
        if !self.started {
            self.open = price;
            self.high = price;
            self.low = price;
            self.open_ts_ms = ts_ms;
            self.started = true;
        } else {
            if price > self.high {
                self.high = price;
            }
            if price < self.low {
                self.low = price;
            }
        }
        self.close = price;
        self.close_ts_ms = ts_ms;
        match side {
            TradeSide::Buy => self.buy_volume += qty,
            TradeSide::Sell => self.sell_volume += qty,
        }
        self.cum_price_qty += price * qty;
        self.cum_qty += qty;
        self.total_trades += 1;
    }

    fn vwap(&self) -> Decimal {
        if self.cum_qty.is_zero() {
            self.close
        } else {
            self.cum_price_qty / self.cum_qty
        }
    }

    /// Consume this partial into a [`Candle`] and reset to a
    /// fresh state ready for the next bucket.
    fn close_into(&mut self) -> Candle {
        let candle = Candle {
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            buy_volume: self.buy_volume,
            sell_volume: self.sell_volume,
            vwap: self.vwap(),
            total_trades: self.total_trades,
            open_ts_ms: self.open_ts_ms,
            close_ts_ms: self.close_ts_ms,
        };
        *self = PartialCandle::new();
        candle
    }

    fn volume(&self) -> Decimal {
        self.buy_volume + self.sell_volume
    }

    fn snapshot(&self) -> Option<Candle> {
        if !self.started {
            return None;
        }
        Some(Candle {
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            buy_volume: self.buy_volume,
            sell_volume: self.sell_volume,
            vwap: self.vwap(),
            total_trades: self.total_trades,
            open_ts_ms: self.open_ts_ms,
            close_ts_ms: self.close_ts_ms,
        })
    }
}

/// Common fields shared by every aggregator.
#[derive(Debug, Clone)]
struct CandleBuffer {
    capacity: usize,
    completed: VecDeque<Candle>,
    partial: PartialCandle,
    /// Number of freshly-closed candles since the last call to
    /// [`last_closed`]. Zero after the method is invoked.
    fresh: usize,
}

impl CandleBuffer {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "candle capacity must be > 0");
        Self {
            capacity,
            completed: VecDeque::with_capacity(capacity),
            partial: PartialCandle::new(),
            fresh: 0,
        }
    }

    fn push_closed(&mut self, candle: Candle) {
        if self.completed.len() == self.capacity {
            self.completed.pop_front();
        }
        self.completed.push_back(candle);
        self.fresh += 1;
    }

    fn completed(&self) -> &VecDeque<Candle> {
        &self.completed
    }

    fn current(&self) -> Option<Candle> {
        self.partial.snapshot()
    }

    /// Take and return the candles closed since the previous
    /// call to this method. Leaves the buffer intact.
    fn take_fresh(&mut self) -> Vec<Candle> {
        let n = self.fresh.min(self.completed.len());
        self.fresh = 0;
        self.completed
            .iter()
            .rev()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

/// Tick-bucketed candles — closes every `ticks_per_bucket`
/// trades regardless of wall-clock time.
#[derive(Debug, Clone)]
pub struct TickCandles {
    ticks_per_bucket: u64,
    buffer: CandleBuffer,
}

impl TickCandles {
    pub fn new(ticks_per_bucket: u64, capacity: usize) -> Self {
        assert!(ticks_per_bucket > 0, "ticks_per_bucket must be > 0");
        Self {
            ticks_per_bucket,
            buffer: CandleBuffer::new(capacity),
        }
    }

    /// Feed a trade. Closes the current candle if the tick
    /// threshold is crossed.
    pub fn update(&mut self, ts_ms: i64, side: TradeSide, price: Decimal, qty: Decimal) {
        self.buffer.partial.absorb(ts_ms, side, price, qty);
        if self.buffer.partial.total_trades >= self.ticks_per_bucket {
            let closed = self.buffer.partial.close_into();
            self.buffer.push_closed(closed);
        }
    }

    pub fn completed(&self) -> &VecDeque<Candle> {
        self.buffer.completed()
    }
    pub fn current(&self) -> Option<Candle> {
        self.buffer.current()
    }
    pub fn last_closed(&mut self) -> Vec<Candle> {
        self.buffer.take_fresh()
    }
}

/// Volume-bucketed candles — closes after every
/// `volume_per_bucket` base asset units. Trades that straddle a
/// bucket boundary are split; the surplus is absorbed into the
/// next candle as a fresh trade, which can trigger further
/// closures inside a single `update` call.
#[derive(Debug, Clone)]
pub struct VolumeCandles {
    volume_per_bucket: Decimal,
    buffer: CandleBuffer,
}

impl VolumeCandles {
    pub fn new(volume_per_bucket: Decimal, capacity: usize) -> Self {
        assert!(
            volume_per_bucket > Decimal::ZERO,
            "volume_per_bucket must be > 0"
        );
        Self {
            volume_per_bucket,
            buffer: CandleBuffer::new(capacity),
        }
    }

    pub fn update(&mut self, ts_ms: i64, side: TradeSide, price: Decimal, qty: Decimal) {
        if qty <= Decimal::ZERO {
            return;
        }
        let room = self.volume_per_bucket - self.buffer.partial.volume();
        if qty <= room {
            self.buffer.partial.absorb(ts_ms, side, price, qty);
            if self.buffer.partial.volume() >= self.volume_per_bucket {
                let closed = self.buffer.partial.close_into();
                self.buffer.push_closed(closed);
            }
        } else {
            // Fill the current bucket to exactly
            // `volume_per_bucket`, close it, then recurse with
            // the surplus. `room` can be zero here (if the
            // caller managed to leave the partial at exactly
            // the cap without closing) — we still close and
            // forward the full qty.
            if room > Decimal::ZERO {
                self.buffer.partial.absorb(ts_ms, side, price, room);
            }
            let closed = self.buffer.partial.close_into();
            self.buffer.push_closed(closed);
            let surplus = qty - room;
            if surplus > Decimal::ZERO {
                self.update(ts_ms, side, price, surplus);
            }
        }
    }

    pub fn completed(&self) -> &VecDeque<Candle> {
        self.buffer.completed()
    }
    pub fn current(&self) -> Option<Candle> {
        self.buffer.current()
    }
    pub fn last_closed(&mut self) -> Vec<Candle> {
        self.buffer.take_fresh()
    }
}

/// Multi-trigger candles — closes on whichever of
/// `(max_duration_ms, max_ticks, max_volume)` fires first.
///
/// Useful when you want volume-normalised candles but need a
/// hard wall-clock floor on candle close latency during a
/// dead market: a trader with `max_volume = 1 BTC` and
/// `max_duration_ms = 5000` will see a candle close at least
/// every 5 s even if no trades arrive (closed on the first
/// trade after the deadline, carrying the elapsed time from the
/// previous open). Matches the upstream Python semantics.
#[derive(Debug, Clone)]
pub struct MultiTriggerCandles {
    max_duration_ms: i64,
    max_ticks: u64,
    max_volume: Decimal,
    buffer: CandleBuffer,
}

impl MultiTriggerCandles {
    pub fn new(max_duration_ms: i64, max_ticks: u64, max_volume: Decimal, capacity: usize) -> Self {
        assert!(max_duration_ms > 0, "max_duration_ms must be > 0");
        assert!(max_ticks > 0, "max_ticks must be > 0");
        assert!(max_volume > Decimal::ZERO, "max_volume must be > 0");
        Self {
            max_duration_ms,
            max_ticks,
            max_volume,
            buffer: CandleBuffer::new(capacity),
        }
    }

    pub fn update(&mut self, ts_ms: i64, side: TradeSide, price: Decimal, qty: Decimal) {
        if qty <= Decimal::ZERO {
            return;
        }
        // Duration trigger — if the incoming trade lands past
        // the deadline of the current partial, close the
        // partial first and restart a fresh one with the
        // incoming trade.
        if self.buffer.partial.started
            && ts_ms >= self.buffer.partial.open_ts_ms + self.max_duration_ms
        {
            let closed = self.buffer.partial.close_into();
            self.buffer.push_closed(closed);
        }

        // Volume trigger — delegate to the volume logic which
        // handles straddling trade splits identically to
        // `VolumeCandles`.
        let room = self.max_volume - self.buffer.partial.volume();
        if qty <= room {
            self.buffer.partial.absorb(ts_ms, side, price, qty);
        } else {
            if room > Decimal::ZERO {
                self.buffer.partial.absorb(ts_ms, side, price, room);
            }
            let closed = self.buffer.partial.close_into();
            self.buffer.push_closed(closed);
            let surplus = qty - room;
            if surplus > Decimal::ZERO {
                self.update(ts_ms, side, price, surplus);
                return;
            }
        }

        // Tick and volume cap checks after absorbing. The
        // duration cap was already handled above.
        if self.buffer.partial.total_trades >= self.max_ticks
            || self.buffer.partial.volume() >= self.max_volume
        {
            let closed = self.buffer.partial.close_into();
            self.buffer.push_closed(closed);
        }
    }

    pub fn completed(&self) -> &VecDeque<Candle> {
        self.buffer.completed()
    }
    pub fn current(&self) -> Option<Candle> {
        self.buffer.current()
    }
    pub fn last_closed(&mut self) -> Vec<Candle> {
        self.buffer.take_fresh()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // ----- TickCandles -----

    #[test]
    fn tick_candles_close_on_exact_tick_count() {
        let mut c = TickCandles::new(3, 10);
        c.update(1, TradeSide::Buy, dec!(100), dec!(1));
        c.update(2, TradeSide::Buy, dec!(101), dec!(1));
        assert!(c.completed().is_empty());
        c.update(3, TradeSide::Sell, dec!(99), dec!(2));
        assert_eq!(c.completed().len(), 1);
        let candle = c.completed().front().unwrap();
        assert_eq!(candle.open, dec!(100));
        assert_eq!(candle.high, dec!(101));
        assert_eq!(candle.low, dec!(99));
        assert_eq!(candle.close, dec!(99));
        assert_eq!(candle.buy_volume, dec!(2));
        assert_eq!(candle.sell_volume, dec!(2));
        assert_eq!(candle.total_trades, 3);
    }

    #[test]
    fn tick_candles_vwap_is_volume_weighted() {
        let mut c = TickCandles::new(2, 10);
        c.update(1, TradeSide::Buy, dec!(100), dec!(1));
        c.update(2, TradeSide::Buy, dec!(110), dec!(3));
        let candle = c.completed().front().unwrap();
        // (100·1 + 110·3) / (1+3) = 430/4 = 107.5
        assert_eq!(candle.vwap, dec!(107.5));
    }

    #[test]
    fn tick_candles_current_is_none_before_first_trade() {
        let c = TickCandles::new(5, 10);
        assert!(c.current().is_none());
    }

    #[test]
    fn tick_candles_current_reflects_in_progress_state() {
        let mut c = TickCandles::new(5, 10);
        c.update(1, TradeSide::Buy, dec!(50), dec!(1));
        c.update(2, TradeSide::Sell, dec!(52), dec!(1));
        let cur = c.current().unwrap();
        assert_eq!(cur.high, dec!(52));
        assert_eq!(cur.low, dec!(50));
        assert_eq!(cur.total_trades, 2);
    }

    #[test]
    fn tick_candles_last_closed_returns_only_fresh_candles() {
        let mut c = TickCandles::new(1, 10);
        c.update(1, TradeSide::Buy, dec!(100), dec!(1));
        c.update(2, TradeSide::Buy, dec!(101), dec!(1));
        let fresh = c.last_closed();
        assert_eq!(fresh.len(), 2);
        let nothing = c.last_closed();
        assert!(nothing.is_empty());
    }

    #[test]
    fn tick_candles_capacity_evicts_oldest() {
        let mut c = TickCandles::new(1, 2);
        c.update(1, TradeSide::Buy, dec!(100), dec!(1));
        c.update(2, TradeSide::Buy, dec!(101), dec!(1));
        c.update(3, TradeSide::Buy, dec!(102), dec!(1));
        assert_eq!(c.completed().len(), 2);
        assert_eq!(c.completed().front().unwrap().close, dec!(101));
    }

    // ----- VolumeCandles -----

    #[test]
    fn volume_candles_close_on_exact_volume() {
        let mut c = VolumeCandles::new(dec!(5), 10);
        c.update(1, TradeSide::Buy, dec!(100), dec!(2));
        c.update(2, TradeSide::Buy, dec!(101), dec!(2));
        assert!(c.completed().is_empty());
        c.update(3, TradeSide::Buy, dec!(102), dec!(1));
        assert_eq!(c.completed().len(), 1);
    }

    #[test]
    fn volume_candles_split_straddling_trade() {
        let mut c = VolumeCandles::new(dec!(5), 10);
        // One huge trade that spans exactly two buckets.
        c.update(1, TradeSide::Buy, dec!(100), dec!(10));
        assert_eq!(c.completed().len(), 2);
        for candle in c.completed().iter() {
            assert_eq!(candle.buy_volume, dec!(5));
        }
    }

    #[test]
    fn volume_candles_triple_split() {
        let mut c = VolumeCandles::new(dec!(2), 10);
        // A 7-unit trade should spawn 3 full candles + 1 unit
        // in the in-progress partial.
        c.update(1, TradeSide::Sell, dec!(50), dec!(7));
        assert_eq!(c.completed().len(), 3);
        let partial = c.current().unwrap();
        assert_eq!(partial.sell_volume, dec!(1));
    }

    // ----- MultiTriggerCandles -----

    #[test]
    fn multi_trigger_fires_on_duration_first() {
        let mut c = MultiTriggerCandles::new(1000, 1_000, dec!(1_000), 10);
        c.update(0, TradeSide::Buy, dec!(100), dec!(1));
        // Second trade 2 s later — past the duration cap.
        c.update(2000, TradeSide::Buy, dec!(101), dec!(1));
        assert_eq!(c.completed().len(), 1);
        let candle = c.completed().front().unwrap();
        assert_eq!(candle.close, dec!(100));
    }

    #[test]
    fn multi_trigger_fires_on_ticks_first() {
        let mut c = MultiTriggerCandles::new(1_000_000, 3, dec!(1_000), 10);
        c.update(0, TradeSide::Buy, dec!(100), dec!(1));
        c.update(1, TradeSide::Buy, dec!(100), dec!(1));
        c.update(2, TradeSide::Buy, dec!(100), dec!(1));
        assert_eq!(c.completed().len(), 1);
    }

    #[test]
    fn multi_trigger_fires_on_volume_first() {
        let mut c = MultiTriggerCandles::new(1_000_000, 1_000, dec!(5), 10);
        c.update(0, TradeSide::Buy, dec!(100), dec!(3));
        c.update(1, TradeSide::Buy, dec!(100), dec!(2));
        assert_eq!(c.completed().len(), 1);
    }

    // ----- Candle helpers -----

    #[test]
    fn candle_net_flow_and_volume_helpers() {
        let candle = Candle {
            open: dec!(100),
            high: dec!(101),
            low: dec!(99),
            close: dec!(100),
            buy_volume: dec!(8),
            sell_volume: dec!(5),
            vwap: dec!(100),
            total_trades: 3,
            open_ts_ms: 0,
            close_ts_ms: 1,
        };
        assert_eq!(candle.volume(), dec!(13));
        assert_eq!(candle.net_flow(), dec!(3));
    }

    /// Non-positive trade qty must not advance any counter.
    #[test]
    fn non_positive_qty_is_ignored_by_all_aggregators() {
        let mut t = TickCandles::new(5, 10);
        t.update(1, TradeSide::Buy, dec!(100), Decimal::ZERO);
        t.update(2, TradeSide::Buy, dec!(100), dec!(-1));
        assert!(t.current().is_none());

        let mut v = VolumeCandles::new(dec!(5), 10);
        v.update(1, TradeSide::Buy, dec!(100), Decimal::ZERO);
        v.update(2, TradeSide::Buy, dec!(100), dec!(-1));
        assert!(v.current().is_none());
    }
}
