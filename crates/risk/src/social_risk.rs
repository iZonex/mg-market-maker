//! Social/sentiment risk layer (Epic G, stage 2).
//!
//! Consumes `mm_sentiment::SentimentTick` and returns the
//! composite risk adjustment the engine applies to its
//! quoting state. Ideology: MM does NOT follow the crowd's
//! direction, it widens / shrinks / retreats when the crowd
//! arrives.
//!
//! # Output semantics
//!
//! Every evaluation returns a [`SocialRiskState`]:
//!
//! - `vol_multiplier` — multiplies base spread (1.0 flat, up
//!   to `max_vol_multiplier`). Fed into the autotuner's
//!   multiplier pipeline alongside news_retreat / lead_lag /
//!   market_resilience.
//! - `size_multiplier` — shrinks per-quote size under spike
//!   conditions (1.0 flat, down to `min_size_multiplier`).
//! - `inv_skew_bps` — signed shift applied to the
//!   reservation mid. Positive when sentiment + acceleration
//!   align bullish; negative bearish. Capped so a single
//!   sentiment spike can't produce a runaway skew.
//! - `kill_trigger` — `true` when `mentions_rate >
//!   kill_mentions_rate` AND cross-validated against the
//!   realised-vol input the engine passes in. Crossing this
//!   threshold is what promotes the signal from "widen" to
//!   "flatten" — a single spiking metric is never enough on
//!   its own.
//!
//! # Cross-validation with market signals
//!
//! [`SocialRiskEngine::evaluate`] takes BOTH a sentiment tick
//! and a *market context* struct (realised vol, OFI z-score).
//! A mentions spike with flat OFI ≈ "chatter, no money" →
//! widen only, no size cut, no kill. Mentions + OFI same
//! direction ≈ "crowd is trading" → full retreat. This is
//! the "связка сигналов" layer the operator asked about —
//! one social stream on its own is noise.

use chrono::{DateTime, Duration, Utc};
use mm_sentiment::SentimentTick;
use rust_decimal::Decimal;
use rust_decimal::prelude::Signed;
use rust_decimal_macros::dec;

/// Operator-tuned knobs. Defaults match the MVP rules
/// sketched in the epic plan:
/// `rate > 2 → widen 1.3x`, `rate > 5 → widen 2x + size 0.5x`,
/// `rate > 10 + vol spike → kill`.
#[derive(Debug, Clone)]
pub struct SocialRiskConfig {
    /// Below this mentions_rate the engine stays at neutral
    /// (`vol_multiplier = 1.0`). Default 2.0.
    pub rate_warn: Decimal,
    /// Between `rate_warn` and `rate_alarm` we linearly ramp
    /// the multiplier. Default 5.0.
    pub rate_alarm: Decimal,
    /// At `rate_alarm` the multiplier saturates at
    /// `max_vol_multiplier`. Default 3.0.
    pub max_vol_multiplier: Decimal,
    /// At `rate_alarm` the size multiplier bottoms at
    /// `min_size_multiplier`. Default 0.5.
    pub min_size_multiplier: Decimal,
    /// Above this rate the kill_trigger is armed. Still
    /// requires market cross-validation before firing.
    /// Default 10.0.
    pub kill_mentions_rate: Decimal,
    /// Realised-vol (annualised, e.g. 0.8 = 80%) above which
    /// the kill_trigger fires. Below this, a mentions spike
    /// is treated as chatter not action. Default 0.8.
    pub kill_vol_threshold: Decimal,
    /// Absolute `sentiment_score_5min` above which the skew
    /// path activates. Protects against stale weak signals.
    /// Default 0.3.
    pub skew_threshold: Decimal,
    /// Maximum absolute skew in bps applied to reservation
    /// price. Default 15.0 bps — same order as the momentum
    /// alpha on the existing autotuner.
    pub max_skew_bps: Decimal,
    /// OFI z-score threshold that promotes "crowd chatter"
    /// to "crowd + flow". Below this, the engine widens but
    /// does NOT apply skew. Default 1.5.
    pub ofi_confirm_z: Decimal,
    /// Staleness window. If `SentimentTick.ts` is older than
    /// this, the engine ignores the tick and returns neutral
    /// state. Default 10 minutes.
    pub staleness: Duration,
}

impl Default for SocialRiskConfig {
    fn default() -> Self {
        Self {
            rate_warn: dec!(2),
            rate_alarm: dec!(5),
            max_vol_multiplier: dec!(3),
            min_size_multiplier: dec!(0.5),
            kill_mentions_rate: dec!(10),
            kill_vol_threshold: dec!(0.8),
            skew_threshold: dec!(0.3),
            max_skew_bps: dec!(15),
            ofi_confirm_z: dec!(1.5),
            staleness: Duration::minutes(10),
        }
    }
}

