//! Bundled strategy graph templates.
//!
//! Small, documented starter graphs an operator loads from the UI
//! palette as "blank-slate → first useful graph" onboarding. Each
//! template is embedded at compile time (`include_str!`) so it
//! ships with the binary without any `data/` filesystem dependency.
//!
//! Templates target three common patterns:
//!
//!   major-spot-basic    — VPIN-driven spread widening
//!   meme-spot-guarded   — toxicity + inventory-urgency flatten
//!   cross-asset-regime  — volatility-regime picks 1.5× / 1.0× mult
//!
//! Adding a new template: drop a valid graph JSON under `templates/`,
//! register it in the `BUILTIN` table, add a test that round-trips
//! it through `Evaluator::build` so broken templates fail CI.

use crate::graph::Graph;
use anyhow::{Context, Result};

/// Bundled template — name + short description + raw JSON body.
#[derive(Debug, Clone)]
pub struct BuiltinTemplate {
    pub name: &'static str,
    pub description: &'static str,
    pub body: &'static str,
}

static MAJOR_SPOT_BASIC: &str = include_str!("../templates/major-spot-basic.json");
static MEME_SPOT_GUARDED: &str = include_str!("../templates/meme-spot-guarded.json");
static CROSS_ASSET_REGIME: &str = include_str!("../templates/cross-asset-regime.json");
static GRID_VIA_GRAPH: &str = include_str!("../templates/grid-via-graph.json");
static AVELLANEDA_VIA_GRAPH: &str = include_str!("../templates/avellaneda-via-graph.json");
static BASIS_CARRY_SPOT_PERP: &str = include_str!("../templates/basis-carry-spot-perp.json");
static PENTEST_SPOOF_CLASSIC: &str = include_str!("../templates/pentest/spoof-classic.json");
static PENTEST_PUMP_AND_DUMP: &str = include_str!("../templates/pentest/pump-and-dump.json");
static PENTEST_RAVE_CYCLE: &str = include_str!("../templates/pentest/rave-cycle.json");
static PENTEST_RAVE_FULL_CAMPAIGN: &str = include_str!("../templates/pentest/rave-full-campaign.json");
static PENTEST_LIQUIDATION_CASCADE: &str = include_str!("../templates/pentest/liquidation-cascade.json");
static PENTEST_BASKET_PUSH: &str = include_str!("../templates/pentest/basket-push.json");
static RUG_DETECTOR_COMPOSITE: &str = include_str!("../templates/rug-detector-composite.json");
static FUNDING_AWARE_QUOTER: &str = include_str!("../templates/funding-aware-quoter.json");
static LIQUIDITY_BURN_GUARD: &str = include_str!("../templates/liquidity-burn-guard.json");
static COST_GATED_QUOTER: &str = include_str!("../templates/cost-gated-quoter.json");
static GLFT_VIA_GRAPH: &str = include_str!("../templates/glft-via-graph.json");
static CROSS_EXCHANGE_BASIC: &str = include_str!("../templates/cross-exchange-basic.json");

