use super::*;

#[test]
fn product_is_linear_perp() {
    let c = BinanceFuturesConnector::testnet("k", "s");
    assert_eq!(c.product(), VenueProduct::LinearPerp);
}

#[test]
fn capabilities_claim_funding_rate_support() {
    let c = BinanceFuturesConnector::testnet("k", "s");
    assert!(c.capabilities().supports_funding_rate);
    assert!(c.capabilities().supports_amend);
    assert!(!c.capabilities().supports_ws_trading);
    assert!(!c.capabilities().supports_fix);
}

/// `PUT /fapi/v1/order` requires `symbol`, `origClientOrderId`,
/// `quantity` and `price` — pin the wire shape so any future
/// refactor of `build_amend_query` cannot silently drop a field.
#[test]
fn amend_query_contains_required_fields() {
    let oid = uuid::Uuid::nil();
    let amend = AmendOrder {
        order_id: oid,
        symbol: "BTCUSDT".into(),
        new_price: Some(dec!(50000.5)),
        new_qty: Some(dec!(0.01)),
    };
    let q = build_amend_query(&amend).unwrap();
    assert!(q.contains("symbol=BTCUSDT"));
    assert!(q.contains(&format!("origClientOrderId={oid}")));
    assert!(q.contains("quantity=0.01"));
    assert!(q.contains("price=50000.5"));
}

/// `GET /fapi/v1/commissionRate` returns a single object with
/// the maker / taker rates as strings. Pin the wire shape so a
/// schema drift breaks the test instead of silently dropping a
/// new tier into the default.
#[test]
fn futures_fee_response_parses_maker_taker_rates() {
    let resp = serde_json::json!({
        "symbol": "BTCUSDT",
        "makerCommissionRate": "0.0002",
        "takerCommissionRate": "0.0004"
    });
    let info = parse_binance_futures_fee_response(&resp).unwrap();
    assert_eq!(info.maker_fee, dec!(0.0002));
    assert_eq!(info.taker_fee, dec!(0.0004));
    assert!(info.vip_tier.is_none());
}

/// Missing price or qty is a programmer error — bail loudly so
/// the engine's amend-fallback path triggers instead of sending
/// a malformed request that the venue rejects with a generic
/// 400.
#[test]
fn amend_query_bails_when_price_or_qty_missing() {
    let amend_no_price = AmendOrder {
        order_id: uuid::Uuid::nil(),
        symbol: "BTCUSDT".into(),
        new_price: None,
        new_qty: Some(dec!(0.01)),
    };
    assert!(build_amend_query(&amend_no_price).is_err());
    let amend_no_qty = AmendOrder {
        order_id: uuid::Uuid::nil(),
        symbol: "BTCUSDT".into(),
        new_price: Some(dec!(50000)),
        new_qty: None,
    };
    assert!(build_amend_query(&amend_no_qty).is_err());
}

/// Listing sniper (Epic F): the futures parser walks the full
/// `/fapi/v1/exchangeInfo` response and maps every row,
/// including contracts in non-trading states that the sniper
/// consumer filters post-hoc.
#[test]
fn list_symbols_parses_full_futures_exchange_info() {
    let resp = serde_json::json!({
        "symbols": [
            {
                "symbol": "BTCUSDT",
                "baseAsset": "BTC",
                "quoteAsset": "USDT",
                "contractStatus": "TRADING",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                    {"filterType": "LOT_SIZE", "stepSize": "0.001"},
                    {"filterType": "MIN_NOTIONAL", "notional": "5"}
                ]
            },
            {
                "symbol": "DEADUSDT",
                "baseAsset": "DEAD",
                "quoteAsset": "USDT",
                "contractStatus": "SETTLING",
                "filters": []
            },
            {
                "symbol": "NEWUSDT",
                "baseAsset": "NEW",
                "quoteAsset": "USDT",
                "contractStatus": "PENDING_TRADING",
                "filters": []
            }
        ]
    });
    let specs = parse_binance_futures_symbols_array(&resp);
    assert_eq!(specs.len(), 3);
    let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
    assert_eq!(btc.tick_size, dec!(0.10));
    assert_eq!(btc.trading_status, TradingStatus::Trading);
    let dead = specs.iter().find(|s| s.symbol == "DEADUSDT").unwrap();
    assert_eq!(dead.trading_status, TradingStatus::Break);
    let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
    assert_eq!(new.trading_status, TradingStatus::PreTrading);
}

