//! Pluggable feed / order latency models for backtest fill
//! simulation.
//!
//! Ported from `hftbacktest/src/backtest/models/latency.rs` (MIT).
//!
//! # Why separate entry and response latency
//!
//! Our existing [`crate::fill_model::ProbabilisticFillConfig`]
//! has a single `latency_ms` field applied uniformly to every
//! fill. Real venues expose two independent latencies:
//!
//! - **Entry latency** — how long from our local order
//!   submission until the exchange's matching engine processes
//!   it. Affects whether your reaction order beats other taker
//!   flow to an empty touch.
//! - **Response latency** — how long from exchange processing
//!   until we see the ack / fill notification locally. Affects
//!   how quickly our own state (live orders, balances) catches
//!   up to the venue.
//!
//! Keeping them separate lets a backtest model asymmetries like
//! "order path is hot, drop acks are slow" (typical for
//! co-located setups that use two different transport channels)
//! or "the whole path is slow but symmetric" (typical for a
//! public-internet retail MM — our deployment).
//!
//! # Sign convention
//!
//! The upstream library uses a signed latency: a **negative**
//! value means the exchange rejected the order and the returned
//! latency represents the time until the local bot received the
//! rejection notice. We preserve that convention — clients that
//! see `latency < 0` know the simulation decided to synthesise a
//! rejection and can treat it accordingly.

/// Feed/order latency model. `timestamp_ns` is the local
/// clock when the request was initiated; the returned value is
/// the latency in the same unit (nanoseconds) to add to the
/// next state transition.
///
/// Both methods take `&mut self` so a stateful model (e.g.
/// historical-data-driven interpolator) can advance its
/// internal cursor across calls.
pub trait LatencyModel {
    /// Latency from local submission to exchange-matching-engine
    /// acceptance. Positive on success; negative indicates the
    /// exchange will reject the order and the magnitude is the
    /// latency until the local bot sees that rejection.
    fn entry(&mut self, timestamp_ns: i64) -> i64;

    /// Latency from exchange acknowledgement to local response
    /// receipt. Always non-negative.
    fn response(&mut self, timestamp_ns: i64) -> i64;
}

/// Deterministic constant latency — returns the same entry and
/// response values for every call regardless of timestamp.
/// Useful as a baseline and for unit tests.
#[derive(Debug, Clone, Copy)]
pub struct ConstantLatency {
    entry_ns: i64,
    response_ns: i64,
}

impl ConstantLatency {
    /// Construct with explicit entry and response latencies, in
    /// nanoseconds. A negative `entry_ns` produces simulated
    /// rejections on every call (rarely what you want, but
    /// useful to pin rejection-path behaviour in tests).
    pub fn new(entry_ns: i64, response_ns: i64) -> Self {
        Self {
            entry_ns,
            response_ns,
        }
    }

    /// Convenience: build from millisecond values.
    pub fn from_ms(entry_ms: i64, response_ms: i64) -> Self {
        Self::new(entry_ms * 1_000_000, response_ms * 1_000_000)
    }

    /// Convenience: build from a symmetric microsecond value.
    pub fn symmetric_us(us: i64) -> Self {
        let ns = us * 1_000;
        Self::new(ns, ns)
    }

    pub fn entry_ns(&self) -> i64 {
        self.entry_ns
    }
    pub fn response_ns(&self) -> i64 {
        self.response_ns
    }
}

impl LatencyModel for ConstantLatency {
    fn entry(&mut self, _timestamp_ns: i64) -> i64 {
        self.entry_ns
    }

    fn response(&mut self, _timestamp_ns: i64) -> i64 {
        self.response_ns
    }
}

