//! Holder-concentration cache.
//!
//! Top-N holder lookups are expensive (heavy JSON, sorted full
//! scan on the provider side) AND the answer rarely changes —
//! a rug-deploy supply distribution stays static for hours /
//! days. A per-token TTL cache keeps us inside the free-tier
//! budget without sacrificing freshness.
//!
//! The cache is write-through — a miss triggers one fetch
//! against the underlying provider, the result is stored with
//! the current timestamp, and subsequent reads within `ttl`
//! return the cached snapshot. On provider error, the cache
//! falls back to the last good entry if one exists — better
//! a minute-stale number than a gap that makes the graph
//! source emit `Missing`.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{HolderEntry, OnchainProvider, OnchainResult};

/// Per-token snapshot. `concentration_pct` is the ratio of
/// top-N holder balance to total supply, clamped to [0, 1].
#[derive(Debug, Clone)]
pub struct ConcentrationSnapshot {
    pub chain: String,
    pub token: String,
    pub top_n: u32,
    pub concentration_pct: Decimal,
    pub top_holders: Vec<HolderEntry>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct HolderConcentrationConfig {
    /// How many top holders to fetch + sum. Default 10 matches
    /// the ZachXBT RAVE write-up ("9 wallets control 95%").
    pub top_n: u32,
    /// TTL for a cached snapshot. Default 1 hour — supply
    /// distribution drifts slowly for most rugs.
    pub ttl: Duration,
}

impl Default for HolderConcentrationConfig {
    fn default() -> Self {
        Self {
            top_n: 10,
            ttl: Duration::hours(1),
        }
    }
}

pub struct HolderConcentrationCache {
    provider: Arc<dyn OnchainProvider>,
    config: HolderConcentrationConfig,
    snapshots: RwLock<HashMap<String, ConcentrationSnapshot>>,
}

impl HolderConcentrationCache {
    pub fn new(
        provider: Arc<dyn OnchainProvider>,
        config: HolderConcentrationConfig,
    ) -> Self {
        Self {
            provider,
            config,
            snapshots: RwLock::new(HashMap::new()),
        }
    }

    fn key(chain: &str, token: &str) -> String {
        format!("{chain}|{}", token.to_lowercase())
    }

    /// Serve the latest concentration view for `(chain, token)`.
    /// Returns the cached snapshot if under TTL; otherwise
    /// refreshes via the provider. On provider error, returns
    /// the last cached snapshot if one exists, else propagates.
    pub async fn get(
        &self,
        chain: &str,
        token: &str,
    ) -> OnchainResult<ConcentrationSnapshot> {
        let key = Self::key(chain, token);
        let now = Utc::now();
        {
            let g = self.snapshots.read().await;
            if let Some(snap) = g.get(&key) {
                if now.signed_duration_since(snap.computed_at) < self.config.ttl {
                    return Ok(snap.clone());
                }
            }
        }
        // Miss or stale — try to refresh.
        match self.fetch(chain, token).await {
            Ok(fresh) => {
                let mut g = self.snapshots.write().await;
                g.insert(key, fresh.clone());
                Ok(fresh)
            }
            Err(e) => {
                // Provider error — serve stale if we have any.
                let g = self.snapshots.read().await;
                if let Some(snap) = g.get(&key) {
                    tracing::warn!(
                        token = %token,
                        error = %e,
                        "holder concentration fetch failed; serving stale snapshot"
                    );
                    return Ok(snap.clone());
                }
                Err(e)
            }
        }
    }

    async fn fetch(
        &self,
        chain: &str,
        token: &str,
    ) -> OnchainResult<ConcentrationSnapshot> {
        let holders = self
            .provider
            .get_top_holders(chain, token, self.config.top_n)
            .await?;
        let top_n_sum: Decimal = holders.iter().map(|h| h.balance).sum();
        // Compute total supply by summing — providers that
        // return ranked holders generally don't include the
        // absolute supply in the holder response, so we'd
        // need a second metadata fetch. Cheaper: fetch it
        // once.
        let metadata = self.provider.get_token_metadata(chain, token).await.ok();
        let total_supply = metadata
            .as_ref()
            .map(|m| m.total_supply)
            .filter(|v| !v.is_zero())
            .unwrap_or(top_n_sum);
        let concentration_pct = if total_supply.is_zero() {
            Decimal::ZERO
        } else {
            (top_n_sum / total_supply)
                .min(rust_decimal_macros::dec!(1))
                .max(Decimal::ZERO)
        };
        Ok(ConcentrationSnapshot {
            chain: chain.to_string(),
            token: token.to_string(),
            top_n: self.config.top_n,
            concentration_pct,
            top_holders: holders,
            computed_at: Utc::now(),
        })
    }

