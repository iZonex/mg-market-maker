//! Template catalog — enumerated list of strategy templates the
//! controller can advise operators to deploy.
//!
//! First cut: hardcoded list mirroring the filenames in
//! `crates/strategy-graph/templates/*.json`. The entries carry a
//! short human-readable description and a variables-hint so the
//! deploy UI can pre-fill the JSON editor with a reasonable
//! starting shape.
//!
//! Future: the strategy-graph crate exposes a manifest that
//! enumerates templates + declares their required variables /
//! credential roles. Controller reads that manifest at startup
//! and serves the same `TemplateRow` shape from it. This
//! hardcoded catalog buys the UI an operator-facing list without
//! coupling the controller to the strategy-graph loader.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TemplateRow {
    /// Stable template name. Matches the filename stem in
    /// `crates/strategy-graph/templates/`.
    pub name: String,
    /// One-line description operators see in the dropdown.
    pub description: String,
    /// Category tag — "maker", "arb", "executor" — the UI can
    /// group dropdown options by.
    pub category: String,
    /// Starter `variables` JSON the UI pre-fills in the deploy
    /// dialog's variables editor. Operator edits in place.
    pub variables_hint: serde_json::Value,
    /// Wave F5 — operator-facing guidance for "when would I pick
    /// this?". 1-2 sentences, shown in the chooser as a hover
    /// or expandable tip so operators can triage without
    /// reading the strategy source.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recommended_for: String,
    /// Wave F5 — cautions / non-obvious gotchas that would bite
    /// an operator who deploys blind (needs hedge venue, requires
    /// testnet first, only safe for highly-liquid majors, etc.).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caveats: String,
    /// Wave F5 — how risky this template is, "low" / "medium" /
    /// "high". Drives a coloured chip in the chooser so a new
    /// operator can't pick `rug-detector-composite` for a stable
    /// major pair by accident.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub risk_band: String,
}

