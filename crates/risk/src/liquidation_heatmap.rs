//! ⚠ R4.2 — forced-liquidation heatmap.
//!
//! **This module powers both defensive and offensive code paths.**
//! On the defensive side, aggregated OI-per-price-level is an
//! input the honest MM uses to widen quotes near a known
//! cascade zone. On the offensive / pentest side, the heatmap
//! is what `Strategy.LiquidationHunt` consumes to target a
//! cascade cluster. The data itself is neutral — the venues
//! publish it — but the downstream consumer is NOT, so the
//! restricted gate on the hunt strategy stays enforced
//! regardless of what the heatmap sees.
//!
//! # What we actually track
//!
//! A rolling window of observed liquidations — NOT a direct
//! read of open interest, which no venue exposes in L3 detail.
//! We bucket by bps-from-mid (20 bps granularity by default),
//! sum notional per bucket, and decay on a half-life. The
//! resulting "observed-cascade" view is a shadow of real OI —
//! good enough to spot large levels nearby without needing
//! proprietary CoinGlass access.
//!
//! # Fail-open semantics
//!
//! Consumers should treat a cold tracker (no liquidations yet
//! observed) as `Value::Missing` — neutral. Never gate a kill-
//! switch on an empty heatmap, or a quiet symbol would look
//! suspicious by accident.

use chrono::{DateTime, Utc};
use mm_common::types::Side;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::BTreeMap;

/// A single observed forced-liquidation event. Mirrors the
/// shape of `mm_exchange_core::events::MarketEvent::Liquidation`
/// but carries no venue enum — the tracker is per-symbol so
/// the venue is implicit.
#[derive(Debug, Clone)]
pub struct LiquidationEvent {
    /// `Sell` = a long was liquidated (market taker sell);
    /// `Buy` = a short was liquidated (market taker buy).
    pub side: Side,
    pub price: Decimal,
    pub qty: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Configuration for [`LiquidationHeatmap`].
#[derive(Debug, Clone)]
pub struct HeatmapConfig {
    /// Window (seconds) to keep a rolling set of events.
    /// Default 1800 = 30 minutes.
    pub window_secs: i64,
    /// Bucket width in bps from mid. Default 20 bps — rough
    /// granularity balancing memory against resolution.
    pub bucket_bps: u32,
    /// Max distance from mid (bps) the heatmap tracks. Default
    /// 500 bps (5%). Events further out are ignored.
    pub max_bps_from_mid: u32,
}

impl Default for HeatmapConfig {
    fn default() -> Self {
        Self {
            window_secs: 1800,
            bucket_bps: 20,
            max_bps_from_mid: 500,
        }
    }
}

/// One bucket's summary — used by `Strategy.LiquidationHunt`
/// to find the nearest high-OI cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct HeatmapBucket {
    /// Signed bps from the current mid. Positive = above mid
    /// (short liquidations cluster here), negative = below
    /// (long liquidations).
    pub signed_bps_from_mid: i32,
    pub notional: Decimal,
    pub event_count: u32,
}

#[derive(Debug)]
pub struct LiquidationHeatmap {
    config: HeatmapConfig,
    events: Vec<LiquidationEvent>,
    last_mid: Decimal,
}

impl LiquidationHeatmap {
    pub fn new() -> Self {
        Self::with_config(HeatmapConfig::default())
    }

    pub fn with_config(config: HeatmapConfig) -> Self {
        Self {
            config,
            events: Vec::new(),
            last_mid: Decimal::ZERO,
        }
    }

    /// Record one forced-liquidation event.
    pub fn on_liquidation(&mut self, ev: LiquidationEvent) {
        self.events.push(ev);
        self.evict();
    }

    /// Update the reference mid. The bucketing uses whatever
    /// mid was last provided; callers feed the book's `mid_price`
    /// on every tick.
    pub fn on_mid(&mut self, mid: Decimal) {
        if mid > Decimal::ZERO {
            self.last_mid = mid;
        }
    }

    fn evict(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::seconds(self.config.window_secs);
        self.events.retain(|e| e.timestamp >= cutoff);
    }

    /// Total notional summed across all in-window events.
    /// Proxy for "how hot the book has been".
    pub fn total_notional(&self) -> Decimal {
        self.events.iter().map(|e| e.price * e.qty).sum()
    }

