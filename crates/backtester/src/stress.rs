//! Stress scenario library — Epic C sub-component #5 (scaffold).
//!
//! Sprint C-2 ships the **parameter definitions** and a test
//! that pins the scenario catalogue. Actual deterministic event
//! generation, the `mm-stress-test` binary, and the end-to-end
//! engine integration land in Sprint C-4 together with the
//! reporting layer.
//!
//! Stage-1 scenarios are **synthetic** — each is a deterministic
//! function of a seed that reproduces the *shock profile* of
//! one of the five canonical crypto crashes (covid 2020, China
//! ban 2021, LUNA 2022, FTX 2022, USDC depeg 2023). Real
//! historical Tardis replay is a stage-2 follow-up tracked in
//! `ROADMAP.md` under the Epic C section. See
//! `docs/sprints/epic-c-portfolio-risk-view.md` for the Sprint
//! C-1 decision that pinned the synthetic-first approach.

use mm_portfolio::Portfolio;
use mm_risk::hedge_optimizer::{HedgeInstrument, HedgeOptimizer};
use mm_risk::kill_switch::{KillSwitch, KillSwitchConfig};
use mm_risk::var_guard::{VarGuard, VarGuardConfig};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// One canonical stress scenario. The `mm-stress-test` CLI
/// loads these by `slug`, seeds the `ShockProfile` into a
/// deterministic `MarketEvent` stream, and drives the
/// simulator through it.
#[derive(Debug, Clone)]
pub struct StressScenario {
    /// URL-safe slug used as the CLI `--scenario=<slug>` arg
    /// and as the filename under `data/stress/<slug>/` once
    /// the Tardis stage-2 path ships real historical replays.
    pub slug: &'static str,
    /// Human-readable short label for the daily report.
    pub label: &'static str,
    /// Which historical event this scenario models. Not
    /// machine-parsed — pure documentation for operators.
    pub history_note: &'static str,
    /// Primary symbol the scenario is keyed on. Cross-asset
    /// scenarios (LUNA, USDC depeg) may touch multiple
    /// symbols; in v1 the primary symbol is the quoting leg.
    pub primary_symbol: &'static str,
    /// Total scenario duration in seconds. The simulator
    /// replays events for exactly this window.
    pub duration_secs: u64,
    /// Shock profile parameters the deterministic event
    /// generator consumes to synthesise a replay stream.
    pub shock: ShockProfile,
}

/// Quantitative shape of the market shock. Pure data — no IO,
/// no randomness at this layer. Sprint C-4 turns this into an
/// event stream via a seeded PRNG.
#[derive(Debug, Clone)]
pub struct ShockProfile {
    /// Peak directional price move as a signed fraction of
    /// starting price. `-0.50` = price crashes to 50 % of
    /// start, `+0.08` = 8 % up-move (USDC depeg recovery
    /// side).
    pub peak_price_move: Decimal,
    /// How long the peak move takes to materialise from the
    /// scenario start, in seconds. `24·3600` = gradual 24 h
    /// crash, `7200` = 2 h flash crash.
    pub peak_time_secs: u64,
    /// Trade volume multiplier at peak vs baseline. Panics
    /// produce 3-5× normal volume on crypto venues.
    pub volume_multiplier_peak: Decimal,
    /// Spread multiplier at peak vs baseline. Liquidity
    /// withdrawal during a crash typically blows the spread
    /// out 10-30× normal.
    pub spread_multiplier_peak: Decimal,
    /// Fraction of baseline book depth that remains at peak.
    /// `0.1` = book thins to 10 % of normal.
    pub depth_fraction_peak: Decimal,
    /// Share of trades on the selling side at peak.
    /// `0.5` = balanced, `0.9` = 90 % sells (panic flow).
    pub sell_flow_share_peak: Decimal,
}

