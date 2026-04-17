use anyhow::{Context, Result};
use mm_common::{Balance, OrderId, PriceLevel, ProductSpec};
use reqwest::Client;
use rust_decimal::Decimal;
use tracing::{debug, warn};

use crate::error::ExchangeError;
use crate::types::*;

/// Read a response body as text without ever silently dropping
/// errors. When `text()` itself fails (connection reset mid-read,
/// invalid UTF-8) we surface a non-empty marker so the subsequent
/// log line shows that the body is missing for a KNOWN reason
/// instead of being indistinguishable from an empty 401.
async fn read_body_or_error(resp: reqwest::Response) -> String {
    match resp.text().await {
        Ok(body) => body,
        Err(e) => format!("<body read failed: {e}>"),
    }
}

/// Parse a Decimal that MUST come out of a trusted exchange
/// response. A malformed field is not a zero-balance situation —
/// zeroing it silently would tell the trading loop the account
/// is empty and cause it to stop quoting (or over-leverage on
/// locked-balance math). We propagate the parse error instead so
/// the caller can decide to retry, alert, or halt.
fn parse_required_decimal(field: &str, raw: &str) -> Result<Decimal> {
    raw.parse::<Decimal>()
        .with_context(|| format!("failed to parse `{field}` from exchange: {raw:?}"))
}

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
            let body = read_body_or_error(resp).await;
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
            let body = read_body_or_error(resp).await;
            warn!(status = %status, %order_id, body = %body, "cancel failed");
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

    /// Get account balances. Parsing errors on the balance decimal
    /// fields are propagated, not silently defaulted — a malformed
    /// response would otherwise make the trading loop believe the
    /// account is empty (stopping quotes) or leak locked-balance
    /// reservation math (over-leveraging). The caller can decide
    /// whether to retry, alert, or halt.
    pub async fn get_balances(&self) -> Result<Vec<Balance>> {
        let url = format!("{}/api/v1/account/balances", self.base_url);
        let http = self.client.get(&url).send().await?;
        let status = http.status();
        if !status.is_success() {
            let body = read_body_or_error(http).await;
            warn!(status = %status, body = %body, "get_balances failed");
            return Err(ExchangeError::Api {
                status: status.as_u16(),
                message: body,
            }
            .into());
        }
        let resp: Vec<BalanceResponse> = http.json().await?;

        resp.into_iter()
            .map(|b| {
                let total = parse_required_decimal(&format!("{}.total", b.asset), &b.total)?;
                let locked = parse_required_decimal(&format!("{}.locked", b.asset), &b.locked)?;
                let available =
                    parse_required_decimal(&format!("{}.available", b.asset), &b.available)?;
                Ok(Balance {
                    asset: b.asset,
                    wallet: mm_common::types::WalletType::Spot,
                    total,
                    locked,
                    available,
                })
            })
            .collect()
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
