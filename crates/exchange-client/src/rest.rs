use anyhow::Result;
use mm_common::{Balance, OrderId, PriceLevel, ProductSpec};
use reqwest::Client;
use rust_decimal::Decimal;
use tracing::{debug, warn};

use crate::error::ExchangeError;
use crate::types::*;

/// HTTP client for the exchange REST API.
pub struct ExchangeRestClient {
    client: Client,
    base_url: String,
}

impl ExchangeRestClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Place an order. Returns order ID and immediate fills.
    pub async fn place_order(&self, req: &PlaceOrderRequest) -> Result<PlaceOrderResponse> {
        let url = format!("{}/api/v1/orders", self.base_url);
        debug!(?req, "placing order");

        let resp = self.client.post(&url).json(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "place order failed");
            return Err(ExchangeError::Api {
                status: status.as_u16(),
                message: body,
            }
            .into());
        }
        Ok(resp.json().await?)
    }

    /// Cancel an order by ID.
    pub async fn cancel_order(&self, order_id: OrderId) -> Result<CancelOrderResponse> {
        let url = format!("{}/api/v1/orders/{}", self.base_url, order_id);
        debug!(%order_id, "cancelling order");

        let resp = self.client.delete(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, %order_id, "cancel failed");
            return Err(ExchangeError::Api {
                status: status.as_u16(),
                message: body,
            }
            .into());
        }
        Ok(resp.json().await?)
    }

    /// Get the L2 orderbook snapshot.
    pub async fn get_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        let url = format!(
            "{}/api/v1/markets/{}/orderbook?depth={}",
            self.base_url, symbol, depth
        );
        let resp: OrderbookResponse = self.client.get(&url).send().await?.json().await?;

        let bids = parse_levels(&resp.bids)?;
        let asks = parse_levels(&resp.asks)?;
        Ok((bids, asks, resp.sequence))
    }

    /// Get account balances.
    pub async fn get_balances(&self) -> Result<Vec<Balance>> {
        let url = format!("{}/api/v1/account/balances", self.base_url);
        let resp: Vec<BalanceResponse> = self.client.get(&url).send().await?.json().await?;

        Ok(resp
            .into_iter()
            .map(|b| Balance {
                asset: b.asset,
                total: b.total.parse().unwrap_or_default(),
                locked: b.locked.parse().unwrap_or_default(),
                available: b.available.parse().unwrap_or_default(),
            })
            .collect())
    }

    /// Get recent trades for a symbol.
    pub async fn get_recent_trades(&self, symbol: &str) -> Result<Vec<mm_common::Trade>> {
        let url = format!("{}/api/v1/markets/{}/trades", self.base_url, symbol);
        Ok(self.client.get(&url).send().await?.json().await?)
    }

    /// Health check.
    pub async fn health(&self) -> Result<bool> {
        let url = format!("{}/health", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// Fetch product spec (if the exchange exposes it, otherwise use config).
    pub async fn get_product(&self, _symbol: &str) -> Result<Option<ProductSpec>> {
        // The exchange doesn't have a public products endpoint yet.
        // Return None and let the caller use config-based specs.
        Ok(None)
    }
}

fn parse_levels(raw: &[[String; 2]]) -> Result<Vec<PriceLevel>> {
    raw.iter()
        .map(|pair| {
            Ok(PriceLevel {
                price: pair[0].parse::<Decimal>()?,
                qty: pair[1].parse::<Decimal>()?,
            })
        })
        .collect()
}