/// Cross-validation inputs the engine passes at each
/// [`SocialRiskEngine::evaluate`] call. Not owned by
/// `SentimentTick` because those come from outside the
/// engine; market state is always-local.
#[derive(Debug, Clone, Copy)]
pub struct MarketContext {
    /// Realised vol, annualised. Same units as
    /// `mm_strategy::VolatilityEstimator::realised_vol`.
    pub realised_vol: Decimal,
    /// Signed OFI z-score. Positive = buy pressure, negative
    /// = sell pressure. Comparable to `|sentiment_score|`
    /// direction.
    pub ofi_z: Decimal,
}

/// Composite output of one evaluation cycle. See module-
/// level docs for semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialRiskState {
    pub vol_multiplier: Decimal,
    pub size_multiplier: Decimal,
    pub inv_skew_bps: Decimal,
    pub kill_trigger: bool,
    /// Human-readable note on which branch produced the
    /// output. Flows into the audit trail so operators can
    /// reconstruct why the engine widened on a given tick.
    pub reason: &'static str,
}

impl SocialRiskState {
    pub fn neutral() -> Self {
        Self {
            vol_multiplier: dec!(1),
            size_multiplier: dec!(1),
            inv_skew_bps: dec!(0),
            kill_trigger: false,
            reason: "neutral",
        }
    }
}

/// State machine. Currently stateless across ticks — the
/// sentiment-side aggregation is done in
/// `mm_sentiment::MentionCounter`, so this struct's only job
/// is to fuse the tick with market context. Kept as a struct
/// (not a free function) so future hysteresis / cooldown
/// state fits here without breaking the engine API.
#[derive(Debug)]
pub struct SocialRiskEngine {
    cfg: SocialRiskConfig,
    /// Timestamp of the last non-neutral state we returned.
    /// Used in a follow-up sprint for cooldown-hysteresis;
    /// populated here so hooking it in is a drop-in change.
    last_active: Option<DateTime<Utc>>,
}

impl SocialRiskEngine {
    pub fn new(cfg: SocialRiskConfig) -> Self {
        Self {
            cfg,
            last_active: None,
        }
    }

    pub fn config(&self) -> &SocialRiskConfig {
        &self.cfg
    }

    /// Evaluate one `(tick, market)` pair. Always returns
    /// `Some` (use `SocialRiskState::neutral()` for the
    /// "nothing to do" branch) so downstream composition is
    /// total.
    pub fn evaluate(
        &mut self,
        tick: &SentimentTick,
        market: MarketContext,
        now: DateTime<Utc>,
    ) -> SocialRiskState {
        // Staleness — a tick that's too old is no signal.
        if now - tick.ts > self.cfg.staleness {
            return SocialRiskState::neutral();
        }

        let rate = tick.mentions_rate;
        let sentiment = tick.sentiment_score_5min;

        // Kill trigger — rate AND vol both above threshold.
        // A spiking rate on a quiet market is chatter, not a
        // dump. Require confirmation from realised vol before
        // escalating from "widen" to "flatten".
        if rate >= self.cfg.kill_mentions_rate
            && market.realised_vol >= self.cfg.kill_vol_threshold
        {
            self.last_active = Some(now);
            return SocialRiskState {
                vol_multiplier: self.cfg.max_vol_multiplier,
                size_multiplier: self.cfg.min_size_multiplier,
                inv_skew_bps: dec!(0),
                kill_trigger: true,
                reason: "kill: rate+vol",
            };
        }

        // Neutral zone — below warn threshold, nothing fires.
        if rate <= self.cfg.rate_warn {
            return SocialRiskState::neutral();
        }

        // Ramp zone — between `rate_warn` and `rate_alarm`,
        // linearly interpolate multiplier + size cut. Above
        // `rate_alarm` we saturate (same tail as the
        // lead-lag guard).
        let span = self.cfg.rate_alarm - self.cfg.rate_warn;
        let ramp = if rate >= self.cfg.rate_alarm {
            dec!(1)
        } else {
            (rate - self.cfg.rate_warn) / span
        };
        let vol_mult = dec!(1) + (self.cfg.max_vol_multiplier - dec!(1)) * ramp;
        let size_mult = dec!(1) - (dec!(1) - self.cfg.min_size_multiplier) * ramp;

        // Skew — active only when BOTH
        //   (a) sentiment is confidently one-sided, AND
        //   (b) OFI agrees with the sentiment direction.
        // On (a) alone we widen without skewing (chatter).
        // On (a)+(b) we tilt the reservation towards the
        // crowd — NOT to chase momentum, but so our next
        // quote doesn't sit inverted to the one-sided flow
        // and eat adverse selection.
        let skew_bps = if sentiment.abs() >= self.cfg.skew_threshold
            && market.ofi_z.abs() >= self.cfg.ofi_confirm_z
            && sentiment.signum() == market.ofi_z.signum()
        {
            self.cfg.max_skew_bps * sentiment.signum() * ramp
        } else {
            dec!(0)
        };

        let reason = if skew_bps != dec!(0) {
            "widen + skew (rate+OFI)"
        } else {
            "widen (rate only)"
        };

        self.last_active = Some(now);
        SocialRiskState {
            vol_multiplier: vol_mult,
            size_multiplier: size_mult,
            inv_skew_bps: skew_bps,
            kill_trigger: false,
            reason,
        }
    }

