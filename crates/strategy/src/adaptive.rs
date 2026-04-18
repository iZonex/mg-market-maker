//! Online AdaptiveTuner (Epic 30 sub-task E30.4).
//!
//! Sits **on top of** `AutoTuner` as an additional multiplier
//! layer. Feedback loop: rolling realised fill rate + spread
//! capture + adverse selection → slow-moving γ adjustment,
//! rate-limited to ±`max_adj_per_min` per one-minute bucket.
//! Absolute bounds prevent any single bad minute (or a cascade of
//! them) from stretching γ into a pathological zone.
//!
//! Multiplier stack (see `docs/research/adaptive-calibration.md`):
//!
//! ```text
//! γ_effective = γ_base(pair_class)
//!             × gamma_mult_regime(AutoTuner)    // 0.6..3.0, fast
//!             × gamma_mult_adaptive(AdaptiveTuner) // 0.25..4.0, slow (this module)
//!             × gamma_mult_manual(ConfigOverride) // operator floor/ceiling
//! ```
//!
//! Opt-in. A freshly-constructed tuner with `enabled=false`
//! returns `1.0` for every multiplier accessor — adding it to the
//! engine is a zero-cost change until an operator sets
//! `market_maker.adaptive_enabled = true`.
//!
//! Design decisions are documented in the research doc referenced
//! above.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Configuration knobs that control the controller's feedback
/// aggressiveness and safety bounds. Ship sensible defaults; the
/// operator can tune per deployment.
#[derive(Debug, Clone)]
pub struct AdaptiveConfig {
    /// Target fills per one-minute bucket. Per-class calibrated:
    /// MajorSpot might expect 5/min, MemeSpot 0.5/min.
    pub target_fills_per_min: Decimal,
    /// Max |Δγ_mult| / minute — 0.05 = 5 % move per bucket.
    pub max_adj_per_min: Decimal,
    /// Hard floor for the multiplier. 0.25 = cannot quote more
    /// than 4× tighter than base.
    pub gamma_factor_min: Decimal,
    /// Hard ceiling. 4.0 = cannot widen beyond 4× base.
    pub gamma_factor_max: Decimal,
    /// If inventory-volatility EWMA exceeds this (base-asset
    /// units), γ nudges up.
    pub inv_vol_threshold: Decimal,
    /// Adverse-selection trigger (bps). Above this, γ nudges up
    /// regardless of fill rate.
    pub adverse_bps_threshold: Decimal,
    /// Bucket length. One minute is the default; shorten for
    /// dev/test scenarios only.
    pub bucket_secs: u64,
    /// Rolling window length in buckets. Default 60 (= 1 hour).
    pub window_buckets: usize,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            target_fills_per_min: dec!(5),
            max_adj_per_min: dec!(0.05),
            gamma_factor_min: dec!(0.25),
            gamma_factor_max: dec!(4.0),
            inv_vol_threshold: dec!(0.005),
            adverse_bps_threshold: dec!(5),
            bucket_secs: 60,
            window_buckets: 60,
        }
    }
}

/// Reason the controller picked its most recent adjustment —
/// logged + published to the dashboard for operator visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjustmentReason {
    /// No change (inside tolerance band or disabled).
    NoOp,
    /// Fill rate below target — γ cut (tighter quotes attract fills).
    TightenForFills,
    /// Inventory volatility too high — γ raised (widen defences).
    WidenForInventory,
    /// Adverse selection too high — γ raised + spread widened.
    WidenForAdverse,
    /// Realised spread capture below fees — γ raised.
    WidenForNegativeEdge,
    /// Rate-limited — controller wanted to move further but clamped.
    RateLimited,
    /// Hit the absolute min/max bound.
    Clamped,
}

