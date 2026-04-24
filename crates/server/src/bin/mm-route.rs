//! `mm-route` — one-shot SOR decision inspector (Epic A stage-2 #3).
//!
//! Builds the same connector bundle the live server would build
//! from a config file, fetches a fresh top-of-book snapshot from
//! every venue the router considers, runs
//! [`GreedyRouter::route`] once, and prints the per-leg decision
//! to stdout. Lets operators sanity-check their multi-venue
//! routing policy without booting the full engine.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --bin mm-route -- \
//!     --config config/mm-route-fixture.toml \
//!     --side buy --qty 2.5 --urgency 0.6
//! ```
//!
//! The `--side` / `--qty` / `--urgency` flags mirror the
//! `MarketMakerEngine::recommend_route` signature. Exit code is
//! `0` on a non-empty decision, `1` on any error, `2` when the
//! router produced an empty decision (no venue could serve the
//! target).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use mm_common::config::{AppConfig, ExchangeConfig, ExchangeType};
use mm_common::types::{ProductSpec, Side};
use mm_engine::sor::cost::VenueCostModel;
use mm_engine::sor::router::GreedyRouter;
use mm_engine::sor::venue_state::{VenueSeed, VenueStateAggregator};
use mm_exchange_core::connector::{ExchangeConnector, VenueId};
use rust_decimal::Decimal;

fn print_usage_and_exit() -> ! {
    eprintln!(
        "usage: mm-route --config PATH --side buy|sell --qty N --urgency 0..1\n\
         \n\
         reads AppConfig from PATH, queries the SOR venue set,\n\
         and prints one GreedyRouter decision."
    );
    std::process::exit(1);
}

struct Args {
    config_path: PathBuf,
    side: Side,
    qty: Decimal,
    urgency: Decimal,
}

fn parse_args() -> Args {
    let mut config: Option<PathBuf> = None;
    let mut side: Option<Side> = None;
    let mut qty: Option<Decimal> = None;
    let mut urgency: Option<Decimal> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                config = it.next().map(PathBuf::from);
            }
            "--side" => match it.next().as_deref() {
                Some("buy") => side = Some(Side::Buy),
                Some("sell") => side = Some(Side::Sell),
                _ => print_usage_and_exit(),
            },
            "--qty" => {
                qty = it.next().and_then(|s| s.parse().ok());
            }
            "--urgency" => {
                urgency = it.next().and_then(|s| s.parse().ok());
            }
            "-h" | "--help" => print_usage_and_exit(),
            _ => print_usage_and_exit(),
        }
    }
    Args {
        config_path: config.unwrap_or_else(|| print_usage_and_exit()),
        side: side.unwrap_or_else(|| print_usage_and_exit()),
        qty: qty.unwrap_or_else(|| print_usage_and_exit()),
        urgency: urgency.unwrap_or_else(|| print_usage_and_exit()),
    }
}

/// Same `VenueId` mapping `server/main.rs` uses — kept local so
/// the CLI doesn't pull the full `main.rs` link graph.
fn venue_id_from_exchange_type(ex: ExchangeType) -> VenueId {
    match ex {
        ExchangeType::Custom => VenueId::Custom,
        ExchangeType::Binance | ExchangeType::BinanceTestnet => VenueId::Binance,
        ExchangeType::Bybit | ExchangeType::BybitTestnet => VenueId::Bybit,
        ExchangeType::HyperLiquid | ExchangeType::HyperLiquidTestnet => VenueId::HyperLiquid,
    }
}

/// Construct one connector from an `ExchangeConfig`. Matches the
/// per-venue routing `server::main::create_hedge_connector` uses
/// — duplicating the match arms here keeps the CLI self-contained
/// without pulling `mm-server::main` as a dep.
fn build_connector(cfg: &ExchangeConfig) -> Result<Arc<dyn ExchangeConnector>> {
    let api_key = cfg.api_key.clone().unwrap_or_default();
    let api_secret = cfg.api_secret.clone().unwrap_or_default();
    match cfg.exchange_type {
        ExchangeType::Custom => Ok(Arc::new(mm_exchange_client::CustomConnector::new(
            &cfg.rest_url,
            &cfg.ws_url,
        ))),
        ExchangeType::Binance => {
            use mm_common::config::ProductType;
            match cfg.product {
                ProductType::Spot => Ok(Arc::new(mm_exchange_binance::BinanceConnector::new(
                    &cfg.rest_url,
                    &cfg.ws_url,
                    &api_key,
                    &api_secret,
                ))),
                ProductType::LinearPerp => Ok(Arc::new(
                    mm_exchange_binance::BinanceFuturesConnector::new(&api_key, &api_secret),
                )),
                ProductType::InversePerp => {
                    anyhow::bail!("Binance COIN-M (inverse perp) not supported by mm-route yet")
                }
            }
        }
        ExchangeType::BinanceTestnet => {
            use mm_common::config::ProductType;
            match cfg.product {
                ProductType::Spot => Ok(Arc::new(mm_exchange_binance::BinanceConnector::testnet(
                    &api_key,
                    &api_secret,
                ))),
                ProductType::LinearPerp => Ok(Arc::new(
                    mm_exchange_binance::BinanceFuturesConnector::testnet(&api_key, &api_secret),
                )),
                ProductType::InversePerp => {
                    anyhow::bail!("Binance COIN-M (inverse perp) not supported by mm-route yet")
                }
            }
        }
        ExchangeType::Bybit => {
            use mm_common::config::ProductType;
            let conn: Arc<dyn ExchangeConnector> = match cfg.product {
                ProductType::Spot => Arc::new(mm_exchange_bybit::BybitConnector::spot(
                    &api_key,
                    &api_secret,
                )),
                ProductType::LinearPerp => Arc::new(mm_exchange_bybit::BybitConnector::linear(
                    &api_key,
                    &api_secret,
                )),
                ProductType::InversePerp => Arc::new(mm_exchange_bybit::BybitConnector::inverse(
                    &api_key,
                    &api_secret,
                )),
            };
            Ok(conn)
        }
        ExchangeType::BybitTestnet => {
            use mm_common::config::ProductType;
            let conn: Arc<dyn ExchangeConnector> = match cfg.product {
                ProductType::Spot => Arc::new(mm_exchange_bybit::BybitConnector::testnet_spot(
                    &api_key,
                    &api_secret,
                )),
                ProductType::LinearPerp => Arc::new(mm_exchange_bybit::BybitConnector::testnet(
                    &api_key,
                    &api_secret,
                )),
                ProductType::InversePerp => Arc::new(
                    mm_exchange_bybit::BybitConnector::testnet_inverse(&api_key, &api_secret),
                ),
            };
            Ok(conn)
        }
        ExchangeType::HyperLiquid => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::new(&api_secret)?,
        )),
        ExchangeType::HyperLiquidTestnet => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::testnet(&api_secret)?,
        )),
    }
}