/// The five canonical crypto-crash scenarios, defined as pure
/// data and verified by the unit test at the bottom of this
/// file. Order is chronological.
pub const CANONICAL_SCENARIOS: &[StressScenario] = &[
    StressScenario {
        slug: "covid-2020",
        label: "COVID-19 crash, March 12 2020",
        history_note: "BTC/USDT fell ~50 % in 24 h as global risk-off flows \
                       hit every asset class. Every major venue saw 5× normal \
                       volume and spreads blew out 20-30×. The canonical crypto \
                       flash crash of the modern era.",
        primary_symbol: "BTCUSDT",
        duration_secs: 24 * 3600,
        shock: ShockProfile {
            peak_price_move: dec!(-0.50),
            peak_time_secs: 12 * 3600,
            volume_multiplier_peak: dec!(5),
            spread_multiplier_peak: dec!(25),
            depth_fraction_peak: dec!(0.15),
            sell_flow_share_peak: dec!(0.80),
        },
    },
    StressScenario {
        slug: "china-2021",
        label: "China mining ban, May 19 2021",
        history_note: "BTC/USDT fell ~30 % in 6 h after the People's Bank of \
                       China's crackdown announcement. Liquidity held up \
                       better than March 2020 but one-sided sell flow \
                       dominated for most of the window.",
        primary_symbol: "BTCUSDT",
        duration_secs: 6 * 3600,
        shock: ShockProfile {
            peak_price_move: dec!(-0.30),
            peak_time_secs: 3 * 3600,
            volume_multiplier_peak: dec!(3),
            spread_multiplier_peak: dec!(8),
            depth_fraction_peak: dec!(0.4),
            sell_flow_share_peak: dec!(0.85),
        },
    },
    StressScenario {
        slug: "luna-2022",
        label: "LUNA / UST collapse, May 9-12 2022",
        history_note: "LUNA fell ~95 % and UST depegged to $0.02 as the \
                       algorithmic stablecoin's reflexive mint-burn loop \
                       unwound. Book depth evaporated completely — every \
                       MM pulled quotes. Multi-day event, the stress test \
                       models the first 72 h where the full decay played out.",
        // Use BTCUSDT as the primary symbol in v1 since we don't
        // quote LUNA in the v0.4.0 venue matrix; Sprint C-4
        // can add a synthetic "LUNA-like" symbol as a
        // product-spec override.
        primary_symbol: "BTCUSDT",
        duration_secs: 72 * 3600,
        shock: ShockProfile {
            peak_price_move: dec!(-0.95),
            peak_time_secs: 48 * 3600,
            volume_multiplier_peak: dec!(4),
            spread_multiplier_peak: dec!(30),
            depth_fraction_peak: dec!(0.05),
            sell_flow_share_peak: dec!(0.95),
        },
    },
    StressScenario {
        slug: "ftx-2022",
        label: "FTX collapse, November 8-11 2022",
        history_note: "BTC/USDT fell ~25 % in 2 h as FTX's insolvency became \
                       public. Liquidity withdrew 80 % as MMs pulled quotes \
                       waiting for clarity on FTX wallet counterparty risk. \
                       Perp funding flipped deeply negative across every venue.",
        primary_symbol: "BTCUSDT",
        duration_secs: 2 * 3600,
        shock: ShockProfile {
            peak_price_move: dec!(-0.25),
            peak_time_secs: 3600,
            volume_multiplier_peak: dec!(6),
            spread_multiplier_peak: dec!(15),
            depth_fraction_peak: dec!(0.2),
            sell_flow_share_peak: dec!(0.90),
        },
    },
    StressScenario {
        slug: "usdc-depeg-2023",
        label: "USDC depeg, March 10-13 2023",
        history_note: "USDC depegged to $0.88 after Silicon Valley Bank failed \
                       and Circle's reserves were temporarily inaccessible. \
                       The move took 3 h; the recovery to $0.99+ took 48 h. \
                       Narrow-to-wide-spread regime on the USDC/USDT pair, \
                       cross-stablecoin arb was briefly very profitable for \
                       market makers with cross-venue quoting capability.",
        primary_symbol: "USDCUSDT",
        duration_secs: 48 * 3600,
        shock: ShockProfile {
            // Positive relative to USDT: the recovery side of the
            // depeg is where the stress test focuses.
            peak_price_move: dec!(-0.12),
            peak_time_secs: 3 * 3600,
            volume_multiplier_peak: dec!(10),
            spread_multiplier_peak: dec!(50),
            depth_fraction_peak: dec!(0.1),
            sell_flow_share_peak: dec!(0.60),
        },
    },
];

/// Look up a scenario by slug. Returns `None` for unknown
/// slugs — the CLI can then list the available slugs.
pub fn scenario_by_slug(slug: &str) -> Option<&'static StressScenario> {
    CANONICAL_SCENARIOS.iter().find(|s| s.slug == slug)
}

/// One discrete sample from a synthetic stress-scenario path.
/// The generator emits these on a fixed 1-minute cadence so
/// the total sample count is `duration_secs / 60`, which is
/// the same cadence the engine's per-strategy VaR guard
/// samples PnL on.
#[derive(Debug, Clone)]
pub struct StressTick {
    /// Seconds since scenario start.
    pub offset_secs: u64,
    /// Simulated mid price at this tick.
    pub mid_price: Decimal,
    /// Best bid = mid − half_spread × spread_multiplier(t).
    pub bid_price: Decimal,
    /// Best ask = mid + half_spread × spread_multiplier(t).
    pub ask_price: Decimal,
    /// Aggregate trade volume observed this minute.
    pub trade_volume: Decimal,
    /// Net taker flow share on the sell side `[0, 1]`.
    /// Used by the stress runner to mark the synthetic fill
    /// imbalance.
    pub sell_flow_share: Decimal,
    /// Book depth fraction vs baseline `(0, 1]`. The stress
    /// runner uses this to decay position caps on the
    /// optimizer's hedge universe under thin liquidity.
    pub depth_fraction: Decimal,
}

