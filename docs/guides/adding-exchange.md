# Adding a New Exchange Connector

## Overview

Each exchange connector implements the `ExchangeConnector` trait from `crates/exchange/core`. The trait defines ~20 methods covering market data, order management, and account queries.

## Step 1: Create the Crate

```bash
mkdir -p crates/exchange/newvenue/src
```

`Cargo.toml`:
```toml
[package]
name = "mm-exchange-newvenue"
edition.workspace = true
version.workspace = true

[dependencies]
mm-common = { workspace = true }
mm-exchange-core = { workspace = true }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
reqwest = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
rust_decimal = { workspace = true }
chrono = { workspace = true }
hmac = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
```

## Step 2: Implement the Trait

### Required Methods (must implement)

```rust
#[async_trait]
impl ExchangeConnector for NewVenueConnector {
    // Identity
    fn venue_id(&self) -> VenueId;
    fn capabilities(&self) -> VenueCapabilities;

    // Market data
    async fn subscribe(&self, symbols: &[String])
        -> Result<UnboundedReceiver<MarketEvent>>;
    async fn get_orderbook(&self, symbol: &str)
        -> Result<(Vec<PriceLevel>, Vec<PriceLevel>)>;

    // Orders
    async fn place_order(&self, order: &NewOrder) -> Result<OrderId>;
    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> Result<()>;
    async fn cancel_all_orders(&self, symbol: &str) -> Result<()>;
    async fn get_open_orders(&self, symbol: &str) -> Result<Vec<LiveOrder>>;

    // Account
    async fn get_balances(&self) -> Result<Vec<Balance>>;
    async fn get_product_spec(&self, symbol: &str) -> Result<ProductSpec>;

    // Health
    async fn health_check(&self) -> Result<bool>;
    async fn rate_limit_remaining(&self) -> u32;
}
```

### Optional Methods (have defaults)

```rust
// Batch orders (default: sequential fallback)
async fn place_orders_batch(&self, orders: &[NewOrder]) -> Result<Vec<OrderId>>;
async fn cancel_orders_batch(&self, symbol: &str, ids: &[OrderId]) -> Result<()>;

// Amend (default: cancel + re-place)
async fn amend_order(&self, amend: &AmendOrder) -> Result<()>;

// Cross-venue (default: "not supported" error)
async fn withdraw(&self, asset, qty, address, network) -> Result<String>;
async fn internal_transfer(&self, asset, qty, from, to) -> Result<String>;

// Fee/borrow (default: "not supported" error)
async fn fetch_fee_tiers(&self, symbol: &str) -> Result<FeeTierInfo>;
async fn get_borrow_rate(&self, asset: &str) -> Result<BorrowRateInfo>;

// Discovery
async fn list_symbols(&self) -> Result<Vec<ProductSpec>>;
```

## Step 3: VenueCapabilities

Tell the engine what your venue supports:

```rust
fn capabilities(&self) -> VenueCapabilities {
    VenueCapabilities {
        supports_batch_place: true,     // place_orders_batch works?
        supports_batch_cancel: true,    // cancel_orders_batch works?
        supports_amend: false,          // native amend (keeps queue priority)?
        supports_ws_trading: true,      // orders via WebSocket?
        supports_fix: false,            // FIX 4.4 protocol?
        max_batch_size: 20,             // max orders per batch call
    }
}
```

**Rule:** Never set a capability to `true` unless the code path is wired and tested. The engine reads these flags to decide order routing.

## Step 4: Market Data (WebSocket)

The `subscribe()` method must:
1. Open a WS connection to the venue
2. Subscribe to the symbol's L2 book + trade stream
3. Parse venue-specific messages into `MarketEvent` variants
4. Send events through the returned `UnboundedReceiver`
5. Handle reconnection on disconnect

```rust
async fn subscribe(&self, symbols: &[String])
    -> Result<UnboundedReceiver<MarketEvent>>
{
    let (tx, rx) = mpsc::unbounded_channel();
    // Spawn WS read loop in background
    tokio::spawn(async move {
        loop {
            // Connect, subscribe, read events
            // On disconnect: log, wait 2s, reconnect
        }
    });
    Ok(rx)
}
```

Key `MarketEvent` variants to produce:
- `BookSnapshot { venue, symbol, bids, asks, sequence }` — initial + periodic full book
- `BookDelta { venue, symbol, bids, asks, sequence }` — incremental updates
- `Trade { venue, trade }` — public trades
- `Fill { venue, fill }` — our fills (from user data stream)
- `Connected { venue }` / `Disconnected { venue }` — connectivity state

## Step 5: Authentication

Most venues use HMAC-SHA256 signed requests. Common pattern:

```rust
async fn signed_request(&self, method: &str, path: &str, body: &str) -> Result<Value> {
    let timestamp = Utc::now().timestamp_millis();
    let prehash = format!("{timestamp}{method}{path}{body}");
    let signature = hmac_sha256(&self.api_secret, &prehash);
    // Add signature to headers and send
}
```

See existing connectors for venue-specific auth schemes:
- Binance: `crates/exchange/binance/src/connector.rs` lines 92-130
- Bybit: `crates/exchange/bybit/src/connector.rs` lines 240-280
- HyperLiquid: `crates/exchange/hyperliquid/src/connector.rs` (EIP-712 / k256)

## Step 6: Register in Server

Add to `create_connector()` in `crates/server/src/main.rs`:

```rust
ExchangeType::NewVenue => Ok(Arc::new(
    NewVenueConnector::new(&cfg.rest_url, &cfg.ws_url, &api_key, &api_secret)
)),
```

Add `NewVenue` to `ExchangeType` enum in `crates/common/src/config.rs`.

## Step 7: Protocol Documentation

Create `docs/protocols/newvenue.md` with:
- REST API base URLs (production + testnet)
- WS endpoints + subscription messages
- Authentication scheme
- Rate limits (requests/min, order rate)
- Error codes and recovery strategies
- Reconnection semantics

## Step 8: Tests

Minimum test coverage:
1. **Auth signature** — verify HMAC against known test vectors
2. **Response parsing** — unit test each parse function with fixture JSON
3. **Capabilities** — assert flags match actual implementation
4. **ProductSpec** — verify tick/lot/fee parsing from venue response

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn capabilities_match_implementation() {
        let c = NewVenueConnector::new(/*...*/);
        let caps = c.capabilities();
        // If supports_amend is true, amend_order must be overridden
        assert!(!caps.supports_amend); // until we implement it
    }
}
```