/// Hand-rolled catalog. Keep entries sorted by category then
/// name so the UI dropdown renders stably.
pub fn catalog() -> Vec<TemplateRow> {
    vec![
        TemplateRow {
            name: "major-spot-basic".into(),
            description: "Plain spot maker — minimal config, sane defaults for majors.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "BTCUSDT / ETHUSDT on Binance, Bybit, OKX. Start here if you've never deployed a maker on this venue before.".into(),
            caveats: "Doesn't auto-tune spread — the baseline is conservative. Expect few fills on tight books until you widen order_size.".into(),
            risk_band: "low".into(),
        },
        TemplateRow {
            name: "avellaneda-via-graph".into(),
            description: "Avellaneda-Stoikov optimal market maker on a single spot venue.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>",
                "gamma": "0.01",
                "spread_bps": "4"
            }),
            recommended_for: "Liquid majors where you want inventory-aware reservation pricing — bigger γ skews you away from held side harder.".into(),
            caveats: "Needs a healthy volatility estimate (first ~60s samples). On thin books the skew dominates and you'll stand too wide; pair with grid instead.".into(),
            risk_band: "medium".into(),
        },
        TemplateRow {
            name: "glft-via-graph".into(),
            description: "GLFT (Guéant-Lehalle-Fernandez-Tapia) with online a/k calibration.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>",
                "inventory_target": "0"
            }),
            recommended_for: "Venue + symbol combos where fill rate matters and you can afford the first 5-10 minute calibration warmup.".into(),
            caveats: "Calibration walker writes a/k from observed fills; paper mode with no fills never calibrates. Use a live-liquid venue or seed with `a` / `k` constants.".into(),
            risk_band: "medium".into(),
        },
        TemplateRow {
            name: "grid-via-graph".into(),
            description: "Symmetric grid quoting on a single venue.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>",
                "levels": "5",
                "step_bps": "10"
            }),
            recommended_for: "Range-bound alts, book depth pools. Predictable inventory usage — useful for paper smoke runs where you want to see orders stacked.".into(),
            caveats: "Doesn't adapt to trend. In a strong move you'll accumulate the wrong side rapidly — always pair with a hard inventory cap.".into(),
            risk_band: "low".into(),
        },
        TemplateRow {
            name: "cost-gated-quoter".into(),
            description: "Quoter that halts when per-trade cost breaches a configured threshold.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>",
                "max_cost_bps": "3"
            }),
            recommended_for: "Fee-sensitive deploys where you can't tolerate quotes that only clear above your cost basis.".into(),
            caveats: "The cost model is maker-fee + borrow-if-short + expected adverse selection — tune max_cost_bps to your venue's actual fee tier.".into(),
            risk_band: "low".into(),
        },
        TemplateRow {
            name: "meme-spot-guarded".into(),
            description: "Spot maker with rug-pull guards active — for long-tail listings.".into(),
            category: "maker".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "New listings, long-tail alts. Composite rug detector kills quoting on pump + thin book + holder concentration signals.".into(),
            caveats: "Aggressive — will stop quoting on many legitimately volatile moves. Not suitable for majors; use major-spot-basic.".into(),
            risk_band: "high".into(),
        },
        TemplateRow {
            name: "cross-exchange-basic".into(),
            description: "Cross-venue maker: quote on A, hedge on B.".into(),
            category: "arb".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<quote_venue_credential_id>",
                "hedge_credential": "<hedge_venue_credential_id>"
            }),
            recommended_for: "Identical asset listed on two venues where maker rebate on A + taker cost on B leaves positive edge.".into(),
            caveats: "Hedge venue WS drop = stops quoting entirely (Wave A15 gate). Both venues' API keys must have trading scope; test on testnets of both first.".into(),
            risk_band: "medium".into(),
        },
        TemplateRow {
            name: "xemm-reactive".into(),
            description: "Dedicated cross-exchange market maker with slippage band.".into(),
            category: "arb".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<quote_venue_credential_id>",
                "hedge_credential": "<hedge_venue_credential_id>",
                "slippage_bps": "2"
            }),
            recommended_for: "Same as cross-exchange-basic but with an explicit slippage budget on the hedge leg — stops hedging if venue B book dies.".into(),
            caveats: "Slippage budget is per-hedge, not cumulative; a 30s run of bad fills can burn more than you realise. Monitor adverse-selection.".into(),
            risk_band: "medium".into(),
        },
        TemplateRow {
            name: "basis-carry-spot-perp".into(),
            description: "Basis-shifted quotes using spot mid + perp reference.".into(),
            category: "arb".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>",
                "ref_credential": "<ref_credential_id>"
            }),
            recommended_for: "Perpetual-spot pairs where basis regime produces exploitable skew in the quoting mid.".into(),
            caveats: "Funding-rate tail-risk: deep contango crunches can flip your PnL in minutes. Always cap max_basis_bps.".into(),
            risk_band: "high".into(),
        },
        TemplateRow {
            name: "funding-aware-quoter".into(),
            description: "Quoter that tilts based on imminent funding payments.".into(),
            category: "arb".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "Perp venues with predictable funding windows — tilts reservation price to collect funding on the held side.".into(),
            caveats: "Miscalibrated funding predictions = adverse fills. Run paper first for at least one funding epoch.".into(),
            risk_band: "high".into(),
        },
        TemplateRow {
            name: "cross-asset-regime".into(),
            description: "Quoter that switches regime based on correlated-asset state.".into(),
            category: "advanced".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "Pairs with strong leader-follower dynamics (e.g. BTC → alt correlation); switches to wider spread when leader moves.".into(),
            caveats: "Assumes the correlation holds — a breakdown (regime change) leaves you trading on stale signals. Monitor lead-lag guard.".into(),
            risk_band: "high".into(),
        },
        TemplateRow {
            name: "liquidity-burn-guard".into(),
            description: "Wrapper that kills quoting on liquidity exhaustion events.".into(),
            category: "advanced".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "Wrap around another quoter for long-tail symbols where book thins out fast in bad regimes.".into(),
            caveats: "Works ABOVE any base strategy — compose, don't replace. The guard is conservative and will pause quoting often.".into(),
            risk_band: "medium".into(),
        },
        TemplateRow {
            name: "rug-detector-composite".into(),
            description: "Composite rug-detector — holder concentration + CEX inflow + thin book.".into(),
            category: "advanced".into(),
            variables_hint: serde_json::json!({
                "primary_credential": "<credential_id>"
            }),
            recommended_for: "Long-tail listings — stops quoting on signals of imminent dump before PnL collapses.".into(),
            caveats: "Requires on-chain data feed (holder concentration). False-positive heavy — paired with manual override on the incident panel.".into(),
            risk_band: "high".into(),
        },
    ]
}
