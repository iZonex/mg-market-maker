    use super::*;

    #[test]
    fn decimals_zero_is_one_dollar_tick() {
        // Perp: BTC szDecimals=5 → pxDecimals = 6-5 = 1 → tick=0.1.
        let spec = HyperLiquidConnector::decimals_to_spec("BTC", 5, false);
        assert_eq!(spec.tick_size, dec!(0.1));
        assert_eq!(spec.lot_size, dec!(0.00001));
        assert_eq!(spec.quote_asset, "USDC");
        assert_eq!(spec.maker_fee, DEFAULT_MAKER_FEE);
    }

    #[test]
    fn decimals_high_sz_caps_at_zero_px() {
        // If szDecimals > max_px (6 perp / 8 spot), pxDecimals
        // saturates to 0 → tick_size=1.
        let spec = HyperLiquidConnector::decimals_to_spec("TOKEN", 8, false);
        assert_eq!(spec.tick_size, dec!(1));
    }

    /// Spot precision rule uses `8 - szDecimals` instead of
    /// `6 - szDecimals`, so a token with the same `szDecimals` gets
    /// two additional decimal places of price precision relative
    /// to its perp counterpart.
    #[test]
    fn spot_precision_uses_eight_minus_sz_decimals() {
        // szDecimals=5 → perp tick=0.1 (6-5=1), spot tick=0.001 (8-5=3)
        let perp = HyperLiquidConnector::decimals_to_spec("BTC", 5, false);
        let spot = HyperLiquidConnector::decimals_to_spec("PURR/USDC", 5, true);
        assert_eq!(perp.tick_size, dec!(0.1));
        assert_eq!(spot.tick_size, dec!(0.001));
        // Spot symbol `BASE/QUOTE` also populates base+quote fields.
        assert_eq!(spot.base_asset, "PURR");
        assert_eq!(spot.quote_asset, "USDC");
    }

    /// Spot connector reports `VenueProduct::Spot`; perp reports
    /// `LinearPerp`. Also verifies `supports_funding_rate` flips.
    #[test]
    fn spot_and_perp_constructors_set_correct_capabilities() {
        let perp = HyperLiquidConnector::testnet(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let spot = HyperLiquidConnector::testnet_spot(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(perp.product(), VenueProduct::LinearPerp);
        assert_eq!(spot.product(), VenueProduct::Spot);
        assert!(perp.capabilities().supports_funding_rate);
        assert!(!spot.capabilities().supports_funding_rate);
    }

    /// The `SPOT_INDEX_OFFSET` constant is wire-load-bearing: HL
    /// expects spot pairs addressed as `10_000 + pair_idx` in the
    /// signed L1 action's `a` field. Pin the constant so any drift
    /// breaks the test before it breaks live signing.
    #[test]
    fn spot_index_offset_is_ten_thousand() {
        assert_eq!(SPOT_INDEX_OFFSET, 10_000);
    }

    #[test]
    fn cloid_roundtrip() {
        let u = Uuid::new_v4();
        let cloid = HyperLiquidConnector::uuid_to_cloid(u);
        assert!(cloid.starts_with("0x"));
        assert_eq!(cloid.len(), 2 + 32);
        let back = HyperLiquidConnector::cloid_to_uuid(&cloid).unwrap();
        assert_eq!(u, back);
    }

    /// Epic 40.4 — HL `clearinghouseState` wire shape. Pin
    /// the decode so a future HL API change fails the test
    /// instead of silently zeroing the guard's ratio.
    #[test]
    fn clearinghouse_margin_parser_extracts_ratio_and_positions() {
        let resp = serde_json::json!({
            "withdrawable": "5000",
            "crossMaintenanceMarginUsed": "500",
            "marginSummary": {
                "accountValue": "10000",
                "totalMarginUsed": "2000",
                "totalNtlPos": "8000"
            },
            "assetPositions": [
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "ETH",
                        "szi": "1.5",
                        "entryPx": "3000",
                        "markPx": "3050",
                        "marginUsed": "450",
                        "liquidationPx": "2800",
                        "leverage": {"type":"isolated","value":10}
                    }
                },
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "BTC",
                        "szi": "-0.1",
                        "entryPx": "50000",
                        "markPx": "50500",
                        "marginUsed": "0",
                        "liquidationPx": "55000",
                        "leverage": {"type":"cross","value":5}
                    }
                },
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "SOL",
                        "szi": "0",
                        "entryPx": "0",
                        "markPx": "0",
                        "marginUsed": "0",
                        "liquidationPx": ""
                    }
                }
            ]
        });
        let info = parse_hl_clearinghouse_margin(&resp).unwrap();
        assert_eq!(info.total_equity, dec!(10000));
        assert_eq!(info.total_maintenance_margin, dec!(500));
        assert_eq!(info.margin_ratio, dec!(0.05));
        // SOL zero-size filtered; ETH + BTC kept.
        assert_eq!(info.positions.len(), 2);
        let eth = info.positions.iter().find(|p| p.symbol == "ETH").unwrap();
        assert_eq!(eth.side, Side::Buy);
        assert_eq!(eth.size, dec!(1.5));
        assert_eq!(eth.isolated_margin, Some(dec!(450)));
        assert_eq!(eth.liq_price, Some(dec!(2800)));
        let btc = info.positions.iter().find(|p| p.symbol == "BTC").unwrap();
        assert_eq!(btc.side, Side::Sell);
        assert_eq!(btc.size, dec!(0.1));
        // Cross-margin position — no isolated margin surfaced.
        assert!(btc.isolated_margin.is_none());
    }

    /// Epic 40.3 — HL `metaAndAssetCtxs` funding wire shape.
    /// Pins the `[meta, ctxs]` two-element array layout and
    /// the 1-hour cadence constant.
    #[test]
    fn funding_rate_parser_reads_ctx_at_index() {
        let resp = serde_json::json!([
            { "universe": [{"name":"BTC"},{"name":"ETH"}] },
            [
                { "funding": "0.000125", "markPx": "50000" },
                { "funding": "-0.00003", "markPx": "3000" }
            ]
        ]);
        let btc = parse_hl_funding_rate(&resp, 0).unwrap();
        assert_eq!(btc.rate, dec!(0.000125));
        assert_eq!(btc.interval, std::time::Duration::from_secs(3600));
        let eth = parse_hl_funding_rate(&resp, 1).unwrap();
        assert_eq!(eth.rate, dec!(-0.00003));
    }

    #[test]
    fn funding_rate_parser_out_of_bounds_returns_none() {
        let resp = serde_json::json!([
            { "universe": [{"name":"BTC"}] },
            [{ "funding": "0.0001" }]
        ]);
        assert!(parse_hl_funding_rate(&resp, 99).is_none());
    }

    #[test]
    fn clearinghouse_margin_parser_zero_equity_saturates_ratio() {
        let resp = serde_json::json!({
            "withdrawable": "0",
            "crossMaintenanceMarginUsed": "0",
            "marginSummary": {
                "accountValue": "0",
                "totalMarginUsed": "0",
                "totalNtlPos": "0"
            },
            "assetPositions": []
        });
        let info = parse_hl_clearinghouse_margin(&resp).unwrap();
        assert_eq!(info.margin_ratio, Decimal::ONE);
    }

    #[test]
    fn format_decimal_truncates_precision() {
        // rust_decimal::round_dp rounds half-to-even and does NOT pad trailing
        // zeros — matches HL Python SDK's float_to_wire_str which strips them.
        assert_eq!(format_decimal(dec!(42000.123456), 1), "42000.1");
        assert_eq!(format_decimal(dec!(0.00012345), 5), "0.00012");
        // Integer-valued decimals stay integer-shaped.
        assert_eq!(format_decimal(dec!(1), 3), "1");
        // Half-even rounding: 0.125 at 2 dp → 0.12 (round to even).
        assert_eq!(format_decimal(dec!(0.125), 2), "0.12");
    }

    /// Capability audit: `VenueCapabilities::supports_ws_trading` must
    /// match the actual presence of the WS post adapter. Protects
    /// against declaring a capability we cannot deliver — the bug this
    /// whole epic was triggered by.
    #[test]
    fn capabilities_match_implementation() {
        let conn = HyperLiquidConnector::testnet(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let caps = conn.capabilities();
        assert!(
            caps.supports_ws_trading,
            "HL declares WS trading — the WS post adapter must exist"
        );
        assert!(
            !caps.supports_amend,
            "HL has no native amend (cancel+place)"
        );
        assert!(!caps.supports_fix, "HL has no FIX gateway");
        // Type-level confirmation that the adapter actually exists:
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_post::HlWsTrader>();
        };
    }

    // ---------- webData2 → BalanceUpdate (P0.1 HL leg) ----------

    /// Perp `webData2` payload → single USDC `BalanceUpdate` tagged
    /// against the perp collateral wallet. Mirrors the field layout
    /// the REST `clearinghouseState` reader uses, so a future schema
    /// drift breaks both the test and the live parser symmetrically.
    #[test]
    fn webdata2_perp_emits_usdc_balance_update() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "user": "0xabc",
                "clearinghouseState": {
                    "withdrawable": "750.50",
                    "marginSummary": { "accountValue": "1000.00" }
                }
            }
        });
        let events = parse_hl_event(&frame, false);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                assert_eq!(asset, "USDC");
                assert_eq!(*wallet, WalletType::UsdMarginedFutures);
                assert_eq!(*total, dec!(1000.00));
                assert_eq!(*available, dec!(750.50));
                assert_eq!(*locked, dec!(249.50));
            }
            _ => panic!("expected BalanceUpdate"),
        }
    }

    /// Perp parser falls back to `withdrawable` as both total and
    /// available when `marginSummary.accountValue` is missing —
    /// guards against an HL edge case where a fresh sub-account has
    /// no open positions and the field is omitted entirely.
    #[test]
    fn webdata2_perp_falls_back_when_account_value_missing() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": { "withdrawable": "42" }
            }
        });
        let events = parse_hl_event(&frame, false);
        assert_eq!(events.len(), 1);
        if let MarketEvent::BalanceUpdate {
            total,
            available,
            locked,
            ..
        } = &events[0]
        {
            assert_eq!(*total, dec!(42));
            assert_eq!(*available, dec!(42));
            assert_eq!(*locked, dec!(0));
        } else {
            panic!("expected BalanceUpdate");
        }
    }

    /// Spot `webData2` payload → one `BalanceUpdate` per non-zero
    /// coin, tagged against the spot wallet bucket. Mirrors the
    /// `spotClearinghouseState.balances[]` shape from the REST path.
    #[test]
    fn webdata2_spot_emits_per_coin_balance_updates() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "user": "0xabc",
                "spotState": {
                    "balances": [
                        { "coin": "USDC", "token": 0, "total": "500.0", "hold": "100.0" },
                        { "coin": "PURR", "token": 1, "total": "10.0", "hold": "0.0" }
                    ]
                }
            }
        });
        let events = parse_hl_event(&frame, true);
        assert_eq!(events.len(), 2);
        if let MarketEvent::BalanceUpdate {
            asset,
            wallet,
            total,
            locked,
            available,
            ..
        } = &events[0]
        {
            assert_eq!(asset, "USDC");
            assert_eq!(*wallet, WalletType::Spot);
            assert_eq!(*total, dec!(500));
            assert_eq!(*locked, dec!(100));
            assert_eq!(*available, dec!(400));
        } else {
            panic!("expected BalanceUpdate");
        }
    }

    /// `is_spot=true` must NOT emit perp `BalanceUpdate`s even when a
    /// `clearinghouseState` snippet sneaks into the payload, and vice
    /// versa. Otherwise a spot connector would surface its operator's
    /// perp collateral as a spot balance and double-count it.
    #[test]
    fn webdata2_routing_is_disjoint_between_spot_and_perp() {
        let mixed = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": {
                    "withdrawable": "100",
                    "marginSummary": { "accountValue": "100" }
                },
                "spotState": {
                    "balances": [{ "coin": "USDC", "total": "5", "hold": "0" }]
                }
            }
        });
        let perp_events = parse_hl_event(&mixed, false);
        assert_eq!(perp_events.len(), 1);
        assert!(matches!(
            perp_events[0],
            MarketEvent::BalanceUpdate {
                wallet: WalletType::UsdMarginedFutures,
                ..
            }
        ));
        let spot_events = parse_hl_event(&mixed, true);
        assert_eq!(spot_events.len(), 1);
        assert!(matches!(
            spot_events[0],
            MarketEvent::BalanceUpdate {
                wallet: WalletType::Spot,
                ..
            }
        ));
    }

    /// `webData2` with no recognisable balance fields is a no-op —
    /// guards against sending spurious zero-balance updates that
    /// would confuse the inventory drift reconciler.
    #[test]
    fn webdata2_with_no_balances_is_silent() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": { "user": "0xabc" }
        });
        assert!(parse_hl_event(&frame, false).is_empty());
        assert!(parse_hl_event(&frame, true).is_empty());
    }

    /// `parse_hl_event_for_test` is the public crate-export the
    /// downstream `mm-engine` integration test pins against. Verify
    /// it dispatches to the same internal parser.
    #[test]
    fn parse_hl_event_for_test_is_a_thin_pass_through() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": {
                    "withdrawable": "1",
                    "marginSummary": { "accountValue": "1" }
                }
            }
        });
        assert_eq!(super::parse_hl_event_for_test(&frame, false).len(), 1);
    }

    /// Listing sniper (Epic F): `/info {type: "meta"}` perp
    /// universe parses into one [`ProductSpec`] per asset, with
    /// `szDecimals` driving the tick/lot precision through the
    /// shared `decimals_to_spec` helper.
    #[test]
    fn list_symbols_perp_meta_parses_universe_array() {
        let resp = serde_json::json!({
            "universe": [
                { "name": "BTC", "szDecimals": 5, "maxLeverage": 50 },
                { "name": "ETH", "szDecimals": 4, "maxLeverage": 50 },
                { "name": "DEAD", "szDecimals": 2, "isDelisted": true }
            ]
        });
        let specs = parse_hl_perp_meta_into_specs(&resp);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTC").unwrap();
        // szDecimals=5 → tick = 10^-(6-5) = 0.1
        assert_eq!(btc.tick_size, dec!(0.1));
        assert_eq!(btc.min_notional, DEFAULT_MIN_NOTIONAL);
        assert_eq!(btc.trading_status, mm_common::types::TradingStatus::Trading);
        let dead = specs.iter().find(|s| s.symbol == "DEAD").unwrap();
        assert_eq!(
            dead.trading_status,
            mm_common::types::TradingStatus::Delisted
        );
    }

    /// Spot meta flows through the token-index → szDecimals lookup
    /// identical to `ensure_asset_map`, then maps each pair through
    /// the shared spec helper.
    #[test]
    fn list_symbols_spot_meta_resolves_pair_precision_via_tokens() {
        let resp = serde_json::json!({
            "tokens": [
                { "name": "USDC", "index": 0, "szDecimals": 2, "weiDecimals": 8 },
                { "name": "PURR", "index": 1, "szDecimals": 5, "weiDecimals": 8 }
            ],
            "universe": [
                { "name": "PURR/USDC", "tokens": [1, 0], "index": 0 }
            ]
        });
        let specs = parse_hl_spot_meta_into_specs(&resp);
        assert_eq!(specs.len(), 1);
        let purr = &specs[0];
        assert_eq!(purr.symbol, "PURR/USDC");
        assert_eq!(purr.base_asset, "PURR");
        assert_eq!(purr.quote_asset, "USDC");
        // Spot precision: 8 - szDecimals(5) = 3 → tick 0.001
        assert_eq!(purr.tick_size, dec!(0.001));
    }

    /// Missing universe / tokens arrays yield an empty vec rather
    /// than panicking — guards against a venue-side schema blip
    /// taking down the listing sniper.
    #[test]
    fn list_symbols_meta_missing_fields_returns_empty() {
        assert!(parse_hl_perp_meta_into_specs(&serde_json::json!({})).is_empty());
        assert!(parse_hl_spot_meta_into_specs(&serde_json::json!({})).is_empty());
    }
