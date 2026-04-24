    use super::*;

    /// `GET /sapi/v1/asset/tradeFee?symbol=BTCUSDT` returns a JSON
    /// array even for a single-symbol query — the helper must
    /// pick the right row out of the array.
    #[test]
    fn spot_fee_response_array_picks_correct_symbol() {
        let resp = serde_json::json!([
            {"symbol": "ETHUSDT", "makerCommission": "0.001", "takerCommission": "0.001"},
            {"symbol": "BTCUSDT", "makerCommission": "0.0008", "takerCommission": "0.001"}
        ]);
        let info = parse_binance_spot_fee_response(&resp, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, dec!(0.0008));
        assert_eq!(info.taker_fee, dec!(0.001));
    }

    /// `dailyInterestRate` × 365 must round-trip through the
    /// `BorrowRateInfo::from_apr` helper into an APR fraction
    /// the BorrowManager understands. Pin the conversion so a
    /// future refactor cannot silently drop the × 365 step.
    #[test]
    fn borrow_rate_response_daily_to_apr() {
        let resp = serde_json::json!([
            {"asset": "BTC", "dailyInterestRate": "0.0001", "timestamp": 1_700_000_000_000_u64}
        ]);
        let info = parse_binance_borrow_rate_response(&resp, "BTC").unwrap();
        // 0.0001 × 365 = 0.0365 → 3.65 % APR
        assert_eq!(info.rate_apr, dec!(0.0365));
        assert_eq!(info.asset, "BTC");
        // 0.0365 × 10_000 / 8_760 ≈ 0.04167 bps/hour
        assert!(
            info.rate_bps_hourly > dec!(0.0416) && info.rate_bps_hourly < dec!(0.0417),
            "got {}",
            info.rate_bps_hourly
        );
    }

    /// Object-shape fallback so the parser is resilient to the
    /// edge case where Binance returns a single record rather
    /// than a wrapping array (mirrors the spot-fee parser).
    #[test]
    fn borrow_rate_response_accepts_object_shape() {
        let resp = serde_json::json!({
            "asset": "BTC",
            "dailyInterestRate": "0.0002"
        });
        let info = parse_binance_borrow_rate_response(&resp, "BTC").unwrap();
        assert_eq!(info.rate_apr, dec!(0.073));
    }

    /// Some Binance edge cases return a bare object instead of an
    /// array — the helper must accept that shape too so the parser
    /// is robust to either response form.
    #[test]
    fn spot_fee_response_object_shape_also_parses() {
        let resp = serde_json::json!({
            "symbol": "BTCUSDT",
            "makerCommission": "0.0009",
            "takerCommission": "0.001"
        });
        let info = parse_binance_spot_fee_response(&resp, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, dec!(0.0009));
    }

    /// Listing sniper (Epic F): `parse_binance_spot_symbols_array`
    /// maps every `symbols[]` row through the shared helper. Pin
    /// the whole-universe shape so a schema drift breaks the test
    /// instead of silently dropping a symbol from the sniper's
    /// view of the venue.
    #[test]
    fn list_symbols_parses_full_exchange_info_response() {
        let resp = serde_json::json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "baseAsset": "BTC",
                    "quoteAsset": "USDT",
                    "status": "TRADING",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.00001"},
                        {"filterType": "NOTIONAL", "minNotional": "10"}
                    ]
                },
                {
                    "symbol": "ETHUSDT",
                    "baseAsset": "ETH",
                    "quoteAsset": "USDT",
                    "status": "TRADING",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.0001"},
                        {"filterType": "NOTIONAL", "minNotional": "10"}
                    ]
                },
                {
                    "symbol": "NEWUSDT",
                    "baseAsset": "NEW",
                    "quoteAsset": "USDT",
                    "status": "PRE_TRADING",
                    "filters": []
                }
            ]
        });
        let specs = parse_binance_spot_symbols_array(&resp);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.base_asset, "BTC");
        assert_eq!(btc.quote_asset, "USDT");
        assert_eq!(btc.tick_size, dec!(0.01));
        assert_eq!(btc.trading_status, TradingStatus::Trading);
        let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
        // PRE_TRADING symbols are still returned — the sniper
        // consumer filters by trading_status post-hoc.
        assert_eq!(new.trading_status, TradingStatus::PreTrading);
    }

    /// Malformed rows (missing `symbol` field) are silently
    /// dropped so the sniper gets the subset the venue returned
    /// cleanly instead of a hard error.
    #[test]
    fn list_symbols_drops_malformed_rows_silently() {
        let resp = serde_json::json!({
            "symbols": [
                {"symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING", "filters": []},
                {"baseAsset": "ETH", "quoteAsset": "USDT", "status": "TRADING", "filters": []}
            ]
        });
        let specs = parse_binance_spot_symbols_array(&resp);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].symbol, "BTCUSDT");
    }

    /// Empty response body (venue returned no `symbols` field)
    /// yields an empty vec rather than panicking.
    #[test]
    fn list_symbols_empty_response_is_empty_vec() {
        let resp = serde_json::json!({});
        assert!(parse_binance_spot_symbols_array(&resp).is_empty());
    }

    /// Capability audit: `supports_ws_trading` and `supports_fix` must
    /// reflect the actual presence of adapter types / session engines.
    #[test]
    fn capabilities_match_implementation() {
        let conn = BinanceConnector::testnet("key", "secret");
        let caps = conn.capabilities();
        assert!(
            caps.supports_ws_trading,
            "Binance declares WS trading — BinanceWsTrader must exist"
        );
        // Type-level confirmation:
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_trade::BinanceWsTrader>();
        };
        // FIX must be `false` until a `fix_trade.rs` adapter lands in
        // this crate (see docs/deployment.md "FIX venue adapters"). The
        // generic session engine in `crates/protocols/fix` is not a
        // substitute for a venue adapter.
        assert!(!caps.supports_fix);
        // Binance Spot has no native amend (only `order.cancelReplace`,
        // which loses queue priority). The capability flag must
        // honestly report `false` — the engine's amend planner reads
        // it to decide whether to fall back to cancel+place. Real
        // amend lives on `BinanceFuturesConnector`.
        assert!(!caps.supports_amend);
    }

    #[test]
    fn binance_transfer_type_mapping() {
        assert_eq!(
            super::binance_transfer_type("SPOT", "FUTURES").unwrap(),
            "MAIN_UMFUTURE"
        );
        assert_eq!(
            super::binance_transfer_type("FUTURES", "SPOT").unwrap(),
            "UMFUTURE_MAIN"
        );
        assert_eq!(
            super::binance_transfer_type("SPOT", "MARGIN").unwrap(),
            "MAIN_MARGIN"
        );
        assert!(super::binance_transfer_type("SPOT", "UNKNOWN").is_err());
    }