    /// All buckets sorted by `signed_bps_from_mid` ASC. Empty
    /// when no events recorded yet or mid hasn't been fed.
    pub fn buckets(&self) -> Vec<HeatmapBucket> {
        if self.last_mid.is_zero() {
            return Vec::new();
        }
        let mut by_bucket: BTreeMap<i32, (Decimal, u32)> = BTreeMap::new();
        let bucket_width = self.config.bucket_bps as i32;
        let max_bps = self.config.max_bps_from_mid as i32;
        for e in &self.events {
            let diff = e.price - self.last_mid;
            if diff.is_zero() {
                continue;
            }
            let ratio = diff / self.last_mid * dec!(10_000);
            let Some(bps_f) = ratio.to_f64() else {
                continue;
            };
            let bps = bps_f as i32;
            if bps.abs() > max_bps {
                continue;
            }
            let bucket = (bps / bucket_width) * bucket_width;
            let entry = by_bucket.entry(bucket).or_insert((Decimal::ZERO, 0));
            entry.0 += e.price * e.qty;
            entry.1 += 1;
        }
        by_bucket
            .into_iter()
            .map(|(bps, (notional, count))| HeatmapBucket {
                signed_bps_from_mid: bps,
                notional,
                event_count: count,
            })
            .collect()
    }

    /// Find the largest cluster within `[min_bps, max_bps]`
    /// from mid on a specified side. `side == Buy` looks for
    /// short-liquidation clusters ABOVE mid; `Sell` looks for
    /// long-liquidation clusters BELOW mid.
    pub fn nearest_cluster_above_threshold(
        &self,
        side: Side,
        threshold_notional: Decimal,
        min_bps: i32,
        max_bps: i32,
    ) -> Option<HeatmapBucket> {
        self.buckets()
            .into_iter()
            .filter(|b| {
                let in_band = b.signed_bps_from_mid.abs() >= min_bps
                    && b.signed_bps_from_mid.abs() <= max_bps;
                let right_side = match side {
                    Side::Buy => b.signed_bps_from_mid > 0,
                    Side::Sell => b.signed_bps_from_mid < 0,
                };
                in_band && right_side && b.notional >= threshold_notional
            })
            .min_by_key(|b| b.signed_bps_from_mid.abs())
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn last_mid(&self) -> Decimal {
        self.last_mid
    }
}

impl Default for LiquidationHeatmap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(side: Side, price: Decimal, qty: Decimal) -> LiquidationEvent {
        LiquidationEvent {
            side,
            price,
            qty,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn buckets_group_by_bps_from_mid() {
        let mut h = LiquidationHeatmap::new();
        h.on_mid(dec!(100));
        // 10.2 → +20 bps bucket (sell side → long liquidation)
        h.on_liquidation(ev(Side::Sell, dec!(100.20), dec!(5)));
        h.on_liquidation(ev(Side::Sell, dec!(100.22), dec!(5)));
        // 9.95 → -50 bps bucket (buy side → short liquidation)
        h.on_liquidation(ev(Side::Buy, dec!(99.50), dec!(10)));
        let b = h.buckets();
        assert!(!b.is_empty(), "expected some buckets");
        // +20 bucket has 2 events, -500 bucket has 1
        let plus_20 = b.iter().find(|x| x.signed_bps_from_mid == 20).unwrap();
        assert_eq!(plus_20.event_count, 2);
    }

    #[test]
    fn nearest_cluster_filters_side_and_threshold() {
        let mut h = LiquidationHeatmap::new();
        h.on_mid(dec!(100));
        // Put a big long-liq cluster below mid at -50 bps.
        for _ in 0..5 {
            h.on_liquidation(ev(Side::Sell, dec!(99.50), dec!(100)));
        }
        // Small cluster above mid at +20 bps.
        h.on_liquidation(ev(Side::Buy, dec!(100.20), dec!(1)));

        // Looking below mid (Sell side, pushing price down).
        let c = h
            .nearest_cluster_above_threshold(Side::Sell, dec!(1000), 10, 200)
            .expect("should find a below-mid cluster");
        assert!(c.signed_bps_from_mid < 0);
        assert!(c.notional >= dec!(1000));

        // Looking above mid with a threshold that excludes the
        // small +20 cluster → None.
        let none = h.nearest_cluster_above_threshold(Side::Buy, dec!(10_000), 10, 200);
        assert!(none.is_none());
    }

    #[test]
    fn empty_tracker_returns_no_buckets() {
        let h = LiquidationHeatmap::new();
        assert!(h.buckets().is_empty());
    }
}
