//! Shared in-memory mock `ExchangeConnector` for engine tests.
//!
//! Used by both `connector_bundle` unit tests and the dual-engine
//! integration test. Kept inside the engine crate so it does not
//! leak into the public API of `mm-exchange-core`.

#![cfg(test)]
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;
use mm_common::types::{Balance, LiveOrder, OrderId, PriceLevel, ProductSpec, WalletType};
use mm_exchange_core::connector::{
    ExchangeConnector, NewOrder, VenueCapabilities, VenueId, VenueProduct,
};
use mm_exchange_core::events::MarketEvent;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;

pub struct MockConnector {
    venue: VenueId,
    product: VenueProduct,
    caps: VenueCapabilities,
    wallet: WalletType,
    events_tx: Mutex<Option<mpsc::UnboundedSender<MarketEvent>>>,
    /// Stubbed top-of-book used by [`MockConnector::get_orderbook`].
    /// Defaults to empty; call [`MockConnector::set_mid`] to feed
    /// the stat-arb driver / any other engine code that pulls
    /// mids through the connector trait.
    orderbook: Mutex<(Vec<PriceLevel>, Vec<PriceLevel>)>,
    pub placed: Mutex<Vec<NewOrder>>,
    pub cancelled: Mutex<Vec<OrderId>>,
    /// Epic E sub-component #1 — call-path counters so tests
    /// can assert the engine routed through the batch helpers
    /// (and not the per-order fallback) on a multi-quote diff.
    pub place_batch_calls: Mutex<usize>,
    pub place_single_calls: Mutex<usize>,
    pub cancel_batch_calls: Mutex<usize>,
    pub cancel_single_calls: Mutex<usize>,
    /// One-shot batch-failure injection. When `true`, the next
    /// `place_orders_batch` (or `cancel_orders_batch`) call
    /// returns `Err` and clears the flag. Used to test the
    /// per-order fallback path.
    fail_next_batch_place: Mutex<bool>,
    fail_next_batch_cancel: Mutex<bool>,
    /// Epic F listing sniper (stage-2): controllable output for
    /// `list_symbols`. `None` (the default) makes `list_symbols`
    /// return `Err(unsupported)` — the same shape the trait's
    /// default impl uses for venues without a public symbol
    /// endpoint. `Some(Ok(vec))` returns the vec; `Some(Err(_))`
    /// simulates a connector-side failure.
    list_symbols_response: Mutex<Option<Result<Vec<ProductSpec>, String>>>,
    /// Epic 2 (cancel_all verification): controllable response
    /// for `get_open_orders`. Empty by default — tests that want
    /// to simulate a surviving order after cancel_all push ids
    /// here via [`MockConnector::set_open_orders`].
    open_orders: Mutex<Vec<LiveOrder>>,
    /// Sprint 18 R12.1 — REST-poll override hooks. Mirror the
    /// `crates/exchange/core/tests/mock_connector_contracts.rs`
    /// Sprint 17 fixture so engine-level tests can drive the
    /// same semantics without duplicating trait impls.
    oi_override: Mutex<Option<mm_exchange_core::connector::OpenInterestInfo>>,
    ls_override: Mutex<Option<mm_exchange_core::connector::LongShortRatio>>,
    leverage_calls: Mutex<Vec<(String, u32)>>,
    leverage_succeeds: Mutex<bool>,
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
                // Sprint 18 — honest perp-vs-spot semantics so
                // spawn_leverage_setup + liquidation-feed gated
                // paths find the right capability on perp mocks.
                supports_liquidation_feed: product.has_funding(),
                supports_set_leverage: product.has_funding(),
            },
            wallet: product.default_wallet(),
            events_tx: Mutex::new(None),
            orderbook: Mutex::new((vec![], vec![])),
            placed: Mutex::new(vec![]),
            cancelled: Mutex::new(vec![]),
            place_batch_calls: Mutex::new(0),
            place_single_calls: Mutex::new(0),
            cancel_batch_calls: Mutex::new(0),
            cancel_single_calls: Mutex::new(0),
            fail_next_batch_place: Mutex::new(false),
            fail_next_batch_cancel: Mutex::new(false),
            list_symbols_response: Mutex::new(None),
            open_orders: Mutex::new(Vec::new()),
            oi_override: Mutex::new(None),
            ls_override: Mutex::new(None),
            leverage_calls: Mutex::new(Vec::new()),
            leverage_succeeds: Mutex::new(true),
        }
    }

    /// Sprint 18 R12.1 — override the mock's
    /// `get_open_interest` return value so `refresh_funding_rate`
    /// tests can assert `last_open_interest` populated.
    pub fn set_oi(&self, value: mm_exchange_core::connector::OpenInterestInfo) {
        *self.oi_override.lock().unwrap() = Some(value);
    }

    /// Sprint 18 R12.1 — override `get_long_short_ratio`.
    pub fn set_ls_ratio(&self, value: mm_exchange_core::connector::LongShortRatio) {
        *self.ls_override.lock().unwrap() = Some(value);
    }

    /// Sprint 18 R12.1 — flip set_leverage to return
    /// `NotSupported` on next call. Used by the
    /// `spawn_leverage_setup` fallback path tests.
    pub fn fail_leverage(&self) {
        *self.leverage_succeeds.lock().unwrap() = false;
    }

    /// Sprint 18 R12.1 — read the recorded `set_leverage`
    /// call history. Each entry is `(symbol, leverage)` as
    /// passed by the caller.
    pub fn leverage_call_history(&self) -> Vec<(String, u32)> {
        self.leverage_calls.lock().unwrap().clone()
    }

    /// Sprint 18 — update the capability flags post-construction.
    /// Useful when a test needs to pretend a spot venue supports
    /// something it normally wouldn't (or vice-versa).
    pub fn set_caps(&mut self, caps: VenueCapabilities) {
        self.caps = caps;
    }

    /// Set the venue's open-order set seen by
    /// `get_open_orders`. Tests that want to simulate a
    /// cancel_all that leaves survivors call this with the
    /// surviving `LiveOrder` so the verification pass picks
    /// them up.
    pub fn set_open_orders(&self, orders: Vec<LiveOrder>) {
        *self.open_orders.lock().unwrap() = orders;
    }

    /// Epic F listing sniper (stage-2): program the next (and
    /// every subsequent) `list_symbols` call to return `Ok(specs)`.
    pub fn set_list_symbols_ok(&self, specs: Vec<ProductSpec>) {
        *self.list_symbols_response.lock().unwrap() = Some(Ok(specs));
    }

    /// Program `list_symbols` to return a connector-side error.
    /// Used by the sniper's "connector Err → scan returns Err"
    /// unit test.
    pub fn set_list_symbols_err(&self, msg: impl Into<String>) {
        *self.list_symbols_response.lock().unwrap() = Some(Err(msg.into()));
    }

    /// Builder: override `max_batch_size` from the default.
    /// Used by Epic E batch-entry tests to exercise both small
    /// (5) and large (20) chunk sizes.
    pub fn with_max_batch_size(mut self, n: usize) -> Self {
        self.caps.max_batch_size = n;
        self
    }

    /// Arm the next `place_orders_batch` call to return `Err`
    /// once. Cleared after the failure fires. Used to test the
    /// per-order fallback path.
    pub fn arm_batch_place_failure(&self) {
        *self.fail_next_batch_place.lock().unwrap() = true;
    }

    /// Arm the next `cancel_orders_batch` call to return `Err`.
    pub fn arm_batch_cancel_failure(&self) {
        *self.fail_next_batch_cancel.lock().unwrap() = true;
    }

    pub fn place_batch_calls(&self) -> usize {
        *self.place_batch_calls.lock().unwrap()
    }
    pub fn place_single_calls(&self) -> usize {
        *self.place_single_calls.lock().unwrap()
    }
    pub fn cancel_batch_calls(&self) -> usize {
        *self.cancel_batch_calls.lock().unwrap()
    }
    pub fn cancel_single_calls(&self) -> usize {
        *self.cancel_single_calls.lock().unwrap()
    }

    /// Push a market event to subscribers. Call **after**
    /// `subscribe()` — before, there is no channel and the event
    /// is dropped.
    pub fn push_event(&self, ev: MarketEvent) {
        if let Some(tx) = self.events_tx.lock().unwrap().as_ref() {
            let _ = tx.send(ev);
        }
    }

    /// Set a synthetic top-of-book derived from `mid`. Used by
    /// driver-level tests that pull mids through the connector
    /// trait (stat-arb, funding-arb). Bids sit one unit below,
    /// asks one unit above; both sides carry qty=10.
    pub fn set_mid(&self, mid: Decimal) {
        *self.orderbook.lock().unwrap() = (
            vec![PriceLevel {
                price: mid - dec!(1),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: mid + dec!(1),
                qty: dec!(10),
            }],
        );
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
        let (tx, rx) = mpsc::unbounded_channel();
        *self.events_tx.lock().unwrap() = Some(tx);
        Ok(rx)
    }
    async fn get_orderbook(
        &self,
        _symbol: &str,
        _depth: u32,
    ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        let (bids, asks) = self.orderbook.lock().unwrap().clone();
        Ok((bids, asks, 0))
    }
    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        *self.place_single_calls.lock().unwrap() += 1;
        self.placed.lock().unwrap().push(order.clone());
        Ok(OrderId::new_v4())
    }
    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        *self.place_batch_calls.lock().unwrap() += 1;
        // One-shot failure injection.
        {
            let mut flag = self.fail_next_batch_place.lock().unwrap();
            if *flag {
                *flag = false;
                return Err(anyhow::anyhow!("injected batch place failure"));
            }
        }
        let mut ids = Vec::with_capacity(orders.len());
        let mut placed = self.placed.lock().unwrap();
        for o in orders {
            placed.push(o.clone());
            ids.push(OrderId::new_v4());
        }
        Ok(ids)
    }
    async fn cancel_order(&self, _symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        *self.cancel_single_calls.lock().unwrap() += 1;
        self.cancelled.lock().unwrap().push(order_id);
        Ok(())
    }
    async fn cancel_orders_batch(
        &self,
        _symbol: &str,
        order_ids: &[OrderId],
    ) -> anyhow::Result<()> {
        *self.cancel_batch_calls.lock().unwrap() += 1;
        {
            let mut flag = self.fail_next_batch_cancel.lock().unwrap();
            if *flag {
                *flag = false;
                return Err(anyhow::anyhow!("injected batch cancel failure"));
            }
        }
        self.cancelled.lock().unwrap().extend_from_slice(order_ids);
        Ok(())
    }
    async fn cancel_all_orders(&self, _symbol: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_open_orders(&self, _symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        Ok(self.open_orders.lock().unwrap().clone())
    }
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        // Return both USDT and BTC so that engine tests
        // calling `refresh_balances` populate the cache for
        // both ask-side (BTC base) and bid-side (USDT quote)
        // affordability checks.
        Ok(vec![
            Balance {
                asset: "USDT".to_string(),
                wallet: self.wallet,
                total: dec!(100_000),
                locked: dec!(0),
                available: dec!(100_000),
            },
            Balance {
                asset: "BTC".to_string(),
                wallet: self.wallet,
                total: dec!(10),
                locked: dec!(0),
                available: dec!(10),
            },
        ])
    }
    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        Ok(ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        })
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }

    // Sprint 18 R12.1 — REST-poll overrides. Each mirrors the
    // semantics of the cross-crate
    // `crates/exchange/core/tests/mock_connector_contracts.rs`
    // fixture: override set → return value, unset → trait
    // default (`Ok(None)` / `Err(NotSupported)`).
    async fn get_open_interest(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Option<mm_exchange_core::connector::OpenInterestInfo>> {
        Ok(self.oi_override.lock().unwrap().clone())
    }

    async fn get_long_short_ratio(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Option<mm_exchange_core::connector::LongShortRatio>> {
        Ok(self.ls_override.lock().unwrap().clone())
    }

    async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
    ) -> Result<(), mm_exchange_core::connector::MarginError> {
        self.leverage_calls
            .lock()
            .unwrap()
            .push((symbol.to_string(), leverage));
        if *self.leverage_succeeds.lock().unwrap() {
            Ok(())
        } else {
            Err(mm_exchange_core::connector::MarginError::NotSupported)
        }
    }

    /// Epic F listing sniper (stage-2): honour the programmed
    /// response set via `set_list_symbols_ok` /
    /// `set_list_symbols_err`. If nothing is programmed, fall
    /// through to the trait default (`Err(unsupported)`) so
    /// pre-existing tests that never touched the field keep the
    /// "venue doesn't expose this" semantics.
    async fn list_symbols(&self) -> anyhow::Result<Vec<ProductSpec>> {
        let programmed = self.list_symbols_response.lock().unwrap().clone();
        match programmed {
            Some(Ok(specs)) => Ok(specs),
            Some(Err(msg)) => Err(anyhow::anyhow!(msg)),
            None => Err(anyhow::anyhow!("list_symbols not supported on this venue")),
        }
    }
}