/// Latency model that scales linearly with recent event volume.
///
/// Starts at `base_entry_ns` / `base_response_ns`. Every
/// `bucket_ns` window, the model computes the event rate and
/// adjusts its multiplier via
/// `1 + load_factor · (rate / ref_rate - 1)`, clamped to
/// `[min_multiplier, max_multiplier]`. Models the very common
/// failure mode where a spike in event traffic stretches the
/// local order-path queue.
///
/// Not from upstream hftbacktest — a homegrown extension that
/// composes cleanly with the `LatencyModel` trait.
#[derive(Debug, Clone, Copy)]
pub struct BackoffOnTrafficLatency {
    base_entry_ns: i64,
    base_response_ns: i64,
    ref_rate: f64,
    load_factor: f64,
    min_multiplier: f64,
    max_multiplier: f64,
    bucket_ns: i64,
    // `None` before the first `record_event`, so t=0 is a
    // valid first-event timestamp and not confused with "not
    // yet initialised".
    window_start_ns: Option<i64>,
    events_in_window: u64,
    current_multiplier: f64,
}

impl BackoffOnTrafficLatency {
    /// Build a backoff model.
    ///
    /// * `base_entry_ns`, `base_response_ns` — latency in an
    ///   idle window.
    /// * `ref_rate` — the "normal" event rate (events per
    ///   second) the base latencies were calibrated for.
    /// * `load_factor` — how hard the multiplier reacts to
    ///   deviations from `ref_rate`. Typical: `0.5`.
    /// * `min_multiplier`, `max_multiplier` — clamps. Typical:
    ///   `0.5` and `5.0`.
    /// * `bucket_ns` — rolling window size for rate estimation.
    pub fn new(
        base_entry_ns: i64,
        base_response_ns: i64,
        ref_rate: f64,
        load_factor: f64,
        min_multiplier: f64,
        max_multiplier: f64,
        bucket_ns: i64,
    ) -> Self {
        assert!(ref_rate > 0.0, "ref_rate must be positive");
        assert!(
            min_multiplier > 0.0 && max_multiplier >= min_multiplier,
            "min/max multiplier range invalid"
        );
        assert!(bucket_ns > 0, "bucket_ns must be positive");
        Self {
            base_entry_ns,
            base_response_ns,
            ref_rate,
            load_factor,
            min_multiplier,
            max_multiplier,
            bucket_ns,
            window_start_ns: None,
            events_in_window: 0,
            current_multiplier: 1.0,
        }
    }

    /// Record an external event (trade, quote, book delta)
    /// that should count toward the load estimate. Crossing a
    /// bucket boundary recomputes the multiplier and starts a
    /// fresh window.
    pub fn record_event(&mut self, timestamp_ns: i64) {
        let start = self.window_start_ns.unwrap_or(timestamp_ns);
        if self.window_start_ns.is_none() {
            self.window_start_ns = Some(timestamp_ns);
        }
        let elapsed = timestamp_ns - start;
        if elapsed >= self.bucket_ns {
            let rate = (self.events_in_window as f64) * 1_000_000_000.0 / (elapsed.max(1) as f64);
            let relative = rate / self.ref_rate - 1.0;
            let raw = 1.0 + self.load_factor * relative;
            self.current_multiplier = raw.clamp(self.min_multiplier, self.max_multiplier);
            self.window_start_ns = Some(timestamp_ns);
            self.events_in_window = 0;
        }
        self.events_in_window += 1;
    }

    pub fn current_multiplier(&self) -> f64 {
        self.current_multiplier
    }
}

impl LatencyModel for BackoffOnTrafficLatency {
    fn entry(&mut self, _timestamp_ns: i64) -> i64 {
        ((self.base_entry_ns as f64) * self.current_multiplier) as i64
    }