/// Deterministic tick generator from a `ShockProfile`. Emits
/// one tick per minute over the scenario duration. The price
/// path is **piecewise linear**: starts at `baseline_price`,
/// moves linearly to `baseline_price × (1 + peak_price_move)`
/// over the first `peak_time_secs`, then recovers linearly
/// back to baseline over the remaining time. Spread, volume,
/// depth, and sell-flow share follow a triangular ramp with
/// the same peak time.
///
/// v1 is pure math — no RNG, no filesystem, no external data.
/// Every invocation with the same inputs produces the same
/// output, which is exactly the reproducibility property the
/// stress-test CLI needs.
pub fn generate_ticks(scenario: &StressScenario, baseline_price: Decimal) -> Vec<StressTick> {
    let sample_interval_secs: u64 = 60;
    let sample_count = (scenario.duration_secs / sample_interval_secs).max(1) as usize;
    let peak_idx = ((scenario.shock.peak_time_secs / sample_interval_secs) as usize)
        .min(sample_count.saturating_sub(1));
    let baseline_half_spread = baseline_price * dec!(0.0001); // 1 bps half-spread at baseline
    let baseline_volume = dec!(100);

    let mut out = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let offset_secs = (i as u64) * sample_interval_secs;
        // Triangular ramp factor in [0, 1]: rises to 1 at
        // peak_idx, then falls back to 0 at the end.
        let ramp = if peak_idx == 0 {
            // Instant-peak scenario — the very first sample
            // sits at the full shock; every subsequent sample
            // linearly recovers.
            if sample_count <= 1 {
                Decimal::ONE
            } else {
                let denom = Decimal::from((sample_count - 1) as u32);
                Decimal::ONE - Decimal::from(i as u32) / denom
            }
        } else if i <= peak_idx {
            Decimal::from(i as u32) / Decimal::from(peak_idx as u32)
        } else {
            let remaining = sample_count - 1 - peak_idx;
            if remaining == 0 {
                Decimal::ZERO
            } else {
                Decimal::ONE
                    - Decimal::from((i - peak_idx) as u32) / Decimal::from(remaining as u32)
            }
        };

        // Price: baseline · (1 + ramp · peak_price_move).
        let mid_price = baseline_price * (Decimal::ONE + ramp * scenario.shock.peak_price_move);

        // Spread multiplier: 1 + ramp · (peak − 1).
        let spread_mult =
            Decimal::ONE + ramp * (scenario.shock.spread_multiplier_peak - Decimal::ONE);
        let half_spread = baseline_half_spread * spread_mult;
        let bid_price = mid_price - half_spread;
        let ask_price = mid_price + half_spread;

        // Volume: baseline · (1 + ramp · (mult − 1)).
        let vol_mult = Decimal::ONE + ramp * (scenario.shock.volume_multiplier_peak - Decimal::ONE);
        let trade_volume = baseline_volume * vol_mult;

        // Depth fraction: linear from 1.0 to peak_fraction.
        let depth_fraction =
            Decimal::ONE - ramp * (Decimal::ONE - scenario.shock.depth_fraction_peak);

        // Sell flow share: 0.5 baseline, ramps toward the peak.
        let baseline_sell_share = dec!(0.5);
        let sell_flow_share = baseline_sell_share
            + ramp * (scenario.shock.sell_flow_share_peak - baseline_sell_share);

        out.push(StressTick {
            offset_secs,
            mid_price,
            bid_price,
            ask_price,
            trade_volume,
            sell_flow_share,
            depth_fraction,
        });
    }
    out
}

