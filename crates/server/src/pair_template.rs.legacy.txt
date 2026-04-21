//! Pair-class template loader (Epic 30).
#![allow(dead_code)] // consumed by E30.4 AdaptiveTuner wiring — see roadmap
//!
//! A template is a partial `AppConfig` — only `[market_maker]`,
//! `[risk]`, and `[toxicity]` subsets, each field `Option<_>`.
//! At engine boot the operator may call
//! `apply_pair_class_template(&mut app_config, class, search_dir)`
//! to fold class-appropriate defaults into the live config BEFORE
//! the user's per-venue config is loaded on top of it — which
//! preserves precedence: `defaults < class-template < user config
//! < env vars`.
//!
//! Templates live under `config/pair-classes/<slug>.toml` — one
//! file per `PairClass` variant. Missing files are a hard error;
//! a typo in the slug should not silently fall back to defaults.

use anyhow::{Context, Result};
use mm_common::config::AppConfig;
use mm_common::pair_class::PairClass;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;

/// Partial config shape, mirrored against `AppConfig` but every
/// field optional so templates can override exactly what the
/// class rationale calls for.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PairTemplate {
    #[serde(default)]
    pub market_maker: MmTemplate,
    #[serde(default)]
    pub risk: RiskTemplate,
    #[serde(default)]
    pub toxicity: ToxicityTemplate,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MmTemplate {
    pub gamma: Option<Decimal>,
    pub kappa: Option<Decimal>,
    pub sigma: Option<Decimal>,
    pub time_horizon_secs: Option<u64>,
    pub num_levels: Option<usize>,
    pub refresh_interval_ms: Option<u64>,
    pub min_spread_bps: Option<Decimal>,
    pub max_distance_bps: Option<Decimal>,
    pub momentum_enabled: Option<bool>,
    pub momentum_window: Option<usize>,
    pub market_resilience_enabled: Option<bool>,
    pub hma_enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RiskTemplate {
    pub max_spread_bps: Option<Decimal>,
    pub stale_book_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToxicityTemplate {
    pub vpin_threshold: Option<Decimal>,
    pub kyle_window: Option<usize>,
}

/// Merge a pair-class template into the mutable `AppConfig`. Only
/// fields explicitly set in the TOML are touched — anything absent
/// keeps the caller-provided value. Returns the number of fields
/// the template actually overrode so the caller can log it.
///
/// `search_dir` is usually `"config/pair-classes"` but can be a
/// test fixture in unit tests.
pub fn apply_pair_class_template(
    cfg: &mut AppConfig,
    class: PairClass,
    search_dir: &Path,
) -> Result<usize> {
    let path = search_dir.join(format!("{}.toml", class.template_slug()));
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("pair-class template not found: {}", path.display()))?;
    let tpl: PairTemplate =
        toml::from_str(&body).with_context(|| format!("template parse failed: {}", path.display()))?;
    Ok(merge_template(cfg, &tpl))
}

fn merge_template(cfg: &mut AppConfig, tpl: &PairTemplate) -> usize {
    let mut applied = 0usize;
    let m = &tpl.market_maker;
    if let Some(v) = m.gamma {
        cfg.market_maker.gamma = v;
        applied += 1;
    }
    if let Some(v) = m.kappa {
        cfg.market_maker.kappa = v;
        applied += 1;
    }
    if let Some(v) = m.sigma {
        cfg.market_maker.sigma = v;
        applied += 1;
    }
    if let Some(v) = m.time_horizon_secs {
        cfg.market_maker.time_horizon_secs = v;
        applied += 1;
    }
    if let Some(v) = m.num_levels {
        cfg.market_maker.num_levels = v;
        applied += 1;
    }
    if let Some(v) = m.refresh_interval_ms {
        cfg.market_maker.refresh_interval_ms = v;
        applied += 1;
    }
    if let Some(v) = m.min_spread_bps {
        cfg.market_maker.min_spread_bps = v;
        applied += 1;
    }
    if let Some(v) = m.max_distance_bps {
        cfg.market_maker.max_distance_bps = v;
        applied += 1;
    }
    if let Some(v) = m.momentum_enabled {
        cfg.market_maker.momentum_enabled = v;
        applied += 1;
    }
    if let Some(v) = m.momentum_window {
        cfg.market_maker.momentum_window = v;
        applied += 1;
    }
    if let Some(v) = m.market_resilience_enabled {
        cfg.market_maker.market_resilience_enabled = v;
        applied += 1;
    }
    if let Some(v) = m.hma_enabled {
        cfg.market_maker.hma_enabled = v;
        applied += 1;
    }

    let r = &tpl.risk;
    if let Some(v) = r.max_spread_bps {
        cfg.risk.max_spread_bps = v;
        applied += 1;
    }
    if let Some(v) = r.stale_book_timeout_secs {
        cfg.risk.stale_book_timeout_secs = v;
        applied += 1;
    }

    let t = &tpl.toxicity;
    if let Some(v) = t.vpin_threshold {
        cfg.toxicity.vpin_threshold = v;
        applied += 1;
    }
    if let Some(v) = t.kyle_window {
        cfg.toxicity.kyle_window = v;
        applied += 1;
    }

    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn base_cfg() -> AppConfig {
        AppConfig::default()
    }

    #[test]
    fn every_shipped_template_loads_and_applies() {
        let dir = std::path::Path::new("../../config/pair-classes");
        for class in [
            PairClass::MajorSpot,
            PairClass::MajorPerp,
            PairClass::AltSpot,
            PairClass::AltPerp,
            PairClass::MemeSpot,
            PairClass::StableStable,
        ] {
            let mut cfg = base_cfg();
            let count = apply_pair_class_template(&mut cfg, class, dir)
                .unwrap_or_else(|e| panic!("{class} failed: {e}"));
            assert!(
                count > 0,
                "{class} template applied 0 fields — either file is empty or merge_template missed a branch"
            );
        }
    }

    #[test]
    fn missing_template_is_a_hard_error() {
        let mut cfg = base_cfg();
        let bad_dir = std::path::Path::new("/definitely/not/a/path");
        let err =
            apply_pair_class_template(&mut cfg, PairClass::MajorSpot, bad_dir).unwrap_err();
        assert!(err.to_string().contains("template not found"));
    }

    #[test]
    fn merge_overrides_only_present_fields() {
        let mut cfg = base_cfg();
        let original_order_size = cfg.market_maker.order_size;
        let original_num_levels = cfg.market_maker.num_levels;
        let tpl = PairTemplate {
            market_maker: MmTemplate {
                gamma: Some(dec!(0.42)),
                ..Default::default()
            },
            ..Default::default()
        };
        let applied = merge_template(&mut cfg, &tpl);
        assert_eq!(applied, 1);
        assert_eq!(cfg.market_maker.gamma, dec!(0.42));
        // Absent template fields don't disturb the user's config.
        assert_eq!(cfg.market_maker.order_size, original_order_size);
        assert_eq!(cfg.market_maker.num_levels, original_num_levels);
    }

    #[test]
    fn major_spot_sets_tight_spread() {
        let dir = std::path::Path::new("../../config/pair-classes");
        let mut cfg = base_cfg();
        apply_pair_class_template(&mut cfg, PairClass::MajorSpot, dir).unwrap();
        assert!(
            cfg.market_maker.min_spread_bps <= dec!(5),
            "MajorSpot should have tight spread, got {}",
            cfg.market_maker.min_spread_bps
        );
    }

    #[test]
    fn meme_spot_sets_wide_spread_and_lower_levels() {
        let dir = std::path::Path::new("../../config/pair-classes");
        let mut cfg = base_cfg();
        apply_pair_class_template(&mut cfg, PairClass::MemeSpot, dir).unwrap();
        assert!(
            cfg.market_maker.min_spread_bps >= dec!(10),
            "MemeSpot should widen min_spread_bps, got {}",
            cfg.market_maker.min_spread_bps
        );
        assert!(
            cfg.market_maker.num_levels <= 2,
            "MemeSpot should trim num_levels on thin books"
        );
    }

    #[test]
    fn stable_stable_flattens_momentum() {
        let dir = std::path::Path::new("../../config/pair-classes");
        let mut cfg = base_cfg();
        apply_pair_class_template(&mut cfg, PairClass::StableStable, dir).unwrap();
        assert!(!cfg.market_maker.momentum_enabled);
        assert!(!cfg.market_maker.hma_enabled);
    }
}