    fn response(&mut self, _timestamp_ns: i64) -> i64 {
        ((self.base_response_ns as f64) * self.current_multiplier) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ConstantLatency ----

    #[test]
    fn constant_latency_returns_stored_value() {
        let mut m = ConstantLatency::new(1_500_000, 2_500_000);
        assert_eq!(m.entry(0), 1_500_000);
        assert_eq!(m.response(0), 2_500_000);
        assert_eq!(m.entry(999_999), 1_500_000);
    }

    #[test]
    fn constant_latency_from_ms_scales_to_ns() {
        let mut m = ConstantLatency::from_ms(3, 7);
        assert_eq!(m.entry(0), 3_000_000);
        assert_eq!(m.response(0), 7_000_000);
    }

    #[test]
    fn constant_latency_symmetric_us_matches_both_sides() {
        let mut m = ConstantLatency::symmetric_us(250);
        assert_eq!(m.entry(0), 250_000);
        assert_eq!(m.response(0), 250_000);
    }

    #[test]
    fn constant_latency_negative_entry_signals_rejection() {
        let mut m = ConstantLatency::new(-1_000_000, 500_000);
        assert!(m.entry(0) < 0);
        // Response latency is still positive — the rejection
        // notice itself travels the normal response path.
        assert!(m.response(0) > 0);
    }

    // ---- BackoffOnTrafficLatency ----

    fn calm_model() -> BackoffOnTrafficLatency {
        BackoffOnTrafficLatency::new(
            1_000_000,     // 1 ms base entry
            2_000_000,     // 2 ms base response
            100.0,         // 100 events/sec reference
            0.5,           // load factor
            0.5,           // min mult
            5.0,           // max mult
            1_000_000_000, // 1s bucket
        )
    }

    #[test]
    fn backoff_starts_at_base_latency_with_no_events() {
        let mut m = calm_model();
        assert_eq!(m.entry(0), 1_000_000);
        assert_eq!(m.response(0), 2_000_000);
        assert!((m.current_multiplier() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn backoff_scales_up_when_rate_exceeds_reference() {
        let mut m = calm_model();
        // Feed 500 events at 2 ms apart → t = 0, 2ms, 4ms, …,
        // 998ms. 500 events spanning exactly 1 000 ms. Then an
        // event at exactly t = 1 000ms triggers the bucket
        // boundary recompute.
        for i in 0..500 {
            m.record_event(i * 2_000_000);
        }
        m.record_event(1_000_000_000);
        // rate = 500 events / 1.0 s = 500 events/s.
        // relative = 500/100 - 1 = 4.
        // raw = 1 + 0.5 * 4 = 3. Inside clamps [0.5, 5.0].
        let mult = m.current_multiplier();
        assert!(
            (mult - 3.0).abs() < 0.1,
            "multiplier should scale with rate, got {mult}"
        );
        assert!(m.entry(0) > 1_000_000);
    }

    #[test]
    fn backoff_clamps_multiplier_to_max() {
        let mut m =
            BackoffOnTrafficLatency::new(1_000_000, 2_000_000, 10.0, 2.0, 0.5, 3.0, 1_000_000_000);
        // 500 events at 2 ms apart span exactly 1 000 ms, plus
        // a boundary event to trigger recompute. rate = 500,
        // relative = 49, raw = 1 + 2 * 49 = 99 → clamped to 3.
        for i in 0..500 {
            m.record_event(i * 2_000_000);
        }
        m.record_event(1_000_000_000);
        assert!((m.current_multiplier() - 3.0).abs() < 0.01);
    }

    #[test]
    fn backoff_clamps_multiplier_to_min_on_idle_window() {
        let mut m =
            BackoffOnTrafficLatency::new(1_000_000, 2_000_000, 100.0, 0.9, 0.5, 5.0, 1_000_000_000);
        // Single event at t=0, then a long idle until t=5s.
        m.record_event(0);
        m.record_event(5_000_000_000);
        // Rate ≈ 1 / 5 s = 0.2 → relative ≈ -0.998 → raw ≈
        // 1 - 0.9 * 0.998 ≈ 0.1 → clamped to 0.5.
        assert!((m.current_multiplier() - 0.5).abs() < 0.01);
    }

    #[test]
    fn backoff_latency_tracks_multiplier() {
        let mut m = calm_model();
        for i in 0..500 {
            m.record_event(i * 2_000_000);
        }
        m.record_event(1_000_000_000);
        let mult = m.current_multiplier();
        assert_eq!(m.entry(0), (1_000_000_f64 * mult) as i64);
        assert_eq!(m.response(0), (2_000_000_f64 * mult) as i64);
    }
}
