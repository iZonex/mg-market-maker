//! Market Resilience score.
//!
//! Event-driven detector for **just-happened** liquidity shocks
//! and their recovery profile. Produces a composite score in
//! `[0, 1]` where 1 is "fully resilient" and 0 is "fragile — a
//! large trade just hit and the book has not recovered".
//!
//! The detector is complementary to the stationary toxicity
//! signals we already run (VPIN, Kyle's Lambda): those measure
//! how adversely-selected a venue is on average, while this
//! measures how stressed it is **right now**, in the last few
//! hundred milliseconds after a large trade.
//!
//! Ported from VisualHFT's `MarketResilienceCalculator.cs`
//! (Apache-2.0). The Rust port simplifies the C# state machine
//! by dropping the L3/L2 dual mode and leaving bias detection
//! as a downstream concern, but keeps the same weighted score
//! formula (trade 30 % / spread-recovery 10 % / depth-recovery
//! 50 % / spread-magnitude 10 %) and the same robust baselines
//! (running median + MAD via `P2Quantile`).
//!
//! Downstream consumers read [`MarketResilienceCalculator::score`]
//! once per tick and feed it into the auto-tuner's spread
//! multiplier and the kill-switch L1 "widen" trigger.

use std::collections::VecDeque;

use mm_common::types::PriceLevel;
use mm_common::P2Quantile;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::features::{immediacy_depth_ask, immediacy_depth_bid};

/// Configuration knobs for the Market Resilience calculator.
/// Defaults mirror the VisualHFT reference implementation.
#[derive(Debug, Clone)]
pub struct MrConfig {
    /// Max time in nanoseconds to wait for shock events
    /// (trade / spread / depth) to occur and then recover
    /// before the anchoring state is cleared. Default 800 ms.
    pub shock_timeout_ns: i64,
    /// Z-score threshold for "large trade" detection on the
    /// rolling trade-size window. Default 2.0.
    pub trade_shock_sigma: f64,
    /// Z-score threshold for robust depth-depletion detection
    /// (via median + MAD on immediacy-weighted depth).
    /// Default 3.0.
    pub depth_z_threshold: f64,
    /// Warmup samples before P² baselines are trusted.
    /// Default 200.
    pub warmup_samples: usize,
    /// Depth recovery target as a fraction of the baseline.
    /// Default 0.9.
    pub recovery_target: f64,
    /// Rolling window size for trade sizes (mean + std).
    /// Default 500.
    pub trade_window: usize,
    /// Rolling window size for spreads (mean + std).
    /// Default 500.
    pub spread_window: usize,
    /// Cap on stored recovery times (used for score
    /// normalisation). Default 500.
    pub recovery_history_cap: usize,
    /// Time-decay window: nanoseconds over which a depressed
    /// score linearly recovers toward 1.0 after the last
    /// shock finalization. Default 5 seconds.
    pub decay_window_ns: i64,
}

impl Default for MrConfig {
    fn default() -> Self {
        Self {
            shock_timeout_ns: 800 * 1_000_000,
            trade_shock_sigma: 2.0,
            depth_z_threshold: 3.0,
            warmup_samples: 200,
            recovery_target: 0.9,
            trade_window: 500,
            spread_window: 500,
            recovery_history_cap: 500,
            decay_window_ns: 5 * 1_000_000_000,
        }
    }
}

/// LOB side flag used by the depth-shock state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobSide {
    None,
    Bid,
    Ask,
    Both,
}

impl LobSide {
    fn has_bid(self) -> bool {
        matches!(self, LobSide::Bid | LobSide::Both)
    }
    fn has_ask(self) -> bool {
        matches!(self, LobSide::Ask | LobSide::Both)
    }
}

#[derive(Debug, Clone, Copy)]
struct TradeShockState {
    time_ns: i64,
    size: Decimal,
}

