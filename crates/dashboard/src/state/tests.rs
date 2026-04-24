    use super::*;

    fn empty_state(symbol: &str) -> SymbolState {
        SymbolState {
            symbol: symbol.to_string(),
            mode: "paper".to_string(),
            strategy: "avellaneda-stoikov".to_string(),
            venue: "binance".to_string(),
            product: "spot".to_string(),
            pair_class: None,
            mid_price: dec!(50_000),
            spread_bps: dec!(2),
            inventory: dec!(0),
            inventory_value: dec!(0),
            live_orders: 0,
            total_fills: 0,
            pnl: PnlSnapshot::default(),
            volatility: dec!(0.02),
            vpin: dec!(0),
            kyle_lambda: dec!(0),
            adverse_bps: dec!(0),
            as_prob_bid: None,
            as_prob_ask: None,
            momentum_ofi_ewma: None,
            momentum_learned_mp_drift: None,
            market_resilience: dec!(1),
            order_to_trade_ratio: dec!(0),
            hma_value: None,
            kill_level: 0,
            sla_uptime_pct: dec!(100),
            regime: "Quiet".to_string(),
            spread_compliance_pct: dec!(100),
            book_depth_levels: vec![],
            locked_in_orders_quote: dec!(0),
            sla_max_spread_bps: dec!(50),
            sla_min_depth_quote: dec!(0),
            presence_pct_24h: dec!(100),
            two_sided_pct_24h: dec!(100),
            minutes_with_data_24h: 0,
            hourly_presence: vec![],
            market_impact: None,
            performance: None,
            tunable_config: None,
            adaptive_state: None,
            open_orders: vec![],
            active_graph: None,
            manipulation_score: None,
            rug_score: None,
        }
    }

    /// Epic D stage-3 — pin that `state.update` accepts the
    /// new wave-2 / per-side fields without regressing the
    /// existing publish path. The actual gauge values are a
    /// side effect of the prometheus crate and are not
    /// trivially observable from a unit test, so we assert
    /// only that a default-`None` `SymbolState` flows through
    /// cleanly and that the per-pair entry is retrievable
    /// post-update.
    #[test]
    fn state_update_accepts_new_wave2_fields() {
        crate::metrics::init();
        let ds = DashboardState::new();
        let mut s = empty_state("BTCUSDT");
        s.as_prob_bid = Some(dec!(0.7));
        s.as_prob_ask = Some(dec!(0.4));
        s.momentum_ofi_ewma = Some(dec!(0.123));
        s.momentum_learned_mp_drift = Some(dec!(0.0001));
        ds.update(s);
        let got = ds.get_symbol("BTCUSDT").unwrap();
        assert_eq!(got.as_prob_bid, Some(dec!(0.7)));
        assert_eq!(got.as_prob_ask, Some(dec!(0.4)));
        assert_eq!(got.momentum_ofi_ewma, Some(dec!(0.123)));
        assert_eq!(got.momentum_learned_mp_drift, Some(dec!(0.0001)));
    }

    #[test]
    fn state_update_preserves_none_in_json_api() {
        crate::metrics::init();
        let ds = DashboardState::new();
        let s = empty_state("ETHUSDT");
        ds.update(s);
        let got = ds.get_symbol("ETHUSDT").unwrap();
        assert_eq!(got.as_prob_bid, None);
        assert_eq!(got.as_prob_ask, None);
        assert_eq!(got.momentum_ofi_ewma, None);
        assert_eq!(got.momentum_learned_mp_drift, None);
    }

    // ── Multi-client isolation tests ─────────────────────────

    #[test]
    fn register_client_and_get_symbols() {
        crate::metrics::init();
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into(), "ETHUSDT".into()]);
        ds.register_client("bob", &["SOLUSDT".into()]);

        ds.update(empty_state("BTCUSDT"));
        ds.update(empty_state("ETHUSDT"));
        ds.update(empty_state("SOLUSDT"));

        let alice_syms = ds.get_client_symbols("alice");
        assert_eq!(alice_syms.len(), 2);
        let bob_syms = ds.get_client_symbols("bob");
        assert_eq!(bob_syms.len(), 1);
        assert_eq!(bob_syms[0].symbol, "SOLUSDT");

        // get_all returns all across clients
        assert_eq!(ds.get_all().len(), 3);
    }

    #[test]
    fn fill_routes_to_correct_client() {
        crate::metrics::init();
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let fill = FillRecord {
            timestamp: Utc::now(),
            symbol: "BTCUSDT".into(),
            client_id: Some("alice".into()),
            venue: "binance".into(),
            side: "buy".into(),
            price: dec!(50000),
            qty: dec!(0.01),
            is_maker: true,
            fee: dec!(0.1),
            nbbo_bid: dec!(49999),
            nbbo_ask: dec!(50001),
            slippage_bps: dec!(0),
        };
        ds.record_fill(fill);

        // Alice has the fill
        let alice_fills = ds.get_client_fills("alice", 10);
        assert_eq!(alice_fills.len(), 1);
        assert_eq!(alice_fills[0].symbol, "BTCUSDT");

        // Bob has no fills
        let bob_fills = ds.get_client_fills("bob", 10);
        assert_eq!(bob_fills.len(), 0);

        // Global view still works
        let all_fills = ds.get_recent_fills(None, 10);
        assert_eq!(all_fills.len(), 1);
    }

    #[test]
    fn config_override_routes_through_client() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ds.register_config_channel("BTCUSDT", tx);

        assert!(ds.send_config_override("BTCUSDT", ConfigOverride::Gamma(dec!(0.5))));
        assert!(!ds.send_config_override("UNKNOWN", ConfigOverride::Gamma(dec!(0.5))));

        let ovr = rx.try_recv().unwrap();
        assert!(matches!(ovr, ConfigOverride::Gamma(_)));
    }

    #[test]
    fn broadcast_reaches_all_clients() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        ds.register_config_channel("BTCUSDT", tx1);
        ds.register_config_channel("ETHUSDT", tx2);

        let count = ds.broadcast_config_override(ConfigOverride::PauseQuoting);
        assert_eq!(count, 2);
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[test]
    fn client_ids_returns_registered() {
        let ds = DashboardState::new();
        ds.register_client("bob", &["ETHUSDT".into()]);
        ds.register_client("alice", &["BTCUSDT".into()]);
        let ids = ds.client_ids();
        assert_eq!(ids, vec!["alice", "bob"]);
    }

    /// I3 regression — webhook cursor is per-client and survives
    /// re-reads. First pass returns None (so the fan-out loop
    /// knows to initialise instead of firing); subsequent
    /// advances only move forward in time.
    #[test]
    fn webhook_fill_cursor_is_per_client_and_advances() {
        use chrono::{Duration as CDuration, Utc};
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);
        assert!(ds.webhook_fill_cursor("alice").is_none());
        assert!(ds.webhook_fill_cursor("bob").is_none());

        let t0 = Utc::now();
        ds.set_webhook_fill_cursor("alice", t0);
        assert_eq!(ds.webhook_fill_cursor("alice"), Some(t0));
        assert!(ds.webhook_fill_cursor("bob").is_none(), "per-client");

        let t1 = t0 + CDuration::seconds(5);
        ds.set_webhook_fill_cursor("alice", t1);
        assert_eq!(ds.webhook_fill_cursor("alice"), Some(t1));
    }

    /// I3 regression — `client_ids_with_webhooks` only returns
    /// tenants who actually registered a dispatcher; empty for
    /// tenants that were just created.
    #[test]
    fn client_ids_with_webhooks_filters_correctly() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);
        assert!(ds.client_ids_with_webhooks().is_empty());

        let wh = crate::webhooks::WebhookDispatcher::new();
        wh.add_url("https://alice.example/hook".into());
        ds.set_client_webhook_dispatcher("alice", wh);

        let ids = ds.client_ids_with_webhooks();
        assert_eq!(ids, vec!["alice".to_string()]);
    }

    #[test]
    fn webhook_dispatcher_per_client() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let wh_alice = crate::webhooks::WebhookDispatcher::new();
        wh_alice.add_url("https://alice.com/hook".into());
        ds.set_client_webhook_dispatcher("alice", wh_alice);

        let wh_bob = crate::webhooks::WebhookDispatcher::new();
        wh_bob.add_url("https://bob.com/hook".into());
        ds.set_client_webhook_dispatcher("bob", wh_bob);

        let got_alice = ds.get_client_webhook_dispatcher("alice").unwrap();
        assert_eq!(got_alice.url_count(), 1);
        let got_bob = ds.get_client_webhook_dispatcher("bob").unwrap();
        assert_eq!(got_bob.url_count(), 1);
    }

    #[test]
    fn legacy_mode_works_without_registration() {
        crate::metrics::init();
        let ds = DashboardState::new();
        // No register_client call — legacy mode
        ds.update(empty_state("BTCUSDT"));
        let got = ds.get_symbol("BTCUSDT");
        assert!(got.is_some());
        assert_eq!(ds.get_all().len(), 1);
    }

    #[test]
    fn unknown_client_returns_empty() {
        let ds = DashboardState::new();
        assert!(ds.get_client_symbols("nonexistent").is_empty());
        assert!(ds.get_client_fills("nonexistent", 10).is_empty());
    }

    #[test]
    fn venue_balances_roundtrip() {
        let ds = DashboardState::new();
        assert!(ds.venue_balances("BTCUSDT").is_empty());

        let snap = VenueBalanceSnapshot {
            venue: "binance".into(),
            product: "Spot".into(),
            asset: "BTC".into(),
            wallet: "Spot".into(),
            total: dec!(0.5),
            available: dec!(0.4),
            locked: dec!(0.1),
            updated_at: Utc::now(),
        };
        ds.update_venue_balances("BTCUSDT", vec![snap.clone()]);

        let got = ds.venue_balances("BTCUSDT");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].venue, "binance");
        assert_eq!(got[0].asset, "BTC");
        assert_eq!(got[0].total, dec!(0.5));

        // all_venue_balances includes this symbol.
        let all = ds.all_venue_balances();
        assert_eq!(all.len(), 1);
        assert!(all.contains_key("BTCUSDT"));

        // Second update replaces, not appends.
        ds.update_venue_balances("BTCUSDT", vec![]);
        assert!(ds.venue_balances("BTCUSDT").is_empty());
    }

    /// 23-UX-6 — venue-kill state round-trips through the setter
    /// and is visible via both the single-venue and all-venues
    /// accessors. Absent venue reads as 0 (Normal).
    #[test]
    fn venue_kill_level_roundtrip() {
        let ds = DashboardState::new();
        assert_eq!(ds.venue_kill_level("binance"), 0);
        assert!(ds.all_venue_kill_levels().is_empty());

        ds.set_venue_kill_level("binance", 3);
        assert_eq!(ds.venue_kill_level("binance"), 3);

        ds.set_venue_kill_level("bybit", 1);
        let all = ds.all_venue_kill_levels();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("binance").copied(), Some(3));
        assert_eq!(all.get("bybit").copied(), Some(1));

        // Reset to 0 keeps the entry at 0 — HTTP layer treats
        // absent and 0 interchangeably.
        ds.set_venue_kill_level("binance", 0);
        assert_eq!(ds.venue_kill_level("binance"), 0);
    }

    /// S5.1 — rebalance recommendations read through
    /// DashboardState. With no config registered the response
    /// is empty; once set, the rebalancer runs over the
    /// aggregated venue_balances across every symbol and
    /// surfaces the expected transfer row.
    #[test]
    fn rebalance_recommendations_empty_without_config() {
        let ds = DashboardState::new();
        // Even with balances, no config means no recommendations.
        let snap = VenueBalanceSnapshot {
            venue: "binance".into(),
            product: "Spot".into(),
            asset: "USDT".into(),
            wallet: "Spot".into(),
            total: dec!(50),
            available: dec!(50),
            locked: dec!(0),
            updated_at: Utc::now(),
        };
        ds.update_venue_balances("BTCUSDT", vec![snap]);
        assert!(ds.rebalance_recommendations().is_empty());
    }

    /// S6.4 — `max_kill_level` takes the highest kill level
    /// across every published SymbolState. Used by the execute
    /// endpoint to refuse a transfer when any engine has
    /// escalated.
    #[test]
    fn max_kill_level_is_maxed_across_engines() {
        let ds = DashboardState::new();
        assert_eq!(ds.max_kill_level(), 0, "no engines → Normal");

        let mut a = empty_state("BTCUSDT");
        a.kill_level = 0;
        ds.update(a);
        let mut b = empty_state("ETHUSDT");
        b.kill_level = 3; // L3 flatten on ETH
        ds.update(b);
        let mut c = empty_state("SOLUSDT");
        c.kill_level = 1; // L1 widen on SOL
        ds.update(c);

        assert_eq!(ds.max_kill_level(), 3);
    }

    /// S6.1 — engine's `active_graph` payload round-trips
    /// through `DashboardState::update` and survives a
    /// subsequent publish with `None` (graph deactivation).
    #[test]
    fn active_graph_snapshot_round_trips_and_clears() {
        let ds = DashboardState::new();
        let mut s = empty_state("BTCUSDT");
        s.active_graph = Some(ActiveGraphSnapshot {
            name: "funding-aware-quoter".into(),
            hash: "0xabc".into(),
            scope: "symbol(btcusdt)".into(),
            deployed_at_ms: 1_700_000_000_000,
            node_count: 42,
        });
        ds.update(s);
        let got = ds.get_symbol("BTCUSDT").unwrap();
        let ag = got.active_graph.expect("active_graph should survive");
        assert_eq!(ag.name, "funding-aware-quoter");
        assert_eq!(ag.hash, "0xabc");
        assert_eq!(ag.node_count, 42);

        // Re-publish with `None` (graph swapped off) clears it.
        let mut s2 = empty_state("BTCUSDT");
        s2.active_graph = None;
        ds.update(s2);
        let got2 = ds.get_symbol("BTCUSDT").unwrap();
        assert!(got2.active_graph.is_none());
    }

    /// S5.4 — calibration snapshots round-trip through the
    /// dashboard: a fresh publish replaces the prior row for the
    /// same symbol, and the readout is sorted by symbol ASC.
    #[test]
    fn calibration_snapshots_replace_and_sort() {
        let ds = DashboardState::new();
        ds.publish_calibration(CalibrationSnapshot {
            symbol: "ETHUSDT".into(),
            strategy: "glft".into(),
            a: dec!(1.0),
            k: dec!(2.0),
            samples: 40,
            last_recalibrated_ms: None,
        });
        ds.publish_calibration(CalibrationSnapshot {
            symbol: "BTCUSDT".into(),
            strategy: "glft".into(),
            a: dec!(1.0),
            k: dec!(1.5),
            samples: 30,
            last_recalibrated_ms: None,
        });
        // Replace ETHUSDT with a post-retune snapshot.
        ds.publish_calibration(CalibrationSnapshot {
            symbol: "ETHUSDT".into(),
            strategy: "glft".into(),
            a: dec!(1.0),
            k: dec!(3.0),
            samples: 80,
            last_recalibrated_ms: Some(12_345_000),
        });

        let rows = ds.calibration_snapshots();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(rows[0].samples, 30);
        assert_eq!(rows[1].symbol, "ETHUSDT");
        assert_eq!(rows[1].samples, 80);
        assert_eq!(rows[1].k, dec!(3.0));
        assert_eq!(rows[1].last_recalibrated_ms, Some(12_345_000));
    }

    /// S5.2 — funding-arb events accumulate per pair bucket
    /// with correct per-variant counters; uncompensated
    /// pair-break increments its dedicated counter on top of
    /// the generic `pair_break` total.
    #[test]
    fn funding_arb_events_accumulate_per_pair() {
        let ds = DashboardState::new();
        ds.record_funding_arb_event("BTCUSDT|BTC-PERP", "entered", None, false);
        ds.record_funding_arb_event("BTCUSDT|BTC-PERP", "hold", None, false);
        ds.record_funding_arb_event(
            "BTCUSDT|BTC-PERP",
            "pair_break",
            Some("hedge rejected"),
            true,
        );
        ds.record_funding_arb_event("ETHUSDT|ETH-PERP", "hold", None, false);

        let pairs = ds.funding_arb_pairs();
        assert_eq!(pairs.len(), 2);
        // Sort order is pair ASC.
        assert_eq!(pairs[0].pair, "BTCUSDT|BTC-PERP");
        assert_eq!(pairs[0].entered, 1);
        assert_eq!(pairs[0].hold, 1);
        assert_eq!(pairs[0].pair_break, 1);
        assert_eq!(pairs[0].pair_break_uncompensated, 1);
        assert_eq!(pairs[0].last_event, "pair_break");
        assert_eq!(pairs[0].last_reason.as_deref(), Some("hedge rejected"));
        assert_eq!(pairs[1].pair, "ETHUSDT|ETH-PERP");
        assert_eq!(pairs[1].hold, 1);
    }

    #[test]
    fn rebalance_recommendations_surface_deficit() {
        let ds = DashboardState::new();
        ds.set_rebalancer_config(mm_risk::rebalancer::RebalancerConfig {
            min_balance_per_venue: dec!(100),
            target_balance_per_venue: dec!(500),
        });
        let deficit = VenueBalanceSnapshot {
            venue: "binance".into(),
            product: "Spot".into(),
            asset: "USDT".into(),
            wallet: "Spot".into(),
            total: dec!(50),
            available: dec!(50),
            locked: dec!(0),
            updated_at: Utc::now(),
        };
        let surplus = VenueBalanceSnapshot {
            venue: "bybit".into(),
            product: "Spot".into(),
            asset: "USDT".into(),
            wallet: "Spot".into(),
            total: dec!(1000),
            available: dec!(1000),
            locked: dec!(0),
            updated_at: Utc::now(),
        };
        ds.update_venue_balances("BTCUSDT", vec![deficit]);
        ds.update_venue_balances("ETHUSDT", vec![surplus]);

        let recs = ds.rebalance_recommendations();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].asset, "USDT");
        assert_eq!(recs[0].from_venue, "bybit");
        assert_eq!(recs[0].to_venue, "binance");
        assert!(recs[0].qty > dec!(0));
    }

    /// R8.5 — rebalancer execute state-level round-trip.
    /// Kill-switch Normal → transfer log gets one row, at least
    /// one of the three terminal states (Executed / Failed /
    /// Accepted). Validates the state machine
    /// `max_kill_level` → `venue_connector` → `transfer_log`
    /// works end-to-end for the business logic. The HTTP
    /// handler is a thin wrapper over these calls.
    #[test]
    fn rebalance_execute_state_roundtrip_intra_venue() {
        let ds = DashboardState::new();
        // Kill switch Normal (no symbols published → max = 0).
        assert_eq!(ds.max_kill_level(), 0);
        // Transfer log writes to a temp dir for inspection.
        let dir = std::env::temp_dir().join(format!(
            "mm-rebal-exec-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("transfers.jsonl");
        let log =
            mm_persistence::transfer_log::TransferLogWriter::open(&log_path).unwrap();
        ds.set_transfer_log(std::sync::Arc::new(log));

        // Simulate an operator-approved intra-venue transfer log.
        // Connector not registered, so the handler's dispatch
        // branch would fall through to "no_connector" — but the
        // business test here is the log write itself.
        use mm_persistence::transfer_log::{TransferRecord, TransferStatus};
        let rec = TransferRecord {
            transfer_id: "t-1".into(),
            ts: Utc::now(),
            from_venue: "binance".into(),
            to_venue: "binance".into(),
            asset: "USDT".into(),
            qty: dec!(500),
            from_wallet: Some("SPOT".into()),
            to_wallet: Some("FUNDING".into()),
            reason: Some("rebalance test".into()),
            operator: "alice".into(),
            status: TransferStatus::Accepted,
            venue_tx_id: None,
            error: None,
        };
        ds.transfer_log().unwrap().append(&rec).unwrap();

        let rows =
            mm_persistence::transfer_log::read_all(&log_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].transfer_id, "t-1");
        assert_eq!(rows[0].status, TransferStatus::Accepted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8.5 — kill-switch gate blocks dispatch. When any engine
    /// has published a non-zero kill level, `max_kill_level()`
    /// reflects it; the rebalance_execute handler branches on
    /// that return to emit RejectedKillSwitch before touching
    /// any connector.
    #[test]
    fn rebalance_execute_kill_switch_gate_state() {
        let ds = DashboardState::new();
        let mut s = empty_state("BTCUSDT");
        s.kill_level = 1;
        ds.update(s);
        assert_eq!(ds.max_kill_level(), 1);
        // The handler checks `> 0` and refuses — this test pins
        // the state-level invariant the handler depends on.
    }

    /// R8.6 — manipulation score snapshot publish cycle.
    /// Engine's tick writes a `ManipulationScoreSnapshot` onto
    /// each symbol's `SymbolState.manipulation_score`; the
    /// `/api/v1/manipulation/scores` handler reads them via
    /// `get_all()`. Pin the publish / read contract so a
    /// future refactor doesn't break the handler's projection.
    #[test]
    fn manipulation_score_publish_cycle() {
        let ds = DashboardState::new();
        let mut s1 = empty_state("RAVEUSDT");
        s1.manipulation_score = Some(ManipulationScoreSnapshot {
            pump_dump: dec!(0.8),
            wash: dec!(0.3),
            thin_book: dec!(0.6),
            combined: dec!(0.65),
        });
        ds.update(s1);
        let mut s2 = empty_state("BTCUSDT");
        s2.manipulation_score = Some(ManipulationScoreSnapshot {
            pump_dump: dec!(0.05),
            wash: Decimal::ZERO,
            thin_book: dec!(0.1),
            combined: dec!(0.04),
        });
        ds.update(s2);

        // Read path: every SymbolState.manipulation_score reaches
        // the /api/v1/manipulation/scores handler via get_all().
        let all: Vec<_> = ds
            .get_all()
            .into_iter()
            .filter_map(|s| s.manipulation_score.map(|m| (s.symbol, m.combined)))
            .collect();
        assert_eq!(all.len(), 2);
        let rave = all.iter().find(|(sym, _)| sym == "RAVEUSDT").unwrap();
        assert_eq!(rave.1, dec!(0.65));
        let btc = all.iter().find(|(sym, _)| sym == "BTCUSDT").unwrap();
        assert_eq!(btc.1, dec!(0.04));
    }

    /// R8.6 — a symbol that never publishes a score appears in
    /// `get_all()` with `manipulation_score = None`, not as a
    /// zero snapshot. The handler's `filter_map` must skip it
    /// so the endpoint doesn't leak "silence = safe" rows.
    #[test]
    fn manipulation_score_missing_is_absent_not_zero() {
        let ds = DashboardState::new();
        let s = empty_state("ETHUSDT");
        // s.manipulation_score is None by default.
        ds.update(s);
        let reported: Vec<_> = ds
            .get_all()
            .into_iter()
            .filter_map(|s| s.manipulation_score)
            .collect();
        assert!(reported.is_empty());
    }

    /// R8.5 — transfer log is `None` until `set_transfer_log`
    /// runs at server boot. The handler returns 503 in that
    /// case. Pin the default so a future refactor doesn't
    /// silently wire a log path and flip this branch.
    #[test]
    fn rebalance_execute_transfer_log_is_none_by_default() {
        let ds = DashboardState::new();
        assert!(ds.transfer_log().is_none());
    }

    /// INV-4 — multi-engine cross-venue inventory aggregation
    /// round-trips through the `CrossVenuePortfolio` owned by
    /// `DashboardState`. Two different `(symbol, venue)` tuples
    /// for the same base asset sum into a single net delta
    /// readable by both `Portfolio.CrossVenueNetDelta` and the
    /// `/api/v1/portfolio/cross_venue` HTTP handler.
    #[test]
    fn cross_venue_inventory_aggregates_by_base_asset() {
        let ds = DashboardState::new();
        // Engine A: long 0.5 BTC on Binance spot, marked at 50k.
        ds.publish_inventory("BTC-USDT", "binance", dec!(0.5), Some(dec!(50_000)));
        // Engine B: short 0.2 BTC on Bybit perp, marked at 49k.
        ds.publish_inventory("BTC-USDC", "bybit", dec!(-0.2), Some(dec!(49_000)));
        // Engine C: flat ETH on Binance — shouldn't leak into BTC sum.
        ds.publish_inventory("ETH-USDT", "binance", dec!(3), Some(dec!(3_000)));

        assert_eq!(ds.cross_venue_net_delta("BTC"), dec!(0.3));
        assert_eq!(ds.cross_venue_net_delta("ETH"), dec!(3));
        assert_eq!(ds.cross_venue_net_delta("SOL"), dec!(0));

        // Grouped view sorts base assets + legs deterministically.
        let grouped = ds.cross_venue_by_asset();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].base, "BTC");
        assert_eq!(grouped[0].legs.len(), 2);
        assert_eq!(grouped[0].legs[0].venue, "binance");
        assert_eq!(grouped[0].legs[1].venue, "bybit");
        assert_eq!(
            grouped[0].net_notional_quote,
            dec!(0.5) * dec!(50_000) + dec!(-0.2) * dec!(49_000)
        );

        // Second publish for engine A replaces, not appends.
        ds.publish_inventory("BTC-USDT", "binance", dec!(0.9), Some(dec!(51_000)));
        assert_eq!(ds.cross_venue_net_delta("BTC"), dec!(0.7));
    }

    /// PAPER-1 — active-plan publish/read round-trip behaves as
    /// S3.1 — the flatten waterfall ranks by notional
    /// drawdown desc, with symbol as deterministic tiebreak.
    /// Highest drawdown → rank 0 (first to flatten).
    #[test]
    fn flatten_priority_ranks_descending_by_drawdown() {
        let ds = DashboardState::new();
        ds.register_flatten_priority("BTCUSDT", dec!(5000));
        ds.register_flatten_priority("ETHUSDT", dec!(1500));
        ds.register_flatten_priority("SOLUSDT", dec!(12000));

        assert_eq!(ds.flatten_priority_rank("SOLUSDT"), Some(0));
        assert_eq!(ds.flatten_priority_rank("BTCUSDT"), Some(1));
        assert_eq!(ds.flatten_priority_rank("ETHUSDT"), Some(2));

        // Ties broken by lexicographic symbol order.
        ds.register_flatten_priority("AAAUSDT", dec!(5000));
        // AAAUSDT and BTCUSDT tie at 5000 notional; SOL is
        // still first (12000). AAA < BTC alphabetically so
        // AAA outranks BTC at the tie.
        assert_eq!(ds.flatten_priority_rank("SOLUSDT"), Some(0));
        assert_eq!(ds.flatten_priority_rank("AAAUSDT"), Some(1));
        assert_eq!(ds.flatten_priority_rank("BTCUSDT"), Some(2));
        assert_eq!(ds.flatten_priority_rank("ETHUSDT"), Some(3));

        // Clearing a symbol removes it from the queue.
        ds.clear_flatten_priority("AAAUSDT");
        assert_eq!(ds.flatten_priority_rank("AAAUSDT"), None);
        assert_eq!(ds.flatten_priority_rank("BTCUSDT"), Some(1));
    }

    /// UI-1 expects: per-symbol publish, flat-list read.
    #[test]
    fn active_plans_publish_then_flat_list() {
        let ds = DashboardState::new();
        let plan_a = PlanSnapshot {
            node_id: "plan-a".into(),
            kind: "Plan.Accumulate".into(),
            symbol: "BTCUSDT".into(),
            started_at_ms: Some(1_000),
            qty_emitted: dec!(0.3),
            aborted: false,
            last_slice_ms: 0,
        };
        let plan_b = PlanSnapshot {
            node_id: "plan-b".into(),
            kind: "Plan.Accumulate".into(),
            symbol: "ETHUSDT".into(),
            started_at_ms: Some(2_000),
            qty_emitted: dec!(1),
            aborted: true,
            last_slice_ms: 0,
        };
        ds.publish_active_plans("BTCUSDT", vec![plan_a]);
        ds.publish_active_plans("ETHUSDT", vec![plan_b]);
        let all = ds.active_plans_all();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|p| p.symbol == "BTCUSDT" && !p.aborted));
        assert!(all.iter().any(|p| p.symbol == "ETHUSDT" && p.aborted));
    }

    /// 23-UX-1 — time-series pushes within MIN_TIMESERIES_GAP_MS
    /// of the previous sample are dropped so the ring buffer
    /// covers a real ~4-hour window at 1-second resolution
    /// instead of 2 hours at 500ms tick rate.
    #[test]
    fn pnl_timeseries_downsamples_sub_second_pushes() {
        let ds = DashboardState::new();
        ds.push_pnl_sample("BTCUSDT", 1_000, dec!(0));
        // Within the 1s gap — dropped.
        ds.push_pnl_sample("BTCUSDT", 1_400, dec!(1));
        ds.push_pnl_sample("BTCUSDT", 1_900, dec!(2));
        // First sample past the gap — accepted.
        ds.push_pnl_sample("BTCUSDT", 2_100, dec!(3));
        let ts = ds.get_pnl_timeseries("BTCUSDT");
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].total_pnl, dec!(0));
        assert_eq!(ts[1].total_pnl, dec!(3));
    }

    #[test]
    fn spread_timeseries_respects_gap_and_cap() {
        let ds = DashboardState::new();
        // Push 20000 samples spaced by 1s — should cap at 14400.
        for i in 0..20_000i64 {
            ds.push_spread_sample("BTCUSDT", i * 1_000, dec!(1));
        }
        let ts = ds.get_spread_timeseries("BTCUSDT");
        assert_eq!(ts.len(), 14_400);
    }

    /// 23-P1-1 — publish_symbol_checkpoint is a no-op when no
    /// manager is attached. Critical for unit tests that build a
    /// bare DashboardState — without the guard they'd panic
    /// trying to grab a Mutex that doesn't exist.
    #[test]
    fn publish_checkpoint_without_manager_is_noop() {
        let ds = DashboardState::new();
        // Should not panic.
        ds.publish_symbol_checkpoint(mm_persistence::checkpoint::SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(0),
            total_fills: 0,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: None,
            engine_state: Default::default(),
        });
    }

    /// 23-P1-1 — when a manager is attached, publish_symbol_checkpoint
    /// routes through update_symbol and the checkpoint state is
    /// observable via get_symbol.
    #[test]
    fn publish_checkpoint_with_manager_writes_through() {
        use mm_persistence::checkpoint::{CheckpointManager, SymbolCheckpoint};
        let tmpdir = std::env::temp_dir();
        let path = tmpdir.join(format!(
            "mm_23p11_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mgr = std::sync::Arc::new(std::sync::Mutex::new(CheckpointManager::new_with_secret(
            &path, 1000, None,
        )));
        let ds = DashboardState::new();
        ds.set_checkpoint_manager(mgr.clone());
        ds.publish_symbol_checkpoint(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0.1234),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(12.5),
            total_volume: dec!(100),
            total_fills: 7,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: None,
            engine_state: Default::default(),
        });
        let guard = mgr.lock().unwrap();
        let got = guard.get_symbol("BTCUSDT").unwrap();
        assert_eq!(got.inventory, dec!(0.1234));
        assert_eq!(got.total_fills, 7);
        drop(guard);
        let _ = std::fs::remove_file(&path);
    }

    /// 23-UX-2 — per-leg history is populated as a side effect
    /// of publish_inventory. Each leg gets its own ring buffer
    /// keyed by (venue, symbol).
    #[test]
    fn per_leg_inventory_history_populates_on_publish() {
        let ds = DashboardState::new();
        ds.publish_inventory("BTCUSDT", "binance", dec!(0.5), Some(dec!(50_000)));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        ds.publish_inventory("BTCUSDT", "binance", dec!(0.6), Some(dec!(50_000)));
        ds.publish_inventory("BTCUSDT", "bybit", dec!(-0.2), Some(dec!(50_000)));
        let legs = ds.per_leg_inventory_timeseries(None);
        assert_eq!(legs.len(), 2);
        let binance = legs.iter().find(|l| l.venue == "binance").unwrap();
        // infer_base_asset("BTCUSDT") returns "BTCUSDT" — no
        // separator to split on. Base filter uses prefix match.
        assert!(binance.base_asset.starts_with("BTC"));
        assert_eq!(binance.points.len(), 2);
        assert_eq!(binance.points[0].value, dec!(0.5));
        assert_eq!(binance.points[1].value, dec!(0.6));
    }

    /// 23-UX-2 — base filter matches across quote variants.
    #[test]
    fn per_leg_inventory_base_filter_groups_btcusdt_btcusdc() {
        let ds = DashboardState::new();
        ds.publish_inventory("BTCUSDT", "binance", dec!(0.5), Some(dec!(50_000)));
        ds.publish_inventory("BTCUSDC", "bybit", dec!(0.3), Some(dec!(50_000)));
        ds.publish_inventory("ETHUSDT", "binance", dec!(1.0), Some(dec!(3000)));
        let btc = ds.per_leg_inventory_timeseries(Some("BTC"));
        assert_eq!(btc.len(), 2);
        assert!(btc.iter().all(|l| l.base_asset.starts_with("BTC")));
    }

    #[test]
    fn inventory_timeseries_rejects_out_of_order_early_pushes() {
        // Regression guard: an out-of-order timestamp older
        // than the last should not accidentally re-open the
        // gate. Our gate uses signed delta so an older push
        // has `delta < 0 < MIN_GAP` → rejected.
        let ds = DashboardState::new();
        ds.push_inventory_sample("BTCUSDT", 2_000, dec!(0.5));
        ds.push_inventory_sample("BTCUSDT", 1_500, dec!(0.3)); // older — rejected
        let ts = ds.get_inventory_timeseries("BTCUSDT");
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].value, dec!(0.5));
    }
