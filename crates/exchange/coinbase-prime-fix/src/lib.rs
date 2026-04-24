//! Coinbase Prime FIX 4.4 connector (Epic E stage-2 skeleton).
//!
//! Scope of this skeleton:
//!
//! - Pure message-building helpers for the four FIX messages
//!   the venue mandates during an MM session (Logon,
//!   NewOrderSingle, OrderCancelRequest, Heartbeat). Each
//!   helper is a unit-tested pure function with a pinned
//!   byte-shape test so a future schema drift fails loudly.
//! - [`auth::sign_logon`] — the HMAC-SHA256 signer with the
//!   exact prehash Coinbase Prime's docs specify. Decoupled
//!   from message layout so tests don't depend on FIX
//!   ordering.
//! - A thin [`CoinbasePrimeConnector`] that implements
//!   [`mm_exchange_core::connector::ExchangeConnector`] as
//!   a façade — every trade-entry method returns
//!   `Err(NotConnected)` until the TCP+TLS session layer is
//!   wired. The façade stubs are intentional: they keep the
//!   crate compiling + linked into `mm-server`'s venue
//!   routing so the rest of the engine can target this
//!   venue id in config, dashboards, and audit event
//!   labels as soon as the session layer lands.
//!
//! Out of scope — deferred until stage-2b:
//!
//! - TCP/TLS session loop (tokio-rustls wrapper driving
//!   `mm_protocols_fix::FixSession` on the wire). The
//!   session state machine + sequence persistence already
//!   exist in `mm-protocols-fix`; hooking them into a live
//!   socket is pure plumbing once a Coinbase Prime test
//!   environment is available.
//! - Execution report (35=8) decode path feeding
//!   `MarketEvent::Fill`.
//! - Market-data subscription (Coinbase Prime uses a
//!   separate FIX-based market-data session; different
//!   endpoint, different message set).
//!
//! # Usage example (when session is wired)
//!
//! ```ignore
//! let conn = CoinbasePrimeConnector::new(CoinbasePrimeCredentials {
//!     api_key: "…".into(),
//!     api_secret_b64: "…".into(),
//!     passphrase: "…".into(),
//!     sender_comp_id: "MM-TRADING".into(),
//! });
//! ```

pub mod auth;
pub mod messages;

use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use mm_common::types::{Balance, LiveOrder, OrderId, ProductSpec, Side as MmSide};
use mm_exchange_core::connector::{
    AmendOrder, ExchangeConnector, NewOrder, VenueCapabilities, VenueId, VenueProduct,
};
use mm_exchange_core::events::MarketEvent;
use tokio::sync::mpsc;

/// Three-part Coinbase Prime FIX credential bundle.
#[derive(Debug, Clone)]
pub struct CoinbasePrimeCredentials {
    /// API key string — goes into `Username` (tag 553) on
    /// the Logon message per Coinbase Prime spec.
    pub api_key: String,
    /// Base64-encoded API secret downloaded alongside the
    /// key. Only this field feeds the HMAC signer in
    /// `auth::sign_logon`; the prehash already carries the
    /// passphrase, so the secret is never sent on the wire.
    pub api_secret_b64: String,
    /// Passphrase configured on the API key. Goes into
    /// `Password` (tag 554) on the Logon message **and**
    /// participates in the HMAC prehash.
    pub passphrase: String,
    /// SenderCompID assigned by Coinbase Prime onboarding.
    /// Distinct from `api_key` — most integrations set it
    /// to a human-readable venue account tag.
    pub sender_comp_id: String,
    /// TargetCompID the venue expects. Typically
    /// `COINBASE` on prod, `COINBASE-SANDBOX` on the test
    /// environment. Exposed in config so operators can
    /// toggle without recompilation.
    pub target_comp_id: String,
    /// Heartbeat interval in seconds the Logon requests.
    /// Default 30 s — Coinbase Prime's spec's recommended
    /// value.
    pub heartbeat_secs: u32,
}

