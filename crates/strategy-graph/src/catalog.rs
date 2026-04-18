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
use crate::nodes::{
    exec, indicators, logic, math, quotes, risk, sinks, sources, stats, strategies,
};
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
        "Book.L2" => Some(Box::new(sources::BookL2)),
        "Trade.Tape" => Some(Box::new(sources::TradeTape)),
        "Balance" => Some(Box::new(sources::BalanceSource)),
        "Funding" => Some(Box::new(sources::FundingSource)),
        "Portfolio.NetDelta" => Some(Box::new(sources::PortfolioNetDelta)),
        "Portfolio.QuoteAvailable" => Some(Box::new(sources::PortfolioQuoteAvailable)),
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
        // Phase 4 — graph-authored quoting
        "Quote.Grid" => Some(Box::new(quotes::Grid)),
        "Quote.Mux" => Some(Box::new(quotes::Mux)),
        "Out.Quotes" => Some(Box::new(sinks::Quotes)),
        "Out.VenueQuotes" => Some(Box::new(sinks::VenueQuotes)),
        "Out.AtomicBundle" => Some(Box::new(sinks::AtomicBundle)),
        // Phase 4 composite strategies (engine overlays via source_inputs)
        "Strategy.Avellaneda" => Some(Box::new(strategies::Avellaneda)),
        "Strategy.GLFT" => Some(Box::new(strategies::Glft)),
        "Strategy.Grid" => Some(Box::new(strategies::Grid)),
        "Strategy.Basis" => Some(Box::new(strategies::Basis)),
        "Strategy.CrossExchange" => Some(Box::new(strategies::CrossExchange)),
        "Strategy.BasisArb" => Some(Box::new(strategies::BasisArb)),
        // Epic R — exploit strategies (pentest-only, restricted)
        "Strategy.Spoof" => Some(Box::new(strategies::Spoof)),
        "Strategy.Wash" => Some(Box::new(strategies::Wash)),
        "Strategy.Ignite" => Some(Box::new(strategies::Ignite)),
        // Epic R — surveillance detectors (engine overlays per tick)
        "Surveillance.SpoofingScore" => Some(Box::new(sources::SpoofingScore)),
        "Surveillance.LayeringScore" => Some(Box::new(sources::LayeringScore)),
        "Surveillance.QuoteStuffingScore" => Some(Box::new(sources::QuoteStuffingScore)),
        "Surveillance.WashScore" => Some(Box::new(sources::WashScore)),
        "Surveillance.MomentumIgnitionScore" => Some(Box::new(sources::MomentumIgnitionScore)),
        "Surveillance.FakeLiquidityScore" => Some(Box::new(sources::FakeLiquidityScore)),
        // Sinks
        "Out.SpreadMult" => Some(Box::new(sinks::SpreadMult)),
        "Out.SizeMult" => Some(Box::new(sinks::SizeMult)),
        "Out.KillEscalate" => Some(Box::new(sinks::KillEscalate)),
        _ => None,
    }
}

/// Human-facing metadata for a catalog kind — used by the UI
/// palette + the in-canvas node label. Keeping this next to the
/// existing `build`/`shape` switch means any new kind added to the
/// catalog gets a compile-time shove to fill in its `meta` arm
/// too, because the `kinds()` snapshot test below flags missing
/// entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMeta {
    /// Short readable name, no prefix (`"Constant"`, not `"Math.Const"`).
    pub label: &'static str,
    /// One-line description of what the node produces / does.
    pub summary: &'static str,
    /// Palette grouping bucket — broader than `kind.split('.').0`:
    /// e.g. `"Sources"` bundles Book + Sentiment + Volatility so the
    /// operator sees them in one place.
    pub group: &'static str,
}

