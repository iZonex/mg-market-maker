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

use chrono::Timelike;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use mm_common::types::{
    Balance, LiveOrder, OrderId, OrderStatus, PriceLevel, ProductSpec, Side, TimeInForce,
    WalletType,
};
use mm_exchange_core::connector::{
    AccountMarginInfo, ExchangeConnector, FundingRate, FundingRateError, MarginError, MarginMode,
    NewOrder, PositionMargin, VenueCapabilities, VenueId, VenueProduct,
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
                // HL perp publishes margin via clearinghouseState;
                // spot has no margin concept.
                supports_margin_info: !is_spot,
                supports_margin_mode: !is_spot,
                // R5.5 — HL perp publishes forced-liquidation
                // events on the `liquidations` channel. Spot
                // has no liquidations.
                supports_liquidation_feed: !is_spot,
                // HL `updateLeverage` action sets per-asset
                // leverage on perps. Spot has no leverage knob.
                supports_set_leverage: !is_spot,
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
        let meta: HlMeta = serde_json::from_value(resp).context("HL meta parse")?;
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
            trading_status: Default::default(),
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

    fn classify_error(&self, err: &anyhow::Error) -> mm_exchange_core::VenueError {
        crate::classify(err)
    }

    fn product(&self) -> VenueProduct {
        if self.is_spot {
            VenueProduct::Spot
        } else {
            VenueProduct::LinearPerp
        }
    }

    async fn subscribe(&self, symbols: &[String]) -> Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let url = self.ws_url.clone();
        let coins: Vec<String> = symbols.to_vec();
        let user_hex = self.key.address_hex();
        let is_spot = self.is_spot;

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
                        // R5.1 — on perp (is_spot == false) also
                        // subscribe to `liquidations` per coin so
                        // the engine's LiquidationHeatmap
                        // receives real HL forced-liquidation
                        // events. Spot has none.
                        for coin in &coins {
                            let sub_book = json!({
                                "method": "subscribe",
                                "subscription": { "type": "l2Book", "coin": coin }
                            });
                            let sub_trades = json!({
                                "method": "subscribe",
                                "subscription": { "type": "trades", "coin": coin }
                            });
                            if write
                                .send(Message::Text(sub_book.to_string()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            if write
                                .send(Message::Text(sub_trades.to_string()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            if !is_spot {
                                let sub_liq = json!({
                                    "method": "subscribe",
                                    "subscription": {
                                        "type": "liquidations",
                                        "coin": coin
                                    }
                                });
                                if write
                                    .send(Message::Text(sub_liq.to_string()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }

                        // Private user streams. `webData2` is the only
                        // HL channel that pushes wallet/clearinghouse
                        // state on change, so it is the analog of
                        // Binance's `outboundAccountPosition` and
                        // Bybit V5's `wallet` topic — without it the
                        // engine's `BalanceCache` would only learn
                        // about HL balance changes via the 60 s
                        // reconcile loop. See ROADMAP P0.1.
                        let sub_user = json!({
                            "method": "subscribe",
                            "subscription": { "type": "userEvents", "user": user_hex }
                        });
                        let sub_orders = json!({
                            "method": "subscribe",
                            "subscription": { "type": "orderUpdates", "user": user_hex }
                        });
                        let sub_webdata = json!({
                            "method": "subscribe",
                            "subscription": { "type": "webData2", "user": user_hex }
                        });
                        let _ = write.send(Message::Text(sub_user.to_string())).await;
                        let _ = write.send(Message::Text(sub_orders.to_string())).await;
                        let _ = write.send(Message::Text(sub_webdata.to_string())).await;

                        while let Some(Ok(msg)) = read.next().await {
                            if let Message::Text(text) = msg {
                                if let Ok(v) = serde_json::from_str::<Value>(&text.to_string()) {
                                    for evt in parse_hl_event(&v, is_spot) {
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
                    cancels.push(HlCancel {
                        a: asset.index,
                        o: oid,
                    });
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
                    let total: Decimal = b.get("total").and_then(|v| v.as_str())?.parse().ok()?;
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

    async fn get_24h_volume_usd(
        &self,
        symbol: &str,
    ) -> anyhow::Result<Option<rust_decimal::Decimal>> {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        // HL bundles 24h notional volume in `metaAndAssetCtxs`
        // (perp) / `spotMetaAndAssetCtxs` (spot). The response is
        // [meta_with_universe, asset_ctxs_parallel_array]; the
        // asset index matches `meta.universe`. We look up the
        // index via our cached asset map rather than re-parsing
        // meta inline.
        let asset = self.asset_for(symbol).await?;
        let req_type = if self.is_spot {
            "spotMetaAndAssetCtxs"
        } else {
            "metaAndAssetCtxs"
        };
        let resp = self
            .info_post(serde_json::json!({ "type": req_type }))
            .await?;
        let ctxs = resp
            .as_array()
            .and_then(|a: &Vec<serde_json::Value>| a.get(1))
            .and_then(|c: &serde_json::Value| c.as_array());
        let Some(ctxs) = ctxs else { return Ok(None) };
        let ctx = ctxs.get(asset.index as usize);
        let vol = ctx
            .and_then(|c: &serde_json::Value| c.get("dayNtlVlm"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .and_then(|s: &str| Decimal::from_str(s).ok());
        Ok(vol)
    }

    /// List every asset in the HL universe for **this** connector's
    /// product (perp vs spot). Queries `/info {type: "meta"}` for
    /// perp and `/info {type: "spotMeta"}` for spot, then maps each
    /// asset through [`HyperLiquidConnector::decimals_to_spec`] so
    /// every spec uses the same precision rule as the single-
    /// symbol `get_product_spec` path.
    ///
    /// HL does not expose a per-asset `min_notional` via `meta`, so
    /// every spec inherits the default `DEFAULT_MIN_NOTIONAL`
    /// (`dec!(10)`). Operators can override per-symbol via config
    /// post-listing; the Epic F sniper only cares about which
    /// symbols exist, not their min-order size.
    ///
    /// HL does surface an `isDelisted` flag on perp assets that
    /// have been removed from the universe. Those rows get
    /// `trading_status = Delisted` so the sniper sees a stable
    /// "known set" even when HL temporarily shows a delisted
    /// symbol before pruning it.
    async fn list_symbols(&self) -> Result<Vec<ProductSpec>> {
        if self.is_spot {
            let resp = self.info_post(json!({ "type": "spotMeta" })).await?;
            Ok(parse_hl_spot_meta_into_specs(&resp))
        } else {
            let resp = self.info_post(json!({ "type": "meta" })).await?;
            Ok(parse_hl_perp_meta_into_specs(&resp))
        }
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

    async fn rate_limit_remaining(&self) -> u32 {
        self.rate_limiter.remaining().await
    }

    /// HyperLiquid perp funding rate via
    /// `POST /info {type:"metaAndAssetCtxs"}` (Epic 40.3). The
    /// response is a two-element array `[meta, ctxs]` where
    /// `ctxs[i].funding` is the rate for `meta.universe[i]`
    /// and the asset index we resolve through `asset_for` is
    /// the same shared index. Cadence is **hardcoded to 1 h** —
    /// HL is the outlier (most venues settle 8 h); the API
    /// does not publish cadence on this endpoint, so we
    /// consume the protocol-level constant and document it in
    /// `docs/protocols/hyperliquid.md`. Spot returns
    /// `NotSupported` before touching the wire.
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, FundingRateError> {
        if self.is_spot {
            return Err(FundingRateError::NotSupported);
        }
        let asset = self
            .asset_for(symbol)
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!("{e}")))?;
        let resp = self
            .info_post(json!({ "type": "metaAndAssetCtxs" }))
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!("{e}")))?;
        parse_hl_funding_rate(&resp, asset.index as usize).ok_or_else(|| {
            FundingRateError::Other(anyhow::anyhow!(
                "no funding rate for {symbol} (index {})",
                asset.index
            ))
        })
    }

    /// Account margin snapshot via `POST /info
    /// {type:"clearinghouseState"}` (Epic 40.4). HL exposes
    /// `marginSummary` (cross-margin totals) and
    /// `assetPositions[]` with per-position
    /// `{leverage, liquidationPx, marginUsed, positionValue}`.
    /// Spot connectors refuse before hitting the wire — HL
    /// spot has no margin concept.
    async fn account_margin_info(&self) -> Result<AccountMarginInfo, MarginError> {
        if self.is_spot {
            return Err(MarginError::NotSupported);
        }
        let resp = self
            .info_post(json!({
                "type": "clearinghouseState",
                "user": self.key.address_hex()
            }))
            .await
            .map_err(MarginError::Other)?;
        parse_hl_clearinghouse_margin(&resp)
            .ok_or_else(|| MarginError::Other(anyhow::anyhow!(
                "malformed HL clearinghouseState response"
            )))
    }

    /// Set per-symbol margin mode + leverage via
    /// `POST /exchange {action: updateLeverage}` (Epic 40.7).
    /// HL couples the two — updating leverage also toggles
    /// `isCross`. We keep the two trait calls separate so
    /// higher-level code can reason about them independently;
    /// the implementation reads back the current leverage from
    /// the asset map and passes it through.
    async fn set_margin_mode(
        &self,
        symbol: &str,
        mode: MarginMode,
    ) -> Result<(), MarginError> {
        if self.is_spot {
            return Err(MarginError::NotSupported);
        }
        let asset = self
            .asset_for(symbol)
            .await
            .map_err(MarginError::Other)?;
        // HL requires a leverage value on every updateLeverage
        // call. Pick a conservative default (1x) when we don't
        // know the current setting; the subsequent
        // `set_leverage` call overrides it.
        let action = json!({
            "type": "updateLeverage",
            "asset": asset.index,
            "isCross": matches!(mode, MarginMode::Cross),
            "leverage": 1,
        });
        self.exchange_post(&action)
            .await
            .map(|_| ())
            .map_err(MarginError::Other)
    }

    async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
    ) -> Result<(), MarginError> {
        if self.is_spot {
            return Err(MarginError::NotSupported);
        }
        let asset = self
            .asset_for(symbol)
            .await
            .map_err(MarginError::Other)?;
        // HL's updateLeverage accepts `isCross`; we preserve
        // the account's current mode by defaulting to `false`
        // (isolated). Operators setting cross mode should
        // always call `set_margin_mode(Cross)` *after*
        // set_leverage so the isCross flag is canonical.
        let action = json!({
            "type": "updateLeverage",
            "asset": asset.index,
            "isCross": false,
            "leverage": leverage,
        });
        self.exchange_post(&action)
            .await
            .map(|_| ())
            .map_err(MarginError::Other)
    }
}

/// Parse a `/info {type: "metaAndAssetCtxs"}` response into a
/// [`FundingRate`] for the asset at `index` (Epic 40.3). HL's
/// funding cadence is a protocol constant (1 h) — consumed
/// verbatim here since the endpoint does not publish it.
pub(crate) fn parse_hl_funding_rate(resp: &Value, index: usize) -> Option<FundingRate> {
    // Shape: [ { universe:[…] }, [ { funding:"…", … }, … ] ].
    let arr = resp.as_array()?;
    let ctxs = arr.get(1)?.as_array()?;
    let ctx = ctxs.get(index)?;
    let rate: Decimal = ctx.get("funding")?.as_str()?.parse().ok()?;
    // HL settles on the top of every hour UTC. Round `now` up
    // to the next hour to produce the anchoring timestamp —
    // the ticker doesn't publish a per-asset next-funding.
    let now = chrono::Utc::now();
    let mins = now.minute();
    let secs = now.second();
    let offset_secs = 3600 - (mins as i64 * 60 + secs as i64);
    let next_funding_time = now + chrono::Duration::seconds(offset_secs);
    Some(FundingRate {
        rate,
        next_funding_time,
        interval: std::time::Duration::from_secs(3600),
    })
}

/// Parse a `/info {type: "clearinghouseState"}` response into an
/// [`AccountMarginInfo`] (Epic 40.4). HL surfaces cross-margin
/// totals in `marginSummary` plus per-position detail in
/// `assetPositions[].position`. Liquidation price is published
/// as `liquidationPx` — we consume it verbatim rather than
/// recomputing (HL's tiered MMR model is not reimplemented
/// locally).
pub(crate) fn parse_hl_clearinghouse_margin(resp: &Value) -> Option<AccountMarginInfo> {
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    let margin_summary = resp.get("marginSummary")?;
    let total_equity = parse_dec(margin_summary.get("accountValue"));
    let total_initial_margin = parse_dec(margin_summary.get("totalMarginUsed"));
    // `crossMaintenanceMarginUsed` is the aggregate MM for
    // cross positions. Isolated buckets have their own MM
    // inside `assetPositions[].position.marginUsed` but HL
    // reports cross MM separately; we use the cross value as
    // the floor and add isolated MMs on top only if the wire
    // shape exposes them — currently HL does not publish a
    // per-isolated MM, so we approximate MM as
    // max(crossMM, 0).
    let total_maintenance_margin = parse_dec(resp.get("crossMaintenanceMarginUsed"));
    let available_balance = parse_dec(resp.get("withdrawable"));
    let margin_ratio = if total_equity > Decimal::ZERO {
        total_maintenance_margin / total_equity
    } else {
        Decimal::ONE
    };
    let positions = resp
        .get("assetPositions")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_hl_position_margin).collect())
        .unwrap_or_default();
    Some(AccountMarginInfo {
        total_equity,
        total_initial_margin,
        total_maintenance_margin,
        available_balance,
        margin_ratio,
        positions,
        reported_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn parse_hl_position_margin(entry: &Value) -> Option<PositionMargin> {
    let pos = entry.get("position")?;
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    let coin = pos.get("coin")?.as_str()?.to_string();
    // HL publishes `szi` = signed size; sign encodes side, raw
    // absolute value is the size.
    let szi_str = pos.get("szi")?.as_str()?;
    let szi: Decimal = szi_str.parse().ok()?;
    if szi == Decimal::ZERO {
        return None;
    }
    let side = if szi > Decimal::ZERO {
        Side::Buy
    } else {
        Side::Sell
    };
    let entry_price = parse_dec(pos.get("entryPx"));
    // Mark price is inside marginSummary per-position table
    // on some HL API versions; fall back to entry if missing.
    let mark_price = pos
        .get("markPx")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Decimal>().ok())
        .unwrap_or(entry_price);
    // Isolated buckets expose `marginUsed`; cross positions
    // have `crossUsage` or leave marginUsed = 0.
    let isolated_margin = pos
        .get("marginUsed")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Decimal>().ok())
        .filter(|d| *d > Decimal::ZERO);
    let liq_price = pos
        .get("liquidationPx")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "null")
        .and_then(|s| s.parse::<Decimal>().ok())
        .filter(|d| *d > Decimal::ZERO);
    Some(PositionMargin {
        symbol: coin,
        side,
        size: szi.abs(),
        entry_price,
        mark_price,
        isolated_margin,
        liq_price,
        // PERP-4 — HyperLiquid does not expose an ADL rank
        // on `clearinghouseState`; leave `None`.
        adl_quantile: None,
    })
}

/// Parse a `/info {type: "meta"}` response (HL perp) into the list
/// of [`ProductSpec`] entries the Epic F listing sniper consumes.
/// Shares the precision rule with
/// [`HyperLiquidConnector::decimals_to_spec`] so single-symbol and
/// whole-universe call sites stay in lockstep. Pure helper so the
/// wire shape is unit-tested without an HTTP client.
///
/// Rows missing a `name` field are dropped silently. Rows with
/// `isDelisted: true` are returned with
/// `trading_status = TradingStatus::Delisted` so the sniper can
/// diff a stable set across scans without treating a delisted
/// asset as "removed".
pub(crate) fn parse_hl_perp_meta_into_specs(resp: &Value) -> Vec<ProductSpec> {
    let universe = match resp.get("universe").and_then(|v| v.as_array()) {
        Some(u) => u,
        None => return Vec::new(),
    };
    universe
        .iter()
        .filter_map(|asset| {
            let name = asset.get("name").and_then(|v| v.as_str())?.to_string();
            let sz_decimals = asset
                .get("szDecimals")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let is_delisted = asset
                .get("isDelisted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut spec = HyperLiquidConnector::decimals_to_spec(&name, sz_decimals, false);
            if is_delisted {
                spec.trading_status = mm_common::types::TradingStatus::Delisted;
            }
            Some(spec)
        })
        .collect()
}

/// Parse a `/info {type: "spotMeta"}` response (HL spot) into the
/// list of [`ProductSpec`] entries the Epic F listing sniper
/// consumes. HL spot pairs use a `universe[]` of pair objects that
/// reference token indices; the per-token `szDecimals` is looked
/// up from the parallel `tokens[]` array — same rule as
/// [`HyperLiquidConnector::ensure_asset_map`].
pub(crate) fn parse_hl_spot_meta_into_specs(resp: &Value) -> Vec<ProductSpec> {
    let tokens = match resp.get("tokens").and_then(|v| v.as_array()) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let pairs = match resp.get("universe").and_then(|v| v.as_array()) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let mut token_sz: HashMap<u64, u32> = HashMap::new();
    for t in tokens {
        if let (Some(idx), Some(sz)) = (
            t.get("index").and_then(|v| v.as_u64()),
            t.get("szDecimals").and_then(|v| v.as_u64()),
        ) {
            token_sz.insert(idx, sz as u32);
        }
    }
    pairs
        .iter()
        .filter_map(|pair| {
            let name = pair.get("name").and_then(|v| v.as_str())?.to_string();
            if name.is_empty() {
                return None;
            }
            let base_token_idx = pair
                .get("tokens")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let sz_decimals = token_sz.get(&base_token_idx).copied().unwrap_or(0);
            Some(HyperLiquidConnector::decimals_to_spec(
                &name,
                sz_decimals,
                true,
            ))
        })
        .collect()
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
/// `is_spot` selects the wallet bucket the `webData2` parser tags
/// `BalanceUpdate` events with — perp accounts use
/// `WalletType::UsdMarginedFutures` (USDC collateral pool), spot
/// accounts use `WalletType::Spot` (per-token holdings).
///
/// Pure function — cloids are deterministic (UUID bytes → hex), so we can
/// decode them back to `OrderId` without any cache lookup.
pub(crate) fn parse_hl_event(v: &Value, is_spot: bool) -> Vec<MarketEvent> {
    let Some(channel) = v.get("channel").and_then(|c| c.as_str()) else {
        return Vec::new();
    };
    let data = v.get("data");
    let venue = VenueId::HyperLiquid;

    match channel {
        "l2Book" => {
            let Some(d) = data else { return Vec::new() };
            let symbol = d
                .get("coin")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
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
                    let timestamp = chrono::DateTime::from_timestamp_millis(time_ms)
                        .unwrap_or_else(chrono::Utc::now);
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
        "liquidations" => {
            // R5.1 — HL perp liquidation channel. Shape per
            // docs: `{"channel":"liquidations","data":[{
            //   "coin":"BTC","side":"A","sz":"0.1","px":"30000",
            //   "time":1700000000000}, ...]}`.
            // Side: "A" = liquidated long (market taker sell),
            //       "B" = liquidated short (market taker buy).
            if is_spot {
                return Vec::new();
            }
            let Some(arr) = data.and_then(|d| d.as_array()) else {
                return Vec::new();
            };
            arr.iter()
                .filter_map(|t| {
                    let symbol = t.get("coin")?.as_str()?.to_string();
                    let price: Decimal = t.get("px")?.as_str()?.parse().ok()?;
                    let qty: Decimal = t.get("sz")?.as_str()?.parse().ok()?;
                    // HL "side" on liquidations semantics per docs:
                    // "A" = long liquidated → taker side = Sell.
                    let side = match t.get("side")?.as_str()? {
                        "A" => Side::Sell,
                        "B" => Side::Buy,
                        _ => return None,
                    };
                    let time_ms = t.get("time").and_then(|v| v.as_i64()).unwrap_or_else(|| {
                        chrono::Utc::now().timestamp_millis()
                    });
                    Some(MarketEvent::Liquidation {
                        venue,
                        symbol,
                        side,
                        qty,
                        price,
                        timestamp_ms: time_ms,
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
                    let timestamp = chrono::DateTime::from_timestamp_millis(time_ms)
                        .unwrap_or_else(chrono::Utc::now);
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
        "webData2" => {
            // HL pushes the user's full clearinghouse + spot state on
            // every change. We translate that into the same
            // `BalanceUpdate` events Binance (`outboundAccountPosition`)
            // and Bybit (`wallet`) emit, so the engine's
            // `BalanceCache` stays current without waiting for the
            // 60 s reconcile poll.
            let Some(d) = data else { return Vec::new() };
            if is_spot {
                parse_hl_spot_balances(d)
            } else {
                parse_hl_perp_balance(d).into_iter().collect()
            }
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

/// Parse a HL perp `webData2` payload into a single USDC
/// `BalanceUpdate`. Mirrors the REST `clearinghouseState` reading in
/// `get_balances`: `marginSummary.accountValue` is the total equity,
/// `withdrawable` is the available portion, and the difference is
/// the margin currently locked into open positions / orders.
fn parse_hl_perp_balance(d: &Value) -> Option<MarketEvent> {
    // The payload nests differently across HL releases — try the
    // documented top-level shape first, then fall back to the
    // commonly-seen `clearinghouseState` nested form.
    let ch = d
        .get("clearinghouseState")
        .or_else(|| d.get("perpClearinghouseState"))
        .unwrap_or(d);
    let withdrawable: Decimal = ch
        .get("withdrawable")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(Decimal::ZERO);
    let account_value: Decimal = ch
        .get("marginSummary")
        .and_then(|m| m.get("accountValue"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(withdrawable);
    if account_value.is_zero() && withdrawable.is_zero() {
        return None;
    }
    let locked = (account_value - withdrawable).max(Decimal::ZERO);
    Some(MarketEvent::BalanceUpdate {
        venue: VenueId::HyperLiquid,
        asset: "USDC".to_string(),
        wallet: WalletType::UsdMarginedFutures,
        total: account_value,
        locked,
        available: withdrawable,
    })
}

/// Parse a HL spot `webData2` payload into one `BalanceUpdate` per
/// non-zero coin balance. Mirrors `spotClearinghouseState.balances`
/// from the REST helper in `get_balances`.
fn parse_hl_spot_balances(d: &Value) -> Vec<MarketEvent> {
    let spot = d
        .get("spotState")
        .or_else(|| d.get("spotClearinghouseState"))
        .unwrap_or(d);
    let Some(balances) = spot.get("balances").and_then(|b| b.as_array()) else {
        return Vec::new();
    };
    balances
        .iter()
        .filter_map(|b| {
            let asset = b.get("coin")?.as_str()?.to_string();
            let total: Decimal = b.get("total").and_then(|v| v.as_str())?.parse().ok()?;
            let hold: Decimal = b
                .get("hold")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(Decimal::ZERO);
            Some(MarketEvent::BalanceUpdate {
                venue: VenueId::HyperLiquid,
                asset,
                wallet: WalletType::Spot,
                total,
                locked: hold,
                available: (total - hold).max(Decimal::ZERO),
            })
        })
        .collect()
}

/// Parse one HL WS frame into `MarketEvent`s — public entry point for
/// integration tests in downstream crates that need to assert the
/// frame → cache path end-to-end without spinning up a live WS
/// session.
pub fn parse_hl_event_for_test(v: &Value, is_spot: bool) -> Vec<MarketEvent> {
    parse_hl_event(v, is_spot)
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

    /// Epic 40.4 — HL `clearinghouseState` wire shape. Pin
    /// the decode so a future HL API change fails the test
    /// instead of silently zeroing the guard's ratio.
    #[test]
    fn clearinghouse_margin_parser_extracts_ratio_and_positions() {
        let resp = serde_json::json!({
            "withdrawable": "5000",
            "crossMaintenanceMarginUsed": "500",
            "marginSummary": {
                "accountValue": "10000",
                "totalMarginUsed": "2000",
                "totalNtlPos": "8000"
            },
            "assetPositions": [
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "ETH",
                        "szi": "1.5",
                        "entryPx": "3000",
                        "markPx": "3050",
                        "marginUsed": "450",
                        "liquidationPx": "2800",
                        "leverage": {"type":"isolated","value":10}
                    }
                },
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "BTC",
                        "szi": "-0.1",
                        "entryPx": "50000",
                        "markPx": "50500",
                        "marginUsed": "0",
                        "liquidationPx": "55000",
                        "leverage": {"type":"cross","value":5}
                    }
                },
                {
                    "type": "oneWay",
                    "position": {
                        "coin": "SOL",
                        "szi": "0",
                        "entryPx": "0",
                        "markPx": "0",
                        "marginUsed": "0",
                        "liquidationPx": ""
                    }
                }
            ]
        });
        let info = parse_hl_clearinghouse_margin(&resp).unwrap();
        assert_eq!(info.total_equity, dec!(10000));
        assert_eq!(info.total_maintenance_margin, dec!(500));
        assert_eq!(info.margin_ratio, dec!(0.05));
        // SOL zero-size filtered; ETH + BTC kept.
        assert_eq!(info.positions.len(), 2);
        let eth = info.positions.iter().find(|p| p.symbol == "ETH").unwrap();
        assert_eq!(eth.side, Side::Buy);
        assert_eq!(eth.size, dec!(1.5));
        assert_eq!(eth.isolated_margin, Some(dec!(450)));
        assert_eq!(eth.liq_price, Some(dec!(2800)));
        let btc = info.positions.iter().find(|p| p.symbol == "BTC").unwrap();
        assert_eq!(btc.side, Side::Sell);
        assert_eq!(btc.size, dec!(0.1));
        // Cross-margin position — no isolated margin surfaced.
        assert!(btc.isolated_margin.is_none());
    }

    /// Epic 40.3 — HL `metaAndAssetCtxs` funding wire shape.
    /// Pins the `[meta, ctxs]` two-element array layout and
    /// the 1-hour cadence constant.
    #[test]
    fn funding_rate_parser_reads_ctx_at_index() {
        let resp = serde_json::json!([
            { "universe": [{"name":"BTC"},{"name":"ETH"}] },
            [
                { "funding": "0.000125", "markPx": "50000" },
                { "funding": "-0.00003", "markPx": "3000" }
            ]
        ]);
        let btc = parse_hl_funding_rate(&resp, 0).unwrap();
        assert_eq!(btc.rate, dec!(0.000125));
        assert_eq!(btc.interval, std::time::Duration::from_secs(3600));
        let eth = parse_hl_funding_rate(&resp, 1).unwrap();
        assert_eq!(eth.rate, dec!(-0.00003));
    }

    #[test]
    fn funding_rate_parser_out_of_bounds_returns_none() {
        let resp = serde_json::json!([
            { "universe": [{"name":"BTC"}] },
            [{ "funding": "0.0001" }]
        ]);
        assert!(parse_hl_funding_rate(&resp, 99).is_none());
    }

    #[test]
    fn clearinghouse_margin_parser_zero_equity_saturates_ratio() {
        let resp = serde_json::json!({
            "withdrawable": "0",
            "crossMaintenanceMarginUsed": "0",
            "marginSummary": {
                "accountValue": "0",
                "totalMarginUsed": "0",
                "totalNtlPos": "0"
            },
            "assetPositions": []
        });
        let info = parse_hl_clearinghouse_margin(&resp).unwrap();
        assert_eq!(info.margin_ratio, Decimal::ONE);
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
        assert!(
            !caps.supports_amend,
            "HL has no native amend (cancel+place)"
        );
        assert!(!caps.supports_fix, "HL has no FIX gateway");
        // Type-level confirmation that the adapter actually exists:
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_post::HlWsTrader>();
        };
    }

    // ---------- webData2 → BalanceUpdate (P0.1 HL leg) ----------

    /// Perp `webData2` payload → single USDC `BalanceUpdate` tagged
    /// against the perp collateral wallet. Mirrors the field layout
    /// the REST `clearinghouseState` reader uses, so a future schema
    /// drift breaks both the test and the live parser symmetrically.
    #[test]
    fn webdata2_perp_emits_usdc_balance_update() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "user": "0xabc",
                "clearinghouseState": {
                    "withdrawable": "750.50",
                    "marginSummary": { "accountValue": "1000.00" }
                }
            }
        });
        let events = parse_hl_event(&frame, false);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                assert_eq!(asset, "USDC");
                assert_eq!(*wallet, WalletType::UsdMarginedFutures);
                assert_eq!(*total, dec!(1000.00));
                assert_eq!(*available, dec!(750.50));
                assert_eq!(*locked, dec!(249.50));
            }
            _ => panic!("expected BalanceUpdate"),
        }
    }

    /// Perp parser falls back to `withdrawable` as both total and
    /// available when `marginSummary.accountValue` is missing —
    /// guards against an HL edge case where a fresh sub-account has
    /// no open positions and the field is omitted entirely.
    #[test]
    fn webdata2_perp_falls_back_when_account_value_missing() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": { "withdrawable": "42" }
            }
        });
        let events = parse_hl_event(&frame, false);
        assert_eq!(events.len(), 1);
        if let MarketEvent::BalanceUpdate {
            total,
            available,
            locked,
            ..
        } = &events[0]
        {
            assert_eq!(*total, dec!(42));
            assert_eq!(*available, dec!(42));
            assert_eq!(*locked, dec!(0));
        } else {
            panic!("expected BalanceUpdate");
        }
    }

    /// Spot `webData2` payload → one `BalanceUpdate` per non-zero
    /// coin, tagged against the spot wallet bucket. Mirrors the
    /// `spotClearinghouseState.balances[]` shape from the REST path.
    #[test]
    fn webdata2_spot_emits_per_coin_balance_updates() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "user": "0xabc",
                "spotState": {
                    "balances": [
                        { "coin": "USDC", "token": 0, "total": "500.0", "hold": "100.0" },
                        { "coin": "PURR", "token": 1, "total": "10.0", "hold": "0.0" }
                    ]
                }
            }
        });
        let events = parse_hl_event(&frame, true);
        assert_eq!(events.len(), 2);
        if let MarketEvent::BalanceUpdate {
            asset,
            wallet,
            total,
            locked,
            available,
            ..
        } = &events[0]
        {
            assert_eq!(asset, "USDC");
            assert_eq!(*wallet, WalletType::Spot);
            assert_eq!(*total, dec!(500));
            assert_eq!(*locked, dec!(100));
            assert_eq!(*available, dec!(400));
        } else {
            panic!("expected BalanceUpdate");
        }
    }

    /// `is_spot=true` must NOT emit perp `BalanceUpdate`s even when a
    /// `clearinghouseState` snippet sneaks into the payload, and vice
    /// versa. Otherwise a spot connector would surface its operator's
    /// perp collateral as a spot balance and double-count it.
    #[test]
    fn webdata2_routing_is_disjoint_between_spot_and_perp() {
        let mixed = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": {
                    "withdrawable": "100",
                    "marginSummary": { "accountValue": "100" }
                },
                "spotState": {
                    "balances": [{ "coin": "USDC", "total": "5", "hold": "0" }]
                }
            }
        });
        let perp_events = parse_hl_event(&mixed, false);
        assert_eq!(perp_events.len(), 1);
        assert!(matches!(
            perp_events[0],
            MarketEvent::BalanceUpdate {
                wallet: WalletType::UsdMarginedFutures,
                ..
            }
        ));
        let spot_events = parse_hl_event(&mixed, true);
        assert_eq!(spot_events.len(), 1);
        assert!(matches!(
            spot_events[0],
            MarketEvent::BalanceUpdate {
                wallet: WalletType::Spot,
                ..
            }
        ));
    }

    /// `webData2` with no recognisable balance fields is a no-op —
    /// guards against sending spurious zero-balance updates that
    /// would confuse the inventory drift reconciler.
    #[test]
    fn webdata2_with_no_balances_is_silent() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": { "user": "0xabc" }
        });
        assert!(parse_hl_event(&frame, false).is_empty());
        assert!(parse_hl_event(&frame, true).is_empty());
    }

    /// `parse_hl_event_for_test` is the public crate-export the
    /// downstream `mm-engine` integration test pins against. Verify
    /// it dispatches to the same internal parser.
    #[test]
    fn parse_hl_event_for_test_is_a_thin_pass_through() {
        let frame = serde_json::json!({
            "channel": "webData2",
            "data": {
                "clearinghouseState": {
                    "withdrawable": "1",
                    "marginSummary": { "accountValue": "1" }
                }
            }
        });
        assert_eq!(super::parse_hl_event_for_test(&frame, false).len(), 1);
    }

    /// Listing sniper (Epic F): `/info {type: "meta"}` perp
    /// universe parses into one [`ProductSpec`] per asset, with
    /// `szDecimals` driving the tick/lot precision through the
    /// shared `decimals_to_spec` helper.
    #[test]
    fn list_symbols_perp_meta_parses_universe_array() {
        let resp = serde_json::json!({
            "universe": [
                { "name": "BTC", "szDecimals": 5, "maxLeverage": 50 },
                { "name": "ETH", "szDecimals": 4, "maxLeverage": 50 },
                { "name": "DEAD", "szDecimals": 2, "isDelisted": true }
            ]
        });
        let specs = parse_hl_perp_meta_into_specs(&resp);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTC").unwrap();
        // szDecimals=5 → tick = 10^-(6-5) = 0.1
        assert_eq!(btc.tick_size, dec!(0.1));
        assert_eq!(btc.min_notional, DEFAULT_MIN_NOTIONAL);
        assert_eq!(btc.trading_status, mm_common::types::TradingStatus::Trading);
        let dead = specs.iter().find(|s| s.symbol == "DEAD").unwrap();
        assert_eq!(
            dead.trading_status,
            mm_common::types::TradingStatus::Delisted
        );
    }

    /// Spot meta flows through the token-index → szDecimals lookup
    /// identical to `ensure_asset_map`, then maps each pair through
    /// the shared spec helper.
    #[test]
    fn list_symbols_spot_meta_resolves_pair_precision_via_tokens() {
        let resp = serde_json::json!({
            "tokens": [
                { "name": "USDC", "index": 0, "szDecimals": 2, "weiDecimals": 8 },
                { "name": "PURR", "index": 1, "szDecimals": 5, "weiDecimals": 8 }
            ],
            "universe": [
                { "name": "PURR/USDC", "tokens": [1, 0], "index": 0 }
            ]
        });
        let specs = parse_hl_spot_meta_into_specs(&resp);
        assert_eq!(specs.len(), 1);
        let purr = &specs[0];
        assert_eq!(purr.symbol, "PURR/USDC");
        assert_eq!(purr.base_asset, "PURR");
        assert_eq!(purr.quote_asset, "USDC");
        // Spot precision: 8 - szDecimals(5) = 3 → tick 0.001
        assert_eq!(purr.tick_size, dec!(0.001));
    }

    /// Missing universe / tokens arrays yield an empty vec rather
    /// than panicking — guards against a venue-side schema blip
    /// taking down the listing sniper.
    #[test]
    fn list_symbols_meta_missing_fields_returns_empty() {
        assert!(parse_hl_perp_meta_into_specs(&serde_json::json!({})).is_empty());
        assert!(parse_hl_spot_meta_into_specs(&serde_json::json!({})).is_empty());
    }
}