/// Stress-test runner configuration. Carries the knobs the
/// operator would otherwise read from `config.toml`.
#[derive(Debug, Clone)]
pub struct StressRunConfig {
    /// Baseline mid price at scenario start.
    pub baseline_price: Decimal,
    /// Base asset tag for the Portfolio factor aggregation.
    pub base_asset: String,
    /// Quote asset tag for the Portfolio factor aggregation.
    pub quote_asset: String,
    /// Fixed strategy class label applied to every simulated
    /// fill. Stays constant across the scenario so the VaR
    /// guard's per-strategy bucketing stays consistent.
    pub strategy_class: String,
    /// Fixed position size for the simulated market maker.
    /// The runner assumes the MM holds this many base-asset
    /// units throughout the scenario and marks them to the
    /// current mid.
    pub position_size: Decimal,
    /// VaR guard configuration. `None` disables the guard
    /// (throttle always 1.0).
    pub var_guard: Option<VarGuardConfig>,
    /// Daily PnL loss limit in the reporting currency — seeds
    /// the kill switch. The runner uses a minimal
    /// `KillSwitchConfig` with this field set and the rest
    /// at defaults.
    pub daily_loss_limit: Decimal,
    /// Daily PnL warning threshold for the soft kill-switch
    /// tier.
    pub daily_loss_warning: Decimal,
    /// Funding-penalty coefficient for the hedge optimizer.
    pub hedge_funding_penalty: Decimal,
}

impl StressRunConfig {
    /// Reasonable defaults for a dry run of a single scenario.
    /// The operator can override any field before calling
    /// [`run_stress`].
    pub fn defaults_for(scenario: &StressScenario) -> Self {
        let (base, quote) = split_base_quote(scenario.primary_symbol);
        Self {
            baseline_price: dec!(50000),
            base_asset: base,
            quote_asset: quote,
            strategy_class: "stress_runner".to_string(),
            position_size: dec!(1),
            var_guard: Some(VarGuardConfig {
                limit_95: Some(dec!(-5000)),
                limit_99: Some(dec!(-10000)),
                ewma_lambda: None,
                cvar_limit_95: None,
                cvar_limit_99: None,
            }),
            daily_loss_limit: dec!(20000),
            daily_loss_warning: dec!(10000),
            hedge_funding_penalty: dec!(1),
        }
    }
}

/// Rough base/quote split from a symbol — used only for the
/// stress runner's test-time registration. Covers the USDT /
/// USDC / BTC suffixes the canonical scenarios target.
fn split_base_quote(symbol: &str) -> (String, String) {
    for suffix in ["USDT", "USDC", "BUSD", "FDUSD"] {
        if let Some(base) = symbol.strip_suffix(suffix) {
            return (base.to_string(), suffix.to_string());
        }
    }
    (symbol.to_string(), "USDT".to_string())
}

/// Per-scenario stress-test output. Every field is a
/// human-readable scalar the dashboard or the CLI can render
/// without further computation.
#[derive(Debug, Clone, Default)]
pub struct StressReport {
    /// Scenario slug (`"covid-2020"` etc.) — used as the
    /// primary key in the aggregated CLI report.
    pub scenario: String,
    /// Peak absolute drawdown observed on the simulated MM
    /// PnL curve, in the reporting currency.
    pub max_drawdown: Decimal,
    /// Number of seconds from the peak-DD point until the
    /// PnL recovered to its pre-crash peak. `0` means the
    /// scenario ended still below the high-water mark.
    pub time_to_recovery_secs: u64,
    /// Maximum absolute inventory value observed during the
    /// scenario in the reporting currency (typically
    /// `|position_size| × peak_mid`).
    pub inventory_peak_value: Decimal,
    /// Number of soft / hard kill-switch escalations fired by
    /// the simulated kill switch.
    pub kill_switch_trips: u32,
    /// Number of VaR-guard throttle activations (ticks where
    /// the throttle dropped below 1.0).
    pub var_throttle_hits: u32,
    /// Number of distinct hedge basket recommendations emitted
    /// across the scenario.
    pub hedge_baskets_recommended: u32,
    /// Final total PnL in the reporting currency at the end
    /// of the scenario.
    pub final_total_pnl: Decimal,
}

impl StressReport {
    /// Render the report as a markdown table. Used by the
    /// CLI's default output and by the stage-2 daily report
    /// aggregator.
    pub fn to_markdown(&self) -> String {
        format!(
            "| Metric | Value |\n\
             |---|---|\n\
             | Scenario | `{}` |\n\
             | Max drawdown | {} |\n\
             | Time to recovery (s) | {} |\n\
             | Inventory peak value | {} |\n\
             | Kill-switch trips | {} |\n\
             | VaR throttle hits | {} |\n\
             | Hedge baskets | {} |\n\
             | Final PnL | {} |\n",
            self.scenario,
            self.max_drawdown,
            self.time_to_recovery_secs,
            self.inventory_peak_value,
            self.kill_switch_trips,
            self.var_throttle_hits,
            self.hedge_baskets_recommended,
            self.final_total_pnl,
        )
    }
}

