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
    fn every_bundled_template_compiles() {
        // Round-trip through the full compile path (validation +
        // topological sort + catalog resolution). Any broken
        // template fails CI here.
        for t in BUILTIN {
            let g = Graph::from_json(t.body).unwrap();
            Evaluator::build(&g)
                .unwrap_or_else(|e| panic!("template {} failed to compile: {e:?}", t.name));
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