/// One-minute bucket of per-symbol trading stats. Rolled up every
/// `bucket_secs` by `maybe_rollover`.
#[derive(Debug, Clone, Default)]
struct Bucket {
    fills: u32,
    volume_quote: Decimal,
    spread_capture_quote: Decimal,
    fee_paid_quote: Decimal,
    adverse_bps_sum: Decimal,
    adverse_bps_count: u32,
}

impl Bucket {
    fn net_edge(&self) -> Decimal {
        self.spread_capture_quote - self.fee_paid_quote
    }
}

/// Online controller state. Holds the rolling history + current
/// multiplier, exposes accessors the engine reads each tick.
pub struct AdaptiveTuner {
    enabled: bool,
    cfg: AdaptiveConfig,
    /// Rolling window of completed buckets. Newest at back.
    buckets: VecDeque<Bucket>,
    /// Bucket currently being filled (not yet rolled over).
    current: Bucket,
    /// Last time we rolled the current bucket into `buckets`.
    last_rollover: Option<Instant>,
    /// Current γ multiplier. Starts at 1.0, moves toward target in
    /// `max_adj_per_min` steps.
    gamma_factor: Decimal,
    /// Target γ multiplier the controller is walking toward.
    gamma_target: Decimal,
    /// EWMA of |inventory − mean| in base-asset units.
    inv_vol_ewma: Decimal,
    /// Previous inventory reading; used to derive Δ for EWMA.
    prev_inventory: Option<Decimal>,
    /// Last reason — published to the dashboard.
    last_reason: AdjustmentReason,
    /// Manual operator overrides. Both optional; when set they
    /// clamp the multiplier within [floor, ceiling] regardless of
    /// what the feedback rules suggest.
    manual_floor: Option<Decimal>,
    manual_ceiling: Option<Decimal>,
}

impl AdaptiveTuner {
    pub fn new(cfg: AdaptiveConfig) -> Self {
        Self {
            enabled: false,
            buckets: VecDeque::with_capacity(cfg.window_buckets),
            cfg,
            current: Bucket::default(),
            last_rollover: None,
            gamma_factor: dec!(1),
            gamma_target: dec!(1),
            inv_vol_ewma: dec!(0),
            prev_inventory: None,
            last_reason: AdjustmentReason::NoOp,
            manual_floor: None,
            manual_ceiling: None,
        }
    }