#[derive(Debug, Clone, Copy)]
struct SpreadShockState {
    time_ns: i64,
    spread: Decimal,
    returned_ns: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct DepthShockState {
    time_ns: i64,
    depleted_side: LobSide,
    bid_baseline: f64,
    ask_baseline: f64,
    bid_trough: f64,
    ask_trough: f64,
    recovered_ns: Option<i64>,
}

/// Running statistics for a fixed-width rolling window on
/// positive real observations. Cheap mean + population std via
/// two running sums. Not Welford — we accept the numerical
/// imprecision because these feed a score in `[0, 1]`, not PnL.
#[derive(Debug, Clone, Default)]
struct RollingStats {
    cap: usize,
    samples: VecDeque<Decimal>,
    sum: Decimal,
    sum_sq: Decimal,
}

impl RollingStats {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            samples: VecDeque::with_capacity(cap),
            sum: Decimal::ZERO,
            sum_sq: Decimal::ZERO,
        }
    }

    fn push(&mut self, v: Decimal) {
        if self.samples.len() == self.cap {
            if let Some(old) = self.samples.pop_front() {
                self.sum -= old;
                self.sum_sq -= old * old;
            }
        }
        self.samples.push_back(v);
        self.sum += v;
        self.sum_sq += v * v;
    }

    fn count(&self) -> usize {
        self.samples.len()
    }

    fn mean(&self) -> Decimal {
        if self.samples.is_empty() {
            Decimal::ZERO
        } else {
            self.sum / Decimal::from(self.samples.len() as i64)
        }
    }

    /// Population standard deviation. Returns `0` when fewer
    /// than 2 samples or when the variance goes slightly
    /// negative due to Decimal accumulation drift.
    fn std(&self) -> Decimal {
        let n = self.samples.len();
        if n < 2 {
            return Decimal::ZERO;
        }
        let mean = self.mean();
        let var = self.sum_sq / Decimal::from(n as i64) - mean * mean;
        if var <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let var_f = var.to_f64().unwrap_or(0.0).max(0.0);
        Decimal::from_f64(var_f.sqrt()).unwrap_or(Decimal::ZERO)
    }
}

/// Composite Market Resilience score detector. Fed with a
/// stream of trades and book snapshots, exposes the current
/// resilience score on demand.
pub struct MarketResilienceCalculator {
    config: MrConfig,
    // Rolling baselines for shock detection.
    trade_sizes: RollingStats,
    spreads: RollingStats,
    // Robust depth baselines — P² median + P² of absolute
    // deviations from that median (a running MAD proxy).
    bid_depth_median: P2Quantile,
    ask_depth_median: P2Quantile,
    bid_depth_mad: P2Quantile,
    ask_depth_mad: P2Quantile,
    depth_samples: usize,
    // Active shock anchoring.
    trade_shock: Option<TradeShockState>,
    spread_shock: Option<SpreadShockState>,
    depth_shock: Option<DepthShockState>,
    // Recovery history for score normalisation.
    spread_recovery_hist: VecDeque<f64>,
    depth_recovery_hist: VecDeque<f64>,
    // Last finalised score and the timestamp of that event
    // (used for linear decay back toward 1.0).
    last_score: Decimal,
    last_event_ns: i64,
}

impl MarketResilienceCalculator {
    pub fn new(config: MrConfig) -> Self {
        Self {
            config,
            trade_sizes: RollingStats::new(500),
            spreads: RollingStats::new(500),
            bid_depth_median: P2Quantile::median(),
            ask_depth_median: P2Quantile::median(),
            bid_depth_mad: P2Quantile::median(),
            ask_depth_mad: P2Quantile::median(),
            depth_samples: 0,
            trade_shock: None,
            spread_shock: None,
            depth_shock: None,
            spread_recovery_hist: VecDeque::with_capacity(500),
            depth_recovery_hist: VecDeque::with_capacity(500),
            last_score: Decimal::ONE,
            last_event_ns: 0,
        }
    }

    pub fn with_defaults() -> Self {
        let cfg = MrConfig::default();
        let mut me = Self::new(cfg.clone());
        me.trade_sizes = RollingStats::new(cfg.trade_window);
        me.spreads = RollingStats::new(cfg.spread_window);
        me.spread_recovery_hist = VecDeque::with_capacity(cfg.recovery_history_cap);
        me.depth_recovery_hist = VecDeque::with_capacity(cfg.recovery_history_cap);
        me
    }