    /// Read-only peek — returns the latest cached snapshot
    /// without triggering a refresh. Useful in hot-path
    /// graph-source overlays where we don't want to stall.
    pub async fn peek(&self, chain: &str, token: &str) -> Option<ConcentrationSnapshot> {
        let g = self.snapshots.read().await;
        g.get(&Self::key(chain, token)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OnchainError, TokenMetadata, TransferEntry};
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockProvider {
        calls: AtomicU32,
    }
    #[async_trait]
    impl OnchainProvider for MockProvider {
        fn name(&self) -> &str { "mock" }
        async fn get_top_holders(
            &self, _c: &str, _t: &str, _l: u32,
        ) -> OnchainResult<Vec<HolderEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![
                HolderEntry { address: "0xa".into(), balance: dec!(500), label: None },
                HolderEntry { address: "0xb".into(), balance: dec!(400), label: None },
            ])
        }
        async fn get_address_transfers(
            &self, _c: &str, _w: &str, _s: DateTime<Utc>,
        ) -> OnchainResult<Vec<TransferEntry>> {
            Ok(Vec::new())
        }
        async fn get_token_metadata(
            &self, c: &str, t: &str,
        ) -> OnchainResult<TokenMetadata> {
            Ok(TokenMetadata {
                chain: c.into(),
                token: t.into(),
                symbol: "RAVE".into(),
                decimals: 18,
                total_supply: dec!(1000),
            })
        }
    }

    /// First call triggers a fetch; second call within TTL
    /// returns the cached snapshot without hitting the
    /// provider.
    #[tokio::test]
    async fn caches_within_ttl() {
        let p = Arc::new(MockProvider { calls: AtomicU32::new(0) });
        let cache = HolderConcentrationCache::new(
            p.clone(),
            HolderConcentrationConfig::default(),
        );
        let s1 = cache.get("eth-mainnet", "0xrave").await.unwrap();
        assert_eq!(s1.concentration_pct, dec!(0.9)); // 900/1000
        let s2 = cache.get("eth-mainnet", "0xrave").await.unwrap();
        assert_eq!(s1.computed_at, s2.computed_at);
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    }

    /// On provider error, the cache serves the last good
    /// snapshot instead of failing the tick.
    #[tokio::test]
    async fn serves_stale_on_error() {
        struct FlakyProvider {
            served_ok: std::sync::atomic::AtomicBool,
        }
        #[async_trait]
        impl OnchainProvider for FlakyProvider {
            fn name(&self) -> &str { "flaky" }
            async fn get_top_holders(
                &self, _c: &str, _t: &str, _l: u32,
            ) -> OnchainResult<Vec<HolderEntry>> {
                if !self.served_ok.swap(true, Ordering::SeqCst) {
                    Ok(vec![HolderEntry {
                        address: "0xa".into(), balance: dec!(100), label: None
                    }])
                } else {
                    Err(OnchainError::RateLimited("flaky".into()))
                }
            }
            async fn get_address_transfers(
                &self, _c: &str, _w: &str, _s: DateTime<Utc>,
            ) -> OnchainResult<Vec<TransferEntry>> {
                Ok(Vec::new())
            }
            async fn get_token_metadata(
                &self, c: &str, t: &str,
            ) -> OnchainResult<TokenMetadata> {
                Ok(TokenMetadata {
                    chain: c.into(), token: t.into(), symbol: "X".into(),
                    decimals: 18, total_supply: dec!(200),
                })
            }
        }

        let p = Arc::new(FlakyProvider {
            served_ok: std::sync::atomic::AtomicBool::new(false),
        });
        let cache = HolderConcentrationCache::new(
            p,
            HolderConcentrationConfig {
                top_n: 10,
                ttl: Duration::milliseconds(1),
            },
        );
        let s1 = cache.get("eth-mainnet", "0xx").await.unwrap();
        assert_eq!(s1.concentration_pct, dec!(0.5));
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        // TTL expired + provider errors → stale served.
        let s2 = cache.get("eth-mainnet", "0xx").await.unwrap();
        assert_eq!(s1.computed_at, s2.computed_at);
    }
}