impl Default for CoinbasePrimeCredentials {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            api_secret_b64: String::new(),
            passphrase: String::new(),
            sender_comp_id: String::new(),
            target_comp_id: "COINBASE".to_string(),
            heartbeat_secs: 30,
        }
    }
}

/// Coinbase Prime FIX 4.4 venue connector (skeleton).
pub struct CoinbasePrimeConnector {
    creds: CoinbasePrimeCredentials,
    capabilities: VenueCapabilities,
}

impl CoinbasePrimeConnector {
    pub fn new(creds: CoinbasePrimeCredentials) -> Self {
        Self {
            creds,
            capabilities: VenueCapabilities {
                max_batch_size: 1,
                supports_amend: false,
                supports_ws_trading: false,
                supports_fix: true,
                max_order_rate: 20,
                supports_funding_rate: false,
                supports_margin_info: false,
                supports_margin_mode: false,
                supports_liquidation_feed: false,
                supports_set_leverage: false,
            },
        }
    }

    /// Read-only accessor for the credential bundle. Used by
    /// the (future) session layer when it boots.
    pub fn credentials(&self) -> &CoinbasePrimeCredentials {
        &self.creds
    }
}

fn not_connected<T>() -> anyhow::Result<T> {
    Err(anyhow!(
        "coinbase prime FIX: TCP+TLS session not wired (stage-2b — see crate docs)"
    ))
}

#[async_trait]
impl ExchangeConnector for CoinbasePrimeConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::Coinbase
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    fn product(&self) -> VenueProduct {
        VenueProduct::Spot
    }

    async fn subscribe(
        &self,
        _symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        not_connected()
    }

    async fn get_orderbook(
        &self,
        _symbol: &str,
        _depth: u32,
    ) -> anyhow::Result<(
        Vec<mm_common::types::PriceLevel>,
        Vec<mm_common::types::PriceLevel>,
        u64,
    )> {
        not_connected()
    }

    async fn place_order(&self, _order: &NewOrder) -> anyhow::Result<OrderId> {
        not_connected()
    }

    async fn place_orders_batch(&self, _orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        not_connected()
    }

    async fn cancel_order(&self, _symbol: &str, _order_id: OrderId) -> anyhow::Result<()> {
        not_connected()
    }

    async fn cancel_orders_batch(
        &self,
        _symbol: &str,
        _order_ids: &[OrderId],
    ) -> anyhow::Result<()> {
        not_connected()
    }

    async fn cancel_all_orders(&self, _symbol: &str) -> anyhow::Result<()> {
        not_connected()
    }

    async fn amend_order(&self, _amend: &AmendOrder) -> anyhow::Result<()> {
        not_connected()
    }

    async fn get_open_orders(&self, _symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        not_connected()
    }

    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        not_connected()
    }

    async fn get_product_spec(&self, _symbol: &str) -> anyhow::Result<ProductSpec> {
        not_connected()
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        // Same semantics as `place_order` etc. — returning
        // `Err` keeps callers from silently believing the
        // connector is live.
        not_connected()
    }

    async fn rate_limit_remaining(&self) -> u32 {
        self.capabilities.max_order_rate
    }
}

/// Re-export conversion helper so test code + future session
/// wiring can map `mm_common` sides to `mm_protocols_fix`
/// sides without reaching into the private modules.
pub fn map_side(side: MmSide) -> mm_protocols_fix::Side {
    match side {
        MmSide::Buy => mm_protocols_fix::Side::Buy,
        MmSide::Sell => mm_protocols_fix::Side::Sell,
    }
}

/// Heartbeat cadence as a `std::time::Duration` — convenience
/// for the session layer.
pub fn heartbeat_duration(creds: &CoinbasePrimeCredentials) -> Duration {
    Duration::from_secs(creds.heartbeat_secs.max(1) as u64)
}