/// Lookup for [`NodeMeta`]. Unknown kinds fall back to a synthesised
/// meta derived from the kind name (used only if a catalog entry was
/// added without updating this table — defensive).
pub fn meta(kind: &str) -> NodeMeta {
    match kind {
        // Sources — pull live data from the engine.
        "Book.L1"              => NodeMeta { label: "L1 book",             summary: "Top-of-book bid/ask price + size (optional venue/symbol/product)", group: "Sources" },
        "Book.L2"              => NodeMeta { label: "L2 book",             summary: "Top-N levels per side off the shared data bus", group: "Sources" },
        "Trade.Tape"           => NodeMeta { label: "Trade tape",          summary: "Rolling public-trade window (count + buy/sell volume + last px)", group: "Sources" },
        "Balance"              => NodeMeta { label: "Balance",             summary: "Wallet balance (total / available / reserved) per venue + asset", group: "Sources" },
        "Funding"              => NodeMeta { label: "Funding",             summary: "Per-perp funding rate + seconds to next funding", group: "Sources" },
        "Portfolio.NetDelta"   => NodeMeta { label: "Net delta",           summary: "Cross-venue net exposure for the asset (long spot + short perp = 0)", group: "Sources" },
        "Portfolio.QuoteAvailable" => NodeMeta { label: "Quote available", summary: "Sum of available USDT/USDC/USD on the named venue", group: "Sources" },
        "Sentiment.Rate"       => NodeMeta { label: "Sentiment rate",      summary: "Social mentions per minute (source asset)", group: "Sources" },
        "Sentiment.Score"      => NodeMeta { label: "Sentiment score",     summary: "Weighted polarity score [-1..1]", group: "Sources" },
        "Volatility.Realised"  => NodeMeta { label: "Realised volatility", summary: "EWMA realised vol (annualised)", group: "Sources" },
        "Toxicity.VPIN"        => NodeMeta { label: "VPIN",                summary: "Volume-synchronised probability of informed trading", group: "Sources" },
        "Momentum.OFIZ"        => NodeMeta { label: "OFI z-score",         summary: "Order-flow imbalance standardised", group: "Sources" },
        "Signal.ImbalanceDepth"=> NodeMeta { label: "Book imbalance",      summary: "L1 depth imbalance [-1..1]", group: "Sources" },
        "Signal.TradeFlow"     => NodeMeta { label: "Trade flow",          summary: "Signed volume over a rolling window", group: "Sources" },
        "Signal.Microprice"    => NodeMeta { label: "Microprice",          summary: "Quantity-weighted mid-price", group: "Sources" },
        "Toxicity.KyleLambda"  => NodeMeta { label: "Kyle's lambda",       summary: "Price impact per unit signed volume", group: "Sources" },
        "Regime.Detector"      => NodeMeta { label: "Regime",              summary: "Current market regime (trend / mean-revert / chop)", group: "Sources" },
        "Strategy.Active"      => NodeMeta { label: "Active strategy",     summary: "Which strategy is running (Avellaneda / GLFT / …)", group: "Sources" },
        "PairClass.Current"    => NodeMeta { label: "Pair class",          summary: "Major / meme / stable / perp classification", group: "Sources" },
        "Risk.MarginRatio"     => NodeMeta { label: "Margin ratio",        summary: "Used-margin / equity", group: "Sources" },
        "Risk.OTR"             => NodeMeta { label: "Order-to-trade ratio",summary: "Surveillance OTR metric", group: "Sources" },
        "Inventory.Level"      => NodeMeta { label: "Inventory level",     summary: "Current net position size", group: "Sources" },

        // Indicators — technical.
        "Indicator.SMA"        => NodeMeta { label: "SMA",                 summary: "Simple moving average over N samples", group: "Indicators" },
        "Indicator.EMA"        => NodeMeta { label: "EMA",                 summary: "Exponential moving average", group: "Indicators" },
        "Indicator.HMA"        => NodeMeta { label: "HMA",                 summary: "Hull moving average", group: "Indicators" },
        "Indicator.RSI"        => NodeMeta { label: "RSI",                 summary: "Relative strength index [0..100]", group: "Indicators" },
        "Indicator.ATR"        => NodeMeta { label: "ATR",                 summary: "Average true range", group: "Indicators" },
        "Indicator.Bollinger"  => NodeMeta { label: "Bollinger bands",     summary: "SMA ± k·stddev envelope", group: "Indicators" },

        // Math / stats / logic — generic combinators.
        "Math.Add"             => NodeMeta { label: "Add",                 summary: "a + b", group: "Math" },
        "Math.Mul"             => NodeMeta { label: "Multiply",            summary: "a × b", group: "Math" },
        "Math.Const"           => NodeMeta { label: "Constant",            summary: "Literal number", group: "Math" },
        "Stats.EWMA"           => NodeMeta { label: "EWMA",                summary: "Exponential moving average (α)", group: "Math" },
        "Logic.And"             => NodeMeta { label: "AND",                summary: "Boolean AND", group: "Logic" },
        "Logic.Mux"             => NodeMeta { label: "Mux (number)",       summary: "Pick a or b by a boolean selector", group: "Logic" },
        "Logic.StringMux"       => NodeMeta { label: "Mux (string)",       summary: "Pick a or b (string) by a boolean selector", group: "Logic" },
        "Cast.ToBool"          => NodeMeta { label: "To bool",             summary: "Threshold a number into a bool", group: "Logic" },
        "Cast.StrategyEq"      => NodeMeta { label: "Strategy == ?",       summary: "True when active strategy matches target", group: "Logic" },
        "Cast.PairClassEq"     => NodeMeta { label: "Pair class == ?",     summary: "True when pair class matches target", group: "Logic" },

        // Risk layer — attenuators/amplifiers applied to downstream sinks.
        "Risk.ToxicityWiden"   => NodeMeta { label: "Widen on toxicity",   summary: "Scale spread by 1 + scale·vpin", group: "Risk" },
        "Risk.InventoryUrgency"=> NodeMeta { label: "Inventory urgency",   summary: "Power-scale by |inv|/cap", group: "Risk" },
        "Risk.CircuitBreaker"  => NodeMeta { label: "Wide-spread breaker", summary: "Fire if spread exceeds threshold (bps)", group: "Risk" },

        // Exec policies — feed Out.Flatten's policy port.
        "Exec.TwapConfig"      => NodeMeta { label: "TWAP policy",         summary: "Time-weighted execution (duration + slices)", group: "Exec" },
        "Exec.VwapConfig"      => NodeMeta { label: "VWAP policy",         summary: "Volume-weighted execution (duration)", group: "Exec" },
        "Exec.PovConfig"       => NodeMeta { label: "POV policy",          summary: "Percentage-of-volume execution", group: "Exec" },
        "Exec.IcebergConfig"   => NodeMeta { label: "Iceberg policy",      summary: "Iceberg execution (display qty)", group: "Exec" },

        // Phase 4 — graph-authored quoting.
        "Quote.Grid"           => NodeMeta { label: "Grid quotes",         summary: "Symmetric bid/ask grid around a mid (step + levels + size)", group: "Quotes" },
        "Quote.Mux"            => NodeMeta { label: "Quote mux",           summary: "Pick quote bundle a or b by a boolean selector", group: "Quotes" },

        // Phase 4 composite strategies — engine runs the real
        // Rust implementation, output feeds into the graph.
        "Strategy.Avellaneda"  => NodeMeta { label: "Avellaneda-Stoikov",  summary: "Classic optimal MM (γ, κ, σ, T)", group: "Strategies" },
        "Strategy.GLFT"        => NodeMeta { label: "GLFT",                summary: "Guéant-Lehalle-Fernandez-Tapia quoting", group: "Strategies" },
        "Strategy.Grid"        => NodeMeta { label: "Grid",                summary: "Symmetric grid around mid (engine-config driven)", group: "Strategies" },
        "Strategy.Basis"       => NodeMeta { label: "Basis",               summary: "Basis-shifted reservation price (spot + ref)", group: "Strategies" },
        "Strategy.CrossExchange"=>NodeMeta { label: "Cross-exchange",      summary: "Make on venue A, hedge on venue B", group: "Strategies" },
        "Strategy.BasisArb"    => NodeMeta { label: "Basis arb (spot/perp)",summary: "Maker-post basis carry across venues with net-delta guard", group: "Strategies" },
        "Strategy.Spoof"       => NodeMeta { label: "Spoof (pentest)",     summary: "⚠ RESTRICTED — large fake order pulled on tick N+1 while real opposite-side captures reaction", group: "Exploit" },
        "Strategy.Wash"        => NodeMeta { label: "Wash (pentest)",      summary: "⚠ RESTRICTED — buy + sell at the same price every tick (self-trade)", group: "Exploit" },
        "Strategy.Ignite"      => NodeMeta { label: "Ignite (pentest)",    summary: "⚠ RESTRICTED — burst cross-through orders for N ticks, rest M, repeat", group: "Exploit" },

        // Epic R — surveillance detectors (safe, defaults-on)
        "Surveillance.SpoofingScore" => NodeMeta { label: "Spoofing score", summary: "Likelihood our own flow looks like spoofing [0..1] + cancel_ratio + lifetime", group: "Surveillance" },
        "Surveillance.LayeringScore" => NodeMeta { label: "Layering score", summary: "Multi-order structured pressure + synchronous cancels [0..1]", group: "Surveillance" },
        "Surveillance.QuoteStuffingScore" => NodeMeta { label: "Quote stuffing score", summary: "High orders/sec + high cancel ratio + near-zero fill rate [0..1]", group: "Surveillance" },
        "Surveillance.WashScore" => NodeMeta { label: "Wash score",        summary: "Self-trade detection (own buy + own sell same price, short window) [0..1]", group: "Surveillance" },
        "Surveillance.MomentumIgnitionScore" => NodeMeta { label: "Momentum ignition score", summary: "Public-tape burst + aggressor dominance + price move [0..1]", group: "Surveillance" },
        "Surveillance.FakeLiquidityScore" => NodeMeta { label: "Fake liquidity score", summary: "L2 levels evaporating within bps-band of mid — the book that disappears on approach", group: "Surveillance" },

        // Sinks — always fire on a trigger, consumed by the engine.
        "Out.SpreadMult"       => NodeMeta { label: "Spread multiplier",   summary: "Final spread scalar applied to quotes", group: "Sinks" },
        "Out.SizeMult"         => NodeMeta { label: "Size multiplier",     summary: "Final size scalar applied to quotes", group: "Sinks" },
        "Out.KillEscalate"     => NodeMeta { label: "Kill-switch escalate",summary: "Raise kill level with a reason", group: "Sinks" },
        "Out.Flatten"          => NodeMeta { label: "Flatten position",    summary: "Fire L4 flatten with the given exec policy", group: "Sinks" },
        "Out.Quotes"           => NodeMeta { label: "Quotes",              summary: "Replace strategy output with a graph-authored quote bundle", group: "Sinks" },
        "Out.VenueQuotes"      => NodeMeta { label: "Venue quotes",        summary: "Multi-venue quote bundle — each entry names its own venue/symbol/product", group: "Sinks" },
        "Out.AtomicBundle"     => NodeMeta { label: "Atomic bundle",       summary: "Maker + hedge pair — both legs fill or both roll back within timeout_ms", group: "Sinks" },

        // Defensive fallback — every catalog kind should have its own
        // arm above. If a new kind sneaks in without a meta entry,
        // the kinds-snapshot test `every_kind_has_nontrivial_meta`
        // catches it in CI.
        _ => NodeMeta { label: "Unknown", summary: "", group: "Misc" },
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
        "Book.L2",
        "Trade.Tape",
        "Balance",
        "Funding",
        "Portfolio.NetDelta",
        "Portfolio.QuoteAvailable",
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
        "Quote.Grid",
        "Quote.Mux",
        "Out.Quotes",
        "Out.VenueQuotes",
        "Out.AtomicBundle",
        "Strategy.Avellaneda",
        "Strategy.GLFT",
        "Strategy.Grid",
        "Strategy.Basis",
        "Strategy.CrossExchange",
        "Strategy.BasisArb",
        "Strategy.Spoof",
        "Strategy.Wash",
        "Strategy.Ignite",
        "Surveillance.SpoofingScore",
        "Surveillance.LayeringScore",
        "Surveillance.QuoteStuffingScore",
        "Surveillance.WashScore",
        "Surveillance.MomentumIgnitionScore",
        "Surveillance.FakeLiquidityScore",
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
    fn every_kind_has_nontrivial_meta() {
        for (kind, _) in kinds() {
            let m = meta(kind);
            assert!(
                !m.label.is_empty() && m.label != "Unknown",
                "kind {kind} is missing a human label in catalog::meta"
            );
            assert!(
                !m.group.is_empty() && m.group != "Misc",
                "kind {kind} is missing a palette group in catalog::meta"
            );
        }
    }

    #[test]
    fn catalog_has_69_nodes_after_epic_r_week_5_fake_liquidity() {
        // 68 after Week 4 + Surveillance.FakeLiquidityScore = 69.
        assert_eq!(kinds().len(), 69, "catalog drift");
    }
}