    pub fn enable(&mut self, on: bool) {
        self.enabled = on;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Operator-controlled clamps — whichever is tighter wins vs
    /// the absolute config bounds. Passing `None` clears the
    /// floor/ceiling.
    pub fn set_manual_bounds(&mut self, floor: Option<Decimal>, ceiling: Option<Decimal>) {
        self.manual_floor = floor;
        self.manual_ceiling = ceiling;
    }

    pub fn last_reason(&self) -> &AdjustmentReason {
        &self.last_reason
    }

    /// Current γ multiplier. 1.0 means no adjustment; returns 1.0
    /// when `enabled == false`.
    pub fn gamma_factor(&self) -> Decimal {
        if self.enabled {
            self.gamma_factor
        } else {
            dec!(1)
        }
    }

    /// Fill event — called by the engine from
    /// `handle_ws_event(Fill)`. `edge_bps` is realised spread
    /// capture in bps (positive = we earned edge), `fee_bps` is
    /// the fee charged on this fill (always non-negative).
    pub fn on_fill(
        &mut self,
        price: Decimal,
        qty: Decimal,
        edge_bps: Decimal,
        fee_bps: Decimal,
    ) {
        if !self.enabled {
            return;
        }
        let notional = price * qty;
        self.current.fills += 1;
        self.current.volume_quote += notional;
        // bps → quote via notional * bps / 10_000.
        self.current.spread_capture_quote += notional * edge_bps / dec!(10_000);
        self.current.fee_paid_quote += notional * fee_bps / dec!(10_000);
    }

    /// Adverse-selection observation — called from the engine's
    /// `AdverseSelectionTracker`. Fed in bps (sign ignored for
    /// aggregation, but convention is positive = adverse).
    pub fn on_adverse(&mut self, bps: Decimal) {
        if !self.enabled {
            return;
        }
        self.current.adverse_bps_sum += bps;
        self.current.adverse_bps_count += 1;
    }

    /// Inventory reading — called on every fill or balance update.
    /// Feeds the EWMA used by the WidenForInventory rule.
    pub fn on_inventory(&mut self, q: Decimal) {
        if !self.enabled {
            return;
        }
        let alpha = dec!(0.1);
        match self.prev_inventory {
            None => {
                self.prev_inventory = Some(q);
            }
            Some(prev) => {
                let delta = (q - prev).abs();
                self.inv_vol_ewma = alpha * delta + (dec!(1) - alpha) * self.inv_vol_ewma;
                self.prev_inventory = Some(q);
            }
        }
    }

    /// Periodic call from the engine's tick loop. When a new
    /// bucket window has elapsed, rolls the current bucket into
    /// the history, evaluates feedback rules, and steps the γ
    /// factor toward the new target (rate-limited).
    pub fn tick(&mut self, now: Instant) {
        if !self.enabled {
            return;
        }
        let last = match self.last_rollover {
            Some(t) => t,
            None => {
                self.last_rollover = Some(now);
                return;
            }
        };
        if now.duration_since(last) < Duration::from_secs(self.cfg.bucket_secs) {
            return;
        }
        // Roll the current bucket.
        let finished = std::mem::take(&mut self.current);
        self.buckets.push_back(finished);
        while self.buckets.len() > self.cfg.window_buckets {
            self.buckets.pop_front();
        }
        self.last_rollover = Some(now);

        // Compute new target + step toward it.
        let (target, reason) = self.compute_target();
        self.gamma_target = target;
        self.last_reason = reason.clone();
        self.step_toward_target();
    }

    fn compute_target(&self) -> (Decimal, AdjustmentReason) {
        // Use the most recent ≤window_buckets of buckets.
        let (total_fills, net_edge, adverse_avg, adverse_count) = self.buckets.iter().fold(
            (0u32, dec!(0), dec!(0), 0u32),
            |(f, e, aa, ac), b| {
                let new_ac = ac + b.adverse_bps_count;
                let new_aa = aa + b.adverse_bps_sum;
                (f + b.fills, e + b.net_edge(), new_aa, new_ac)
            },
        );
        let n = Decimal::from(self.buckets.len().max(1) as u32);
        let fills_per_min = Decimal::from(total_fills) / n;
        let adverse_avg = if adverse_count == 0 {
            dec!(0)
        } else {
            adverse_avg / Decimal::from(adverse_count)
        };

        let mut target = self.gamma_factor;
        let mut reason = AdjustmentReason::NoOp;

        // Rule 1 — adverse selection dominates. Widen hard.
        if adverse_avg > self.cfg.adverse_bps_threshold {
            target = self.gamma_factor * dec!(1.10);
            reason = AdjustmentReason::WidenForAdverse;
        // Rule 2 — net edge negative (fees eating spread). Widen.
        } else if net_edge < dec!(0) {
            target = self.gamma_factor * dec!(1.05);
            reason = AdjustmentReason::WidenForNegativeEdge;
        // Rule 3 — inventory volatility high. Widen gently.
        } else if self.inv_vol_ewma > self.cfg.inv_vol_threshold {
            target = self.gamma_factor * dec!(1.03);
            reason = AdjustmentReason::WidenForInventory;
        // Rule 4 — under-filling + inventory calm + edge positive → tighten.
        } else if fills_per_min < self.cfg.target_fills_per_min / dec!(2) {
            target = self.gamma_factor * dec!(0.95);
            reason = AdjustmentReason::TightenForFills;
        }

        (target, reason)
    }

    fn step_toward_target(&mut self) {
        let delta = self.gamma_target - self.gamma_factor;
        let step_limit = self.cfg.max_adj_per_min * self.gamma_factor;
        let step = if delta.abs() > step_limit {
            if matches!(self.last_reason, AdjustmentReason::NoOp) {
                // Target is close enough — no movement.
                dec!(0)
            } else {
                let sign = if delta > dec!(0) { dec!(1) } else { dec!(-1) };
                self.last_reason = AdjustmentReason::RateLimited;
                sign * step_limit
            }
        } else {
            delta
        };
        let new_factor = self.gamma_factor + step;

        // Absolute bounds from cfg, tightened further by manual
        // operator clamps. Operator bounds always win.
        let lo = self
            .manual_floor
            .map(|m| m.max(self.cfg.gamma_factor_min))
            .unwrap_or(self.cfg.gamma_factor_min);
        let hi = self
            .manual_ceiling
            .map(|m| m.min(self.cfg.gamma_factor_max))
            .unwrap_or(self.cfg.gamma_factor_max);
        let clamped = new_factor.clamp(lo, hi);
        if clamped != new_factor {
            self.last_reason = AdjustmentReason::Clamped;
        }
        self.gamma_factor = clamped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_cfg() -> AdaptiveConfig {
        // 1-second buckets for deterministic tests.
        AdaptiveConfig {
            bucket_secs: 1,
            window_buckets: 10,
            target_fills_per_min: dec!(5),
            max_adj_per_min: dec!(0.10),
            ..AdaptiveConfig::default()
        }
    }

    #[test]
    fn disabled_returns_neutral_multiplier() {
        let t = AdaptiveTuner::new(AdaptiveConfig::default());
        assert_eq!(t.gamma_factor(), dec!(1));
    }

    #[test]
    fn disabled_ignores_events() {
        let mut t = AdaptiveTuner::new(AdaptiveConfig::default());
        t.on_fill(dec!(100), dec!(1), dec!(-10), dec!(5));
        t.on_adverse(dec!(50));
        t.on_inventory(dec!(1));
        assert_eq!(t.gamma_factor(), dec!(1));
    }

    #[test]
    fn widens_on_high_adverse() {
        let mut t = AdaptiveTuner::new(fast_cfg());
        t.enable(true);
        let start = Instant::now();
        t.tick(start);

        // Stuff a bucket with heavy adverse selection.
        for _ in 0..5 {
            t.on_adverse(dec!(20));
            t.on_fill(dec!(100), dec!(1), dec!(5), dec!(1));
        }
        let advanced = start + Duration::from_secs(2);
        t.tick(advanced);
        assert!(t.gamma_factor() > dec!(1), "expected widening, got {}", t.gamma_factor());
    }

    #[test]
    fn widens_on_negative_net_edge() {
        let mut t = AdaptiveTuner::new(fast_cfg());
        t.enable(true);
        let start = Instant::now();
        t.tick(start);

        // Spread capture 1 bps, fees 5 bps — net negative.
        for _ in 0..5 {
            t.on_fill(dec!(100), dec!(1), dec!(1), dec!(5));
        }
        let advanced = start + Duration::from_secs(2);
        t.tick(advanced);
        assert!(t.gamma_factor() > dec!(1));
        assert!(matches!(
            t.last_reason(),
            AdjustmentReason::WidenForNegativeEdge
                | AdjustmentReason::RateLimited
                | AdjustmentReason::Clamped
        ));
    }

    #[test]
    fn tightens_on_low_fill_rate_with_positive_edge() {
        let mut t = AdaptiveTuner::new(fast_cfg());
        t.enable(true);
        let start = Instant::now();
        t.tick(start);

        // Single fill per bucket, tiny but positive net edge.
        for i in 0..5 {
            t.on_fill(dec!(100), dec!(0.01), dec!(5), dec!(1));
            let tick_at = start + Duration::from_secs((i + 1) as u64 * 2);
            t.tick(tick_at);
        }
        assert!(t.gamma_factor() < dec!(1), "expected tightening, got {}", t.gamma_factor());
    }

    #[test]
    fn rate_limit_caps_single_step() {
        let mut cfg = fast_cfg();
        cfg.max_adj_per_min = dec!(0.05);
        let mut t = AdaptiveTuner::new(cfg);
        t.enable(true);
        let start = Instant::now();
        t.tick(start);

        // Crank adverse hard.
        for _ in 0..100 {
            t.on_adverse(dec!(100));
            t.on_fill(dec!(100), dec!(1), dec!(5), dec!(1));
        }
        t.tick(start + Duration::from_secs(2));
        // Single step: should be ≤ 5 % from the starting 1.0.
        assert!(t.gamma_factor() <= dec!(1.06));
    }

    #[test]
    fn absolute_bounds_cap_runaway_target() {
        let mut cfg = fast_cfg();
        cfg.max_adj_per_min = dec!(5.0); // allow huge single step
        cfg.gamma_factor_max = dec!(2.0); // but cap absolute ceiling
        let mut t = AdaptiveTuner::new(cfg);
        t.enable(true);
        let start = Instant::now();
        t.tick(start);
        // Drive adverse selection for many buckets so γ climbs
        // past the cap, proving the hard bound holds.
        for i in 1..=30 {
            for _ in 0..100 {
                t.on_adverse(dec!(500));
                t.on_fill(dec!(100), dec!(1), dec!(-20), dec!(1));
            }
            t.tick(start + Duration::from_secs(i * 2));
        }
        assert!(t.gamma_factor() <= dec!(2.0));
        // Latest reason is either Clamped (most recent step hit
        // the wall) or one of the widening reasons if the final
        // step landed exactly on the ceiling without triggering
        // the not-equal branch.
        let reason = t.last_reason().clone();
        assert!(
            matches!(
                reason,
                AdjustmentReason::Clamped | AdjustmentReason::WidenForAdverse
            ),
            "unexpected reason {:?} with γ={}",
            reason,
            t.gamma_factor()
        );
    }

    #[test]
    fn manual_ceiling_tighter_than_config_wins() {
        let mut cfg = fast_cfg();
        cfg.gamma_factor_max = dec!(4.0);
        cfg.max_adj_per_min = dec!(5.0);
        let mut t = AdaptiveTuner::new(cfg);
        t.enable(true);
        t.set_manual_bounds(None, Some(dec!(1.2)));
        let start = Instant::now();
        t.tick(start);
        for _ in 0..20 {
            t.on_adverse(dec!(200));
            t.on_fill(dec!(100), dec!(1), dec!(-5), dec!(1));
        }
        t.tick(start + Duration::from_secs(2));
        assert!(t.gamma_factor() <= dec!(1.2));
    }

    #[test]
    fn inventory_vol_triggers_widening() {
        let mut cfg = fast_cfg();
        cfg.inv_vol_threshold = dec!(0.001);
        let mut t = AdaptiveTuner::new(cfg);
        t.enable(true);
        let start = Instant::now();
        t.tick(start);
        for i in 0..30 {
            let q = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
            t.on_inventory(q);
            t.on_fill(dec!(100), dec!(0.1), dec!(5), dec!(1));
        }
        t.tick(start + Duration::from_secs(2));
        assert!(t.gamma_factor() > dec!(1));
    }

    #[test]
    fn rollover_keeps_at_most_window_buckets() {
        let mut cfg = fast_cfg();
        cfg.window_buckets = 3;
        let mut t = AdaptiveTuner::new(cfg);
        t.enable(true);
        let start = Instant::now();
        t.tick(start);
        for i in 1..=10 {
            t.on_fill(dec!(100), dec!(1), dec!(5), dec!(1));
            t.tick(start + Duration::from_secs(i * 2));
        }
        assert_eq!(t.buckets.len(), 3);
    }
}