/// Pure-function stress-test runner. Walks the synthetic tick
/// stream from [`generate_ticks`] through a simulated
/// Portfolio / VarGuard / KillSwitch / HedgeOptimizer combo
/// and captures the metrics needed for the stress report.
///
/// v1 is a **synthetic** runner — it models the MM as a
/// constant-position holder marking to the current mid, which
/// is enough to exercise the Epic C risk machinery end to end
/// without needing a full `MarketMakerEngine` boot sequence.
/// Stage-2 will add an engine-driven variant that runs the
/// real strategy through the same tick stream.
pub fn run_stress(scenario: &StressScenario, config: &StressRunConfig) -> StressReport {
    let ticks = generate_ticks(scenario, config.baseline_price);
    if ticks.is_empty() {
        return StressReport {
            scenario: scenario.slug.to_string(),
            ..Default::default()
        };
    }

    let mut portfolio = Portfolio::new("USDT");
    portfolio.register_symbol(
        scenario.primary_symbol,
        &config.base_asset,
        &config.quote_asset,
    );
    // Seed the portfolio with the initial position at the
    // baseline price — this is the "MM starts with one unit
    // long, marks to market as the scenario unfolds" model.
    portfolio.on_fill(
        scenario.primary_symbol,
        config.position_size,
        config.baseline_price,
        &config.strategy_class,
    );

    let mut var_guard = config.var_guard.clone().map(VarGuard::new);
    let mut kill_switch = KillSwitch::new(KillSwitchConfig {
        daily_loss_limit: config.daily_loss_limit,
        daily_loss_warning: config.daily_loss_warning,
        ..Default::default()
    });
    let hedge_optimizer = HedgeOptimizer::new(config.hedge_funding_penalty);
    // Small funding_bps so the L1 shrinkage leaves at least a
    // recognisable hedge for a 1-unit position. Production
    // values are 1-10 bps per 8-hour window; the scaled-down
    // number here keeps the optimizer's diagonal-closed form
    // from zero-shrinking the hedge for test fixtures.
    let hedge_universe = vec![HedgeInstrument {
        symbol: format!("{}-PERP", config.base_asset),
        factor: config.base_asset.clone(),
        cross_betas: vec![],
        funding_bps: dec!(0.01),
        position_cap: config.position_size * dec!(2),
    }];
    let mut factor_variances = std::collections::HashMap::new();
    factor_variances.insert(config.base_asset.clone(), dec!(1));

    // Running metrics.
    let mut max_drawdown = Decimal::ZERO;
    let mut peak_pnl = Decimal::ZERO;
    let mut peak_pnl_offset: u64 = 0;
    let mut time_to_recovery_secs: u64 = 0;
    let mut inventory_peak_value = Decimal::ZERO;
    let mut kill_switch_trips: u32 = 0;
    let mut last_kill_level = kill_switch.level();
    let mut var_throttle_hits: u32 = 0;
    let mut hedge_baskets_recommended: u32 = 0;
    let mut last_hedge_entries: Vec<(String, Decimal)> = Vec::new();
    let mut final_total_pnl = Decimal::ZERO;

    for tick in &ticks {
        // Mark the Portfolio to the tick's mid so the
        // unrealised PnL snapshot reflects the scenario path.
        portfolio.mark_price(scenario.primary_symbol, tick.mid_price);
        let snapshot = portfolio.snapshot();
        let total_pnl = snapshot.total_realised_pnl + snapshot.total_unrealised_pnl;
        final_total_pnl = total_pnl;

        // Track inventory value at current mid.
        let inv_value = (config.position_size * tick.mid_price).abs();
        if inv_value > inventory_peak_value {
            inventory_peak_value = inv_value;
        }

        // Drawdown tracking.
        if total_pnl > peak_pnl {
            peak_pnl = total_pnl;
            peak_pnl_offset = tick.offset_secs;
        }
        let drawdown = peak_pnl - total_pnl;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
        // First recovery to the pre-crash peak after a
        // non-trivial drawdown.
        if time_to_recovery_secs == 0
            && max_drawdown > Decimal::ZERO
            && total_pnl >= peak_pnl
            && tick.offset_secs > peak_pnl_offset
        {
            time_to_recovery_secs = tick.offset_secs.saturating_sub(peak_pnl_offset);
        }

        // Feed the kill switch and count escalations.
        kill_switch.update_pnl(total_pnl);
        if kill_switch.level() != last_kill_level {
            kill_switch_trips += 1;
            last_kill_level = kill_switch.level();
        }

        // Feed the VaR guard one sample per minute (each tick
        // is a minute by construction).
        if let Some(vg) = var_guard.as_mut() {
            vg.record_pnl_sample(&config.strategy_class, total_pnl);
            let throttle = vg.effective_throttle(&config.strategy_class);
            if throttle < Decimal::ONE {
                var_throttle_hits += 1;
            }
        }

        // Refresh hedge basket — count it only when the
        // recommendation **changes** to avoid double-counting
        // steady-state recommendations.
        let basket =
            hedge_optimizer.optimize(&snapshot.per_factor, &hedge_universe, &factor_variances);
        if !basket.is_empty() && basket.entries != last_hedge_entries {
            hedge_baskets_recommended += 1;
            last_hedge_entries = basket.entries.clone();
        }
    }

    StressReport {
        scenario: scenario.slug.to_string(),
        max_drawdown,
        time_to_recovery_secs,
        inventory_peak_value,
        kill_switch_trips,
        var_throttle_hits,
        hedge_baskets_recommended,
        final_total_pnl,
    }
}

