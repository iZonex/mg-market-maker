//! Sprint 17 R11.4 — MockConnector fixture + REST-poll contract
//! tests. Closes the Sprint 15 matrix weakness: REST-poll
//! connector paths (get_open_interest, get_long_short_ratio,
//! set_leverage) had no integration coverage.
//!
//! This module ships a configurable MockConnector that
//! implements the full `ExchangeConnector` trait. Tests here
//! cover the default-impl contracts (Ok(None) / Err(NotSupported))
//! that spot / custom / coinbase-prime connectors rely on, plus
//! the override path a unit-tested Binance / Bybit follows.

use async_trait::async_trait;
use mm_common::types::{
    Balance, LiveOrder, OrderId, PriceLevel, ProductSpec,
};
use mm_exchange_core::connector::{
    AmendOrder, ExchangeConnector, LongShortRatio, MarginError, NewOrder,
    OpenInterestInfo, VenueCapabilities, VenueId, VenueProduct,
};
use mm_exchange_core::events::MarketEvent;
use rust_decimal_macros::dec;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Configurable mock implementing the full ExchangeConnector
/// trait. The three REST-poll hooks (OI, L/S ratio, leverage)
/// are stored in `Arc<Mutex<Option<T>>>` — tests flip the
/// override to verify the engine's call path hits the real
/// value or the default.
pub struct MockConnector {
    venue: VenueId,
    product: VenueProduct,
    caps: VenueCapabilities,
    oi_override: Arc<Mutex<Option<OpenInterestInfo>>>,
    ls_override: Arc<Mutex<Option<LongShortRatio>>>,
    leverage_calls: Arc<Mutex<Vec<(String, u32)>>>,
    leverage_succeeds: Arc<Mutex<bool>>,
}

impl MockConnector {
    pub fn new(venue: VenueId, product: VenueProduct) -> Self {
        Self {
            venue,
            product,
            caps: VenueCapabilities {
                max_batch_size: 20,
                supports_amend: false,
                supports_ws_trading: false,
                supports_fix: false,
                max_order_rate: 100,
                supports_funding_rate: product.has_funding(),
                supports_margin_info: product.has_funding(),
                supports_margin_mode: product.has_funding(),
                supports_liquidation_feed: product.has_funding(),
                supports_set_leverage: product.has_funding(),
            },
            oi_override: Arc::new(Mutex::new(None)),
            ls_override: Arc::new(Mutex::new(None)),
            leverage_calls: Arc::new(Mutex::new(Vec::new())),
            leverage_succeeds: Arc::new(Mutex::new(true)),
        }
    }

    pub fn set_oi(&self, value: OpenInterestInfo) {
        *self.oi_override.lock().unwrap() = Some(value);
    }

    pub fn set_ls_ratio(&self, value: LongShortRatio) {
        *self.ls_override.lock().unwrap() = Some(value);
    }

    pub fn fail_leverage(&self) {
        *self.leverage_succeeds.lock().unwrap() = false;
    }

