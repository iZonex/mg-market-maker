//! HyperLiquid perp DEX connector implementing `ExchangeConnector`.
//!
//! REST endpoints: `/info` (public reads) and `/exchange` (signed writes).
//! WebSocket: single multiplexed connection, subscribe to `l2Book` + `trades`
//! per coin plus `userEvents` + `orderUpdates` for our wallet address.
//!
//! Identity is driven by cloid: we generate a UUID for every order we place
//! and pass its 16 bytes as a 0x-prefixed 128-bit hex string in the `c`
//! field. Cancels go through `cancelByCloid`, which means we never have to
//! round-trip the exchange's numeric `oid`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use mm_common::types::{
    Balance, LiveOrder, OrderId, OrderStatus, PriceLevel, ProductSpec, Side, TimeInForce,
    WalletType,
};
use mm_exchange_core::connector::{
    ExchangeConnector, NewOrder, VenueCapabilities, VenueId, VenueProduct,
};
use mm_exchange_core::events::MarketEvent;
use mm_exchange_core::metrics::ORDER_ENTRY_LATENCY;
use mm_exchange_core::rate_limiter::RateLimiter;
use reqwest::Client;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::{sign_l1_action, PrivateKey};
use crate::types::{
    HlCancel, HlCancelAction, HlCancelByCloid, HlCancelByCloidAction, HlExchangePayload, HlLimit,
    HlMeta, HlOrder, HlOrderAction, HlOrderTif,
};
use crate::ws_post::HlWsTrader;

const MAINNET_BASE: &str = "https://api.hyperliquid.xyz";
const TESTNET_BASE: &str = "https://api.hyperliquid-testnet.xyz";
const MAINNET_WS: &str = "wss://api.hyperliquid.xyz/ws";
const TESTNET_WS: &str = "wss://api.hyperliquid-testnet.xyz/ws";

/// HL perp price precision constant: max decimals = 6 - szDecimals.
const PERP_MAX_DECIMALS: u32 = 6;
/// HL spot price precision constant: max decimals = 8 - szDecimals.
const SPOT_MAX_DECIMALS: u32 = 8;
/// HL spot asset-index offset. Spot pair N is addressed as
/// `a = 10000 + N` in the L1 action wire format, per HL docs.
const SPOT_INDEX_OFFSET: u32 = 10_000;

/// Default HL maker/taker fees (base tier, perps).
const DEFAULT_MAKER_FEE: Decimal = dec!(0.00015);
const DEFAULT_TAKER_FEE: Decimal = dec!(0.00045);
const DEFAULT_MIN_NOTIONAL: Decimal = dec!(10);

#[derive(Debug, Clone)]
struct AssetMeta {
    index: u32,
    sz_decimals: u32,
}

pub struct HyperLiquidConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    key: PrivateKey,
    is_mainnet: bool,
    vault_address: Option<[u8; 20]>,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
    /// Coin → metadata, lazily populated from `/info meta`.
    asset_map: Arc<RwLock<HashMap<String, AssetMeta>>>,
    /// Asset index → coin name, built alongside `asset_map`.
    asset_index_to_name: Arc<RwLock<HashMap<u32, String>>>,
    /// Optional WS post trader. When `Some` and connected, order entry
    /// routes through WS; on disconnect we fall through to REST.
    ws_trader: Option<Arc<HlWsTrader>>,
    /// `true` for spot connectors, `false` for perps. Drives:
    /// - `ensure_asset_map` picks `spotMeta` vs `meta`
    /// - `build_hl_order` adds `SPOT_INDEX_OFFSET` to the asset index
    /// - precision rule uses `SPOT_MAX_DECIMALS` instead of `PERP_MAX_DECIMALS`
    /// - `get_balances` queries `spotClearinghouseState`
    /// - `product()` returns `Spot` instead of `LinearPerp`
    is_spot: bool,
}

impl HyperLiquidConnector {
    /// Mainnet perp connector (HL's primary product).
    pub fn new(private_key_hex: &str) -> Result<Self> {
        Self::with_urls(private_key_hex, MAINNET_BASE, MAINNET_WS, true, false)
    }

    /// Testnet perp connector.
    pub fn testnet(private_key_hex: &str) -> Result<Self> {
        Self::with_urls(private_key_hex, TESTNET_BASE, TESTNET_WS, false, false)
    }

    /// Mainnet spot connector. HL spot uses `@N` pair indices,
    /// `spotMeta` for asset metadata, and `spotClearinghouseState`
    /// for balances — but the EIP-712 signing is unchanged.
    pub fn spot(private_key_hex: &str) -> Result<Self> {
        Self::with_urls(private_key_hex, MAINNET_BASE, MAINNET_WS, true, true)
    }

