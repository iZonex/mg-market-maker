use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_common::types::{Fill, PriceLevel, Side};
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_exchange_core::events::MarketEvent;
use mm_strategy::AvellanedaStoikov;
use mm_strategy_graph::{Edge, Graph, GraphNode, NodeId, PortRef, Scope};
use uuid::Uuid;

fn sample_config() -> AppConfig {
    AppConfig::default()
}

fn sample_product(symbol: &str) -> ProductSpec {
    ProductSpec {
        symbol: symbol.to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.0001),
        min_notional: dec!(10),
        maker_fee: dec!(0.0001),
        taker_fee: dec!(0.0005),
        trading_status: Default::default(),
    }
}

fn build_engine() -> MarketMakerEngine {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let bundle = ConnectorBundle::single(primary);
    MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        sample_config(),
        sample_product("BTCUSDT"),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    )
}

fn feed_snapshot(
    engine: &mut MarketMakerEngine,
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
) {
    engine.handle_ws_event(MarketEvent::BookSnapshot {
        venue: VenueId::Binance,
        symbol: "BTCUSDT".into(),
        bids: bids
            .into_iter()
            .map(|(price, qty)| PriceLevel { price, qty })
            .collect(),
        asks: asks
            .into_iter()
            .map(|(price, qty)| PriceLevel { price, qty })
            .collect(),
        sequence: 1,
    });
}

fn sink_graph(source_kind: &str, source_port: &str, source_cfg: serde_json::Value) -> Graph {
    let src = NodeId::new();
    let sink = NodeId::new();
    let mut g = Graph::empty("t", Scope::Symbol("BTCUSDT".into()));
    g.nodes.push(GraphNode {
        id: src,
        kind: source_kind.into(),
        config: source_cfg,
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: sink,
        kind: "Out.SpreadMult".into(),
        config: serde_json::Value::Null,
        pos: (0.0, 0.0),
    });
    g.edges.push(Edge {
        from: PortRef {
            node: src,
            port: source_port.into(),
        },
        to: PortRef {
            node: sink,
            port: "mult".into(),
        },
    });
    g
}

/// Cost.Sweep against an empty book emits `Missing` on every
/// port; SpreadMult sink doesn't fire; multiplier stays at
/// its `1.0` default.
#[test]
fn cost_sweep_on_empty_book_leaves_spread_mult_default() {
    let mut engine = build_engine();
    let g = sink_graph(
        "Cost.Sweep",
        "impact_bps",
        serde_json::json!({"side": "buy", "size": "1"}),
    );
    engine.swap_strategy_graph(&g).expect("compiles");
    engine.tick_strategy_graph();
    assert_eq!(engine.auto_tuner.graph_spread_mult(), dec!(1));
}

/// Cost.Sweep against a stocked book emits `impact_bps` as a
/// number; SpreadMult sink picks it up. A 3-BTC buy against
/// thin asks at 50_100 / 50_200 vs mid ~50_050 produces
/// impact well north of 10 bps, so the floored multiplier
/// lands above the default 1.0.
#[test]
fn cost_sweep_on_stocked_book_propagates_impact_bps() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5)), (dec!(49_900), dec!(10))],
        vec![(dec!(50_100), dec!(2)), (dec!(50_200), dec!(5))],
    );
    let g = sink_graph(
        "Cost.Sweep",
        "impact_bps",
        serde_json::json!({"side": "buy", "size": "3"}),
    );
    engine.swap_strategy_graph(&g).expect("compiles");
    engine.tick_strategy_graph();
    let mult = engine.auto_tuner.graph_spread_mult();
    assert!(
        mult > dec!(1),
        "Cost.Sweep impact_bps should propagate past SpreadMult floor, got {mult}"
    );
}

/// Risk.UnrealizedIfFlatten with zero inventory emits
/// `Missing`; the sink stays silent.
#[test]
fn risk_unrealized_without_inventory_leaves_spread_mult_default() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5))],
        vec![(dec!(50_100), dec!(5))],
    );
    let g = sink_graph("Risk.UnrealizedIfFlatten", "value", serde_json::Value::Null);
    engine.swap_strategy_graph(&g).expect("compiles");
    engine.tick_strategy_graph();
    assert_eq!(engine.auto_tuner.graph_spread_mult(), dec!(1));
}

/// Risk.UnrealizedIfFlatten with a profitable long position
/// emits a positive number (quote-asset PnL). A long of
/// 1 BTC at cost 49_500 against a 50_000-bid book produces
/// a flatten VWAP near 50_000 → PnL ≈ +500 USDT. SpreadMult
/// floors at 1 but preserves numbers ≥ 1, and 500 ≫ 1, so
/// the multiplier moves off its default.
#[test]
fn risk_unrealized_with_profitable_long_propagates_pnl() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5)), (dec!(49_900), dec!(5))],
        vec![(dec!(50_100), dec!(5))],
    );
    engine.inventory_manager.on_fill(&Fill {
        trade_id: 1,
        order_id: Uuid::new_v4(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: dec!(49_500),
        qty: dec!(1),
        is_maker: true,
        timestamp: chrono::Utc::now(),
    });
    let g = sink_graph("Risk.UnrealizedIfFlatten", "value", serde_json::Value::Null);
    engine.swap_strategy_graph(&g).expect("compiles");
    engine.tick_strategy_graph();
    let mult = engine.auto_tuner.graph_spread_mult();
    assert!(
        mult > dec!(1),
        "profitable-long flatten PnL should propagate to SpreadMult, got {mult}"
    );
}