#[test]
fn testnet_uses_testnet_urls() {
    let c = BinanceFuturesConnector::testnet("k", "s");
    assert!(c.base_url.contains("binancefuture.com"));
    assert!(c.ws_url.contains("binancefuture.com"));
}

#[test]
fn mainnet_uses_fapi_urls() {
    let c = BinanceFuturesConnector::new("k", "s");
    assert!(c.base_url.contains("fapi.binance.com"));
    assert!(c.ws_url.contains("fstream.binance.com"));
}

/// Default impl of `get_funding_rate` lives in the trait; we
/// override it. The override returns `Err(Other)` when the venue
/// is unreachable (tested on a non-routable URL) rather than
/// falling through to `NotSupported`.
#[tokio::test]
async fn get_funding_rate_returns_other_not_notsupported_on_network_fail() {
    let c = BinanceFuturesConnector::with_urls(
        "http://127.0.0.1:1", // unreachable
        "ws://127.0.0.1:1",
        "k",
        "s",
    );
    let err = c.get_funding_rate("BTCUSDT").await.unwrap_err();
    match err {
        FundingRateError::NotSupported => {
            panic!("futures connector must NOT report NotSupported")
        }
        FundingRateError::Other(_) => {}
    }
}

/// Epic 40.4 — pin the `/fapi/v2/account` wire shape so a
/// venue schema drift fails the test instead of silently
/// zeroing the guard's ratio (which would hide the account
/// from the kill switch until someone noticed).
#[test]
fn account_margin_parser_extracts_ratio_and_positions() {
    let resp = serde_json::json!({
        "totalMarginBalance": "10000.50",
        "totalInitialMargin": "2000.00",
        "totalMaintMargin": "500.00",
        "availableBalance": "8000.00",
        "positions": [
            {
                "symbol": "BTCUSDT",
                "positionAmt": "0.050",
                "entryPrice": "50000.0",
                "markPrice": "50500.0",
                "isolatedMargin": "250.0",
                "liquidationPrice": "45000.0"
            },
            {
                "symbol": "DUMMY",
                "positionAmt": "0",
                "entryPrice": "0",
                "markPrice": "0",
                "isolatedMargin": "0",
                "liquidationPrice": "0"
            },
            {
                "symbol": "ETHUSDT",
                "positionAmt": "-1.5",
                "entryPrice": "3000",
                "markPrice": "2900",
                "isolatedMargin": "0",
                "liquidationPrice": "4500"
            }
        ]
    });
    let info = parse_binance_futures_account(&resp).unwrap();
    assert_eq!(info.total_equity, dec!(10000.50));
    assert_eq!(info.total_maintenance_margin, dec!(500.00));
    // 500 / 10000.50 ≈ 0.0499975…
    assert!(info.margin_ratio > dec!(0.049));
    assert!(info.margin_ratio < dec!(0.051));
    // DUMMY zero-size row filtered; BTCUSDT + ETHUSDT kept.
    assert_eq!(info.positions.len(), 2);
    let btc = info
        .positions
        .iter()
        .find(|p| p.symbol == "BTCUSDT")
        .unwrap();
    assert_eq!(btc.side, Side::Buy);
    assert_eq!(btc.size, dec!(0.050));
    assert_eq!(btc.isolated_margin, Some(dec!(250.0)));
    assert_eq!(btc.liq_price, Some(dec!(45000.0)));
    let eth = info
        .positions
        .iter()
        .find(|p| p.symbol == "ETHUSDT")
        .unwrap();
    assert_eq!(eth.side, Side::Sell);
    assert_eq!(eth.size, dec!(1.5));
    // Cross-margin position: no isolated allocation.
    assert!(eth.isolated_margin.is_none());
}

/// Malformed `totalMarginBalance` → zero equity → the
/// parser saturates ratio at 1.0 so the guard's
/// `CancelAll` threshold is guaranteed to trip regardless
/// of the MM field. Better to over-escalate than to
/// silently pass through a near-zero ratio from a drifted
/// schema.
#[test]
fn account_margin_parser_zero_equity_forces_ratio_one() {
    let resp = serde_json::json!({
        "totalMarginBalance": "0",
        "totalInitialMargin": "0",
        "totalMaintMargin": "0",
        "availableBalance": "0",
        "positions": []
    });
    let info = parse_binance_futures_account(&resp).unwrap();
    assert_eq!(info.margin_ratio, Decimal::ONE);
}