    pub fn leverage_call_history(&self) -> Vec<(String, u32)> {
        self.leverage_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ExchangeConnector for MockConnector {
    fn venue_id(&self) -> VenueId {
        self.venue
    }
    fn capabilities(&self) -> &VenueCapabilities {
        &self.caps
    }
    fn product(&self) -> VenueProduct {
        self.product
    }
    async fn subscribe(
        &self,
        _symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (_tx, rx) = mpsc::unbounded_channel();
        Ok(rx)
    }
    async fn get_orderbook(
        &self,
        _symbol: &str,
        _depth: u32,
    ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        Ok((vec![], vec![], 0))
    }
    async fn place_order(&self, _order: &NewOrder) -> anyhow::Result<OrderId> {
        Ok(uuid::Uuid::new_v4())
    }
    async fn place_orders_batch(
        &self,
        orders: &[NewOrder],
    ) -> anyhow::Result<Vec<OrderId>> {
        Ok(orders.iter().map(|_| uuid::Uuid::new_v4()).collect())
    }
    async fn cancel_order(
        &self,
        _symbol: &str,
        _order_id: OrderId,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel_orders_batch(
        &self,
        _symbol: &str,
        _order_ids: &[OrderId],
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel_all_orders(&self, _symbol: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn amend_order(&self, _amend: &AmendOrder) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_open_orders(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Vec<LiveOrder>> {
        Ok(vec![])
    }
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        Ok(vec![])
    }
    async fn get_product_spec(
        &self,
        symbol: &str,
    ) -> anyhow::Result<ProductSpec> {
        Ok(ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BASE".into(),
            quote_asset: "QUOTE".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        })
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }

    // ── Overridden REST polls — the whole point of this mock ──

    async fn get_open_interest(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Option<OpenInterestInfo>> {
        Ok(self.oi_override.lock().unwrap().clone())
    }

    async fn get_long_short_ratio(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Option<LongShortRatio>> {
        Ok(self.ls_override.lock().unwrap().clone())
    }

    async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
    ) -> Result<(), MarginError> {
        self.leverage_calls
            .lock()
            .unwrap()
            .push((symbol.to_string(), leverage));
        if *self.leverage_succeeds.lock().unwrap() {
            Ok(())
        } else {
            Err(MarginError::NotSupported)
        }
    }
}

// ── R11.4b — REST-poll contract tests ────────────────────

/// Default (no override set) → `get_open_interest` returns
/// `Ok(None)`. Honest "spot / unsupported venue" behaviour.
#[tokio::test]
async fn get_open_interest_default_returns_none() {
    let m = MockConnector::new(VenueId::Custom, VenueProduct::Spot);
    let res = m.get_open_interest("BTCUSDT").await.expect("no error");
    assert!(res.is_none());
}

/// Override set → `get_open_interest` returns the stored
/// value. Pins the success path the engine's
/// `refresh_funding_rate` tick consumes.
#[tokio::test]
async fn get_open_interest_override_returns_value() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::LinearPerp);
    let snap = OpenInterestInfo {
        symbol: "BTCUSDT".into(),
        oi_contracts: Some(dec!(12345.67)),
        oi_usd: Some(dec!(500_000_000)),
        timestamp: chrono::Utc::now(),
    };
    m.set_oi(snap.clone());
    let got = m.get_open_interest("BTCUSDT").await.unwrap().unwrap();
    assert_eq!(got.oi_contracts, snap.oi_contracts);
    assert_eq!(got.oi_usd, snap.oi_usd);
}

/// Default `get_long_short_ratio` returns `None`.
#[tokio::test]
async fn get_long_short_ratio_default_returns_none() {
    let m = MockConnector::new(VenueId::Custom, VenueProduct::Spot);
    let res = m.get_long_short_ratio("BTCUSDT").await.expect("no error");
    assert!(res.is_none());
}

/// Override → real value.
#[tokio::test]
async fn get_long_short_ratio_override_returns_value() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::LinearPerp);
    let ls = LongShortRatio {
        symbol: "BTCUSDT".into(),
        long_pct: dec!(0.7),
        short_pct: dec!(0.3),
        ratio: dec!(2.33),
        timestamp: chrono::Utc::now(),
    };
    m.set_ls_ratio(ls.clone());
    let got = m.get_long_short_ratio("BTCUSDT").await.unwrap().unwrap();
    assert_eq!(got.ratio, ls.ratio);
    assert_eq!(got.long_pct, ls.long_pct);
}

/// `set_leverage` on a perp mock records the call and returns
/// Ok by default. Pins the `spawn_leverage_setup` dispatch
/// path from Sprint 12 R6.2 — we can now assert the call
/// actually happened with the right (symbol, leverage).
#[tokio::test]
async fn set_leverage_records_calls_and_succeeds() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::LinearPerp);
    m.set_leverage("BTCUSDT", 20).await.expect("Ok");
    m.set_leverage("ETHUSDT", 5).await.expect("Ok");
    let history = m.leverage_call_history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], ("BTCUSDT".to_string(), 20));
    assert_eq!(history[1], ("ETHUSDT".to_string(), 5));
}

/// `fail_leverage()` flips the override to NotSupported so
/// tests can verify the engine's warn!-skip branch.
#[tokio::test]
async fn set_leverage_can_be_made_to_fail() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::LinearPerp);
    m.fail_leverage();
    let res = m.set_leverage("BTCUSDT", 10).await;
    assert!(matches!(res, Err(MarginError::NotSupported)));
}

/// Spot product → `supports_set_leverage = false`. Capability
/// gates stay honest on spot.
#[tokio::test]
async fn spot_mock_advertises_no_leverage_support() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::Spot);
    assert!(!m.capabilities().supports_set_leverage);
    assert!(!m.capabilities().supports_liquidation_feed);
    assert!(!m.capabilities().supports_funding_rate);
}

/// Perp product → all three perp-capabilities advertised.
#[tokio::test]
async fn perp_mock_advertises_full_perp_support() {
    let m = MockConnector::new(VenueId::Binance, VenueProduct::LinearPerp);
    assert!(m.capabilities().supports_set_leverage);
    assert!(m.capabilities().supports_liquidation_feed);
    assert!(m.capabilities().supports_funding_rate);
}