/// One venue the router will consider.
struct VenueTriple {
    venue: VenueId,
    symbol: String,
    connector: Arc<dyn ExchangeConnector>,
}

/// Collect the full list of venues the router will consider —
/// primary + hedge (if configured) + every `sor_extra_venues`
/// entry.
fn collect_venues(config: &AppConfig) -> Result<Vec<VenueTriple>> {
    let mut out = Vec::new();
    let primary_sym = config
        .symbols
        .first()
        .cloned()
        .context("config.symbols is empty — mm-route needs at least one primary symbol")?;
    let primary = build_connector(&config.exchange)?;
    out.push(VenueTriple {
        venue: venue_id_from_exchange_type(config.exchange.exchange_type),
        symbol: primary_sym.clone(),
        connector: primary,
    });
    if let Some(h) = &config.hedge {
        let hedge = build_connector(&h.exchange)?;
        out.push(VenueTriple {
            venue: venue_id_from_exchange_type(h.exchange.exchange_type),
            symbol: h.pair.hedge_symbol.clone(),
            connector: hedge,
        });
    }
    for v in &config.sor_extra_venues {
        let conn = build_connector(&v.exchange)?;
        out.push(VenueTriple {
            venue: venue_id_from_exchange_type(v.exchange.exchange_type),
            symbol: v.symbol.clone(),
            connector: conn,
        });
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    let raw = std::fs::read_to_string(&args.config_path)
        .with_context(|| format!("reading config {:?}", args.config_path))?;
    let config: AppConfig = toml::from_str(&raw).context("parsing AppConfig TOML")?;
    let venues = collect_venues(&config)?;
    if venues.is_empty() {
        anyhow::bail!("no venues configured — aborting");
    }

    let mut agg = VenueStateAggregator::new();
    println!("Probing {} venue(s)...", venues.len());
    for v in &venues {
        let product: ProductSpec = match v.connector.get_product_spec(&v.symbol).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "  ⚠ {:?} get_product_spec({}) failed: {e}",
                    v.venue, v.symbol
                );
                continue;
            }
        };
        let mut seed = VenueSeed::new(&v.symbol, product, args.qty);
        match v.connector.get_orderbook(&v.symbol, 1).await {
            Ok((bids, asks, _seq)) => {
                if let Some(b) = bids.first() {
                    seed.best_bid = b.price;
                }
                if let Some(a) = asks.first() {
                    seed.best_ask = a.price;
                }
            }
            Err(e) => eprintln!("  ⚠ {:?} get_orderbook({}) failed: {e}", v.venue, v.symbol),
        }
        agg.register_venue(v.venue, seed);
        println!(
            "  {:?} {}  bid={}  ask={}",
            v.venue,
            v.symbol,
            agg.seed(v.venue)
                .map(|s| s.best_bid)
                .unwrap_or(Decimal::ZERO),
            agg.seed(v.venue)
                .map(|s| s.best_ask)
                .unwrap_or(Decimal::ZERO),
        );
    }

    // Use the synthetic-snapshot path — it takes a
    // `(venue, remaining)` tuple slice and returns one
    // snapshot per registered venue without hitting the
    // rate-limit network call.
    let venue_budgets: Vec<_> = agg.venues().into_iter().map(|v| (v, 100u32)).collect();
    let snapshots = agg.collect_synthetic(&venue_budgets);
    let router = GreedyRouter::new(VenueCostModel::default_v1());
    let decision = router.route(args.side, args.qty, args.urgency, &snapshots);

    println!();
    println!("== Route decision ==");
    println!("  side      {:?}", decision.target_side);
    println!("  qty       {}", decision.target_qty);
    println!("  urgency   {}", args.urgency);
    println!("  legs      {}", decision.legs.len());
    if decision.legs.is_empty() {
        println!("  (router produced no legs — no venue could serve the target)");
        std::process::exit(2);
    }
    for (i, leg) in decision.legs.iter().enumerate() {
        println!(
            "  leg[{i}]  {:?}  qty={}  {}  cost={} bps",
            leg.venue,
            leg.qty,
            if leg.is_taker { "TAKER" } else { "MAKER" },
            leg.expected_cost_bps,
        );
    }
    Ok(())
}