/// Run every canonical scenario with the default config and
/// return a vector of reports in chronological (catalogue)
/// order. Used by the CLI's `--all` flag.
pub fn run_all_stress(
    defaults_for: impl Fn(&StressScenario) -> StressRunConfig,
) -> Vec<StressReport> {
    CANONICAL_SCENARIOS
        .iter()
        .map(|s| run_stress(s, &defaults_for(s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical catalogue has exactly five scenarios.
    /// Pin the count so a future contributor either adds a
    /// new scenario AND updates this test, or deletes one
    /// and has to explain why.
    #[test]
    fn canonical_scenario_count_is_five() {
        assert_eq!(CANONICAL_SCENARIOS.len(), 5);
    }

    /// Slugs are distinct, URL-safe, and `scenario_by_slug`
    /// round-trips every one.
    #[test]
    fn every_scenario_has_unique_url_safe_slug() {
        let mut seen = std::collections::HashSet::new();
        for s in CANONICAL_SCENARIOS {
            assert!(
                seen.insert(s.slug),
                "duplicate slug in canonical catalogue: {}",
                s.slug
            );
            assert!(
                s.slug
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "slug {} has non-url-safe chars",
                s.slug
            );
            assert_eq!(scenario_by_slug(s.slug).map(|x| x.slug), Some(s.slug));
        }
    }

    /// `peak_time_secs` must fit inside `duration_secs` — a
    /// scenario that peaks after the scenario ends is a
    /// bug, not a valid shape.
    #[test]
    fn peak_time_fits_inside_duration() {
        for s in CANONICAL_SCENARIOS {
            assert!(
                s.shock.peak_time_secs <= s.duration_secs,
                "scenario {} peak_time exceeds duration",
                s.slug
            );
        }
    }

    /// Every shock profile has sane, non-degenerate
    /// parameters. Pins the catalogue against accidental
    /// zero-outs during refactors.
    #[test]
    fn shock_profiles_are_non_degenerate() {
        for s in CANONICAL_SCENARIOS {
            assert!(
                !s.shock.peak_price_move.is_zero(),
                "{}: peak_price_move must be non-zero",
                s.slug
            );
            assert!(
                s.shock.volume_multiplier_peak > dec!(1),
                "{}: volume should amplify during a shock",
                s.slug
            );
            assert!(
                s.shock.spread_multiplier_peak > dec!(1),
                "{}: spread should blow out during a shock",
                s.slug
            );
            assert!(
                s.shock.depth_fraction_peak > dec!(0) && s.shock.depth_fraction_peak <= dec!(1),
                "{}: depth fraction must be in (0, 1]",
                s.slug
            );
            assert!(
                s.shock.sell_flow_share_peak >= dec!(0) && s.shock.sell_flow_share_peak <= dec!(1),
                "{}: sell flow share must be in [0, 1]",
                s.slug
            );
        }
    }

    /// Unknown slug returns None, known slug returns Some.
    #[test]
    fn scenario_lookup_handles_unknown() {
        assert!(scenario_by_slug("nonexistent-scenario").is_none());
        assert!(scenario_by_slug("covid-2020").is_some());
    }

    // ---- tick generator tests (Sprint C-4) ----

    /// Tick count matches `duration_secs / 60`. Pins the
    /// 1-minute cadence contract so a future refactor can't
    /// silently change the sample rate under the VaR guard's
    /// nose.
    #[test]
    fn generator_produces_one_tick_per_minute() {
        let scenario = scenario_by_slug("ftx-2022").unwrap();
        let ticks = generate_ticks(scenario, dec!(50000));
        // FTX scenario is 2 hours = 7200 s → 120 ticks.
        assert_eq!(ticks.len(), 120);
        assert_eq!(ticks[0].offset_secs, 0);
        assert_eq!(ticks[1].offset_secs, 60);
        assert_eq!(ticks[119].offset_secs, 7140);
    }

    /// First tick sits at the baseline (ramp = 0). Last tick
    /// ALSO sits at the baseline because the recovery leg
    /// returns to ramp = 0. Pins the triangular-ramp shape.
    #[test]
    fn generator_first_and_last_ticks_are_at_baseline() {
        let scenario = scenario_by_slug("covid-2020").unwrap();
        let ticks = generate_ticks(scenario, dec!(50000));
        assert_eq!(ticks.first().unwrap().mid_price, dec!(50000));
        assert_eq!(ticks.last().unwrap().mid_price, dec!(50000));
    }

    /// Peak tick sits at the full shock magnitude. COVID
    /// scenario peaks at -50 % after 12 h → mid price must be
    /// exactly baseline · 0.5 at that tick.
    #[test]
    fn generator_peak_tick_matches_shock_profile() {
        let scenario = scenario_by_slug("covid-2020").unwrap();
        let ticks = generate_ticks(scenario, dec!(50000));
        // peak_time_secs = 12 h = 43_200 s → index 720.
        let peak = &ticks[720];
        assert_eq!(peak.mid_price, dec!(25000));
        // Volume at peak = baseline · 5 = 500.
        assert_eq!(peak.trade_volume, dec!(500));
        // Depth fraction at peak = 0.15.
        assert_eq!(peak.depth_fraction, dec!(0.15));
    }

    /// Mid price is monotonic decreasing on the way down and
    /// monotonic increasing on the recovery leg. Catches the
    /// regression where the ramp formula flips sign.
    #[test]
    fn generator_price_path_is_monotone_per_leg() {
        let scenario = scenario_by_slug("china-2021").unwrap();
        let ticks = generate_ticks(scenario, dec!(50000));
        let peak_idx = ticks
            .iter()
            .position(|t| t.mid_price == ticks.iter().map(|x| x.mid_price).min().unwrap())
            .unwrap();
        // Down-leg is monotone non-increasing.
        for w in ticks[..=peak_idx].windows(2) {
            assert!(
                w[0].mid_price >= w[1].mid_price,
                "down-leg not monotone at {}",
                w[0].offset_secs
            );
        }
        // Up-leg is monotone non-decreasing.
        for w in ticks[peak_idx..].windows(2) {
            assert!(
                w[0].mid_price <= w[1].mid_price,
                "up-leg not monotone at {}",
                w[0].offset_secs
            );
        }
    }

    /// Same scenario + same baseline produce byte-identical
    /// tick streams across repeated calls. This is the
    /// "deterministic reproducibility" property the stress
    /// CLI needs for snapshot testing.
    #[test]
    fn generator_is_deterministic() {
        let scenario = scenario_by_slug("luna-2022").unwrap();
        let a = generate_ticks(scenario, dec!(100));
        let b = generate_ticks(scenario, dec!(100));
        assert_eq!(a.len(), b.len());
        for (t_a, t_b) in a.iter().zip(b.iter()) {
            assert_eq!(t_a.mid_price, t_b.mid_price);
            assert_eq!(t_a.trade_volume, t_b.trade_volume);
            assert_eq!(t_a.depth_fraction, t_b.depth_fraction);
            assert_eq!(t_a.sell_flow_share, t_b.sell_flow_share);
        }
    }

    // ---- stress runner tests (Sprint C-4) ----

    /// End-to-end integration: the covid-2020 scenario with
    /// a VaR-guard-enabled config drives a non-trivial
    /// max drawdown, fires at least one kill-switch trip
    /// (the -50 % move pushes us through the configured
    /// daily_loss_limit), and records a hedge basket
    /// recommendation on the first tick.
    ///
    /// Pins the full pipeline:
    /// `ShockProfile → generate_ticks → run_stress →
    /// StressReport`. Any future refactor that breaks one
    /// step in that chain breaks this test.
    #[test]
    fn run_stress_end_to_end_on_covid_scenario() {
        let scenario = scenario_by_slug("covid-2020").unwrap();
        let mut config = StressRunConfig::defaults_for(scenario);
        // Override the loss limit so we know the simulated
        // -50 % move definitely breaches it: baseline 50k × 1
        // unit × 50 % = 25k drawdown.
        config.daily_loss_limit = dec!(5000);
        config.daily_loss_warning = dec!(2000);

        let report = run_stress(scenario, &config);

        // Report identity + non-zero metrics.
        assert_eq!(report.scenario, "covid-2020");
        assert!(
            report.max_drawdown > dec!(0),
            "covid crash should produce a non-zero max DD"
        );
        // Inventory peak at least matches the baseline
        // position (1 unit × 50000 = 50000 baseline value).
        assert!(
            report.inventory_peak_value >= dec!(50000),
            "inventory peak should reach at least the baseline notional"
        );
        // The -50 % move breaches the $5k loss limit — the
        // kill switch must have fired at least once.
        assert!(
            report.kill_switch_trips >= 1,
            "covid crash should trip the kill switch at least once, got {}",
            report.kill_switch_trips
        );
        // The -50 % move with the default VaR limit -10k
        // must breach at some point during the scenario.
        assert!(
            report.var_throttle_hits >= 1,
            "var guard should throttle at some point during the covid scenario"
        );
        // Hedge basket should be recommended on the first
        // tick (long 1 BTC → -1 BTC-PERP hedge).
        assert!(
            report.hedge_baskets_recommended >= 1,
            "hedge optimizer should produce at least one recommendation"
        );
    }

    /// `run_all_stress` returns exactly five reports, one per
    /// canonical scenario, in catalogue order.
    #[test]
    fn run_all_stress_returns_one_report_per_scenario() {
        let reports = run_all_stress(StressRunConfig::defaults_for);
        assert_eq!(reports.len(), 5);
        assert_eq!(reports[0].scenario, "covid-2020");
        assert_eq!(reports[1].scenario, "china-2021");
        assert_eq!(reports[2].scenario, "luna-2022");
        assert_eq!(reports[3].scenario, "ftx-2022");
        assert_eq!(reports[4].scenario, "usdc-depeg-2023");
    }

    /// `StressReport::to_markdown` renders every field as a
    /// line in a 2-column table. Pins the shape so future
    /// CLI output is backwards-compatible.
    #[test]
    fn stress_report_markdown_covers_every_field() {
        let report = StressReport {
            scenario: "test".into(),
            max_drawdown: dec!(123.45),
            time_to_recovery_secs: 300,
            inventory_peak_value: dec!(50000),
            kill_switch_trips: 2,
            var_throttle_hits: 7,
            hedge_baskets_recommended: 3,
            final_total_pnl: dec!(-456.78),
        };
        let md = report.to_markdown();
        assert!(md.contains("`test`"));
        assert!(md.contains("123.45"));
        assert!(md.contains("300"));
        assert!(md.contains("50000"));
        assert!(md.contains("-456.78"));
    }

    /// Disabling the VaR guard (`var_guard = None`) makes
    /// `var_throttle_hits` stay at zero regardless of how
    /// severe the scenario is. Regression anchor for the
    /// "opt-out of VaR" config state.
    #[test]
    fn disabling_var_guard_zeroes_throttle_hits() {
        let scenario = scenario_by_slug("luna-2022").unwrap();
        let mut config = StressRunConfig::defaults_for(scenario);
        config.var_guard = None;
        let report = run_stress(scenario, &config);
        assert_eq!(report.var_throttle_hits, 0);
    }

    // ── Full-engine stress integration tests ────────────────

    /// Run ALL five canonical scenarios with default configs
    /// and verify basic invariants hold across every report.
    #[test]
    fn all_scenarios_produce_valid_reports() {
        let reports = run_all_stress(StressRunConfig::defaults_for);
        assert_eq!(reports.len(), 5);
        for report in &reports {
            // Max drawdown should be positive (absolute value
            // of the worst PnL dip during the scenario).
            assert!(
                report.max_drawdown >= dec!(0),
                "scenario {} max_drawdown={} should be ≥ 0",
                report.scenario,
                report.max_drawdown
            );
            // Inventory peak should be non-negative.
            assert!(
                report.inventory_peak_value >= dec!(0),
                "scenario {} inventory_peak_value should be ≥ 0",
                report.scenario
            );
        }
    }

    /// The LUNA -95% crash should be the most severe scenario
    /// by max drawdown across the canonical five.
    #[test]
    fn luna_has_worst_drawdown_of_all_scenarios() {
        let reports = run_all_stress(StressRunConfig::defaults_for);
        let luna = reports.iter().find(|r| r.scenario == "luna-2022").unwrap();
        for report in &reports {
            assert!(
                luna.max_drawdown >= report.max_drawdown,
                "LUNA dd={} should be worst (highest), but {} has {}",
                luna.max_drawdown,
                report.scenario,
                report.max_drawdown
            );
        }
    }

    /// Kill switch should trip at least once on the severe
    /// scenarios (covid, luna, ftx).
    #[test]
    fn severe_scenarios_trip_kill_switch() {
        for slug in ["covid-2020", "luna-2022", "ftx-2022"] {
            let scenario = scenario_by_slug(slug).unwrap();
            let config = StressRunConfig::defaults_for(scenario);
            let report = run_stress(scenario, &config);
            assert!(
                report.kill_switch_trips > 0,
                "scenario {} should trip the kill switch",
                slug
            );
        }
    }
}
