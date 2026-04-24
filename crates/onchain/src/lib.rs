//! Epic R3 — multi-provider on-chain surveillance.
//!
//! The [`OnchainProvider`] trait is the single contract every
//! downstream consumer (holder concentration cache, suspect
//! wallet tracker, graph source) talks to. Four provider
//! implementations ship in the box — pick whichever free-tier
//! coverage fits:
//!
//!   * [`goldrush::GoldRushProvider`] — Covalent / GoldRush.
//!     ~50 chains (EVM + Solana + Cosmos). ~1000 req/day free.
//!   * [`etherscan::EtherscanFamilyProvider`] — Etherscan +
//!     BscScan + PolygonScan + ArbiScan + OptimisticEtherscan.
//!     Same API shape, per-chain base URL. 5 req/s + 100k/day
//!     free. EVM-only.
//!   * [`moralis::MoralisProvider`] — Moralis. EVM-only. 40k
//!     compute-units/day free.
//!   * [`alchemy::AlchemyProvider`] — Alchemy JSON-RPC. EVM-only.
//!     300M compute units/month free.
//!
//! Operators configure a primary in `[onchain]` config and
//! optional fallback in `[onchain.fallback]`. The
//! [`cache::HolderConcentrationCache`] transparently wraps the
//! primary with a TTL cache so 10-symbol polling never hits the
//! rate limit. The [`tracker::SuspectWalletTracker`] walks
//! operator-provided wallet lists and publishes a rolling
//! `inflow_rate_bps` (notional into monitored CEX deposit
//! addresses over the last window, as bps of symbol volume).
//!
//! Fail-open by design: every call returns `Result<T>` with an
//! [`OnchainError`] variant covering rate-limit, auth failure,
//! unreachable, decoding error. Callers propagate into the
//! dashboard `Missing` / graph `Value::Missing` so a rate-
//! limited provider never halts quoting.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub mod alchemy;
pub mod cache;
pub mod etherscan;
pub mod goldrush;
pub mod moralis;
pub mod tracker;

/// Chain identifier. Free-form string so providers can map it
/// to their native identifier — GoldRush uses a chain name
/// (`"eth-mainnet"`), Alchemy uses the same, Etherscan uses a
/// distinct base URL per chain, Moralis uses numeric chain id.
/// The string here is the canonical slug the operator supplies
/// in config; each provider has a `resolve_chain` method.
pub type ChainId = String;

/// Token address, stored as a plain string so non-EVM chains
/// (Solana base58, Cosmos bech32) slot in without a new type.
pub type TokenAddress = String;
pub type WalletAddress = String;

/// One row in a token's holder list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolderEntry {
    pub address: WalletAddress,
    /// Raw token units (NOT normalised by `decimals`). Callers
    /// that want a fraction divide by `supply_normalised`.
    pub balance: Decimal,
    /// Optional venue / label the provider returned (e.g.
    /// `"Binance Hot 12"` or `"Uniswap v3"`). `None` when the
    /// provider doesn't label addresses or the free tier
    /// withholds the field.
    pub label: Option<String>,
}

/// One transfer in a wallet's history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEntry {
    pub from: WalletAddress,
    pub to: WalletAddress,
    /// Token address — a wallet's history mixes assets, so
    /// consumers filter on this before summing.
    pub token: TokenAddress,
    /// Token value (raw units, not normalised).
    pub value: Decimal,
    pub tx_hash: String,
    pub timestamp: DateTime<Utc>,
}

/// Token metadata used to normalise raw balances / transfers
/// into human-readable units and to compute supply
/// concentration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenMetadata {
    pub chain: ChainId,
    pub token: TokenAddress,
    pub symbol: String,
    pub decimals: u8,
    /// Total supply in raw units. Divide by `10^decimals` for
    /// the human-readable figure.
    pub total_supply: Decimal,
}

/// Error envelope every provider returns. The four variants
/// map to the decisions an upstream consumer has to make —
/// retry, back off, fail-open, or halt.
#[derive(Debug, thiserror::Error)]
pub enum OnchainError {
    /// Rate-limited. Caller should back off; cache layers
    /// treat this as "serve the last good snapshot".
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// Auth failed (bad key, missing key). Consumer should
    /// stop retrying until an operator rotates the key.
    #[error("auth failed: {0}")]
    Auth(String),
    /// Network / HTTP error.
    #[error("network error: {0}")]
    Network(String),
    /// Response decoded but the provider returned a payload
    /// we didn't expect (schema drift, free-tier field
    /// omission, malformed JSON).
    #[error("decode error: {0}")]
    Decode(String),
    /// Chain not supported by this provider. Callers with a
    /// fallback provider can retry there.
    #[error("unsupported chain: {0}")]
    UnsupportedChain(String),
}

pub type OnchainResult<T> = std::result::Result<T, OnchainError>;

/// The contract every on-chain provider implements.
///
/// All methods are `async`. Implementations should handle
/// their own retry + rate-limit budget internally — the caller
/// treats the result as the final word.
#[async_trait]
pub trait OnchainProvider: Send + Sync {
    /// Human-readable name — `"goldrush"`, `"etherscan"`, …
    /// Used in logs / dashboards so operators know which
    /// provider is answering a given request.
    fn name(&self) -> &str;

    /// Fetch the top-`limit` holders of `token` on `chain`,
    /// sorted by balance DESC.
    async fn get_top_holders(
        &self,
        chain: &str,
        token: &str,
        limit: u32,
    ) -> OnchainResult<Vec<HolderEntry>>;

    /// Fetch transfers involving `wallet` since `since_ts`.
    /// Providers that only expose a pagination cursor or
    /// block number should internally walk pages until they
    /// cross the timestamp.
    async fn get_address_transfers(
        &self,
        chain: &str,
        wallet: &str,
        since_ts: DateTime<Utc>,
    ) -> OnchainResult<Vec<TransferEntry>>;

    /// Fetch token metadata — symbol, decimals, total supply.
    async fn get_token_metadata(&self, chain: &str, token: &str) -> OnchainResult<TokenMetadata>;
}
