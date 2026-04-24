//! Suspect-wallet inflow tracker.
//!
//! Operator supplies a per-symbol wallet list (e.g. team +
//! known-whale addresses). The tracker polls each wallet's
//! recent transfers, sums notional moving INTO known CEX
//! deposit addresses, and publishes a rolling `inflow_rate` —
//! units of token flowing into exchanges over the last window.
//!
//! Classic RAVE signal: before the dump, team wallets deposit
//! millions of tokens to CEX addresses so they can sell. A
//! non-zero `inflow_rate` on the symbol's team wallets is the
//! clearest possible early warning.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{OnchainProvider, OnchainResult};

#[derive(Debug, Clone)]
pub struct SuspectWalletConfig {
    /// Wallets the operator wants to watch, keyed by symbol.
    /// The full list of team + known-insider addresses.
    pub suspects_per_symbol: HashMap<String, Vec<String>>,
    /// Known CEX deposit address allowlist (lowercase). Any
    /// transfer whose destination is in this set counts as an
    /// inflow event; destinations outside the set are ignored.
    pub cex_deposit_addresses: HashSet<String>,
    /// Rolling window for inflow rate. Default 24 h — team
    /// loading CEX before a dump usually completes inside a
    /// day.
    pub window: Duration,
}

impl Default for SuspectWalletConfig {
    fn default() -> Self {
        Self {
            suspects_per_symbol: HashMap::new(),
            cex_deposit_addresses: HashSet::new(),
            window: Duration::hours(24),
        }
    }
}

/// Per-symbol snapshot — total notional that moved from any
/// suspect wallet to any known-CEX deposit address over the
/// last [`SuspectWalletConfig::window`].
#[derive(Debug, Clone, Default)]
pub struct InflowSnapshot {
    pub symbol: String,
    pub chain: String,
    /// Raw token units summed across all suspect → CEX hops.
    /// Not normalised by decimals — consumers divide or
    /// compare against total supply / circulating when the
    /// comparison needs a fraction.
    pub inflow_total: Decimal,
    /// Number of discrete transfer events.
    pub event_count: u32,
    pub computed_at: DateTime<Utc>,
}

pub struct SuspectWalletTracker {
    provider: Arc<dyn OnchainProvider>,
    config: SuspectWalletConfig,
    snapshots: RwLock<HashMap<String, InflowSnapshot>>,
}

impl SuspectWalletTracker {
    pub fn new(provider: Arc<dyn OnchainProvider>, config: SuspectWalletConfig) -> Self {
        Self {
            provider,
            config,
            snapshots: RwLock::new(HashMap::new()),
        }
    }

    /// Refresh one symbol's snapshot. Walks every wallet in
    /// `suspects_per_symbol[symbol]`, fetches recent transfers
    /// via the provider, filters on destination ∈ CEX
    /// allowlist, sums the notional, stores the result.
    pub async fn refresh(&self, symbol: &str, chain: &str) -> OnchainResult<InflowSnapshot> {
        let wallets = self
            .config
            .suspects_per_symbol
            .get(symbol)
            .cloned()
            .unwrap_or_default();
        let since = Utc::now() - self.config.window;
        let mut total = Decimal::ZERO;
        let mut count = 0u32;
        for w in wallets {
            let transfers = match self.provider.get_address_transfers(chain, &w, since).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        wallet = %w,
                        error = %e,
                        "suspect wallet fetch failed; skipping"
                    );
                    continue;
                }
            };
            for t in transfers {
                // Only count outbound flow (wallet → CEX).
                if !t.from.eq_ignore_ascii_case(&w) {
                    continue;
                }
                let to_lower = t.to.to_lowercase();
                if !self.config.cex_deposit_addresses.contains(&to_lower) {
                    continue;
                }
                total += t.value;
                count += 1;
            }
        }
        let snap = InflowSnapshot {
            symbol: symbol.to_string(),
            chain: chain.to_string(),
            inflow_total: total,
            event_count: count,
            computed_at: Utc::now(),
        };
        let mut g = self.snapshots.write().await;
        g.insert(symbol.to_string(), snap.clone());
        Ok(snap)
    }

    /// Read-only peek for the graph-source overlay.
    pub async fn peek(&self, symbol: &str) -> Option<InflowSnapshot> {
        let g = self.snapshots.read().await;
        g.get(symbol).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HolderEntry, OnchainError, TokenMetadata, TransferEntry};
    use async_trait::async_trait;
    use rust_decimal_macros::dec;

    struct StubProvider {
        transfers: Vec<TransferEntry>,
    }
    #[async_trait]
    impl OnchainProvider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }
        async fn get_top_holders(
            &self,
            _c: &str,
            _t: &str,
            _l: u32,
        ) -> OnchainResult<Vec<HolderEntry>> {
            Ok(Vec::new())
        }
        async fn get_address_transfers(
            &self,
            _c: &str,
            _w: &str,
            _s: DateTime<Utc>,
        ) -> OnchainResult<Vec<TransferEntry>> {
            Ok(self.transfers.clone())
        }
        async fn get_token_metadata(&self, _c: &str, _t: &str) -> OnchainResult<TokenMetadata> {
            Err(OnchainError::UnsupportedChain("stub".into()))
        }
    }

    #[tokio::test]
    async fn counts_only_suspect_to_cex_flow() {
        let suspect = "0xsuspect";
        let cex = "0xcex".to_string();
        let bystander = "0xbystander";
        let transfers = vec![
            // Suspect → CEX: counted.
            TransferEntry {
                from: suspect.into(),
                to: cex.clone(),
                token: "0xrave".into(),
                value: dec!(1000),
                tx_hash: "0xh1".into(),
                timestamp: Utc::now(),
            },
            // Suspect → random: skipped.
            TransferEntry {
                from: suspect.into(),
                to: "0xrandom".into(),
                token: "0xrave".into(),
                value: dec!(5000),
                tx_hash: "0xh2".into(),
                timestamp: Utc::now(),
            },
            // Bystander → CEX: different `from`, not counted.
            TransferEntry {
                from: bystander.into(),
                to: cex.clone(),
                token: "0xrave".into(),
                value: dec!(9000),
                tx_hash: "0xh3".into(),
                timestamp: Utc::now(),
            },
            // Suspect → CEX again: counted.
            TransferEntry {
                from: suspect.into(),
                to: cex.clone(),
                token: "0xrave".into(),
                value: dec!(2000),
                tx_hash: "0xh4".into(),
                timestamp: Utc::now(),
            },
        ];
        let p = Arc::new(StubProvider { transfers });
        let mut suspects = HashMap::new();
        suspects.insert("RAVEUSDT".into(), vec![suspect.into()]);
        let mut cex_set = HashSet::new();
        cex_set.insert(cex);
        let cfg = SuspectWalletConfig {
            suspects_per_symbol: suspects,
            cex_deposit_addresses: cex_set,
            window: Duration::hours(24),
        };
        let tracker = SuspectWalletTracker::new(p, cfg);
        let snap = tracker.refresh("RAVEUSDT", "eth-mainnet").await.unwrap();
        assert_eq!(snap.inflow_total, dec!(3000));
        assert_eq!(snap.event_count, 2);
    }
}