/// Build a Phase IV reactive-XEMM style graph end-to-end:
///
///   Trade.OwnFill ──fired──┐
///                ──side────┤
///                ──qty ────┼──► Quote.Hedge ──quotes──► Out.VenueQuotesIf
///                ──price ──┤                            ▲
///                          └────────────────────────────┘ (trigger = fired)
///
///   + baseline Math.Const → Out.SpreadMult (validator
///     requires a SpreadMult sink).
///
/// The hedge node is configured with empty venue/symbol so
/// it emits plain `Value::Quotes` targeting the test engine's
/// own venue — the sink therefore populates
/// `graph_quotes_override` (the self-venue path) instead of
/// calling out to `dashboard.send_config_override` (which the
/// test harness doesn't wire).
fn build_xemm_reactive_graph(hedge_cross_bps: &str) -> Graph {
    let own_fill = NodeId::new();
    let hedge = NodeId::new();
    let sink = NodeId::new();
    let const_node = NodeId::new();
    let baseline_sink = NodeId::new();
    let mut g = Graph::empty("xemm-reactive-test", Scope::Symbol("BTCUSDT".into()));
    g.nodes.push(GraphNode {
        id: own_fill,
        kind: "Trade.OwnFill".into(),
        config: serde_json::json!({
            "venue": "",          // match any venue (MockConnector = binance)
            "symbol": "",
            "role": "any",
            "side_filter": "any",
        }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: hedge,
        kind: "Quote.Hedge".into(),
        config: serde_json::json!({
            // Empty hedge venue → emits plain Quotes targeting
            // this engine's venue so graph_quotes_override
            // captures the hedge (no dashboard needed).
            "hedge_venue": "",
            "hedge_symbol": "",
            "hedge_product": "",
            "cross_bps": hedge_cross_bps,
        }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: sink,
        kind: "Out.VenueQuotesIf".into(),
        config: serde_json::Value::Null,
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: const_node,
        kind: "Math.Const".into(),
        config: serde_json::json!({ "value": "1" }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: baseline_sink,
        kind: "Out.SpreadMult".into(),
        config: serde_json::Value::Null,
        pos: (0.0, 0.0),
    });
    // fill payload → hedge inputs
    for (port_out, port_in) in [
        ("fired", "fired"),
        ("side", "side"),
        ("qty", "qty"),
        ("price", "price"),
    ] {
        g.edges.push(Edge {
            from: PortRef {
                node: own_fill,
                port: port_out.into(),
            },
            to: PortRef {
                node: hedge,
                port: port_in.into(),
            },
        });
    }
    // hedge.quotes → sink.quotes
    g.edges.push(Edge {
        from: PortRef {
            node: hedge,
            port: "quotes".into(),
        },
        to: PortRef {
            node: sink,
            port: "quotes".into(),
        },
    });
    // fired → sink.trigger
    g.edges.push(Edge {
        from: PortRef {
            node: own_fill,
            port: "fired".into(),
        },
        to: PortRef {
            node: sink,
            port: "trigger".into(),
        },
    });
    // baseline sink
    g.edges.push(Edge {
        from: PortRef {
            node: const_node,
            port: "value".into(),
        },
        to: PortRef {
            node: baseline_sink,
            port: "mult".into(),
        },
    });
    g
}

/// Without a fill event, the `Trade.OwnFill` source emits
/// `fired=false`, `Out.VenueQuotesIf` suppresses the sink,
/// and `graph_quotes_override` stays `None`. Proves the gate
/// fails closed — a graph deployed without fills never
/// places a phantom hedge.
#[test]
fn xemm_reactive_without_fill_emits_no_hedge() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5))],
        vec![(dec!(50_100), dec!(5))],
    );
    let g = build_xemm_reactive_graph("10");
    engine.swap_strategy_graph(&g).expect("compiles");
    engine.tick_strategy_graph();
    assert!(
        engine.graph_quotes_override.is_none(),
        "no fill → VenueQuotesIf must not fire; got override={:?}",
        engine.graph_quotes_override
    );
}