    /// Timestamp of the last non-neutral evaluation. `None`
    /// until the first firing.
    pub fn last_active(&self) -> Option<DateTime<Utc>> {
        self.last_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_tick() -> SentimentTick {
        SentimentTick {
            asset: "BTC".into(),
            ts: Utc::now(),
            mentions_5min: 10,
            mentions_1h: 60,
            mentions_rate: dec!(1),
            mentions_acceleration: dec!(0),
            sentiment_score_5min: dec!(0),
            sentiment_score_prev: dec!(0),
            sentiment_delta: dec!(0),
        }
    }

    fn neutral_market() -> MarketContext {
        MarketContext {
            realised_vol: dec!(0.4),
            ofi_z: dec!(0),
        }
    }

    #[test]
    fn flat_rate_is_neutral() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let state = e.evaluate(&base_tick(), neutral_market(), Utc::now());
        assert_eq!(state, SocialRiskState::neutral());
    }

    #[test]
    fn mid_rate_ramps_multiplier_without_killing() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(3); // half-way between warn (2) and alarm (5)
        let state = e.evaluate(&tick, neutral_market(), Utc::now());
        assert!(!state.kill_trigger);
        assert!(state.vol_multiplier > dec!(1));
        assert!(state.vol_multiplier < dec!(3));
        assert!(state.size_multiplier < dec!(1));
        assert!(state.size_multiplier > dec!(0.5));
    }

    #[test]
    fn saturates_at_alarm_and_stays_there() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(5);
        let a = e.evaluate(&tick, neutral_market(), Utc::now());
        tick.mentions_rate = dec!(7);
        let b = e.evaluate(&tick, neutral_market(), Utc::now());
        assert_eq!(a.vol_multiplier, dec!(3));
        assert_eq!(b.vol_multiplier, dec!(3));
        assert_eq!(a.size_multiplier, dec!(0.5));
        assert!(!a.kill_trigger);
        assert!(!b.kill_trigger);
    }

    #[test]
    fn high_rate_alone_does_not_kill() {
        // No vol confirmation — mentions spike on a quiet
        // market is chatter. Widens, does not flatten.
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(15); // well past kill threshold
        let state = e.evaluate(&tick, neutral_market(), Utc::now());
        assert!(!state.kill_trigger);
        assert_eq!(state.vol_multiplier, dec!(3));
    }

    #[test]
    fn rate_plus_vol_triggers_kill() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(12);
        let market = MarketContext {
            realised_vol: dec!(1.2),
            ofi_z: dec!(0),
        };
        let state = e.evaluate(&tick, market, Utc::now());
        assert!(state.kill_trigger);
        assert_eq!(state.vol_multiplier, dec!(3));
        assert_eq!(state.inv_skew_bps, dec!(0)); // kill doesn't skew
    }

    #[test]
    fn skew_only_when_sentiment_and_ofi_agree() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(4);
        tick.sentiment_score_5min = dec!(0.8);
        // Case A — neutral OFI: widen, no skew.
        let a = e.evaluate(&tick, neutral_market(), Utc::now());
        assert_eq!(a.inv_skew_bps, dec!(0));
        assert!(a.vol_multiplier > dec!(1));

        // Case B — OFI confirms bullish direction: skew
        // positive (tilt reservation up so our bid sits
        // closer to the crowd, ask steps away).
        let bullish_ofi = MarketContext {
            realised_vol: dec!(0.3),
            ofi_z: dec!(2),
        };
        let b = e.evaluate(&tick, bullish_ofi, Utc::now());
        assert!(b.inv_skew_bps > dec!(0));
    }

    #[test]
    fn opposing_sentiment_and_ofi_produce_no_skew() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(4);
        tick.sentiment_score_5min = dec!(0.8); // bullish
        let bearish_ofi = MarketContext {
            realised_vol: dec!(0.3),
            ofi_z: dec!(-2),
        };
        let state = e.evaluate(&tick, bearish_ofi, Utc::now());
        // Social says up, flow says down — classic noisy
        // headline with a real-money dump underneath. Widen,
        // but do NOT tilt.
        assert_eq!(state.inv_skew_bps, dec!(0));
    }

    #[test]
    fn stale_tick_returns_neutral() {
        let mut e = SocialRiskEngine::new(SocialRiskConfig::default());
        let mut tick = base_tick();
        tick.mentions_rate = dec!(10);
        tick.ts = Utc::now() - Duration::minutes(30);
        let state = e.evaluate(&tick, neutral_market(), Utc::now());
        assert_eq!(state, SocialRiskState::neutral());
    }
}