    /// Resilience score at `now_ns`, linearly decayed back
    /// toward `1.0` over `config.decay_window_ns` since the
    /// last shock finalisation. Always in `[0, 1]`.
    pub fn score(&self, now_ns: i64) -> Decimal {
        if self.last_event_ns == 0 {
            return Decimal::ONE;
        }
        let elapsed = (now_ns - self.last_event_ns).max(0);
        if elapsed >= self.config.decay_window_ns {
            return Decimal::ONE;
        }
        let frac = (elapsed as f64) / (self.config.decay_window_ns as f64);
        let last = self.last_score.to_f64().unwrap_or(1.0);
        let decayed = last + (1.0 - last) * frac;
        Decimal::from_f64(decayed.clamp(0.0, 1.0)).unwrap_or(Decimal::ONE)
    }

    /// Non-decayed last finalised score. Useful for tests and
    /// diagnostics; production consumers should prefer [`score`].
    pub fn raw_score(&self) -> Decimal {
        self.last_score
    }

    /// Feed a new trade into the detector. Must be called for
    /// every observed public trade — the detector uses the
    /// rolling window of sizes as the baseline against which a
    /// "large" trade is judged.
    pub fn on_trade(&mut self, size: Decimal, now_ns: i64) {
        if size <= Decimal::ZERO {
            return;
        }
        // Expire a stale anchor before potentially accepting a
        // new one — matches the C# TRADE_OnDataReceived logic.
        if let Some(anchor) = self.trade_shock {
            if now_ns - anchor.time_ns > self.config.shock_timeout_ns {
                self.trade_shock = None;
                self.spread_shock = None;
                self.depth_shock = None;
            }
        }
        if self.trade_shock.is_none() && self.is_large_trade(size) {
            self.trade_shock = Some(TradeShockState {
                time_ns: now_ns,
                size,
            });
        } else {
            self.trade_sizes.push(size);
        }
        self.maybe_finalise(now_ns);
    }

    /// Feed a new order-book snapshot into the detector. The
    /// caller is responsible for passing **consistent** top-N
    /// price levels on each side — the detector does not
    /// de-duplicate or sort input levels.
    pub fn on_book(&mut self, bids: &[PriceLevel], asks: &[PriceLevel], now_ns: i64) {
        if bids.is_empty() || asks.is_empty() {
            return;
        }
        let spread = asks[0].price - bids[0].price;
        if spread <= Decimal::ZERO {
            return;
        }

        // Expire stale trade anchor first.
        if let Some(anchor) = self.trade_shock {
            if now_ns - anchor.time_ns > self.config.shock_timeout_ns {
                self.trade_shock = None;
                self.spread_shock = None;
                self.depth_shock = None;
                self.spreads.push(spread);
                return;
            }
        }

        // ---- Spread shock detection / recovery ----
        self.update_spread_state(spread, now_ns);
        self.spreads.push(spread);

        // ---- Depth shock detection / recovery ----
        self.update_depth_state(bids, asks, spread, now_ns);

        self.maybe_finalise(now_ns);
    }

    fn is_large_trade(&self, size: Decimal) -> bool {
        if self.trade_sizes.count() < 3 {
            return false;
        }
        let mean = self.trade_sizes.mean();
        let std = self.trade_sizes.std();
        let k = Decimal::from_f64(self.config.trade_shock_sigma).unwrap_or(dec!(2));
        size > mean + k * std
    }

    fn is_wide_spread(&self, spread: Decimal) -> bool {
        if self.spreads.count() < 3 {
            return false;
        }
        let mean = self.spreads.mean();
        let std = self.spreads.std();
        let k = Decimal::from_f64(self.config.trade_shock_sigma).unwrap_or(dec!(2));
        spread > mean + k * std
    }

    fn update_spread_state(&mut self, spread: Decimal, now_ns: i64) {
        match self.spread_shock {
            None => {
                if self.spread_shock.is_none()
                    && self.is_wide_spread(spread)
                    && self.trade_shock.is_some()
                {
                    self.spread_shock = Some(SpreadShockState {
                        time_ns: now_ns,
                        spread,
                        returned_ns: None,
                    });
                }
            }
            Some(state) if state.returned_ns.is_none() => {
                if now_ns - state.time_ns > self.config.shock_timeout_ns {
                    self.spread_shock = None;
                } else if spread < self.spreads.mean() {
                    if let Some(s) = self.spread_shock.as_mut() {
                        s.returned_ns = Some(now_ns);
                    }
                }
            }
            Some(state) => {
                // Already recovered — just monitor timeout.
                if now_ns - state.time_ns > self.config.shock_timeout_ns {
                    self.spread_shock = None;
                }
            }
        }
    }

