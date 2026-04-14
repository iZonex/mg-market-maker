//! Lookahead-bias detector for backtest-driving indicators and signals.
//!
//! The detector wraps any pure function `f(prefix) -> Vec<V>` that
//! takes a prefix of an event stream and returns one value per event
//! consumed. It verifies that the value at position `i` is independent
//! of events at positions `> i`.
//!
//! Concretely, for every truncation point `t` in `1..=N`:
//!
//! - `full[t-1]`  = `f(events[..N])[t-1]` — value computed with the
//!   full future available
//! - `prefix[t-1]` = `f(events[..t])[t-1]` — value computed with only
//!   the past available
//!
//! If the implementation has no lookahead these must match exactly.
//! Any divergence is a leak: the indicator's value at time `t-1` is
//! reading information from events after `t-1`.
//!
//! Complexity is `O(N²)` — this is a testing tool, not a runtime check.
//!
//! ## Usage
//!
//! ```no_run
//! use mm_backtester::lookahead::check_lookahead;
//!
//! // Your indicator: a pure function prefix → Vec<value>.
//! fn my_sma(prices: &[f64]) -> Vec<f64> {
//!     let mut out = Vec::with_capacity(prices.len());
//!     let mut sum = 0.0;
//!     for (i, p) in prices.iter().enumerate() {
//!         sum += p;
//!         out.push(sum / (i + 1) as f64);
//!     }
//!     out
//! }
//!
//! let prices = vec![1.0, 2.0, 3.0, 4.0, 5.0];
//! let report = check_lookahead(&prices, my_sma);
//! assert!(report.is_clean());
//! ```

/// A single observed lookahead leak.
#[derive(Debug, Clone)]
pub struct LookaheadLeak<V> {
    /// Index at which the leak was observed.
    pub index: usize,
    /// What the indicator returned when only `events[..=index]` was available.
    pub prefix_value: V,
    /// What the indicator returned for the same index when the full
    /// stream was available. Different from `prefix_value` → leak.
    pub full_value: V,
}

/// Outcome of a lookahead check.
#[derive(Debug, Clone)]
pub struct LookaheadReport<V> {
    pub events_tested: usize,
    pub leaks: Vec<LookaheadLeak<V>>,
}

impl<V> LookaheadReport<V> {
    pub fn is_clean(&self) -> bool {
        self.leaks.is_empty()
    }

    pub fn leak_count(&self) -> usize {
        self.leaks.len()
    }
}

/// Run the lookahead check.
///
/// `run` is called `N+1` times in total: once on the full stream,
/// and then once on each prefix `events[..t]` for `t` in `1..=N`. It
/// must return **one value per event consumed** — the returned
/// `Vec<V>` must have the same length as its input slice.
///
/// # Panics
///
/// Panics if `run` returns a trace of the wrong length. That is a
/// programming error in the indicator wrapper, not a lookahead issue.
pub fn check_lookahead<E, V, F>(events: &[E], run: F) -> LookaheadReport<V>
where
    V: PartialEq + Clone + std::fmt::Debug,
    F: Fn(&[E]) -> Vec<V>,
{
    let full = run(events);
    assert_eq!(
        full.len(),
        events.len(),
        "run() must return exactly one value per input event"
    );

    let mut leaks = Vec::new();
    for t in 1..=events.len() {
        let prefix = run(&events[..t]);
        assert_eq!(
            prefix.len(),
            t,
            "run(prefix[..{t}]) returned {} values, expected {t}",
            prefix.len()
        );
        let last = t - 1;
        if full[last] != prefix[last] {
            leaks.push(LookaheadLeak {
                index: last,
                prefix_value: prefix[last].clone(),
                full_value: full[last].clone(),
            });
        }
    }

    LookaheadReport {
        events_tested: events.len(),
        leaks,
    }
}