    /// Testnet spot connector.
    pub fn testnet_spot(private_key_hex: &str) -> Result<Self> {
        Self::with_urls(private_key_hex, TESTNET_BASE, TESTNET_WS, false, true)
    }

    fn with_urls(
        private_key_hex: &str,
        base_url: &str,
        ws_url: &str,
        is_mainnet: bool,
        is_spot: bool,
    ) -> Result<Self> {
        let key = PrivateKey::from_hex(private_key_hex)?;
        info!(
            address = %key.address_hex(),
            mainnet = is_mainnet,
            is_spot,
            "HyperLiquid connector initialized"
        );
        Ok(Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            ws_url: ws_url.to_string(),
            key,
            is_mainnet,
            vault_address: None,
            // HL per-address REST limit is ~1200/min. Match Binance budget.
            rate_limiter: RateLimiter::new(1200, Duration::from_secs(60), 0.8),
            capabilities: VenueCapabilities {
                max_batch_size: 20,
                supports_amend: false, // Cancel+place via default trait impl.
                supports_ws_trading: true, // wired via `enable_ws_trading`.
                supports_fix: false,
                max_order_rate: 100,
                // Spot has no funding; perp does.
                supports_funding_rate: !is_spot,
            },
            asset_map: Arc::new(RwLock::new(HashMap::new())),
            asset_index_to_name: Arc::new(RwLock::new(HashMap::new())),
            ws_trader: None,
            is_spot,
        })
    }

    /// Spin up the WS post trader on its own dedicated socket. After
    /// this, `place_order`, `cancel_order`, and `cancel_orders_batch`
    /// attempt the WS path first and fall back to REST on disconnect.
    ///
    /// Returns `&mut self` so the caller can chain it after
    /// construction. Must be called before handing the connector to
    /// the engine as `Arc<dyn ExchangeConnector>`.
    pub fn enable_ws_trading(&mut self) {
        let trader = HlWsTrader::connect(self.ws_url.clone(), self.key.clone(), self.is_mainnet);
        self.ws_trader = Some(Arc::new(trader));
    }

    /// POST to `/info` — public, no signing.
    async fn info_post(&self, body: Value) -> Result<Value> {
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/info", self.base_url);
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("HL /info {status}: {text}");
        }
        serde_json::from_str(&text).context("HL /info decode")
    }

    /// POST a signed action to `/exchange`.
    async fn exchange_post<A: serde::Serialize>(&self, action: &A) -> Result<Value> {
        self.rate_limiter.acquire(1).await;
        let nonce = chrono::Utc::now().timestamp_millis() as u64;
        let sig = sign_l1_action(
            &self.key,
            action,
            nonce,
            self.vault_address.as_ref(),
            self.is_mainnet,
        )?;
        let payload = HlExchangePayload {
            action,
            nonce,
            signature: sig.to_json(),
            vault_address: self
                .vault_address
                .as_ref()
                .map(|a| format!("0x{}", hex::encode(a))),
        };
        let url = format!("{}/exchange", self.base_url);
        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("HL /exchange {status}: {text}");
        }
        let v: Value = serde_json::from_str(&text).context("HL /exchange decode")?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            anyhow::bail!("HL /exchange error: {v}");
        }
        Ok(v)
    }

    /// Ensure the asset map is populated. Called lazily by methods that need
    /// to resolve a symbol to an asset index.
    ///
    /// Perp connectors query `/info {type: "meta"}` — the universe is
    /// a flat list of coin names and the array index **is** the asset
    /// index used in the L1 action's `a` field.
    ///
    /// Spot connectors query `/info {type: "spotMeta"}` — the universe
    /// is a list of pairs (base/quote tokens); the pair's array index
    /// becomes the asset ID after adding `SPOT_INDEX_OFFSET` (10_000)
    /// per HL's wire convention.
    async fn ensure_asset_map(&self) -> Result<()> {
        if !self.asset_map.read().await.is_empty() {
            return Ok(());
        }

        if self.is_spot {
            // Spot meta response shape:
            //   { "tokens": [{name, szDecimals, weiDecimals, index, …}],
            //     "universe": [{name, tokens: [base_idx, quote_idx], index}] }
            let resp = self.info_post(json!({ "type": "spotMeta" })).await?;
            let tokens = resp
                .get("tokens")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let pairs = resp
                .get("universe")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Build token index → szDecimals lookup so we can read
            // the base-asset precision for each pair.
            let mut token_sz: HashMap<u64, u32> = HashMap::new();
            for t in &tokens {
                if let (Some(idx), Some(sz)) = (
                    t.get("index").and_then(|v| v.as_u64()),
                    t.get("szDecimals").and_then(|v| v.as_u64()),
                ) {
                    token_sz.insert(idx, sz as u32);
                }
            }

            let mut map = self.asset_map.write().await;
            let mut rev = self.asset_index_to_name.write().await;
            for (pair_idx, pair) in pairs.iter().enumerate() {
                let pair_idx = pair_idx as u32;
                let name = pair
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                // Pair's base token is the first entry in its `tokens`
                // array; size precision comes from the token's
                // `szDecimals`.
                let base_token_idx = pair
                    .get("tokens")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let sz_decimals = token_sz.get(&base_token_idx).copied().unwrap_or(0);
                let asset_id = SPOT_INDEX_OFFSET + pair_idx;
                map.insert(
                    name.clone(),
                    AssetMeta {
                        index: asset_id,
                        sz_decimals,
                    },
                );
                rev.insert(asset_id, name);
            }
            info!(count = map.len(), "HL spot asset map loaded");
            return Ok(());
        }

        let resp = self.info_post(json!({ "type": "meta" })).await?;
        let meta: HlMeta =
            serde_json::from_value(resp).context("HL meta parse")?;
        let mut map = self.asset_map.write().await;
        let mut rev = self.asset_index_to_name.write().await;
        for (idx, asset) in meta.universe.iter().enumerate() {
            let idx = idx as u32;
            map.insert(
                asset.name.clone(),
                AssetMeta {
                    index: idx,
                    sz_decimals: asset.sz_decimals,
                },
            );
            rev.insert(idx, asset.name.clone());
        }
        info!(count = map.len(), "HL perp asset map loaded");
        Ok(())
    }

    async fn asset_for(&self, symbol: &str) -> Result<AssetMeta> {
        self.ensure_asset_map().await?;
        let map = self.asset_map.read().await;
        map.get(symbol)
            .cloned()
            .ok_or_else(|| anyhow!("unknown HL asset: {symbol}"))
    }

    /// Derive tick/lot sizes for an HL asset. Perp precision rule:
    /// `max_px_decimals = 6 - szDecimals`. Spot precision rule:
    /// `max_px_decimals = 8 - szDecimals`. See
    /// docs/research/spot-mm-specifics.md §15 for the rule.
    fn decimals_to_spec(symbol: &str, sz_decimals: u32, is_spot: bool) -> ProductSpec {
        let max_px = if is_spot {
            SPOT_MAX_DECIMALS
        } else {
            PERP_MAX_DECIMALS
        };
        let px_decimals = max_px.saturating_sub(sz_decimals);
        let tick_size = decimal_from_neg_pow10(px_decimals);
        let lot_size = decimal_from_neg_pow10(sz_decimals);
        // Spot pair names include the quote token after a slash, e.g.
        // "PURR/USDC"; perp symbols are just coin names.
        let (base_asset, quote_asset) = if is_spot {
            match symbol.split_once('/') {
                Some((b, q)) => (b.to_string(), q.to_string()),
                None => (symbol.to_string(), "USDC".to_string()),
            }
        } else {
            (symbol.to_string(), "USDC".to_string())
        };
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset,
            quote_asset,
            tick_size,
            lot_size,
            min_notional: DEFAULT_MIN_NOTIONAL,
            maker_fee: DEFAULT_MAKER_FEE,
            taker_fee: DEFAULT_TAKER_FEE,
        }
    }

    /// Turn a UUID into the 0x-prefixed 32-char hex cloid HL expects.
    fn uuid_to_cloid(id: Uuid) -> String {
        format!("0x{}", hex::encode(id.as_bytes()))
    }

    fn cloid_to_uuid(cloid: &str) -> Option<Uuid> {
        let trimmed = cloid.trim_start_matches("0x");
        let bytes = hex::decode(trimmed).ok()?;
        if bytes.len() != 16 {
            return None;
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&bytes);
        Some(Uuid::from_bytes(arr))
    }

    async fn build_hl_order(&self, order: &NewOrder, cloid: String) -> Result<HlOrder> {
        let asset = self.asset_for(&order.symbol).await?;
        let is_buy = matches!(order.side, Side::Buy);
        let price = order
            .price
            .ok_or_else(|| anyhow!("HL orders must carry a price"))?;
        let tif = match order.time_in_force {
            Some(TimeInForce::PostOnly) | None => "Alo",
            Some(TimeInForce::Gtc) | Some(TimeInForce::Gtd) | Some(TimeInForce::Day) => "Gtc",
            Some(TimeInForce::Ioc) => "Ioc",
            Some(TimeInForce::Fok) => "Ioc", // HL has no FOK — treat as IOC.
        };
        let max_px = if self.is_spot {
            SPOT_MAX_DECIMALS
        } else {
            PERP_MAX_DECIMALS
        };
        let px_decimals = max_px.saturating_sub(asset.sz_decimals) as usize;
        let sz_decimals = asset.sz_decimals as usize;
        Ok(HlOrder {
            a: asset.index,
            b: is_buy,
            p: format_decimal(price, px_decimals),
            s: format_decimal(order.qty, sz_decimals),
            r: false,
            t: HlOrderTif::Limit {
                limit: HlLimit { tif: tif.into() },
            },
            c: Some(cloid),
        })
    }
}

