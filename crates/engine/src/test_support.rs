//! Shared in-memory mock `ExchangeConnector` for engine tests.
//!
//! Used by both `connector_bundle` unit tests and the dual-engine
//! integration test. Kept inside the engine crate so it does not
//! leak into the public API of `mm-exchange-core`.

#![cfg(test)]

#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;
use mm_common::types::{
    Balance, LiveOrder, OrderId, PriceLevel, ProductSpec, WalletType,
};
use mm_exchange_core::connector::{
    ExchangeConnector, NewOrder, VenueCapabilities, VenueId, VenueProduct,
};
use mm_exchange_core::events::MarketEvent;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;

pub struct MockConnector {
    venue: VenueId,
    product: VenueProduct,
    caps: VenueCapabilities,
    wallet: WalletType,
    events_tx: Mutex<Option<mpsc::UnboundedSender<MarketEvent>>>,
    pub placed: Mutex<Vec<NewOrder>>,
    pub cancelled: Mutex<Vec<OrderId>>,
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
            },
            wallet: product.default_wallet(),
            events_tx: Mutex::new(None),
            placed: Mutex::new(vec![]),
            cancelled: Mutex::new(vec![]),
        }
    }

    /// Push a market event to subscribers. Call **after**
    /// `subscribe()` — before, there is no channel and the event
    /// is dropped.
    pub fn push_event(&self, ev: MarketEvent) {
        if let Some(tx) = self.events_tx.lock().unwrap().as_ref() {
            let _ = tx.send(ev);
        }
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
        Ok((vec![], vec![], 0))
    }
    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        self.placed.lock().unwrap().push(order.clone());
        Ok(OrderId::new_v4())
    }
    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        let mut ids = Vec::with_capacity(orders.len());
        let mut placed = self.placed.lock().unwrap();
        for o in orders {
            placed.push(o.clone());
            ids.push(OrderId::new_v4());
        }
        Ok(ids)
    }
    async fn cancel_order(&self, _symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        self.cancelled.lock().unwrap().push(order_id);
        Ok(())
    }
    async fn cancel_orders_batch(
        &self,
        _symbol: &str,
        order_ids: &[OrderId],
    ) -> anyhow::Result<()> {
        self.cancelled.lock().unwrap().extend_from_slice(order_ids);
        Ok(())
    }
    async fn cancel_all_orders(&self, _symbol: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_open_orders(&self, _symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        Ok(vec![])
    }
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        Ok(vec![Balance {
            asset: "USDT".to_string(),
            wallet: self.wallet,
            total: dec!(100_000),
            locked: dec!(0),
            available: dec!(100_000),
        }])
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
        })
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

