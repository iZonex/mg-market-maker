//! Etherscan-family `OnchainProvider` impl. One code path
//! covers Etherscan + BscScan + PolygonScan + ArbiScan +
//! OptimisticEtherscan — the APIs are byte-identical, only the
//! base URL + API key differ per chain.
//!
//! Endpoints hit:
//!   * `?module=token&action=tokenholderlist&contractaddress=…`
//!     (PRO tier — free tier returns 403; the impl gracefully
//!     degrades to `UnsupportedChain` so the fallback provider
//!     can pick up)
//!   * `?module=account&action=tokentx&address=…` (free tier)
//!   * `?module=token&action=tokeninfo&contractaddress=…`
//!
//! Auth: `apikey` query param. Free tier: 5 req/s, 100k/day.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use crate::{
    HolderEntry, OnchainError, OnchainProvider, OnchainResult, TokenMetadata,
    TransferEntry,
};

#[derive(Debug, Clone)]
pub struct EtherscanFamilyConfig {
    /// API key. Etherscan's free tier gives one key that works
    /// across BscScan / PolygonScan / ArbiScan as well in
    /// 2026 — one key, one rate bucket.
    pub api_key: String,
    /// `chain_slug -> base_url` mapping. Operator picks the
    /// slug in their config; the provider translates here.
    pub bases: HashMap<String, String>,
    pub timeout_secs: u64,
}

impl EtherscanFamilyConfig {
    /// Default mapping covering the common chains.
    pub fn default_bases() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("eth-mainnet".into(), "https://api.etherscan.io/api".into());
        m.insert("bsc-mainnet".into(), "https://api.bscscan.com/api".into());
        m.insert(
            "polygon-mainnet".into(),
            "https://api.polygonscan.com/api".into(),
        );
        m.insert(
            "arbitrum-mainnet".into(),
            "https://api.arbiscan.io/api".into(),
        );
        m.insert(
            "optimism-mainnet".into(),
            "https://api-optimistic.etherscan.io/api".into(),
        );
        m
    }

    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            bases: Self::default_bases(),
            timeout_secs: 10,
        }
    }
}

pub struct EtherscanFamilyProvider {
    config: EtherscanFamilyConfig,
    client: reqwest::Client,
}

impl EtherscanFamilyProvider {
    pub fn new(config: EtherscanFamilyConfig) -> OnchainResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        Ok(Self { config, client })
    }

    fn base(&self, chain: &str) -> OnchainResult<&str> {
        self.config
            .bases
            .get(chain)
            .map(String::as_str)
            .ok_or_else(|| OnchainError::UnsupportedChain(chain.to_string()))
    }

    async fn get_json(
        &self,
        chain: &str,
        query: &[(&str, String)],
    ) -> OnchainResult<serde_json::Value> {
        let base = self.base(chain)?;
        let mut req = self.client.get(base);
        for (k, v) in query {
            req = req.query(&[(*k, v.as_str())]);
        }
        req = req.query(&[("apikey", self.config.api_key.as_str())]);
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
                Err(OnchainError::RateLimited(format!("etherscan {chain}")))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(OnchainError::Auth(format!("etherscan {chain}")))
            }
            s => Err(OnchainError::Network(format!("etherscan http {s}"))),
        }
    }
}

#[async_trait]
impl OnchainProvider for EtherscanFamilyProvider {
    fn name(&self) -> &str {
        "etherscan"
    }

    async fn get_top_holders(
        &self,
        chain: &str,
        token: &str,
        limit: u32,
    ) -> OnchainResult<Vec<HolderEntry>> {
        let v = self
            .get_json(
                chain,
                &[
                    ("module", "token".into()),
                    ("action", "tokenholderlist".into()),
                    ("contractaddress", token.into()),
                    ("page", "1".into()),
                    ("offset", limit.to_string()),
                ],
            )
            .await?;
        // Etherscan wraps success OR error in the same shape.
        // "status":"0" + "message":"NOTOK" = usually PRO-tier
        // endpoint locked for the free key.
        if v.get("status").and_then(|s| s.as_str()) == Some("0") {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("etherscan error");
            if msg.contains("Max") || msg.contains("rate") {
                return Err(OnchainError::RateLimited(msg.into()));
            }
            // Free tier usually hits here — fail-open via the
            // UnsupportedChain variant so the fallback provider
            // can take over.
            return Err(OnchainError::UnsupportedChain(
                "etherscan tokenholderlist requires PRO tier".into(),
            ));
        }
        let items = v
            .get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(items.len());
        for row in items {
            let address = row
                .get("TokenHolderAddress")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string();
            let balance_s = row
                .get("TokenHolderQuantity")
                .and_then(|b| b.as_str())
                .unwrap_or("0");
            let balance = Decimal::from_str(balance_s).unwrap_or(Decimal::ZERO);
            out.push(HolderEntry { address, balance, label: None });
        }
        Ok(out)
    }

    async fn get_address_transfers(
        &self,
        chain: &str,
        wallet: &str,
        since_ts: DateTime<Utc>,
    ) -> OnchainResult<Vec<TransferEntry>> {
        let v = self
            .get_json(
                chain,
                &[
                    ("module", "account".into()),
                    ("action", "tokentx".into()),
                    ("address", wallet.into()),
                    ("startblock", "0".into()),
                    ("endblock", "99999999".into()),
                    ("sort", "desc".into()),
                ],
            )
            .await?;
        if v.get("status").and_then(|s| s.as_str()) == Some("0") {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if msg == "No transactions found" {
                return Ok(Vec::new());
            }
            if msg.contains("Max") || msg.contains("rate") {
                return Err(OnchainError::RateLimited(msg.into()));
            }
            return Err(OnchainError::Decode(msg.to_string()));
        }
        let items = v
            .get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for row in items {
            let ts_s = row
                .get("timeStamp")
                .and_then(|t| t.as_str())
                .unwrap_or("0");
            let ts_i = ts_s.parse::<i64>().unwrap_or(0);
            let ts = DateTime::from_timestamp(ts_i, 0).unwrap_or_else(Utc::now);
            if ts < since_ts {
                break;
            }
            let from = row.get("from").and_then(|a| a.as_str()).unwrap_or_default().to_string();
            let to = row.get("to").and_then(|a| a.as_str()).unwrap_or_default().to_string();
            let token = row
                .get("contractAddress")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string();
            let value_s = row
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let value = Decimal::from_str(value_s).unwrap_or(Decimal::ZERO);
            let tx_hash = row
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string();
            out.push(TransferEntry { from, to, token, value, tx_hash, timestamp: ts });
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
                chain,
                &[
                    ("module", "token".into()),
                    ("action", "tokeninfo".into()),
                    ("contractaddress", token.into()),
                ],
            )
            .await?;
        if v.get("status").and_then(|s| s.as_str()) == Some("0") {
            return Err(OnchainError::UnsupportedChain(
                "etherscan tokeninfo PRO tier".into(),
            ));
        }
        let item = v
            .get("result")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| OnchainError::Decode("etherscan tokeninfo empty".into()))?;
        let symbol = item
            .get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let decimals = item
            .get("divisor")
            .and_then(|d| d.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .map(|d| (d as f64).log10() as u8)
            .unwrap_or(18);
        let total_supply = item
            .get("totalSupply")
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
