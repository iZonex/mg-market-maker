//! Alchemy `OnchainProvider` impl — JSON-RPC with enhanced
//! methods. EVM-only.
//!
//! Methods hit:
//!   * `alchemy_getAssetTransfers` (wallet history)
//!   * `alchemy_getTokenBalances` + `alchemy_getTokenMetadata`
//!
//! NOTE on holders: Alchemy doesn't have a direct "top holders"
//! endpoint. A Moralis / GoldRush fallback should be used for
//! that; this impl returns `UnsupportedChain` for
//! `get_top_holders` so the operator's configured fallback
//! picks up.
//!
//! Auth: API key in the URL path.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use std::time::Duration;

use crate::{
    HolderEntry, OnchainError, OnchainProvider, OnchainResult, TokenMetadata,
    TransferEntry,
};

#[derive(Debug, Clone)]
pub struct AlchemyConfig {
    pub api_key: String,
    /// `chain_slug -> base_url` mapping. Alchemy exposes one
    /// URL per chain; operator picks the slug in config.
    pub bases: std::collections::HashMap<String, String>,
    pub timeout_secs: u64,
}

impl AlchemyConfig {
    pub fn default_bases(api_key: &str) -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        let root = |net: &str| format!("https://{net}.g.alchemy.com/v2/{api_key}");
        m.insert("eth-mainnet".into(), root("eth-mainnet"));
        m.insert("polygon-mainnet".into(), root("polygon-mainnet"));
        m.insert("arbitrum-mainnet".into(), root("arb-mainnet"));
        m.insert("optimism-mainnet".into(), root("opt-mainnet"));
        m.insert("base-mainnet".into(), root("base-mainnet"));
        m
    }

    pub fn new(api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        let bases = Self::default_bases(&key);
        Self {
            api_key: key,
            bases,
            timeout_secs: 10,
        }
    }
}

pub struct AlchemyProvider {
    config: AlchemyConfig,
    client: reqwest::Client,
}

impl AlchemyProvider {
    pub fn new(config: AlchemyConfig) -> OnchainResult<Self> {
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

    async fn rpc(
        &self,
        chain: &str,
        method: &str,
        params: serde_json::Value,
    ) -> OnchainResult<serde_json::Value> {
        let url = self.base(chain)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| OnchainError::Network(e.to_string()))?;
        match resp.status() {
            StatusCode::OK => {
                let v: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| OnchainError::Decode(e.to_string()))?;
                if let Some(err) = v.get("error") {
                    return Err(OnchainError::Decode(err.to_string()));
                }
                v.get("result")
                    .cloned()
                    .ok_or_else(|| OnchainError::Decode("alchemy rpc: no result".into()))
            }
            StatusCode::TOO_MANY_REQUESTS => {
                Err(OnchainError::RateLimited(format!("alchemy {method}")))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(OnchainError::Auth(format!("alchemy {method}")))
            }
            s => Err(OnchainError::Network(format!("alchemy http {s}"))),
        }
    }

    /// Parse hex string `0x1ab...` into Decimal.
    fn hex_to_decimal(s: &str) -> Decimal {
        let stripped = s.strip_prefix("0x").unwrap_or(s);
        u128::from_str_radix(stripped, 16)
            .map(Decimal::from)
            .unwrap_or(Decimal::ZERO)
    }
}

#[async_trait]
impl OnchainProvider for AlchemyProvider {
    fn name(&self) -> &str {
        "alchemy"
    }

    async fn get_top_holders(
        &self,
        _chain: &str,
        _token: &str,
        _limit: u32,
    ) -> OnchainResult<Vec<HolderEntry>> {
        // Alchemy's enhanced API doesn't offer a holder list.
        // Fail-open — the operator's fallback provider
        // (GoldRush / Moralis) picks up.
        Err(OnchainError::UnsupportedChain(
            "alchemy has no token_holders endpoint — use GoldRush / Moralis".into(),
        ))
    }

    async fn get_address_transfers(
        &self,
        chain: &str,
        wallet: &str,
        since_ts: DateTime<Utc>,
    ) -> OnchainResult<Vec<TransferEntry>> {
        // alchemy_getAssetTransfers — category=erc20 filters
        // to token flow. Block range: walk from a block an
        // hour before `since_ts` to "latest".
        let params = serde_json::json!([{
            "fromAddress": wallet,
            "category": ["erc20"],
            "withMetadata": true,
            "excludeZeroValue": true,
            "maxCount": "0x64",
            "order": "desc"
        }]);
        let v = self.rpc(chain, "alchemy_getAssetTransfers", params).await?;
        let transfers = v
            .get("transfers")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for t in transfers {
            let ts = t
                .get("metadata")
                .and_then(|m| m.get("blockTimestamp"))
                .and_then(|s| s.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            if ts < since_ts {
                break;
            }
            let from = t.get("from").and_then(|a| a.as_str()).unwrap_or_default().to_string();
            let to = t.get("to").and_then(|a| a.as_str()).unwrap_or_default().to_string();
            let token = t
                .get("rawContract")
                .and_then(|rc| rc.get("address"))
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string();
            let value_raw = t
                .get("rawContract")
                .and_then(|rc| rc.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("0x0");
            let value = Self::hex_to_decimal(value_raw);
            let tx_hash = t
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
        let meta = self
            .rpc(chain, "alchemy_getTokenMetadata", serde_json::json!([token]))
            .await?;
        let symbol = meta
            .get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let decimals = meta
            .get("decimals")
            .and_then(|d| d.as_u64())
            .unwrap_or(18) as u8;
        // Alchemy metadata doesn't include total_supply on the
        // free tier. Fall back to a single eth_call if needed
        // or leave zero so consumers that need supply pick a
        // different provider.
        let total_supply = Decimal::ZERO;
        Ok(TokenMetadata {
            chain: chain.to_string(),
            token: token.to_string(),
            symbol,
            decimals,
            total_supply,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_decimal_round_trip() {
        assert_eq!(AlchemyProvider::hex_to_decimal("0x0"), Decimal::ZERO);
        assert_eq!(AlchemyProvider::hex_to_decimal("0x10"), Decimal::from(16));
        assert_eq!(AlchemyProvider::hex_to_decimal("0xff"), Decimal::from(255));
    }
}