    fn update_depth_state(
        &mut self,
        bids: &[PriceLevel],
        asks: &[PriceLevel],
        spread: Decimal,
        now_ns: i64,
    ) {
        // Current immediacy-weighted depth per side. We
        // compute in f64 (spread-unit distances are unitless
        // ratios, P² lives in f64) while using Decimal for the
        // spread basis to avoid lossy conversion of prices.
        let d_bid = immediacy_depth_bid(bids, spread).to_f64().unwrap_or(0.0);
        let d_ask = immediacy_depth_ask(asks, spread).to_f64().unwrap_or(0.0);

        // Update P² medians for depth.
        self.bid_depth_median.observe(d_bid);
        self.ask_depth_median.observe(d_ask);
        let bid_med = self.bid_depth_median.estimate();
        let ask_med = self.ask_depth_median.estimate();

        // Update P²-based MAD proxies once the medians have
        // stabilised — the C# code waits until sample count
        // >= 5, we match that.
        if self.depth_samples >= 5 {
            self.bid_depth_mad.observe((d_bid - bid_med).abs());
            self.ask_depth_mad.observe((d_ask - ask_med).abs());
        }
        self.depth_samples += 1;

        if self.depth_samples < self.config.warmup_samples {
            return;
        }

        // Detect depletion via robust z-score.
        let bid_mad = self.bid_depth_mad.estimate().max(1e-12);
        let ask_mad = self.ask_depth_mad.estimate().max(1e-12);
        let bid_z = (bid_med - d_bid) / bid_mad;
        let ask_z = (ask_med - d_ask) / ask_mad;

        let bid_depleted = bid_z >= self.config.depth_z_threshold && d_bid < bid_med;
        let ask_depleted = ask_z >= self.config.depth_z_threshold && d_ask < ask_med;
        let depleted = match (bid_depleted, ask_depleted) {
            (true, true) => LobSide::Both,
            (true, false) => LobSide::Bid,
            (false, true) => LobSide::Ask,
            (false, false) => LobSide::None,
        };

        match self.depth_shock {
            None => {
                if depleted != LobSide::None && self.trade_shock.is_some() {
                    self.depth_shock = Some(DepthShockState {
                        time_ns: now_ns,
                        depleted_side: depleted,
                        bid_baseline: bid_med,
                        ask_baseline: ask_med,
                        bid_trough: d_bid,
                        ask_trough: d_ask,
                        recovered_ns: None,
                    });
                }
            }
            Some(mut state) if state.recovered_ns.is_none() => {
                if now_ns - state.time_ns > self.config.shock_timeout_ns {
                    self.depth_shock = None;
                    return;
                }
                if d_bid < state.bid_trough {
                    state.bid_trough = d_bid;
                }
                if d_ask < state.ask_trough {
                    state.ask_trough = d_ask;
                }

                let denom_bid = (state.bid_baseline - state.bid_trough).max(1e-12);
                let denom_ask = (state.ask_baseline - state.ask_trough).max(1e-12);
                let rec_bid = ((d_bid - state.bid_trough) / denom_bid).clamp(0.0, 1.0);
                let rec_ask = ((d_ask - state.ask_trough) / denom_ask).clamp(0.0, 1.0);

                let bid_hit =
                    state.depleted_side.has_bid() && rec_bid >= self.config.recovery_target;
                let ask_hit =
                    state.depleted_side.has_ask() && rec_ask >= self.config.recovery_target;
                if bid_hit || ask_hit {
                    state.recovered_ns = Some(now_ns);
                }
                self.depth_shock = Some(state);
            }
            Some(state) => {
                if now_ns - state.time_ns > self.config.shock_timeout_ns {
                    self.depth_shock = None;
                }
            }
        }
    }