/// `10^-n` as a `Decimal` for small n. HL decimals never exceed ~10.
fn decimal_from_neg_pow10(n: u32) -> Decimal {
    if n == 0 {
        return dec!(1);
    }
    let mut s = String::from("0.");
    for _ in 1..n {
        s.push('0');
    }
    s.push('1');
    s.parse().unwrap_or(dec!(0.01))
}

/// Format a decimal with exactly `places` fractional digits.
fn format_decimal(d: Decimal, places: usize) -> String {
    // rust_decimal's Display gives full precision; we truncate/round to the
    // lot/tick scale. Use .round_dp which rounds-half-even.
    let rounded = d.round_dp(places as u32);
    // Avoid scientific notation; Decimal::Display never uses it.
    rounded.to_string()
}

#[async_trait]
impl ExchangeConnector for HyperLiquidConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::HyperLiquid
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    fn product(&self) -> VenueProduct {
        if self.is_spot {
            VenueProduct::Spot
        } else {
            VenueProduct::LinearPerp
        }
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let url = self.ws_url.clone();
        let coins: Vec<String> = symbols.to_vec();
        let user_hex = self.key.address_hex();

        // Make sure the asset map is loaded so event parsers can translate
        // asset indices back to symbols.
        self.ensure_asset_map().await?;

        info!(url = %url, coins = ?coins, "subscribing to HyperLiquid streams");

        tokio::spawn(async move {
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::connect_async;
            use tokio_tungstenite::tungstenite::Message;

            loop {
                match connect_async(&url).await {
                    Ok((ws, _)) => {
                        let _ = tx.send(MarketEvent::Connected {
                            venue: VenueId::HyperLiquid,
                        });
                        let (mut write, mut read) = ws.split();

                        // Public market data: l2Book + trades per coin.
                        for coin in &coins {
                            let sub_book = json!({
                                "method": "subscribe",
                                "subscription": { "type": "l2Book", "coin": coin }
                            });
                            let sub_trades = json!({
                                "method": "subscribe",
                                "subscription": { "type": "trades", "coin": coin }
                            });
                            if write.send(Message::Text(sub_book.to_string())).await.is_err() {
                                break;
                            }
                            if write.send(Message::Text(sub_trades.to_string())).await.is_err() {
                                break;
                            }
                        }

                        // Private user streams.
                        let sub_user = json!({
                            "method": "subscribe",
                            "subscription": { "type": "userEvents", "user": user_hex }
                        });
                        let sub_orders = json!({
                            "method": "subscribe",
                            "subscription": { "type": "orderUpdates", "user": user_hex }
                        });
                        let _ = write.send(Message::Text(sub_user.to_string())).await;
                        let _ = write.send(Message::Text(sub_orders.to_string())).await;

                        while let Some(Ok(msg)) = read.next().await {
                            if let Message::Text(text) = msg {
                                if let Ok(v) = serde_json::from_str::<Value>(&text.to_string()) {
                                    for evt in parse_hl_event(&v) {
                                        if tx.send(evt).is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        let _ = tx.send(MarketEvent::Disconnected {
                            venue: VenueId::HyperLiquid,
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "HL WS connect failed");
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(rx)
    }

    async fn get_orderbook(
        &self,
        symbol: &str,
        _depth: u32,
    ) -> Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        let resp = self
            .info_post(json!({ "type": "l2Book", "coin": symbol }))
            .await?;
        let levels = resp
            .get("levels")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("HL l2Book missing levels"))?;
        if levels.len() != 2 {
            anyhow::bail!("HL l2Book expected 2 sides, got {}", levels.len());
        }
        let bids = parse_hl_levels(&levels[0]);
        let asks = parse_hl_levels(&levels[1]);
        let seq = resp.get("time").and_then(|v| v.as_u64()).unwrap_or(0);
        Ok((bids, asks, seq))
    }

    async fn place_order(&self, order: &NewOrder) -> Result<OrderId> {
        let uuid = Uuid::new_v4();
        let cloid = Self::uuid_to_cloid(uuid);
        let hl_order = self.build_hl_order(order, cloid).await?;

        // Try WS path first when enabled and connected.
        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                let ws_t0 = Instant::now();
                let ws_result = ws.place_order(hl_order.clone()).await;
                ORDER_ENTRY_LATENCY
                    .with_label_values(&["hyperliquid", "ws", "place_order"])
                    .observe(ws_t0.elapsed().as_secs_f64());
                match ws_result {
                    Ok(_) => {
                        debug!(%uuid, path = "ws", "placed HL order");
                        return Ok(uuid);
                    }
                    Err(e) => {
                        warn!(error = %e, "HL WS place_order failed; falling back to REST");
                    }
                }
            }
        }

        let action = HlOrderAction::new(vec![hl_order]);
        let rest_t0 = Instant::now();
        let rest_result = self.exchange_post(&action).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["hyperliquid", "rest", "place_order"])
            .observe(rest_t0.elapsed().as_secs_f64());
        rest_result?;
        debug!(%uuid, path = "rest", "placed HL order");
        Ok(uuid)
    }

    async fn place_orders_batch(&self, orders: &[NewOrder]) -> Result<Vec<OrderId>> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }
        let mut uuids = Vec::with_capacity(orders.len());
        let mut hl_orders = Vec::with_capacity(orders.len());
        for o in orders {
            let uuid = Uuid::new_v4();
            let cloid = Self::uuid_to_cloid(uuid);
            let hl = self.build_hl_order(o, cloid).await?;
            hl_orders.push(hl);
            uuids.push(uuid);
        }

        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                match ws.place_orders(hl_orders.clone()).await {
                    Ok(_) => {
                        debug!(count = uuids.len(), path = "ws", "placed HL batch");
                        return Ok(uuids);
                    }
                    Err(e) => {
                        warn!(error = %e, "HL WS place_orders failed; falling back to REST");
                    }
                }
            }
        }

        let action = HlOrderAction::new(hl_orders);
        self.exchange_post(&action).await?;
        Ok(uuids)
    }

    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> Result<()> {
        let asset = self.asset_for(symbol).await?;
        let cloid = Self::uuid_to_cloid(order_id);

        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                match ws.cancel_by_cloid(asset.index, cloid.clone()).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, "HL WS cancel failed; falling back to REST");
                    }
                }
            }
        }

        let action = HlCancelByCloidAction::new(vec![HlCancelByCloid {
            asset: asset.index,
            cloid,
        }]);
        self.exchange_post(&action).await?;
        Ok(())
    }

    async fn cancel_orders_batch(&self, symbol: &str, order_ids: &[OrderId]) -> Result<()> {
        if order_ids.is_empty() {
            return Ok(());
        }
        let asset = self.asset_for(symbol).await?;
        let cloids: Vec<String> = order_ids
            .iter()
            .map(|id| Self::uuid_to_cloid(*id))
            .collect();

        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                match ws.cancel_batch_by_cloid(asset.index, cloids.clone()).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, "HL WS cancel batch failed; falling back to REST");
                    }
                }
            }
        }

        let cancels: Vec<HlCancelByCloid> = cloids
            .into_iter()
            .map(|cloid| HlCancelByCloid {
                asset: asset.index,
                cloid,
            })
            .collect();
        let action = HlCancelByCloidAction::new(cancels);
        self.exchange_post(&action).await?;
        Ok(())
    }

    async fn cancel_all_orders(&self, symbol: &str) -> Result<()> {
        let asset = self.asset_for(symbol).await?;
        let open = self
            .info_post(json!({ "type": "openOrders", "user": self.key.address_hex() }))
            .await?;
        let mut cancels = Vec::new();
        if let Some(arr) = open.as_array() {
            for o in arr {
                let coin = o.get("coin").and_then(|v| v.as_str());
                if coin != Some(symbol) {
                    continue;
                }
                if let Some(oid) = o.get("oid").and_then(|v| v.as_u64()) {
                    cancels.push(HlCancel { a: asset.index, o: oid });
                }
            }
        }
        if cancels.is_empty() {
            return Ok(());
        }
        let action = HlCancelAction::new(cancels);
        self.exchange_post(&action).await?;
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> Result<Vec<LiveOrder>> {
        let resp = self
            .info_post(json!({ "type": "openOrders", "user": self.key.address_hex() }))
            .await?;
        let arr = resp.as_array().cloned().unwrap_or_default();
        let out = arr
            .iter()
            .filter(|o| o.get("coin").and_then(|v| v.as_str()) == Some(symbol))
            .filter_map(|o| {
                let side_str = o.get("side")?.as_str()?;
                let side = match side_str {
                    "B" => Side::Buy,
                    "A" => Side::Sell,
                    _ => return None,
                };
                let price: Decimal = o.get("limitPx")?.as_str()?.parse().ok()?;
                let qty: Decimal = o.get("origSz")?.as_str()?.parse().ok()?;
                let filled: Decimal = {
                    let orig: Decimal = o.get("origSz")?.as_str()?.parse().ok()?;
                    let rem: Decimal = o.get("sz")?.as_str()?.parse().ok()?;
                    orig - rem
                };
                let ts = o.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
                let created_at =
                    chrono::DateTime::from_timestamp_millis(ts).unwrap_or_else(chrono::Utc::now);
                // If the order was placed by us, the cloid decodes back to
                // its UUID. Otherwise (external orders, edge-case), we
                // synthesize a fresh UUID so tracking stays intact.
                let order_id = o
                    .get("cloid")
                    .and_then(|v| v.as_str())
                    .and_then(Self::cloid_to_uuid)
                    .unwrap_or_else(Uuid::new_v4);
                Some(LiveOrder {
                    order_id,
                    symbol: symbol.to_string(),
                    side,
                    price,
                    qty,
                    filled_qty: filled,
                    status: OrderStatus::Open,
                    created_at,
                })
            })
            .collect();
        Ok(out)
    }

    async fn get_balances(&self) -> Result<Vec<Balance>> {
        if self.is_spot {
            // Spot wallet: `spotClearinghouseState.balances[]` — one
            // entry per token the user holds. Each has `coin`, `token`,
            // `total`, `hold` (locked).
            let resp = self
                .info_post(json!({
                    "type": "spotClearinghouseState",
                    "user": self.key.address_hex()
                }))
                .await?;
            let balances = resp
                .get("balances")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            return Ok(balances
                .iter()
                .filter_map(|b| {
                    let asset = b.get("coin")?.as_str()?.to_string();
                    let total: Decimal =
                        b.get("total").and_then(|v| v.as_str())?.parse().ok()?;
                    let hold: Decimal = b
                        .get("hold")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_default();
                    Some(Balance {
                        asset,
                        wallet: WalletType::Spot,
                        total,
                        locked: hold,
                        available: total - hold,
                    })
                })
                .filter(|b| b.total > Decimal::ZERO)
                .collect());
        }

        let resp = self
            .info_post(json!({
                "type": "clearinghouseState",
                "user": self.key.address_hex()
            }))
            .await?;
        let withdrawable: Decimal = resp
            .get("withdrawable")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let account_value: Decimal = resp
            .get("marginSummary")
            .and_then(|m| m.get("accountValue"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(withdrawable);
        let locked = (account_value - withdrawable).max(Decimal::ZERO);
        Ok(vec![Balance {
            asset: "USDC".into(),
            // HL perps margin in USDC — perp collateral wallet.
            wallet: WalletType::UsdMarginedFutures,
            total: account_value,
            locked,
            available: withdrawable,
        }])
    }

    async fn get_product_spec(&self, symbol: &str) -> Result<ProductSpec> {
        let asset = self.asset_for(symbol).await?;
        Ok(Self::decimals_to_spec(
            symbol,
            asset.sz_decimals,
            self.is_spot,
        ))
    }

    async fn health_check(&self) -> Result<bool> {
        match self.info_post(json!({ "type": "meta" })).await {
            Ok(_) => Ok(true),
            Err(e) => {
                warn!(error = %e, "HL health check failed");
                Ok(false)
            }
        }
    }
}

fn parse_hl_levels(side: &Value) -> Vec<PriceLevel> {
    side.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let px: Decimal = lvl.get("px")?.as_str()?.parse().ok()?;
                    let sz: Decimal = lvl.get("sz")?.as_str()?.parse().ok()?;
                    Some(PriceLevel { price: px, qty: sz })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a single HL WS message into zero or more `MarketEvent`s.
///
/// Pure function — cloids are deterministic (UUID bytes → hex), so we can
/// decode them back to `OrderId` without any cache lookup.
fn parse_hl_event(v: &Value) -> Vec<MarketEvent> {
    let Some(channel) = v.get("channel").and_then(|c| c.as_str()) else {
        return Vec::new();
    };
    let data = v.get("data");
    let venue = VenueId::HyperLiquid;

    match channel {
        "l2Book" => {
            let Some(d) = data else { return Vec::new() };
            let symbol = d.get("coin").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let time = d.get("time").and_then(|t| t.as_u64()).unwrap_or(0);
            let Some(levels) = d.get("levels").and_then(|l| l.as_array()) else {
                return Vec::new();
            };
            if levels.len() != 2 {
                return Vec::new();
            }
            vec![MarketEvent::BookSnapshot {
                venue,
                symbol,
                bids: parse_hl_levels(&levels[0]),
                asks: parse_hl_levels(&levels[1]),
                sequence: time,
            }]
        }
        "trades" => {
            let Some(arr) = data.and_then(|d| d.as_array()) else {
                return Vec::new();
            };
            arr.iter()
                .filter_map(|t| {
                    let symbol = t.get("coin")?.as_str()?.to_string();
                    let price: Decimal = t.get("px")?.as_str()?.parse().ok()?;
                    let qty: Decimal = t.get("sz")?.as_str()?.parse().ok()?;
                    let side = match t.get("side")?.as_str()? {
                        "B" => Side::Buy,
                        "A" => Side::Sell,
                        _ => return None,
                    };
                    let tid = t.get("tid").and_then(|v| v.as_u64()).unwrap_or(0);
                    let time_ms = t.get("time").and_then(|v| v.as_i64()).unwrap_or(0);
                    let timestamp =
                        chrono::DateTime::from_timestamp_millis(time_ms).unwrap_or_else(chrono::Utc::now);
                    Some(MarketEvent::Trade {
                        venue,
                        trade: mm_common::types::Trade {
                            trade_id: tid,
                            symbol,
                            price,
                            qty,
                            taker_side: side,
                            timestamp,
                        },
                    })
                })
                .collect()
        }
        "user" | "userEvents" => {
            let Some(fills) = data.and_then(|d| d.get("fills")).and_then(|f| f.as_array()) else {
                return Vec::new();
            };
            fills
                .iter()
                .filter_map(|f| {
                    let symbol = f.get("coin")?.as_str()?.to_string();
                    let price: Decimal = f.get("px")?.as_str()?.parse().ok()?;
                    let qty: Decimal = f.get("sz")?.as_str()?.parse().ok()?;
                    let side = match f.get("side")?.as_str()? {
                        "B" => Side::Buy,
                        "A" => Side::Sell,
                        _ => return None,
                    };
                    let tid = f.get("tid").and_then(|v| v.as_u64()).unwrap_or(0);
                    let time_ms = f.get("time").and_then(|v| v.as_i64()).unwrap_or(0);
                    let timestamp =
                        chrono::DateTime::from_timestamp_millis(time_ms).unwrap_or_else(chrono::Utc::now);
                    let order_id = f
                        .get("cloid")
                        .and_then(|v| v.as_str())
                        .and_then(HyperLiquidConnector::cloid_to_uuid)
                        .unwrap_or_else(Uuid::new_v4);
                    let is_maker = f
                        .get("crossed")
                        .and_then(|c| c.as_bool())
                        .map(|c| !c)
                        .unwrap_or(true);
                    Some(MarketEvent::Fill {
                        venue,
                        fill: mm_common::types::Fill {
                            trade_id: tid,
                            order_id,
                            symbol,
                            side,
                            price,
                            qty,
                            is_maker,
                            timestamp,
                        },
                    })
                })
                .collect()
        }
        "orderUpdates" => {
            let Some(arr) = data.and_then(|d| d.as_array()) else {
                return Vec::new();
            };
            arr.iter()
                .filter_map(|u| {
                    let status_str = u.get("status")?.as_str()?;
                    let status = match status_str {
                        "open" => OrderStatus::Open,
                        "filled" => OrderStatus::Filled,
                        "canceled" | "marginCanceled" => OrderStatus::Cancelled,
                        "rejected" => OrderStatus::Rejected,
                        _ => OrderStatus::Open,
                    };
                    let order = u.get("order")?;
                    let orig_sz: Decimal = order.get("origSz")?.as_str()?.parse().ok()?;
                    let rem_sz: Decimal = order.get("sz")?.as_str()?.parse().ok()?;
                    let filled_qty = orig_sz - rem_sz;
                    let order_id = order
                        .get("cloid")
                        .and_then(|v| v.as_str())
                        .and_then(HyperLiquidConnector::cloid_to_uuid)
                        .unwrap_or_else(Uuid::new_v4);
                    Some(MarketEvent::OrderUpdate {
                        venue,
                        order_id,
                        status,
                        filled_qty,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_zero_is_one_dollar_tick() {
        // Perp: BTC szDecimals=5 → pxDecimals = 6-5 = 1 → tick=0.1.
        let spec = HyperLiquidConnector::decimals_to_spec("BTC", 5, false);
        assert_eq!(spec.tick_size, dec!(0.1));
        assert_eq!(spec.lot_size, dec!(0.00001));
        assert_eq!(spec.quote_asset, "USDC");
        assert_eq!(spec.maker_fee, DEFAULT_MAKER_FEE);
    }

    #[test]
    fn decimals_high_sz_caps_at_zero_px() {
        // If szDecimals > max_px (6 perp / 8 spot), pxDecimals
        // saturates to 0 → tick_size=1.
        let spec = HyperLiquidConnector::decimals_to_spec("TOKEN", 8, false);
        assert_eq!(spec.tick_size, dec!(1));
    }

    /// Spot precision rule uses `8 - szDecimals` instead of
    /// `6 - szDecimals`, so a token with the same `szDecimals` gets
    /// two additional decimal places of price precision relative
    /// to its perp counterpart.
    #[test]
    fn spot_precision_uses_eight_minus_sz_decimals() {
        // szDecimals=5 → perp tick=0.1 (6-5=1), spot tick=0.001 (8-5=3)
        let perp = HyperLiquidConnector::decimals_to_spec("BTC", 5, false);
        let spot = HyperLiquidConnector::decimals_to_spec("PURR/USDC", 5, true);
        assert_eq!(perp.tick_size, dec!(0.1));
        assert_eq!(spot.tick_size, dec!(0.001));
        // Spot symbol `BASE/QUOTE` also populates base+quote fields.
        assert_eq!(spot.base_asset, "PURR");
        assert_eq!(spot.quote_asset, "USDC");
    }

    /// Spot connector reports `VenueProduct::Spot`; perp reports
    /// `LinearPerp`. Also verifies `supports_funding_rate` flips.
    #[test]
    fn spot_and_perp_constructors_set_correct_capabilities() {
        let perp = HyperLiquidConnector::testnet(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let spot = HyperLiquidConnector::testnet_spot(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(perp.product(), VenueProduct::LinearPerp);
        assert_eq!(spot.product(), VenueProduct::Spot);
        assert!(perp.capabilities().supports_funding_rate);
        assert!(!spot.capabilities().supports_funding_rate);
    }

    /// The `SPOT_INDEX_OFFSET` constant is wire-load-bearing: HL
    /// expects spot pairs addressed as `10_000 + pair_idx` in the
    /// signed L1 action's `a` field. Pin the constant so any drift
    /// breaks the test before it breaks live signing.
    #[test]
    fn spot_index_offset_is_ten_thousand() {
        assert_eq!(SPOT_INDEX_OFFSET, 10_000);
    }

    #[test]
    fn cloid_roundtrip() {
        let u = Uuid::new_v4();
        let cloid = HyperLiquidConnector::uuid_to_cloid(u);
        assert!(cloid.starts_with("0x"));
        assert_eq!(cloid.len(), 2 + 32);
        let back = HyperLiquidConnector::cloid_to_uuid(&cloid).unwrap();
        assert_eq!(u, back);
    }

    #[test]
    fn format_decimal_truncates_precision() {
        // rust_decimal::round_dp rounds half-to-even and does NOT pad trailing
        // zeros — matches HL Python SDK's float_to_wire_str which strips them.
        assert_eq!(format_decimal(dec!(42000.123456), 1), "42000.1");
        assert_eq!(format_decimal(dec!(0.00012345), 5), "0.00012");
        // Integer-valued decimals stay integer-shaped.
        assert_eq!(format_decimal(dec!(1), 3), "1");
        // Half-even rounding: 0.125 at 2 dp → 0.12 (round to even).
        assert_eq!(format_decimal(dec!(0.125), 2), "0.12");
    }

    /// Capability audit: `VenueCapabilities::supports_ws_trading` must
    /// match the actual presence of the WS post adapter. Protects
    /// against declaring a capability we cannot deliver — the bug this
    /// whole epic was triggered by.
    #[test]
    fn capabilities_match_implementation() {
        let conn = HyperLiquidConnector::testnet(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let caps = conn.capabilities();
        assert!(
            caps.supports_ws_trading,
            "HL declares WS trading — the WS post adapter must exist"
        );
        assert!(!caps.supports_amend, "HL has no native amend (cancel+place)");
        assert!(!caps.supports_fix, "HL has no FIX gateway");
        // Type-level confirmation that the adapter actually exists:
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_post::HlWsTrader>();
        };
    }
}
