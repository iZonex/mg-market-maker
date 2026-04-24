//! GoldRush (Covalent) `OnchainProvider` impl. Free tier
//! documented at <https://goldrush.dev>.
//!
//! Endpoints hit:
//!   * `GET /v1/{chain}/tokens/{token}/token_holders/`
//!   * `GET /v1/{chain}/address/{addr}/transactions_v3/`
//!   * `GET /v1/{chain}/tokens/{token}/` (metadata)
//!
//! Auth: basic-auth header, API key as username, empty password.
//! Free tier: ~1000 req/day, ~4 req/s.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::time::Duration;

use crate::{
    HolderEntry, OnchainError, OnchainProvider, OnchainResult, TokenMetadata, TransferEntry,
};

const DEFAULT_BASE_URL: &str = "https://api.covalenthq.com";

/// Configuration. `base_url` overrideable for testing against a
/// mock server; defaults to the public endpoint.
#[derive(Debug, Clone)]
pub struct GoldRushConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout_secs: u64,
}

impl GoldRushConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout_secs: 10,
        }
    }
}

pub struct GoldRushProvider {
    config: GoldRushConfig,
    client: reqwest::Client,
}

impl GoldRushProvider {
    pub fn new(config: GoldRushConfig) -> OnchainResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        Ok(Self { config, client })
    }

    async fn get_json(&self, path: &str) -> OnchainResult<serde_json::Value> {
        let url = format!("{}{}", self.config.base_url, path);
        let resp = self
            .client
            .get(&url)
            .basic_auth(&self.config.api_key, Some(""))
            .send()
            .await
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| OnchainError::Decode(e.to_string())),
            StatusCode::TOO_MANY_REQUESTS => {
                Err(OnchainError::RateLimited(format!("goldrush {url}")))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(OnchainError::Auth(format!("goldrush {url}")))
            }
            s => Err(OnchainError::Network(format!("goldrush http {s}"))),
        }
    }
}

#[async_trait]
impl OnchainProvider for GoldRushProvider {
    fn name(&self) -> &str {
        "goldrush"
    }

    async fn get_top_holders(
        &self,
        chain: &str,
        token: &str,
        limit: u32,
    ) -> OnchainResult<Vec<HolderEntry>> {
        let path = format!("/v1/{chain}/tokens/{token}/token_holders/?page-size={limit}");
        let v = self.get_json(&path).await?;
        let items = v
            .get("data")
            .and_then(|d| d.get("items"))
            .and_then(|i| i.as_array())
            .ok_or_else(|| OnchainError::Decode("goldrush token_holders: missing items".into()))?;
        let mut out = Vec::with_capacity(items.len());
        for row in items {
            let address = row
                .get("address")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string();
            let balance_s = row.get("balance").and_then(|b| b.as_str()).unwrap_or("0");
            let balance = Decimal::from_str(balance_s).unwrap_or(Decimal::ZERO);
            out.push(HolderEntry {
                address,
                balance,
                label: None,
            });
        }
        Ok(out)
    }

    async fn get_address_transfers(
        &self,
        chain: &str,
        wallet: &str,
        since_ts: DateTime<Utc>,
    ) -> OnchainResult<Vec<TransferEntry>> {
        // GoldRush free tier: transactions_v3 page 0 is newest.
        // We walk only page 0 to stay inside the free budget;
        // callers poll often enough that one page suffices.
        let path = format!("/v1/{chain}/address/{wallet}/transactions_v3/?page-size=100");
        let v = self.get_json(&path).await?;
        let items = v
            .get("data")
            .and_then(|d| d.get("items"))
            .and_then(|i| i.as_array())
            .ok_or_else(|| {
                OnchainError::Decode("goldrush transactions_v3: missing items".into())
            })?;
        let mut out = Vec::new();
        for tx in items {
            let ts_s = tx
                .get("block_signed_at")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let ts = DateTime::parse_from_rfc3339(ts_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            if ts < since_ts {
                break;
            }
            let tx_hash = tx
                .get("tx_hash")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string();
            // Walk log_events for ERC-20 Transfer topics.
            let Some(logs) = tx.get("log_events").and_then(|l| l.as_array()) else {
                continue;
            };
            for log in logs {
                let Some(decoded) = log.get("decoded") else {
                    continue;
                };
                let name = decoded.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name != "Transfer" {
                    continue;
                }
                let params = decoded
                    .get("params")
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut from = String::new();
                let mut to = String::new();
                let mut value = Decimal::ZERO;
                for p in &params {
                    let key = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let val = p.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    match key {
                        "from" => from = val.to_string(),
                        "to" => to = val.to_string(),
                        "value" => value = Decimal::from_str(val).unwrap_or(Decimal::ZERO),
                        _ => {}
                    }
                }
                let token = log
                    .get("sender_address")
                    .and_then(|a| a.as_str())
                    .unwrap_or_default()
                    .to_string();
                out.push(TransferEntry {
                    from,
                    to,
                    token,
                    value,
                    tx_hash: tx_hash.clone(),
                    timestamp: ts,
                });
            }
        }
        Ok(out)
    }

    async fn get_token_metadata(&self, chain: &str, token: &str) -> OnchainResult<TokenMetadata> {
        let path = format!("/v1/{chain}/tokens/{token}/");
        let v = self.get_json(&path).await?;
        let item = v
            .get("data")
            .and_then(|d| d.get("items"))
            .and_then(|i| i.as_array())
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| OnchainError::Decode("goldrush token metadata: missing items".into()))?;
        let symbol = item
            .get("contract_ticker_symbol")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let decimals = item
            .get("contract_decimals")
            .and_then(|d| d.as_u64())
            .unwrap_or(0) as u8;
        let total_supply = item
            .get("total_supply")
            .and_then(|t| t.as_str())
            .map(|s| Decimal::from_str(s).unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO);
        Ok(TokenMetadata {
            chain: chain.to_string(),
            token: token.to_string(),
            symbol,
            decimals,
            total_supply,
        })
    }
}
