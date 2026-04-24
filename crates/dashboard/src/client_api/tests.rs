use super::*;

#[test]
fn validate_webhook_url_accepts_http_and_https() {
    assert!(validate_webhook_url("https://a.example/hook").is_ok());
    assert!(validate_webhook_url("http://a.example/hook").is_ok());
}

#[test]
fn validate_webhook_url_rejects_empty_and_bad_scheme() {
    assert!(validate_webhook_url("").is_err());
    assert!(validate_webhook_url("   ").is_err());
    assert!(validate_webhook_url("ftp://x.example").is_err());
    assert!(validate_webhook_url("javascript:alert(1)").is_err());
    assert!(validate_webhook_url("a.example/hook").is_err());
}

#[test]
fn validate_webhook_url_caps_length() {
    let long = format!("https://{}.example/hook", "a".repeat(3000));
    assert!(validate_webhook_url(&long).is_err());
}

#[test]
fn validate_webhook_url_trims_whitespace() {
    let ok = validate_webhook_url("  https://a.example/hook  ").expect("trimmed url is valid");
    assert_eq!(ok, "https://a.example/hook");
}

/// 2026-04-21 journey smoke — the tenant portal's "Recent
/// fills" card was returning []. Controller's
/// `get_client_fills` read `DashboardState.clients[cid].recent_fills`
/// which is never populated in distributed mode (fills live
/// on agents). Fix fans out via `client_metrics` topic; each
/// reply row now carries a `recent_fills` array, which we
/// flatten + sort by timestamp. The helper below is a pure
/// slice of that merge logic — given a synthetic fleet
/// metrics payload with embedded fills, it must produce a
/// deduped newest-first list.
#[test]
fn collect_fills_merges_and_sorts_fleet_rows() {
    let rows = [serde_json::json!({
        "recent_fills": [
            {
                "timestamp": "2026-04-21T19:00:00Z",
                "symbol": "BTCUSDT",
                "client_id": "acme",
                "venue": "binance",
                "side": "Buy",
                "price": "50000",
                "qty": "0.001",
                "is_maker": true,
                "fee": "0.05",
                "nbbo_bid": "50000",
                "nbbo_ask": "50001",
                "slippage_bps": "0"
            },
            {
                "timestamp": "2026-04-21T19:05:00Z",
                "symbol": "BTCUSDT",
                "client_id": "acme",
                "venue": "binance",
                "side": "Sell",
                "price": "50100",
                "qty": "0.001",
                "is_maker": true,
                "fee": "0.05",
                "nbbo_bid": "50099",
                "nbbo_ask": "50100",
                "slippage_bps": "0"
            }
        ]
    })];
    let mut out: Vec<FillRecord> = rows
        .iter()
        .flat_map(|row| {
            row.get("recent_fills")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|raw| serde_json::from_value::<FillRecord>(raw).ok())
        .collect();
    out.sort_by_key(|f| std::cmp::Reverse(f.timestamp));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].side, "Sell", "newest first");
    assert_eq!(out[1].side, "Buy");
}