/// Convenience wrapper for `Vec<V>` equality checks where `V` is
/// numeric and a tiny rounding tolerance is acceptable.
pub fn check_lookahead_approx<E, F>(events: &[E], run: F, tolerance: f64) -> LookaheadReport<f64>
where
    F: Fn(&[E]) -> Vec<f64>,
{
    let full = run(events);
    assert_eq!(full.len(), events.len());

    let mut leaks = Vec::new();
    for t in 1..=events.len() {
        let prefix = run(&events[..t]);
        let last = t - 1;
        let diff = (full[last] - prefix[last]).abs();
        if diff > tolerance {
            leaks.push(LookaheadLeak {
                index: last,
                prefix_value: prefix[last],
                full_value: full[last],
            });
        }
    }

    LookaheadReport {
        events_tested: events.len(),
        leaks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clean SMA: value at index i depends only on events[..=i].
    fn clean_sma(prices: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(prices.len());
        let mut sum = 0.0;
        for (i, p) in prices.iter().enumerate() {
            sum += p;
            out.push(sum / (i + 1) as f64);
        }
        out
    }

    /// Leaky "SMA": at every index it looks ahead by one.
    fn leaky_sma(prices: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(prices.len());
        for i in 0..prices.len() {
            let next = prices.get(i + 1).copied().unwrap_or(prices[i]);
            out.push((prices[i] + next) / 2.0);
        }
        out
    }

    /// Clean EWMA with a fixed alpha — each step depends only on the
    /// previous state and the current sample.
    fn clean_ewma(samples: &[f64]) -> Vec<f64> {
        let alpha = 0.1;
        let mut out = Vec::with_capacity(samples.len());
        let mut state = 0.0;
        for (i, s) in samples.iter().enumerate() {
            if i == 0 {
                state = *s;
            } else {
                state = alpha * s + (1.0 - alpha) * state;
            }
            out.push(state);
        }
        out
    }

    /// Indicator that peeks at the global max — classic lookahead bug.
    fn leaky_global_max(samples: &[f64]) -> Vec<f64> {
        let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        samples.iter().map(|_| max).collect()
    }

    #[test]
    fn clean_sma_has_no_leak() {
        let prices = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
        let report = check_lookahead_approx(&prices, clean_sma, 1e-12);
        assert!(
            report.is_clean(),
            "clean SMA flagged as leaky: {:?}",
            report.leaks
        );
    }

    #[test]
    fn clean_ewma_has_no_leak() {
        let samples: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        let report = check_lookahead_approx(&samples, clean_ewma, 1e-12);
        assert!(report.is_clean());
    }

    #[test]
    fn leaky_sma_is_caught_at_every_index_except_the_last() {
        let prices = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let report = check_lookahead_approx(&prices, leaky_sma, 1e-12);
        // Every index except the last one reads the next element;
        // the last one falls back to itself, which is detection-
        // dependent but at minimum the first N-1 must leak.
        assert!(report.leak_count() >= prices.len() - 1);
    }

    #[test]
    fn global_max_leak_is_detected() {
        let samples = vec![1.0, 5.0, 2.0, 10.0, 3.0];
        let report = check_lookahead_approx(&samples, leaky_global_max, 1e-12);
        // Every index except the one where the actual max sits should
        // show a discrepancy.
        assert!(report.leak_count() >= 3);
    }

    #[test]
    fn report_shape_is_sensible() {
        let prices = vec![1.0, 2.0];
        let report = check_lookahead_approx(&prices, leaky_sma, 1e-12);
        assert_eq!(report.events_tested, 2);
        // First index leaks (reads index 1). Second index might also
        // depending on fallback; we just check at least one leak.
        assert!(!report.is_clean());
    }

    #[test]
    fn empty_events_is_trivially_clean() {
        let prices: Vec<f64> = vec![];
        let report = check_lookahead_approx(&prices, clean_sma, 1e-12);
        assert!(report.is_clean());
        assert_eq!(report.events_tested, 0);
    }

    /// Generic (non-approx) variant with integer values.
    #[test]
    fn integer_indicator_roundtrip() {
        let events = vec![10, 20, 30, 40];
        let clean_running_max = |xs: &[i64]| -> Vec<i64> {
            let mut out = Vec::with_capacity(xs.len());
            let mut m = i64::MIN;
            for x in xs {
                m = m.max(*x);
                out.push(m);
            }
            out
        };
        let report = check_lookahead(&events, clean_running_max);
        assert!(report.is_clean());
    }
}
