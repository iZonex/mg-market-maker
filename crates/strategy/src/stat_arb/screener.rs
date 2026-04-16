//! Background pair screener (Epic B, stage-2).
//!
//! Periodically tests all configured symbol pairs for
//! cointegration and emits results for operator review. Runs
//! as a standalone async task alongside the engine — does NOT
//! auto-spawn `StatArbDriver` instances (that's an operator
//! decision after reviewing the screen).
//!
//! # Design
//!
//! The screener maintains a rolling price buffer per symbol
//! and runs the Engle-Granger test on every pair combination
//! when the buffer has enough samples. Results are collected
//! into `ScreenResult` structs that the operator can query
//! via the dashboard or audit trail.

use std::collections::HashMap;
use std::collections::VecDeque;

use rust_decimal::Decimal;

use super::cointegration::{CointegrationResult, EngleGrangerTest};

/// Maximum price samples retained per symbol.
pub const MAX_PRICE_SAMPLES: usize = 500;

/// Minimum samples before a pair is eligible for screening.
pub const MIN_SCREEN_SAMPLES: usize = 50;

/// Result of screening one pair.
#[derive(Debug, Clone)]
pub struct ScreenResult {
    pub y_symbol: String,
    pub x_symbol: String,
    pub cointegration: Option<CointegrationResult>,
    pub sample_size: usize,
}

/// Background pair screener. Accumulates mid-price samples
/// per symbol and runs cointegration tests on demand.
#[derive(Debug, Clone)]
pub struct PairScreener {
    /// Rolling price buffer per symbol.
    prices: HashMap<String, VecDeque<Decimal>>,
    /// Symbol pairs to screen (y, x).
    pairs: Vec<(String, String)>,
}