    fn maybe_finalise(&mut self, now_ns: i64) {
        let spread_ready = matches!(
            self.spread_shock,
            Some(s) if s.returned_ns.is_some()
        );
        let depth_ready = matches!(
            self.depth_shock,
            Some(s) if s.recovered_ns.is_some()
        );
        if !spread_ready && !depth_ready {
            return;
        }

        let mut total_weight = 0.0_f64;
        let mut weighted = 0.0_f64;

        // Component 0: trade shock severity (30 %).
        const W_TRADE: f64 = 0.3;
        if let Some(t) = self.trade_shock {
            let mean = self.trade_sizes.mean();
            let std = self.trade_sizes.std();
            if std > Decimal::ZERO {
                let z = ((t.size - mean) / std).to_f64().unwrap_or(0.0);
                let trade_score = (1.0 - z / 6.0).clamp(0.0, 1.0);
                weighted += W_TRADE * trade_score;
                total_weight += W_TRADE;
            }
        }

        // Component 1: spread recovery timing (10 %).
        const W_SPREAD: f64 = 0.1;
        if let Some(s) = self.spread_shock {
            if let Some(ret_ns) = s.returned_ns {
                let observed = ((ret_ns - s.time_ns).abs() as f64).max(0.0);
                let avg_hist = if self.spread_recovery_hist.is_empty() {
                    observed
                } else {
                    let sum: f64 = self.spread_recovery_hist.iter().sum();
                    sum / (self.spread_recovery_hist.len() as f64)
                };
                let score = (avg_hist / (avg_hist + observed + 1e-9)).clamp(0.0, 1.0);
                weighted += W_SPREAD * score;
                total_weight += W_SPREAD;
                if self.spread_recovery_hist.len() == self.config.recovery_history_cap {
                    self.spread_recovery_hist.pop_front();
                }
                self.spread_recovery_hist.push_back(observed);
            }
        }

        // Component 2: depth recovery timing (50 %).
        const W_DEPTH: f64 = 0.5;
        if let Some(d) = self.depth_shock {
            if let Some(ret_ns) = d.recovered_ns {
                let observed = ((ret_ns - d.time_ns).abs() as f64).max(0.0);
                let avg_hist = if self.depth_recovery_hist.is_empty() {
                    observed
                } else {
                    let sum: f64 = self.depth_recovery_hist.iter().sum();
                    sum / (self.depth_recovery_hist.len() as f64)
                };
                let score = (avg_hist / (avg_hist + observed + 1e-9)).clamp(0.0, 1.0);
                weighted += W_DEPTH * score;
                total_weight += W_DEPTH;
                if self.depth_recovery_hist.len() == self.config.recovery_history_cap {
                    self.depth_recovery_hist.pop_front();
                }
                self.depth_recovery_hist.push_back(observed);
            }
        }

        // Component 3: spread shock magnitude (10 %).
        const W_MAG: f64 = 0.1;
        if let Some(s) = self.spread_shock {
            let avg = if self.spreads.count() == 0 {
                s.spread
            } else {
                self.spreads.mean()
            };
            if avg > Decimal::ZERO {
                let ratio = (s.spread / avg).to_f64().unwrap_or(1.0).max(1e-9);
                let score = (1.0 / ratio).clamp(0.0, 1.0);
                weighted += W_MAG * score;
                total_weight += W_MAG;
            }
        }

        let final_score = if total_weight > 0.0 {
            (weighted / total_weight).clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.last_score = Decimal::from_f64(final_score).unwrap_or(Decimal::ONE);
        self.last_event_ns = now_ns;

        // Reset the shock state — next shock cycle starts from
        // a clean slate.
        self.trade_shock = None;
        self.spread_shock = None;
        self.depth_shock = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    fn level(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    fn deep_book() -> (Vec<PriceLevel>, Vec<PriceLevel>) {
        let bids = vec![
            level(dec!(100), dec!(50)),
            level(dec!(99), dec!(40)),
            level(dec!(98), dec!(30)),
            level(dec!(97), dec!(20)),
        ];
        let asks = vec![
            level(dec!(101), dec!(50)),
            level(dec!(102), dec!(40)),
            level(dec!(103), dec!(30)),
            level(dec!(104), dec!(20)),
        ];
        (bids, asks)
    }

    fn small_config() -> MrConfig {
        MrConfig {
            warmup_samples: 5,
            shock_timeout_ns: 1_000_000_000,
            decay_window_ns: 10_000_000_000,
            ..MrConfig::default()
        }
    }

    /// Before any shock, the score is the resilient default 1.0.
    #[test]
    fn score_starts_at_one() {
        let calc = MarketResilienceCalculator::new(small_config());
        assert_eq!(calc.score(0), Decimal::ONE);
        assert_eq!(calc.raw_score(), Decimal::ONE);
    }

    /// Small trades never trigger the shock path — the score
    /// stays at the resilient default.
    #[test]
    fn small_trades_do_not_trigger_shock() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        for i in 0..20 {
            calc.on_trade(dec!(1), i * 1_000_000);
        }
        assert_eq!(calc.raw_score(), Decimal::ONE);
    }

    /// A single huge trade relative to the rolling window
    /// registers as a trade shock (verified indirectly via a
    /// subsequent spread recovery that produces a finalised
    /// score below 1.0).
    #[test]
    fn large_trade_plus_spread_recovery_finalises_score_below_one() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        // Warm up rolling windows with small trades and tight
        // spreads. Tight spreads = 0.1.
        let (bids, asks) = deep_book();
        for i in 0..50 {
            calc.on_trade(dec!(1), i * 100_000);
            calc.on_book(&bids, &asks, i * 100_000);
        }
        // Large trade and widened spread = big shock.
        calc.on_trade(dec!(100), 60_000_000);
        let wide_asks = vec![
            level(dec!(105), dec!(50)),
            level(dec!(106), dec!(40)),
            level(dec!(107), dec!(30)),
        ];
        calc.on_book(&bids, &wide_asks, 60_000_000);
        // Spread returns to mean.
        calc.on_book(&bids, &asks, 80_000_000);
        let s = calc.raw_score();
        assert!(
            s < Decimal::ONE,
            "expected a finalised resilience score below 1.0, got {s}"
        );
        assert!(s >= Decimal::ZERO, "score must be non-negative, got {s}");
    }

    /// Non-positive trade sizes are ignored.
    #[test]
    fn non_positive_trade_sizes_are_ignored() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        calc.on_trade(Decimal::ZERO, 1);
        calc.on_trade(dec!(-5), 2);
        assert_eq!(calc.raw_score(), Decimal::ONE);
    }

