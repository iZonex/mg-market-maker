/// Kinds that are **intentionally** not matched by a
/// `"Kind" =>` arm in `tick_strategy_graph`. Each entry
/// needs a one-line rationale so a future reader knows
/// whether the skip is valid.
const EXEMPT: &[(&str, &str)] = &[
        // ── Pure graph-internal nodes: evaluate from inputs,
        //    no engine state required. No arm by design.
        ("Math.Add", "math op — evaluated by graph"),
        ("Math.Mul", "math op — evaluated by graph"),
        ("Math.Const", "constant — evaluated by graph"),
        ("Stats.EWMA", "stateless EWMA — evaluated by graph"),
        ("Cast.ToBool", "bool cast — evaluated by graph"),
        ("Cast.StrategyEq", "tag cmp — evaluated by graph"),
        ("Cast.PairClassEq", "tag cmp — evaluated by graph"),
        ("Logic.And", "boolean AND — evaluated by graph"),
        ("Logic.Mux", "switch — evaluated by graph"),
        ("Logic.StringMux", "string switch — evaluated by graph"),
        ("Indicator.SMA", "stateful — evaluated by graph from `x` input"),
        ("Indicator.EMA", "stateful — evaluated by graph from `x` input"),
        ("Indicator.HMA", "stateful — evaluated by graph from `x` input"),
        ("Indicator.RSI", "stateful — evaluated by graph from `x` input"),
        ("Indicator.ATR", "stateful — evaluated by graph from high/low/close"),
        ("Indicator.Bollinger", "stateful — evaluated by graph from `x` input"),
        ("Exec.TwapConfig", "policy emitter — evaluated by graph"),
        ("Exec.VwapConfig", "policy emitter — evaluated by graph"),
        ("Exec.PovConfig", "policy emitter — evaluated by graph"),
        ("Exec.IcebergConfig", "policy emitter — evaluated by graph"),
        ("Quote.Grid", "quote combinator — evaluated by graph"),
        ("Quote.Mux", "quote switch — evaluated by graph"),
        ("Plan.Accumulate", "plan FSM — evaluated by graph"),
        ("Strategy.QueueAware", "scaler — evaluated by graph from its inputs"),
        // ── Strategy.* source nodes use a separate
        //    per-node pool + wildcard overlay (see the pool
        //    construction at ~1640 and the Strategy.*
        //    lookup at ~3466). Not a by-name match arm in
        //    `tick_strategy_graph`, but they ARE engine-wired.
        ("Strategy.Avellaneda", "strategy pool wildcard lookup"),
        ("Strategy.GLFT", "strategy pool wildcard lookup"),
        ("Strategy.Grid", "strategy pool wildcard lookup"),
        ("Strategy.Basis", "strategy pool wildcard lookup"),
        ("Strategy.CrossExchange", "strategy pool wildcard lookup"),
        ("Strategy.BasisArb", "strategy pool wildcard lookup"),
        // Pentest strategies — same pool-backed pattern.
        ("Strategy.Spoof", "pentest — pool-backed"),
        ("Strategy.Wash", "pentest — pool-backed"),
        ("Strategy.Ignite", "pentest — pool-backed"),
        ("Strategy.Mark", "pentest — pool-backed"),
        ("Strategy.Layer", "pentest — pool-backed"),
        ("Strategy.Stuff", "pentest — pool-backed"),
        ("Strategy.CrossMarket", "pentest — pool-backed"),
        ("Strategy.LatencyHunt", "pentest — pool-backed"),
        ("Strategy.RebateFarm", "pentest — pool-backed"),
        ("Strategy.Imbalance", "pentest — pool-backed"),
        ("Strategy.ReactCancel", "pentest — pool-backed"),
        ("Strategy.OneSided", "pentest — pool-backed"),
        ("Strategy.InvPush", "pentest — pool-backed"),
        ("Strategy.NonFill", "pentest — pool-backed"),
        ("Strategy.CascadeHunter", "pentest — pool-backed"),
        ("Strategy.BasketPush", "pentest — direct overlay (not pool-backed) — parses basket config + emits VenueQuotes legs"),
        ("Strategy.PumpAndDump", "pentest — pool-backed"),
        ("Strategy.LeverageBuilder", "pentest — pool-backed"),
        ("Strategy.LiquidationHunt", "pentest — pool-backed"),
        ("Strategy.CampaignOrchestrator", "pentest — pool-backed, real FSM in mm-strategy"),
        // ── Sinks: fire via `SinkAction` harvest in evaluator,
        //    not the source overlay loop.
        ("Out.SpreadMult", "sink — SinkAction::SpreadMult"),
        ("Out.SizeMult", "sink — SinkAction::SizeMult"),
        ("Out.KillEscalate", "sink — SinkAction::KillEscalate"),
        ("Out.Flatten", "sink — SinkAction::Flatten"),
        ("Out.Quotes", "sink — SinkAction::Quotes"),
        ("Out.VenueQuotes", "sink — SinkAction::VenueQuotes"),
        (
            "Out.VenueQuotesIf",
            "sink — SinkAction::VenueQuotes (gated) or ::Quotes when input is plain Quotes",
        ),
        ("Out.AtomicBundle", "sink — SinkAction::AtomicBundle"),
    ];

#[test]
fn every_catalog_kind_is_wired_or_explicitly_exempt() {
    let engine_src: &str = include_str!("../market_maker.rs");
    let exempt: std::collections::HashSet<&str> = EXEMPT.iter().map(|(k, _)| *k).collect();

    let mut missing: Vec<String> = Vec::new();
    for (kind, shape) in mm_strategy_graph::catalog::kinds() {
        if exempt.contains(kind) {
            continue;
        }
        // Nodes that declare input ports compute their
        // output from those inputs via `evaluate()` — the
        // engine doesn't need a source-overlay arm for
        // them (Risk.ToxicityWiden / Risk.InventoryUrgency
        // / Risk.CircuitBreaker are the canonical case).
        if !shape.inputs.is_empty() {
            continue;
        }
        // Source overlay patterns the engine uses:
        //   `"Kind" =>`                (solo arm)
        //   `"Kind" | "OtherKind" =>`  (compound arm, left)
        //   `| "Kind" =>`              (compound arm, right)
        //   `| "Kind" |`               (compound arm, middle)
        // Any of them counts as "wired".
        let solo = format!("\"{kind}\" =>");
        let left = format!("\"{kind}\" |");
        let right = format!("| \"{kind}\" =>");
        let mid = format!("| \"{kind}\" |");
        let wired = engine_src.contains(&solo)
            || engine_src.contains(&left)
            || engine_src.contains(&right)
            || engine_src.contains(&mid);
        if !wired {
            missing.push(kind.to_string());
        }
    }

    assert!(
        missing.is_empty(),
        "catalog kinds with no engine arm (and no exemption): {missing:?}. \
             Either add a match arm in `tick_strategy_graph` or document the \
             skip in `graph_catalog_coverage::EXEMPT` with a one-line reason."
    );
}

/// Companion guard — any EXEMPT entry that doesn't
/// match a current catalog kind is probably a rename
/// drift. Fail so the next audit notices.
#[test]
fn every_exempt_kind_still_exists_in_catalog() {
    let live: std::collections::HashSet<&str> = mm_strategy_graph::catalog::kinds()
        .iter()
        .map(|(k, _)| *k)
        .collect();
    let stale: Vec<&&str> = EXEMPT
        .iter()
        .map(|(k, _)| k)
        .filter(|k| !live.contains(*k))
        .collect();
    assert!(
        stale.is_empty(),
        "EXEMPT entries whose catalog kind no longer exists: {stale:?}"
    );
}