/// With a primary-venue fill, the full chain fires:
///   Fill(Buy, 0.01 @ 50_000) →
///     Trade.OwnFill(fired=true, side=+1, qty=0.01, price=50_000)
///     → Quote.Hedge(side=Sell, qty=0.01, price=50_000 − 10bps)
///     → Out.VenueQuotesIf(trigger=true) → SinkAction::Quotes
///     → graph_quotes_override populated with one ask.
///
/// Proves Phase IV graph-native reactive XEMM works
/// end-to-end: source overlay + conditional sink + hedge-leg
/// transform all compose to produce exactly one counter-side
/// quote at the expected crossed price.
#[test]
fn xemm_reactive_fill_produces_opposite_hedge() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5))],
        vec![(dec!(50_100), dec!(5))],
    );
    let g = build_xemm_reactive_graph("10");
    engine.swap_strategy_graph(&g).expect("compiles");

    // Inject an own-fill via the same path the live engine
    // uses. The `handle_ws_event(Fill)` handler pushes into
    // `pending_own_fills` which the graph source drains at
    // tick time.
    let fill = Fill {
        trade_id: 1,
        order_id: Uuid::new_v4(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: dec!(50_000),
        qty: dec!(0.01),
        is_maker: true,
        timestamp: chrono::Utc::now(),
    };
    engine.handle_ws_event(MarketEvent::Fill {
        venue: VenueId::Binance,
        fill,
    });
    assert_eq!(
        engine.pending_own_fills.len(),
        1,
        "fill must land in the pending buffer"
    );

    engine.tick_strategy_graph();

    // Drain happened — buffer is empty.
    assert!(
        engine.pending_own_fills.is_empty(),
        "tick_strategy_graph must drain pending_own_fills"
    );

    // Hedge fired — graph_quotes_override carries one ask
    // (opposite side of the Buy fill) at crossed price.
    let pairs = engine
        .graph_quotes_override
        .as_ref()
        .expect("hedge must fire on fill");
    assert_eq!(pairs.len(), 1);
    let ask = pairs[0]
        .ask
        .as_ref()
        .expect("hedge must be a Sell (opposite of Buy fill)");
    assert!(
        pairs[0].bid.is_none(),
        "buy-fill hedge must be sell-only, got bid {:?}",
        pairs[0].bid
    );
    assert_eq!(ask.qty, dec!(0.01));
    // Price = 50_000 − 10bps = 50_000 − 50 = 49_950, then
    // rounded to the product's tick_size (0.01) — identity
    // on this value.
    assert_eq!(ask.price, dec!(49_950));
}

/// Fill on a symbol that doesn't match the `Trade.OwnFill`
/// filter (symbol = "ETHUSDT") does NOT fire the hedge —
/// per-node filter works.
#[test]
fn xemm_reactive_symbol_filter_suppresses_unmatched_fills() {
    let mut engine = build_engine();
    feed_snapshot(
        &mut engine,
        vec![(dec!(50_000), dec!(5))],
        vec![(dec!(50_100), dec!(5))],
    );
    let own_fill = NodeId::new();
    let hedge = NodeId::new();
    let sink = NodeId::new();
    let const_node = NodeId::new();
    let baseline_sink = NodeId::new();
    let mut g = Graph::empty("xemm-filtered-test", Scope::Symbol("BTCUSDT".into()));
    g.nodes.push(GraphNode {
        id: own_fill,
        kind: "Trade.OwnFill".into(),
        // Filter wants ETH fills only — the BTC fill below
        // must be skipped.
        config: serde_json::json!({
            "venue": "",
            "symbol": "ETHUSDT",
            "role": "any",
            "side_filter": "any",
        }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: hedge,
        kind: "Quote.Hedge".into(),
        config: serde_json::json!({
            "hedge_venue": "",
            "hedge_symbol": "",
            "hedge_product": "",
            "cross_bps": "10",
        }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: sink,
        kind: "Out.VenueQuotesIf".into(),
        config: serde_json::Value::Null,
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: const_node,
        kind: "Math.Const".into(),
        config: serde_json::json!({ "value": "1" }),
        pos: (0.0, 0.0),
    });
    g.nodes.push(GraphNode {
        id: baseline_sink,
        kind: "Out.SpreadMult".into(),
        config: serde_json::Value::Null,
        pos: (0.0, 0.0),
    });
    for (port_out, port_in) in [
        ("fired", "fired"),
        ("side", "side"),
        ("qty", "qty"),
        ("price", "price"),
    ] {
        g.edges.push(Edge {
            from: PortRef {
                node: own_fill,
                port: port_out.into(),
            },
            to: PortRef {
                node: hedge,
                port: port_in.into(),
            },
        });
    }
    g.edges.push(Edge {
        from: PortRef {
            node: hedge,
            port: "quotes".into(),
        },
        to: PortRef {
            node: sink,
            port: "quotes".into(),
        },
    });
    g.edges.push(Edge {
        from: PortRef {
            node: own_fill,
            port: "fired".into(),
        },
        to: PortRef {
            node: sink,
            port: "trigger".into(),
        },
    });
    g.edges.push(Edge {
        from: PortRef {
            node: const_node,
            port: "value".into(),
        },
        to: PortRef {
            node: baseline_sink,
            port: "mult".into(),
        },
    });
    engine.swap_strategy_graph(&g).expect("compiles");

    engine.handle_ws_event(MarketEvent::Fill {
        venue: VenueId::Binance,
        fill: Fill {
            trade_id: 1,
            order_id: Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(50_000),
            qty: dec!(0.01),
            is_maker: true,
            timestamp: chrono::Utc::now(),
        },
    });
    engine.tick_strategy_graph();

    assert!(
        engine.graph_quotes_override.is_none(),
        "symbol filter must suppress the hedge"
    );
}
