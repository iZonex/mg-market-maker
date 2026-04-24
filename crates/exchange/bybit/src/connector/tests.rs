    use super::*;
    use rust_decimal_macros::dec as rd;

    /// `POST /v5/order/amend` body must carry `category`, `symbol`,
    /// `orderId`, plus only the optional fields the caller actually
    /// changed. Pins the wire shape so a refactor of
    /// `build_amend_body` cannot silently drop fields.
    #[test]
    fn amend_body_carries_category_symbol_and_id() {
        let oid = uuid::Uuid::nil();
        let amend = AmendOrder {
            order_id: oid,
            symbol: "BTCUSDT".into(),
            new_price: Some(rd!(50000.5)),
            new_qty: Some(rd!(0.01)),
        };
        let body = build_amend_body("linear", &amend);
        assert_eq!(body["category"], "linear");
        assert_eq!(body["symbol"], "BTCUSDT");
        assert_eq!(body["orderId"], oid.to_string());
        assert_eq!(body["price"], "50000.5");
        assert_eq!(body["qty"], "0.01");
    }

    /// Bybit V5 fee-rate response: pull the maker / taker rates
    /// out of the `list` array. Wire shape pinned so a future
    /// schema drift breaks the test before it silently drops a
    /// fee tier into a default.
    #[test]
    fn fee_rate_response_parses_maker_taker_for_symbol() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "takerFeeRate": "0.00055",
                    "makerFeeRate": "0.0002"
                }
            ]
        });
        let info = parse_bybit_fee_rate_response(&result, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, rd!(0.0002));
        assert_eq!(info.taker_fee, rd!(0.00055));
        assert!(info.vip_tier.is_none());
    }

    #[test]
    fn fee_rate_response_picks_correct_symbol_row() {
        let result = serde_json::json!({
            "list": [
                {"symbol": "ETHUSDT", "takerFeeRate": "0.001", "makerFeeRate": "0.0005"},
                {"symbol": "BTCUSDT", "takerFeeRate": "0.00055", "makerFeeRate": "0.0002"}
            ]
        });
        let info = parse_bybit_fee_rate_response(&result, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, rd!(0.0002));
    }

    /// Optional fields are omitted entirely (not sent as empty
    /// strings) so Bybit doesn't reject the request with
    /// `params error` on the missing one.
    #[test]
    fn amend_body_omits_unset_optional_fields() {
        let amend = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: Some(rd!(50000)),
            new_qty: None,
        };
        let body = build_amend_body("spot", &amend);
        assert!(body.get("price").is_some());
        assert!(body.get("qty").is_none());
        let amend2 = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: None,
            new_qty: Some(rd!(0.01)),
        };
        let body2 = build_amend_body("spot", &amend2);
        assert!(body2.get("qty").is_some());
        assert!(body2.get("price").is_none());
    }

    /// Listing sniper (Epic F): parse a whole `instruments-info`
    /// response into `ProductSpec` rows, mapping per-instrument
    /// `status` to `TradingStatus` so the sniper consumer filters
    /// out pre-launch / settling contracts post-hoc.
    #[test]
    fn list_symbols_parses_v5_instruments_info_payload() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.10"},
                    "lotSizeFilter": {"qtyStep": "0.001", "minOrderAmt": "5"}
                },
                {
                    "symbol": "NEWUSDT",
                    "baseCoin": "NEW",
                    "quoteCoin": "USDT",
                    "status": "PreLaunch",
                    "priceFilter": {"tickSize": "0.0001"},
                    "lotSizeFilter": {"qtyStep": "1"}
                },
                {
                    "symbol": "DEADUSDT",
                    "baseCoin": "DEAD",
                    "quoteCoin": "USDT",
                    "status": "Closed",
                    "priceFilter": {"tickSize": "0.01"},
                    "lotSizeFilter": {"qtyStep": "0.01"}
                }
            ]
        });
        let specs = parse_bybit_instruments_list(&result);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.tick_size, rd!(0.10));
        assert_eq!(btc.trading_status, TradingStatus::Trading);
        let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
        assert_eq!(new.trading_status, TradingStatus::PreTrading);
        let dead = specs.iter().find(|s| s.symbol == "DEADUSDT").unwrap();
        assert_eq!(dead.trading_status, TradingStatus::Delisted);
    }

    /// Capability audit: declared capabilities must match implementation.
    ///
    /// `supports_ws_trading` must stay `false` until `BybitWsTrader` is
    /// wired into `place_order` / `cancel_order` / `cancel_all_orders`.
    /// The adapter type still exists in the crate (verified below), but
    /// until the V5 auth mechanism is pinned in live-testnet (see
    /// docs/deployment.md §3 under "operator next steps"), the
    /// capability must honestly report the unwired state so a
    /// capability-driven router cannot pick the WS path.
    #[test]
    fn capabilities_match_implementation() {
        let conn = BybitConnector::testnet("key", "secret");
        let caps = conn.capabilities();
        assert!(
            !caps.supports_ws_trading,
            "BybitWsTrader is not wired into place_order yet — capability must report false",
        );
        // Type-level confirmation that the adapter type exists for the
        // future wiring. This is a compile-only check, not end-to-end.
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_trade::BybitWsTrader>();
        };
        assert!(caps.supports_amend);
        assert!(
            !caps.supports_fix,
            "Bybit FIX not yet wired; session engine lives in protocols/fix but no venue adapter"
        );
    }

    /// Each `BybitCategory` maps to the correct `VenueProduct`.
    #[test]
    fn category_venue_product_mapping() {
        assert_eq!(BybitCategory::Spot.venue_product(), VenueProduct::Spot);
        assert_eq!(
            BybitCategory::Linear.venue_product(),
            VenueProduct::LinearPerp
        );
        assert_eq!(
            BybitCategory::Inverse.venue_product(),
            VenueProduct::InversePerp
        );
    }

    /// `as_str` emits the V5 REST category string.
    #[test]
    fn category_as_str_matches_v5_wire_format() {
        assert_eq!(BybitCategory::Spot.as_str(), "spot");
        assert_eq!(BybitCategory::Linear.as_str(), "linear");
        assert_eq!(BybitCategory::Inverse.as_str(), "inverse");
    }

    /// Spot and inverse constructors do not claim funding-rate
    /// support (spot has no funding; inverse is handled but we
    /// keep the flag true only for categories that pay funding).
    #[test]
    fn supports_funding_rate_tracks_category() {
        let spot = BybitConnector::spot("k", "s");
        let linear = BybitConnector::linear("k", "s");
        let inverse = BybitConnector::inverse("k", "s");
        assert!(!spot.capabilities().supports_funding_rate);
        assert!(linear.capabilities().supports_funding_rate);
        assert!(inverse.capabilities().supports_funding_rate);
    }

    /// UX-VENUE-3 — Bybit V5 topic symbols are case-sensitive.
    /// `build_subscribe_topics` must uppercase every input
    /// regardless of what the caller passed, otherwise the
    /// server silently accepts the subscription but never
    /// pushes book / trade / liquidation data.
    #[test]
    fn subscribe_topics_uppercase_symbols() {
        let t = build_subscribe_topics(&["btcusdt".to_string()], 50, true);
        assert!(
            t.contains(&"orderbook.50.BTCUSDT".to_string()),
            "expected uppercase BTCUSDT in topics: {t:?}",
        );
        assert!(t.contains(&"publicTrade.BTCUSDT".to_string()));
        assert!(t.contains(&"liquidation.BTCUSDT".to_string()));
        for topic in &t {
            assert!(
                !topic.contains("btcusdt"),
                "lowercase symbol leaked through: {topic}",
            );
        }
    }

    #[test]
    fn subscribe_topics_omits_liquidation_for_spot() {
        let t = build_subscribe_topics(&["ETHUSDT".to_string()], 50, false);
        assert!(t.iter().any(|x| x.contains("orderbook.50.ETHUSDT")));
        assert!(t.iter().any(|x| x.contains("publicTrade.ETHUSDT")));
        assert!(!t.iter().any(|x| x.starts_with("liquidation.")));
    }

    #[test]
    fn subscribe_topics_fan_out_multiple_symbols() {
        let t = build_subscribe_topics(
            &["btcusdt".to_string(), "ethusdt".to_string()],
            50,
            true,
        );
        // 3 topics per symbol × 2 symbols = 6.
        assert_eq!(t.len(), 6);
        assert!(t.contains(&"orderbook.50.BTCUSDT".to_string()));
        assert!(t.contains(&"orderbook.50.ETHUSDT".to_string()));
    }

    /// `product()` returns the right `VenueProduct` for each
    /// constructor.
    #[test]
    fn product_matches_constructor() {
        assert_eq!(BybitConnector::spot("k", "s").product(), VenueProduct::Spot);
        assert_eq!(
            BybitConnector::linear("k", "s").product(),
            VenueProduct::LinearPerp
        );
        assert_eq!(
            BybitConnector::inverse("k", "s").product(),
            VenueProduct::InversePerp
        );
    }

    /// Testnet variants use the testnet base URLs and the WS URL
    /// suffix picks up the right category.
    #[test]
    fn testnet_variants_use_testnet_urls_with_correct_ws_suffix() {
        let spot = BybitConnector::testnet_spot("k", "s");
        assert!(spot.base_url.contains("testnet"));
        assert!(spot.ws_url.contains("stream-testnet"));
        assert!(spot.ws_url.ends_with("/spot"));

        let linear = BybitConnector::testnet("k", "s");
        assert!(linear.ws_url.ends_with("/linear"));

        let inverse = BybitConnector::testnet_inverse("k", "s");
        assert!(inverse.ws_url.ends_with("/inverse"));
    }

    /// `with_wallet` override works — useful for classic sub-
    /// accounts where spot is a separate bucket from Unified.
    #[test]
    fn with_wallet_overrides_default() {
        let c = BybitConnector::spot("k", "s").with_wallet(WalletType::Spot);
        assert_eq!(c.wallet, WalletType::Spot);
        // But the default (without override) was Unified for spot.
        let d = BybitConnector::spot("k", "s");
        assert_eq!(d.wallet, WalletType::Unified);
    }

    /// Legacy `::new` and `::testnet` constructors still produce a
    /// linear connector so existing call sites in `server/main.rs`
    /// and tests keep working without changes.
    #[test]
    fn legacy_constructors_map_to_linear() {
        assert_eq!(
            BybitConnector::new("k", "s").category,
            BybitCategory::Linear
        );
        assert_eq!(
            BybitConnector::testnet("k", "s").category,
            BybitCategory::Linear
        );
    }

    // ---- Epic F stage-3: multi-category list_symbols ----

    /// `parse_bybit_instruments_list` is the shared helper
    /// that `list_symbols_all_categories` calls per category
    /// before merging. Verify three independent fixtures
    /// (one per category shape) parse + merge cleanly without
    /// collisions across category boundaries.
    #[test]
    fn parse_bybit_multi_category_merge_preserves_all_rows() {
        let spot = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.10"},
                    "lotSizeFilter": {"qtyStep": "0.001", "minOrderAmt": "5"}
                }
            ]
        });
        let linear = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.5"},
                    "lotSizeFilter": {"qtyStep": "0.001"}
                },
                {
                    "symbol": "ETHUSDT",
                    "baseCoin": "ETH",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.05"},
                    "lotSizeFilter": {"qtyStep": "0.01"}
                }
            ]
        });
        let inverse = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSD",
                    "baseCoin": "BTC",
                    "quoteCoin": "USD",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.5"},
                    "lotSizeFilter": {"qtyStep": "1"}
                }
            ]
        });
        let mut merged = parse_bybit_instruments_list(&spot);
        merged.extend(parse_bybit_instruments_list(&linear));
        merged.extend(parse_bybit_instruments_list(&inverse));
        // Both categories list BTCUSDT — listing sniper
        // consumer dedupes per-(symbol, category) externally;
        // the parser preserves both rows.
        assert_eq!(merged.len(), 4);
        let symbols: Vec<&str> = merged.iter().map(|p| p.symbol.as_str()).collect();
        assert!(symbols.contains(&"BTCUSDT"));
        assert!(symbols.contains(&"ETHUSDT"));
        assert!(symbols.contains(&"BTCUSD"));
        // Spot BTCUSDT and linear BTCUSDT have distinct
        // tick sizes — the merge preserves both.
        let btcusdt_ticks: Vec<rust_decimal::Decimal> = merged
            .iter()
            .filter(|p| p.symbol == "BTCUSDT")
            .map(|p| p.tick_size)
            .collect();
        assert_eq!(btcusdt_ticks.len(), 2);
        assert!(btcusdt_ticks.contains(&dec!(0.10)));
        assert!(btcusdt_ticks.contains(&dec!(0.5)));
    }

    #[test]
    fn parse_bybit_empty_categories_merge_yields_empty() {
        let empty = serde_json::json!({"list": []});
        let mut merged = parse_bybit_instruments_list(&empty);
        merged.extend(parse_bybit_instruments_list(&empty));
        merged.extend(parse_bybit_instruments_list(&empty));
        assert!(merged.is_empty());
    }

    /// Epic 40.4 — Bybit V5 account margin wire shape. Both
    /// `wallet-balance` and `position-list` payloads are needed;
    /// pin the combined shape so a V5 schema drift fails the
    /// test before it silently drops the guard's ratio to zero.
    #[test]
    fn account_margin_parser_reads_unified_wallet_and_positions() {
        let wallet = serde_json::json!({
            "list": [
                {
                    "accountType": "UNIFIED",
                    "totalEquity": "10000.50",
                    "totalInitialMargin": "2000",
                    "totalMaintenanceMargin": "500",
                    "totalAvailableBalance": "8000"
                }
            ]
        });
        let positions = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "side": "Buy",
                    "size": "0.05",
                    "avgPrice": "50000",
                    "markPrice": "50500",
                    "positionIM": "250",
                    "liqPrice": "45000"
                },
                {
                    "symbol": "ETHUSDT",
                    "side": "Sell",
                    "size": "0",
                    "avgPrice": "0",
                    "markPrice": "0",
                    "positionIM": "0",
                    "liqPrice": ""
                }
            ]
        });
        let info = parse_bybit_account_margin(&wallet, &positions).unwrap();
        assert_eq!(info.total_equity, rd!(10000.50));
        assert_eq!(info.total_maintenance_margin, rd!(500));
        assert!(info.margin_ratio > rd!(0.049));
        assert!(info.margin_ratio < rd!(0.051));
        // Zero-size ETHUSDT filtered out.
        assert_eq!(info.positions.len(), 1);
        let btc = &info.positions[0];
        assert_eq!(btc.symbol, "BTCUSDT");
        assert_eq!(btc.side, Side::Buy);
        assert_eq!(btc.isolated_margin, Some(rd!(250)));
        assert_eq!(btc.liq_price, Some(rd!(45000)));
    }

    /// Epic 40.3 — Bybit V5 ticker funding wire shape.
    #[test]
    fn funding_rate_parser_reads_ticker_row() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "fundingRate": "0.0001",
                    "nextFundingTime": "1700000000000"
                },
                {
                    "symbol": "ETHUSDT",
                    "fundingRate": "-0.0002",
                    "nextFundingTime": "1700000000000"
                }
            ]
        });
        let eth = parse_bybit_funding_rate(&result, "ETHUSDT").unwrap();
        assert_eq!(eth.rate, rd!(-0.0002));
        assert_eq!(eth.interval, std::time::Duration::from_secs(8 * 3600));
    }

    #[test]
    fn funding_rate_parser_returns_none_on_missing_symbol() {
        // Empty list → None.
        let result = serde_json::json!({ "list": [] });
        assert!(parse_bybit_funding_rate(&result, "BTCUSDT").is_none());
    }

    #[test]
    fn account_margin_parser_zero_equity_saturates_ratio() {
        let wallet = serde_json::json!({
            "list": [
                {
                    "accountType": "UNIFIED",
                    "totalEquity": "0",
                    "totalInitialMargin": "0",
                    "totalMaintenanceMargin": "0",
                    "totalAvailableBalance": "0"
                }
            ]
        });
        let positions = serde_json::json!({"list": []});
        let info = parse_bybit_account_margin(&wallet, &positions).unwrap();
        assert_eq!(info.margin_ratio, Decimal::ONE);
    }
