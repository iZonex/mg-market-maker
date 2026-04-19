//! Moralis `OnchainProvider` impl. EVM-only.
//!
//! Endpoints hit:
//!   * `GET /api/v2.2/erc20/{token}/owners?chain=…&limit=…`
//!   * `GET /api/v2.2/wallets/{addr}/history?chain=…`
//!   * `GET /api/v2.2/erc20/metadata?chain=…&addresses[0]=…`
//!
//! Auth: `X-API-Key` header. Free tier: 40k compute-units/day.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::time::Duration;

use crate::{
    HolderEntry, OnchainError, OnchainProvider, OnchainResult, TokenMetadata,
    TransferEntry,
};

const DEFAULT_BASE_URL: &str = "https://deep-index.moralis.io";

#[derive(Debug, Clone)]
pub struct MoralisConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout_secs: u64,
}

impl MoralisConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout_secs: 10,
        }
    }
}

pub struct MoralisProvider {
    config: MoralisConfig,
    client: reqwest::Client,
}

impl MoralisProvider {
    pub fn new(config: MoralisConfig) -> OnchainResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        Ok(Self { config, client })
    }

    /// Moralis uses numeric chain IDs (0x1, 0x38, 0x89, …) or
    /// slugs (`eth`, `bsc`, `polygon`). Normalise our
    /// canonical slug into Moralis's shape.
    fn moralis_chain(chain: &str) -> &str {
        match chain {
            "eth-mainnet" => "eth",
            "bsc-mainnet" => "bsc",
            "polygon-mainnet" => "polygon",
            "arbitrum-mainnet" => "arbitrum",
            "optimism-mainnet" => "optimism",
            "base-mainnet" => "base",
            other => other,
        }
    }

    async fn get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> OnchainResult<serde_json::Value> {
        let url = format!("{}{}", self.config.base_url, path);
        let mut req = self.client.get(&url);
        for (k, v) in query {
            req = req.query(&[(*k, v.as_str())]);
        }
        req = req.header("X-API-Key", &self.config.api_key).header("Accept", "application/json");
        let resp = req
            .send()
            .await
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| OnchainError::Decode(e.to_string())),
            StatusCode::TOO_MANY_REQUESTS => {
                Err(OnchainError::RateLimited(format!("moralis {url}")))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(OnchainError::Auth(format!("moralis {url}")))
            }
            s => Err(OnchainError::Network(format!("moralis http {s}"))),
        }
    }
}

#[async_trait]
impl OnchainProvider for MoralisProvider {
    fn name(&self) -> &str {
        "moralis"
    }

    async fn get_top_holders(
        &self,
        chain: &str,
        token: &str,
        limit: u32,
    ) -> OnchainResult<Vec<HolderEntry>> {
        let path = format!("/api/v2.2/erc20/{token}/owners");
        let v = self
            .get_json(
                &path,
                &[
                    ("chain", Self::moralis_chain(chain).into()),
                    ("limit", limit.to_string()),
                    ("order", "DESC".into()),
                ],
            )
            .await?;
        let items = v
            .get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(items.len());
        for row in items {
            let address = row
                .get("owner_address")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string();
            let balance_s = row
                .get("balance")
                .and_then(|b| b.as_str())
                .unwrap_or("0");
            let balance = Decimal::from_str(balance_s).unwrap_or(Decimal::ZERO);
            let label = row
                .get("owner_address_label")
                .and_then(|l| l.as_str())
                .map(str::to_string);
            out.push(HolderEntry { address, balance, label });
        }
        Ok(out)
    }

    async fn get_address_transfers(
        &self,
        chain: &str,
        wallet: &str,
        since_ts: DateTime<Utc>,
    ) -> OnchainResult<Vec<TransferEntry>> {
        let path = format!("/api/v2.2/wallets/{wallet}/history");
        let v = self
            .get_json(
                &path,
                &[
                    ("chain", Self::moralis_chain(chain).into()),
                    ("from_date", since_ts.to_rfc3339()),
                    ("limit", "100".into()),
                ],
            )
            .await?;
        let items = v
            .get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for tx in items {
            let ts_s = tx.get("block_timestamp").and_then(|s| s.as_str()).unwrap_or("");
            let ts = DateTime::parse_from_rfc3339(ts_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            if ts < since_ts {
                continue;
            }
            let tx_hash = tx
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string();
            let Some(transfers) = tx.get("erc20_transfers").and_then(|t| t.as_array()) else {
                continue;
            };
            for t in transfers {
                let from = t.get("from_address").and_then(|a| a.as_str()).unwrap_or_default().to_string();
                let to = t.get("to_address").and_then(|a| a.as_str()).unwrap_or_default().to_string();
                let token = t.get("address").and_then(|a| a.as_str()).unwrap_or_default().to_string();
                let value_s = t.get("value").and_then(|v| v.as_str()).unwrap_or("0");
                let value = Decimal::from_str(value_s).unwrap_or(Decimal::ZERO);
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

    async fn get_token_metadata(
        &self,
        chain: &str,
        token: &str,
    ) -> OnchainResult<TokenMetadata> {
        let v = self
            .get_json(
                "/api/v2.2/erc20/metadata",
                &[
                    ("chain", Self::moralis_chain(chain).into()),
                    ("addresses[0]", token.into()),
                ],
            )
            .await?;
        let item = v
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| OnchainError::Decode("moralis erc20 metadata empty".into()))?;
        let symbol = item
            .get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let decimals = item
            .get("decimals")
            .and_then(|d| d.as_str())
            .and_then(|s| s.parse::<u8>().ok())
            .or_else(|| item.get("decimals").and_then(|d| d.as_u64()).map(|u| u as u8))
            .unwrap_or(18);
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