    /// Score decays back toward 1.0 after the decay window
    /// elapses without new shocks.
    #[test]
    fn score_decays_back_to_one_after_decay_window() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        // Force a low raw score by poking internals via the
        // public on_trade / on_book API would require a full
        // shock cycle. Skip that: directly seed the state.
        calc.last_score = dec!(0.2);
        calc.last_event_ns = 1_000_000_000;
        // Half-way through the decay window (5s default, set
        // to 10s in small_config).
        let mid = calc.score(1_000_000_000 + 5_000_000_000);
        assert!(mid > dec!(0.5) && mid < dec!(0.7), "mid={mid}");
        // Past the decay window — fully recovered.
        let after = calc.score(1_000_000_000 + 11_000_000_000);
        assert_eq!(after, Decimal::ONE);
    }

    /// Empty books are ignored — no panic, no state advance.
    #[test]
    fn empty_books_are_ignored() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        let empty: Vec<PriceLevel> = vec![];
        calc.on_book(&empty, &empty, 0);
        assert_eq!(calc.raw_score(), Decimal::ONE);
    }

    /// Inverted books (bid >= ask) are ignored — they'd
    /// produce a negative spread which is nonsense.
    #[test]
    fn inverted_books_are_ignored() {
        let mut calc = MarketResilienceCalculator::new(small_config());
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(99), dec!(10))];
        calc.on_book(&bids, &asks, 0);
        assert_eq!(calc.raw_score(), Decimal::ONE);
    }

    /// LobSide helpers correctly identify covered sides.
    #[test]
    fn lobside_helpers() {
        assert!(LobSide::Bid.has_bid());
        assert!(!LobSide::Bid.has_ask());
        assert!(LobSide::Ask.has_ask());
        assert!(LobSide::Both.has_bid());
        assert!(LobSide::Both.has_ask());
        assert!(!LobSide::None.has_bid());
        assert!(!LobSide::None.has_ask());
    }

    /// Rolling stats: mean and std on a constant stream
    /// collapse to `(value, 0)`.
    #[test]
    fn rolling_stats_on_constant_stream() {
        let mut rs = RollingStats::new(10);
        for _ in 0..10 {
            rs.push(dec!(7));
        }
        assert_eq!(rs.mean(), dec!(7));
        assert_eq!(rs.std(), Decimal::ZERO);
    }

    /// Rolling stats evict the oldest sample once the window
    /// is full.
    #[test]
    fn rolling_stats_evicts_old_samples() {
        let mut rs = RollingStats::new(3);
        rs.push(dec!(1));
        rs.push(dec!(2));
        rs.push(dec!(3));
        rs.push(dec!(4)); // evicts `1`
        assert_eq!(rs.count(), 3);
        assert_eq!(rs.mean(), dec!(3));
    }

    /// Default config matches the VisualHFT reference values.
    #[test]
    fn default_config_matches_reference_values() {
        let cfg = MrConfig::default();
        assert_eq!(cfg.shock_timeout_ns, 800_000_000);
        assert!((cfg.trade_shock_sigma - 2.0).abs() < 1e-9);
        assert!((cfg.depth_z_threshold - 3.0).abs() < 1e-9);
        assert_eq!(cfg.warmup_samples, 200);
        assert!((cfg.recovery_target - 0.9).abs() < 1e-9);
    }

    // ── Property-based tests (Epic 13) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn trade_size_strat()(units in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(units, 4)
        }
    }

    proptest! {
        /// score() always lies in [0, 1] — this is the
        /// dashboard-gauge contract. Tested against random trade
        /// streams + random elapsed times, including the
        /// no-events-yet case where score must default to 1.0.
        #[test]
        fn score_is_bounded_in_0_1(
            sizes in proptest::collection::vec(trade_size_strat(), 0..40),
            query_delay_ns in 0i64..10_000_000_000i64,
        ) {
            let mut mr = MarketResilienceCalculator::with_defaults();
            let mut now = 0i64;
            for s in &sizes {
                now += 1_000_000;  // 1ms per trade
                mr.on_trade(*s, now);
            }
            let s = mr.score(now + query_delay_ns);
            prop_assert!(s >= dec!(0), "score {} < 0", s);
            prop_assert!(s <= dec!(1), "score {} > 1", s);
        }

        /// Fresh calculator (no events) always reports score 1.0
        /// regardless of the query time. Catches a regression
        /// where a division-by-zero in the decay window would
        /// surface as NaN or 0 on a cold engine.
        #[test]
        fn fresh_calculator_is_full_resilience(
            now_ns in 0i64..1_000_000_000_000i64,
        ) {
            let mr = MarketResilienceCalculator::with_defaults();
            prop_assert_eq!(mr.score(now_ns), dec!(1));
        }

        /// After enough time past the last event, score decays
        /// back to exactly 1.0. The calculator models a
        /// self-healing market — a sustained low score would
        /// get stuck under a bug that forgot to reset the
        /// anchor timestamp.
        #[test]
        fn score_decays_to_one_after_decay_window(
            sizes in proptest::collection::vec(trade_size_strat(), 1..30),
            extra_time_ns in 0i64..1_000_000_000i64,
        ) {
            let mut mr = MarketResilienceCalculator::with_defaults();
            let mut now = 0i64;
            for s in &sizes {
                now += 1_000_000;
                mr.on_trade(*s, now);
            }
            // Jump past the full decay window + any extra time.
            let far_future = now + 10_000_000_000 + extra_time_ns;
            prop_assert_eq!(mr.score(far_future), dec!(1));
        }

        /// Zero / negative trade size is silently ignored —
        /// never produces a NaN or crashes the detector.
        /// Defensive against connectors that forward malformed
        /// trades.
        #[test]
        fn zero_or_negative_size_is_ignored(
            now_ns in 1i64..1_000_000_000_000i64,
        ) {
            let mut mr = MarketResilienceCalculator::with_defaults();
            mr.on_trade(dec!(0), now_ns);
            mr.on_trade(-dec!(5), now_ns);
            prop_assert_eq!(mr.score(now_ns), dec!(1));
        }
    }
}