impl PairScreener {
    /// Construct a screener for the given pairs.
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        let mut prices = HashMap::new();
        for (y, x) in &pairs {
            prices.entry(y.clone()).or_insert_with(VecDeque::new);
            prices.entry(x.clone()).or_insert_with(VecDeque::new);
        }
        Self { prices, pairs }
    }

    /// Push a new mid-price sample for a symbol.
    pub fn push_price(&mut self, symbol: &str, mid: Decimal) {
        if let Some(buf) = self.prices.get_mut(symbol) {
            if buf.len() >= MAX_PRICE_SAMPLES {
                buf.pop_front();
            }
            buf.push_back(mid);
        }
    }

    /// Number of price samples for a symbol.
    pub fn sample_count(&self, symbol: &str) -> usize {
        self.prices.get(symbol).map(|b| b.len()).unwrap_or(0)
    }

    /// Run cointegration tests on all configured pairs.
    /// Returns results for pairs with enough samples; skips
    /// pairs where either leg has fewer than `MIN_SCREEN_SAMPLES`.
    pub fn screen_all(&self) -> Vec<ScreenResult> {
        let mut results = Vec::new();
        for (y_sym, x_sym) in &self.pairs {
            let (Some(y_buf), Some(x_buf)) = (self.prices.get(y_sym), self.prices.get(x_sym))
            else {
                continue;
            };
            let n = y_buf.len().min(x_buf.len());
            if n < MIN_SCREEN_SAMPLES {
                results.push(ScreenResult {
                    y_symbol: y_sym.clone(),
                    x_symbol: x_sym.clone(),
                    cointegration: None,
                    sample_size: n,
                });
                continue;
            }
            // Align to the most recent `n` samples.
            let y_slice: Vec<Decimal> = y_buf
                .iter()
                .copied()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let x_slice: Vec<Decimal> = x_buf
                .iter()
                .copied()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let coint = EngleGrangerTest::run(&y_slice, &x_slice);
            results.push(ScreenResult {
                y_symbol: y_sym.clone(),
                x_symbol: x_sym.clone(),
                cointegration: coint,
                sample_size: n,
            });
        }
        results
    }

    /// Screen a single pair by index.
    pub fn screen_pair(&self, idx: usize) -> Option<ScreenResult> {
        let (y_sym, x_sym) = self.pairs.get(idx)?;
        let y_buf = self.prices.get(y_sym)?;
        let x_buf = self.prices.get(x_sym)?;
        let n = y_buf.len().min(x_buf.len());
        if n < MIN_SCREEN_SAMPLES {
            return Some(ScreenResult {
                y_symbol: y_sym.clone(),
                x_symbol: x_sym.clone(),
                cointegration: None,
                sample_size: n,
            });
        }
        let y_slice: Vec<Decimal> = y_buf
            .iter()
            .copied()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let x_slice: Vec<Decimal> = x_buf
            .iter()
            .copied()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let coint = EngleGrangerTest::run(&y_slice, &x_slice);
        Some(ScreenResult {
            y_symbol: y_sym.clone(),
            x_symbol: x_sym.clone(),
            cointegration: coint,
            sample_size: n,
        })
    }

    /// Number of configured pairs.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn lcg_innovations(seed: u64, n: usize, range: i64) -> Vec<Decimal> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            let v = ((s >> 16) & 0x7fff) as i64;
            out.push(Decimal::from(v % (2 * range + 1) - range));
        }
        out
    }

    #[test]
    fn screener_detects_cointegrated_pair() {
        let mut screener = PairScreener::new(vec![("Y".into(), "X".into())]);
        let innov = lcg_innovations(1234, 200, 3);
        let eps = lcg_innovations(5678, 200, 5);
        let mut x_val = dec!(100);
        for i in 0..200 {
            x_val += innov[i];
            let y_val = dec!(2) * x_val + eps[i] / dec!(10);
            screener.push_price("X", x_val);
            screener.push_price("Y", y_val);
        }
        let results = screener.screen_all();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(r.cointegration.is_some());
        assert!(r.cointegration.as_ref().unwrap().is_cointegrated);
    }

    #[test]
    fn screener_rejects_independent_walks() {
        let mut screener = PairScreener::new(vec![("A".into(), "B".into())]);
        let i1 = lcg_innovations(111, 200, 3);
        let i2 = lcg_innovations(222, 200, 3);
        let (mut va, mut vb) = (dec!(100), dec!(50));
        for i in 0..200 {
            va += i1[i];
            vb += i2[i];
            screener.push_price("A", va);
            screener.push_price("B", vb);
        }
        let results = screener.screen_all();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(r.cointegration.is_some());
        assert!(!r.cointegration.as_ref().unwrap().is_cointegrated);
    }

    #[test]
    fn screener_skips_insufficient_samples() {
        let mut screener = PairScreener::new(vec![("Y".into(), "X".into())]);
        for i in 0..10 {
            screener.push_price("X", Decimal::from(100 + i));
            screener.push_price("Y", Decimal::from(200 + i));
        }
        let results = screener.screen_all();
        assert_eq!(results.len(), 1);
        assert!(results[0].cointegration.is_none());
        assert_eq!(results[0].sample_size, 10);
    }

    #[test]
    fn rolling_buffer_caps_at_max() {
        let mut screener = PairScreener::new(vec![("A".into(), "B".into())]);
        for i in 0..(MAX_PRICE_SAMPLES + 100) {
            screener.push_price("A", Decimal::from(i as u64));
            screener.push_price("B", Decimal::from(i as u64));
        }
        assert_eq!(screener.sample_count("A"), MAX_PRICE_SAMPLES);
        assert_eq!(screener.sample_count("B"), MAX_PRICE_SAMPLES);
    }

    #[test]
    fn multi_pair_screening() {
        let mut screener =
            PairScreener::new(vec![("A".into(), "B".into()), ("C".into(), "D".into())]);
        let innov = lcg_innovations(42, 200, 3);
        let eps = lcg_innovations(99, 200, 5);
        let mut x_val = dec!(100);
        for i in 0..200 {
            x_val += innov[i];
            screener.push_price("A", dec!(2) * x_val + eps[i] / dec!(10));
            screener.push_price("B", x_val);
            screener.push_price("C", Decimal::from(50 + (i as u64)));
            screener.push_price("D", Decimal::from(100 + (i as u64)));
        }
        let results = screener.screen_all();
        assert_eq!(results.len(), 2);
    }
}