const BUILTIN: &[BuiltinTemplate] = &[
    BuiltinTemplate {
        name: "major-spot-basic",
        description: "VPIN → spread widening (minimum viable graph for BTCUSDT / ETHUSDT).",
        body: MAJOR_SPOT_BASIC,
    },
    BuiltinTemplate {
        name: "meme-spot-guarded",
        description: "Toxicity + inventory urgency → auto-flatten via VWAP on 80 % cap fill.",
        body: MEME_SPOT_GUARDED,
    },
    BuiltinTemplate {
        name: "cross-asset-regime",
        description: "Volatility regime gate: 1.5× spread when vol > 60 %, 1.0× otherwise.",
        body: CROSS_ASSET_REGIME,
    },
    BuiltinTemplate {
        name: "grid-via-graph",
        description: "Phase 4 reference: full graph-authored symmetric grid via Quote.Grid + Out.Quotes.",
        body: GRID_VIA_GRAPH,
    },
    BuiltinTemplate {
        name: "avellaneda-via-graph",
        description: "Phase 4 reference: the engine's Avellaneda-Stoikov wrapped as a composite node (uses live config).",
        body: AVELLANEDA_VIA_GRAPH,
    },
    BuiltinTemplate {
        name: "basis-carry-spot-perp",
        description: "Multi-Venue ref: reads Binance spot + Bybit perp L1 from the DataBus, feeds a Basis strategy that quotes on this engine's venue. End-to-end demo of cross-venue reads.",
        body: BASIS_CARRY_SPOT_PERP,
    },
    BuiltinTemplate {
        name: "pentest-spoof-classic",
        description: "⚠ PENTEST ONLY — Strategy.Spoof + co-located SpoofingScore guard that trips kill L4 when the detector catches us. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode.",
        body: PENTEST_SPOOF_CLASSIC,
    },
    BuiltinTemplate {
        name: "pentest-pump-and-dump",
        description: "⚠ PENTEST ONLY — Strategy.PumpAndDump FSM (accumulate → pump → distribute → dump) paired with Surveillance.ManipulationScore as the self-kill guard. Reproduces the RAVE cycle on a test venue so the detectors fire against the exploit. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode.",
        body: PENTEST_PUMP_AND_DUMP,
    },
    // RS-5 — risk-aware starter graphs using the new Phase II
    // sources (Funding / Risk / Cost). Operators clone and edit.
    BuiltinTemplate {
        name: "funding-aware-quoter",
        description: "Widens spread 2× when within 60 s of the next funding settle. Uses Funding.seconds_to_next + Cast.ToBool + Logic.Mux → Out.SpreadMult.",
        body: FUNDING_AWARE_QUOTER,
    },
    BuiltinTemplate {
        name: "liquidity-burn-guard",
        description: "Shrinks size to zero when Risk.UnrealizedIfFlatten drops below −1000 quote. Catches a book that thinned under us before the drawdown compounds.",
        body: LIQUIDITY_BURN_GUARD,
    },
    BuiltinTemplate {
        name: "cost-gated-quoter",
        description: "Pauses quoting when Cost.CumulativeToday passes 100 quote units. Useful for intraday cost-budget limits on low-edge pairs.",
        body: COST_GATED_QUOTER,
    },
    // S6.2 — starter templates mirroring the legacy single-strategy
    // slot, so operators coming from `strategy=glft` or
    // `strategy=cross_exchange` config can pick an equivalent
    // graph without hand-authoring nodes.
    BuiltinTemplate {
        name: "glft-via-graph",
        description: "GLFT single-strategy starter (mirror of legacy strategy=glft). Clone + tweak γ, κ on Strategy.GLFT node config.",
        body: GLFT_VIA_GRAPH,
    },
    BuiltinTemplate {
        name: "cross-exchange-basic",
        description: "CrossExchange make-A / hedge-B single-strategy starter (mirror of legacy strategy=cross_exchange).",
        body: CROSS_EXCHANGE_BASIC,
    },
    // R2.14 — operator-ready composite rug detector. Wraps the
    // engine's Avellaneda quoter with a Surveillance.RugScore
    // guard that trips kill-switch L2 (WidenSpreads → stop new
    // orders) on combined ≥ 0.6. Clone-and-deploy for any
    // symbol; pair with `symbol_circulating_supply` + `[onchain]`
    // config to activate every sub-signal.
    BuiltinTemplate {
        name: "rug-detector-composite",
        description: "Avellaneda quoter + Surveillance.RugScore guard → Cast.ToBool(≥0.6) → Out.KillEscalate(WidenSpreads). One-click deploy for any symbol; feeds off manipulation + on-chain + listing-age + mcap proxy signals aggregated by the engine.",
        body: RUG_DETECTOR_COMPOSITE,
    },
    // R2.15 — exploit + guard pentest template. Runs the
    // PumpAndDump FSM under `MM_ALLOW_RESTRICTED=yes-pentest-mode` AND checks
    // if the defensive RugScore catches its own attack —
    // mirror image of what the user's "other agent" will do to
    // stress-test their own exchange's surveillance stack.
    BuiltinTemplate {
        name: "pentest-rave-cycle",
        description: "⚠ PENTEST ONLY — Strategy.PumpAndDump runs the RAVE 4-phase cycle; Surveillance.RugScore watches the tape and trips kill L4 if combined ≥ 0.5. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode.",
        body: PENTEST_RAVE_CYCLE,
    },
    BuiltinTemplate {
        name: "pentest-rave-full-campaign",
        description: "⚠⚠⚠ PENTEST ONLY — FULL MULTI-PHASE RAVE CAMPAIGN. CampaignOrchestrator chains accumulate → leverage_long → liquidation_hunt → distribute → dump across the configured timeline. Surveillance.RugScore self-guard trips kill L4 on ≥ 0.5. Running this against any venue you don't own / aren't explicitly authorized to pentest is illegal under MiFID II / Dodd-Frank / MiCA and a ToS violation everywhere. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode. OPERATOR MUST CONFIRM: (1) authorized pentest only, (2) own exchange or written authorization, (3) compliance review complete. THIS TOOL IS A PENTEST INSTRUMENT, NOT AN ATTACK KIT.",
        body: PENTEST_RAVE_FULL_CAMPAIGN,
    },
    BuiltinTemplate {
        name: "pentest-liquidation-cascade",
        description: "⚠⚠⚠ PENTEST ONLY — LIQUIDATION CASCADE TRIGGER. Combines Signal.LongShortRatio (crowd positioning) + Signal.LiquidationLevelEstimate (forward-looking cluster distance) → Strategy.CascadeHunter (gated crossing push). Surveillance.RugScore self-guard fires at 0.5. Reproduces the 2021-05 BTC flash-crash pattern (see docs/research/liquidation-cascades.md for the full public-investigation reference). Running this on any venue you don't own or aren't explicitly authorized to pentest is market manipulation under MAR Article 12 / CEA §9(a) / MiCA Article 92 and a ToS violation everywhere. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode + written venue-owner authorization + compliance review sign-off before deploy.",
        body: PENTEST_LIQUIDATION_CASCADE,
    },
    BuiltinTemplate {
        name: "pentest-basket-push",
        description: "⚠⚠⚠ PENTEST ONLY — CORRELATED BASKET PUSH. Strategy.BasketPush fans VenueQuotes across a pre-configured 3-symbol basket (placeholder: RAVEUSDT + SIRENUSDT + MYXUSDT) to replay the 2026-04 ZachXBT correlated-group attack shape. Surveillance.RugScore self-guard trips kill L4 on combined ≥ 0.5. Running on any venue without written owner authorization is market manipulation under MAR Art. 12 / CEA §9(a) / MiCA Art. 92. Requires MM_ALLOW_RESTRICTED=yes-pentest-mode + compliance review sign-off + the explicit basket members replaced with your authorized pentest symbols before deploy.",
        body: PENTEST_BASKET_PUSH,
    },
];

