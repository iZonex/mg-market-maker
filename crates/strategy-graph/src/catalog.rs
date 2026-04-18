//! Catalog — hydrates a `Node.kind: String` + config JSON into a
//! `Box<dyn NodeKind>` plus exposes the `KindShape` view for graph
//! validation without constructing the node.
//!
//! Two surfaces:
//!
//!   [`build`] — takes the kind + node config, returns a configured
//!               instance ready for `evaluate`. Used by the
//!               evaluator at compile time.
//!   [`shape`] — shape-only lookup (port names + types + restricted
//!               flag), used by `Graph::validate`. Avoids
//!               instantiating configurable nodes whose parsed
//!               config might fail — the validator only cares
//!               about shape.

use crate::graph::KindShape;
use crate::node::NodeKind;
use crate::nodes::{exec, indicators, logic, math, risk, sinks, sources, stats};
use serde_json::Value as Json;

/// Construct a node by its catalog key + raw config JSON.
/// `config` is allowed to be `Null` for configless nodes.
/// Returns `None` on unknown kind or on config-parse failure —
/// the caller (evaluator build) maps that to the appropriate
/// validation error.
pub fn build(kind: &str, config: &Json) -> Option<Box<dyn NodeKind>> {
    match kind {
        // Math
        "Math.Add" => Some(Box::new(math::Add)),
        "Math.Mul" => Some(Box::new(math::Mul)),
        "Math.Const" => math::Const::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>),
        // Stats — configurable α
        "Stats.EWMA" => stats::Ewma::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>),
        // Cast — configurable threshold + comparator
        "Cast.ToBool" => {
            math::ToBool::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        // Logic
        "Logic.And" => Some(Box::new(logic::And)),
        "Logic.Mux" => Some(Box::new(logic::Mux)),
        // Sources — port-only, values come in via source_inputs at eval
        "Book.L1" => Some(Box::new(sources::BookL1)),
        "Sentiment.Rate" => Some(Box::new(sources::SentimentRate)),
        "Sentiment.Score" => Some(Box::new(sources::SentimentScore)),
        "Volatility.Realised" => Some(Box::new(sources::VolatilityRealised)),
        "Toxicity.VPIN" => Some(Box::new(sources::ToxicityVpin)),
        "Momentum.OFIZ" => Some(Box::new(sources::MomentumOfiZ)),
        // Phase 2 Wave A — strategy + pair-class metadata sources
        "Strategy.Active" => Some(Box::new(sources::StrategyActive)),
        "PairClass.Current" => Some(Box::new(sources::PairClassCurrent)),
        "Cast.StrategyEq" => {
            logic::StrategyEq::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Cast.PairClassEq" => {
            logic::PairClassEq::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        // Phase 2 Wave B — risk layer
        "Risk.MarginRatio" => Some(Box::new(sources::RiskMarginRatio)),
        "Risk.OTR" => Some(Box::new(sources::RiskOtr)),
        "Inventory.Level" => Some(Box::new(sources::InventoryLevel)),
        "Risk.ToxicityWiden" => {
            risk::ToxicityWiden::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Risk.InventoryUrgency" => {
            risk::InventoryUrgency::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Risk.CircuitBreaker" => {
            risk::CircuitBreaker::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        // Phase 2 Wave C — indicators + signal sources
        "Indicator.SMA" => {
            indicators::SmaNode::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Indicator.EMA" => {
            indicators::EmaNode::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Indicator.HMA" => {
            indicators::HmaNode::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Indicator.RSI" => {
            indicators::RsiNode::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Indicator.ATR" => {
            indicators::AtrNode::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Indicator.Bollinger" => indicators::BollingerNode::from_config(config)
            .map(|n| Box::new(n) as Box<dyn NodeKind>),
        "Signal.ImbalanceDepth" => Some(Box::new(sources::SignalImbalance)),
        "Signal.TradeFlow" => Some(Box::new(sources::SignalTradeFlow)),
        "Signal.Microprice" => Some(Box::new(sources::SignalMicroprice)),
        "Toxicity.KyleLambda" => Some(Box::new(sources::KyleLambda)),
        "Regime.Detector" => Some(Box::new(sources::RegimeDetector)),
        // Phase 2 Wave D — exec algo presets + flatten
        "Logic.StringMux" => Some(Box::new(logic::StringMux)),
        "Exec.TwapConfig" => {
            exec::TwapConfig::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Exec.VwapConfig" => {
            exec::VwapConfig::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Exec.PovConfig" => {
            exec::PovConfig::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Exec.IcebergConfig" => {
            exec::IcebergConfig::from_config(config).map(|n| Box::new(n) as Box<dyn NodeKind>)
        }
        "Out.Flatten" => Some(Box::new(sinks::Flatten)),
        // Sinks
        "Out.SpreadMult" => Some(Box::new(sinks::SpreadMult)),
        "Out.SizeMult" => Some(Box::new(sinks::SizeMult)),
        "Out.KillEscalate" => Some(Box::new(sinks::KillEscalate)),
        _ => None,
    }
}

/// Shape-only lookup. We instantiate a default version of the node
/// just to read its declared ports; configurable nodes provide a
/// `Default` that has the canonical shape.
pub fn shape(kind: &str) -> Option<KindShape> {
    let node = build(kind, &Json::Null)?;
    Some(KindShape {
        inputs: node
            .input_ports()
            .iter()
            .map(|p| (p.name.clone(), p.ty))
            .collect(),
        outputs: node
            .output_ports()
            .iter()
            .map(|p| (p.name.clone(), p.ty))
            .collect(),
        restricted: node.restricted(),
    })
}

/// Snapshot of every kind in the catalog. The `/api/v1/strategy/catalog`
/// endpoint will call this to render the UI node palette.
pub fn kinds() -> Vec<(&'static str, KindShape)> {
    let ks: &[&str] = &[
        "Math.Add",
        "Math.Mul",
        "Math.Const",
        "Stats.EWMA",
        "Cast.ToBool",
        "Logic.And",
        "Logic.Mux",
        "Book.L1",
        "Sentiment.Rate",
        "Sentiment.Score",
        "Volatility.Realised",
        "Toxicity.VPIN",
        "Momentum.OFIZ",
        "Strategy.Active",
        "PairClass.Current",
        "Cast.StrategyEq",
        "Cast.PairClassEq",
        "Risk.MarginRatio",
        "Risk.OTR",
        "Inventory.Level",
        "Risk.ToxicityWiden",
        "Risk.InventoryUrgency",
        "Risk.CircuitBreaker",
        "Indicator.SMA",
        "Indicator.EMA",
        "Indicator.HMA",
        "Indicator.RSI",
        "Indicator.ATR",
        "Indicator.Bollinger",
        "Signal.ImbalanceDepth",
        "Signal.TradeFlow",
        "Signal.Microprice",
        "Toxicity.KyleLambda",
        "Regime.Detector",
        "Logic.StringMux",
        "Exec.TwapConfig",
        "Exec.VwapConfig",
        "Exec.PovConfig",
        "Exec.IcebergConfig",
        "Out.Flatten",
        "Out.SpreadMult",
        "Out.SizeMult",
        "Out.KillEscalate",
    ];
    ks.iter()
        .filter_map(|k| shape(k).map(|s| (*k, s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PortType;

    #[test]
    fn catalog_builds_known_kinds() {
        let null = Json::Null;
        assert!(build("Math.Add", &null).is_some());
        assert!(build("Math.Mul", &null).is_some());
        assert!(build("Math.Const", &null).is_some());
        assert!(build("Stats.EWMA", &null).is_some());
        assert!(build("Cast.ToBool", &null).is_some());
        assert!(build("Logic.And", &null).is_some());
        assert!(build("Logic.Mux", &null).is_some());
        assert!(build("Book.L1", &null).is_some());
        assert!(build("Sentiment.Rate", &null).is_some());
        assert!(build("Sentiment.Score", &null).is_some());
        assert!(build("Volatility.Realised", &null).is_some());
        assert!(build("Toxicity.VPIN", &null).is_some());
        assert!(build("Momentum.OFIZ", &null).is_some());
        assert!(build("Out.SpreadMult", &null).is_some());
        assert!(build("Out.SizeMult", &null).is_some());
        assert!(build("Out.KillEscalate", &null).is_some());
    }

    #[test]
    fn catalog_returns_none_for_unknown() {
        assert!(build("Math.Unknown", &Json::Null).is_none());
    }

    #[test]
    fn add_shape_has_two_number_inputs_one_number_output() {
        let sh = shape("Math.Add").unwrap();
        assert_eq!(sh.inputs.len(), 2);
        assert!(sh.inputs.iter().all(|(_, t)| *t == PortType::Number));
        assert_eq!(sh.outputs.len(), 1);
        assert_eq!(sh.outputs[0].1, PortType::Number);
    }

    #[test]
    fn kinds_snapshot_nonempty_and_all_buildable() {
        let list = kinds();
        assert!(!list.is_empty());
        let null = Json::Null;
        for (k, _) in &list {
            assert!(
                build(k, &null).is_some(),
                "catalog key {k} failed to build"
            );
        }
    }

    #[test]
    fn catalog_has_43_nodes_after_wave_d() {
        // MVP 16 + Wave A (4) + Wave B (6) + Wave C (11) +
        // Wave D (Logic.StringMux + 4 Exec.*Config + Out.Flatten
        // = 6) = 43.
        assert_eq!(kinds().len(), 43, "catalog drift");
    }
}