/// List of all bundled templates.
pub fn list() -> Vec<BuiltinTemplate> {
    BUILTIN.to_vec()
}

/// Load a template by name, parsing the bundled JSON. Returns `None`
/// for unknown names. JSON parse errors surface as `Err` — a broken
/// template is a build-time issue (a CI test catches it).
pub fn load(name: &str) -> Option<Result<Graph>> {
    let raw = BUILTIN.iter().find(|t| t.name == name)?.body;
    Some(Graph::from_json(raw).with_context(|| format!("parse template {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::Evaluator;

    #[test]
    fn every_bundled_template_parses() {
        for t in BUILTIN {
            let g = Graph::from_json(t.body)
                .unwrap_or_else(|e| panic!("template {} failed to parse: {e}", t.name));
            assert_eq!(g.name, t.name, "template name must match key");
        }
    }

    #[test]
    fn every_safe_template_compiles() {
        // Round-trip through the full compile path (validation +
        // topological sort + catalog resolution). Any broken
        // template fails CI here. Pentest templates are skipped —
        // they're tested separately against the restricted gate.
        for t in BUILTIN {
            if t.name.starts_with("pentest-") {
                continue;
            }
            let g = Graph::from_json(t.body).unwrap();
            Evaluator::build(&g)
                .unwrap_or_else(|e| panic!("template {} failed to compile: {e:?}", t.name));
        }
    }

    #[test]
    fn pentest_templates_refused_without_env() {
        // Epic R — exploit templates must be refused unless the
        // runtime opts in. This test proves the gate actually
        // fires; ship + change of the gate without fixing this
        // test is a red flag.
        use crate::graph::ValidationError;
        for t in BUILTIN {
            if !t.name.starts_with("pentest-") {
                continue;
            }
            let g = Graph::from_json(t.body).unwrap();
            let err = Evaluator::build(&g).expect_err(
                "pentest template should refuse to compile without the restricted env",
            );
            assert!(
                matches!(err, ValidationError::RestrictedNotAllowed(_)),
                "expected RestrictedNotAllowed, got {err:?}"
            );
        }
    }

    #[test]
    fn load_unknown_returns_none() {
        assert!(load("no-such-template").is_none());
    }

    #[test]
    fn load_known_returns_ok() {
        let result = load("major-spot-basic").expect("known template");
        result.expect("template parses");
    }
}
