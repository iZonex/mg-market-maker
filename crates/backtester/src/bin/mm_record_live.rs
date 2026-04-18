//! Record public market data from Binance / Bybit / HyperLiquid
//! to a JSONL file compatible with
//! [`mm_backtester::data::RecordedEvent`].
//!
//! No API keys required — all three venues expose public
//! orderbook + trade streams. Same output shape across venues so
//! `mm-calibrate` reads any recording without modification.
//!
//! Usage:
//! ```bash
//! # Binance spot (default)
//! cargo run -p mm-backtester --bin mm-record-live -- \
//!   --venue binance --symbol btcusdt --duration-secs 300 \
//!   --out data/recorded/binance-btcusdt.jsonl
//!
//! # Bybit V5 spot
//! cargo run -p mm-backtester --bin mm-record-live -- \
//!   --venue bybit --symbol BTCUSDT --out data/recorded/bybit-btcusdt.jsonl
//!
//! # HyperLiquid perp
//! cargo run -p mm-backtester --bin mm-record-live -- \
//!   --venue hl --symbol BTC --out data/recorded/hl-btc.jsonl
//! ```

use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use mm_backtester::data::{EventRecorder, RecordedEvent};
use mm_common::types::{Price, PriceLevel, Qty, Side};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info};

#[derive(Clone, Copy, Debug)]
enum Venue {
    Binance,
    Bybit,
    HyperLiquid,
}

impl Venue {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "binance" => Some(Venue::Binance),
            "bybit" => Some(Venue::Bybit),
            "hl" | "hyperliquid" | "hyper_liquid" => Some(Venue::HyperLiquid),
            _ => None,
        }
    }
}

struct Args {
    venue: Venue,
    symbol: String,
    duration: Duration,
    out: PathBuf,
}

fn parse_args() -> Args {
    let mut venue = Venue::Binance;
    let mut symbol_override: Option<String> = None;
    let mut duration_secs = 300u64;
    let mut out_override: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--venue" => {
                venue = Venue::parse(&args[i + 1]).unwrap_or_else(|| {
                    eprintln!("unknown --venue {} (binance|bybit|hl)", args[i + 1]);
                    std::process::exit(2);
                });
                i += 2;
            }
            "--symbol" => {
                symbol_override = Some(args[i + 1].clone());
                i += 2;
            }
            "--duration-secs" => {
                duration_secs = args[i + 1].parse().unwrap_or(300);
                i += 2;
            }
            "--out" => {
                out_override = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => i += 1,
        }
    }
    let symbol = symbol_override.unwrap_or_else(|| default_symbol(venue));
    let out = out_override.unwrap_or_else(|| default_out(venue, &symbol));
    Args {
        venue,
        symbol,
        duration: Duration::from_secs(duration_secs),
        out,
    }
}

fn default_symbol(venue: Venue) -> String {
    match venue {
        Venue::Binance => "btcusdt".into(),
        Venue::Bybit => "BTCUSDT".into(),
        Venue::HyperLiquid => "BTC".into(),
    }
}

fn default_out(venue: Venue, symbol: &str) -> PathBuf {
    let tag = match venue {
        Venue::Binance => "binance",
        Venue::Bybit => "bybit",
        Venue::HyperLiquid => "hl",
    };
    PathBuf::from(format!(
        "data/recorded/{tag}-{}.jsonl",
        symbol.to_ascii_lowercase()
    ))
}

fn parse_level_pair(raw: &[String; 2]) -> Option<PriceLevel> {
    let price = Decimal::from_str(&raw[0]).ok()?;
    let qty = Decimal::from_str(&raw[1]).ok()?;
    if qty.is_zero() {
        None
    } else {
        Some(PriceLevel { price, qty })
    }
}

fn parse_levels(raw: &[[String; 2]]) -> Vec<PriceLevel> {
    raw.iter().filter_map(parse_level_pair).collect()
}

// ── Binance ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BinDepth {
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
    #[serde(default, rename = "lastUpdateId")]
    last_update_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BinTrade {
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    qty: String,
    #[serde(rename = "m")]
    buyer_is_maker: bool,
}

#[derive(Debug, Deserialize)]
struct BinEnvelope {
    stream: String,
    data: serde_json::Value,
}

async fn record_binance(args: &Args, recorder: &mut EventRecorder) -> Result<(u64, u64)> {
    let sym = args.symbol.to_ascii_lowercase();
    // Seed with REST snapshot so the first sequence anchors the stream.
    let snap_url = format!(
        "https://api.binance.com/api/v3/depth?symbol={}&limit=100",
        sym.to_ascii_uppercase()
    );
    #[derive(Deserialize)]
    struct Snap {
        bids: Vec<[String; 2]>,
        asks: Vec<[String; 2]>,
        #[serde(rename = "lastUpdateId")]
        last_update_id: u64,
    }
    let snap: Snap = reqwest::get(&snap_url)
        .await
        .context("binance snapshot")?
        .json()
        .await
        .context("binance snapshot decode")?;
    recorder.record(&RecordedEvent::BookSnapshot {
        timestamp: Utc::now(),
        bids: parse_levels(&snap.bids),
        asks: parse_levels(&snap.asks),
        sequence: snap.last_update_id,
    })?;

    let url = format!(
        "wss://stream.binance.com:9443/stream?streams={sym}@depth20@100ms/{sym}@trade"
    );
    let (ws, _) = connect_async(&url).await.context("binance ws")?;
    let (_w, mut r) = ws.split();
    info!(%url, "binance ws connected");

    let deadline = tokio::time::Instant::now() + args.duration;
    let (mut book, mut trade) = (0u64, 0u64);
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, r.next()).await {
        let Ok(Message::Text(txt)) = msg else { continue };
        let Ok(env) = serde_json::from_str::<BinEnvelope>(&txt) else {
            continue;
        };
        if env.stream.ends_with("@trade") {
            if let Ok(t) = serde_json::from_value::<BinTrade>(env.data) {
                let (Ok(p), Ok(q)) =
                    (Decimal::from_str(&t.price), Decimal::from_str(&t.qty))
                else {
                    continue;
                };
                let side = if t.buyer_is_maker { Side::Sell } else { Side::Buy };
                recorder.record(&RecordedEvent::Trade {
                    timestamp: Utc::now(),
                    price: p as Price,
                    qty: q as Qty,
                    taker_side: side,
                })?;
                trade += 1;
            }
        } else if env.stream.contains("@depth20") {
            if let Ok(d) = serde_json::from_value::<BinDepth>(env.data) {
                recorder.record(&RecordedEvent::BookSnapshot {
                    timestamp: Utc::now(),
                    bids: parse_levels(&d.bids),
                    asks: parse_levels(&d.asks),
                    sequence: d.last_update_id.unwrap_or(0),
                })?;
                book += 1;
                if book.is_multiple_of(500) {
                    let _ = recorder.flush();
                }
            }
        }
    }
    Ok((book, trade))
}

// ── Bybit V5 spot ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BybitOrderbookData {
    #[serde(default)]
    b: Vec<[String; 2]>,
    #[serde(default)]
    a: Vec<[String; 2]>,
    #[serde(default)]
    u: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BybitTrade {
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "v")]
    qty: String,
    #[serde(rename = "S")]
    side: String,
}

async fn record_bybit(args: &Args, recorder: &mut EventRecorder) -> Result<(u64, u64)> {
    let sym = args.symbol.to_ascii_uppercase();
    let url = "wss://stream.bybit.com/v5/public/spot";
    let (ws, _) = connect_async(url).await.context("bybit ws")?;
    let (mut w, mut r) = ws.split();

    let sub = serde_json::json!({
        "op": "subscribe",
        "args": [
            format!("orderbook.50.{sym}"),
            format!("publicTrade.{sym}"),
        ]
    });
    w.send(Message::text(sub.to_string()))
        .await
        .context("bybit subscribe")?;
    info!(%url, %sym, "bybit ws connected + subscribed");

    let deadline = tokio::time::Instant::now() + args.duration;
    let (mut book, mut trade) = (0u64, 0u64);
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, r.next()).await {
        let Ok(Message::Text(txt)) = msg else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        let Some(topic) = v.get("topic").and_then(|x| x.as_str()) else {
            continue;
        };
        if topic.starts_with("orderbook.") {
            if let Some(d) = v.get("data") {
                if let Ok(ob) = serde_json::from_value::<BybitOrderbookData>(d.clone()) {
                    recorder.record(&RecordedEvent::BookSnapshot {
                        timestamp: Utc::now(),
                        bids: parse_levels(&ob.b),
                        asks: parse_levels(&ob.a),
                        sequence: ob.u.unwrap_or(0),
                    })?;
                    book += 1;
                    if book.is_multiple_of(500) {
                        let _ = recorder.flush();
                    }
                }
            }
        } else if topic.starts_with("publicTrade.") {
            if let Some(arr) = v.get("data").and_then(|x| x.as_array()) {
                for t in arr {
                    let Ok(t) = serde_json::from_value::<BybitTrade>(t.clone()) else {
                        continue;
                    };
                    let (Ok(p), Ok(q)) =
                        (Decimal::from_str(&t.price), Decimal::from_str(&t.qty))
                    else {
                        continue;
                    };
                    let side = match t.side.as_str() {
                        "Buy" => Side::Buy,
                        "Sell" => Side::Sell,
                        _ => continue,
                    };
                    recorder.record(&RecordedEvent::Trade {
                        timestamp: Utc::now(),
                        price: p as Price,
                        qty: q as Qty,
                        taker_side: side,
                    })?;
                    trade += 1;
                }
            }
        }
    }
    Ok((book, trade))
}

// ── HyperLiquid ─────────────────────────────────────────────

async fn record_hyperliquid(args: &Args, recorder: &mut EventRecorder) -> Result<(u64, u64)> {
    let coin = args.symbol.clone();
    let url = "wss://api.hyperliquid.xyz/ws";
    let (ws, _) = connect_async(url).await.context("hl ws")?;
    let (mut w, mut r) = ws.split();

    for sub_type in ["l2Book", "trades"] {
        let msg = serde_json::json!({
            "method": "subscribe",
            "subscription": {"type": sub_type, "coin": coin},
        });
        w.send(Message::text(msg.to_string()))
            .await
            .context("hl subscribe")?;
    }
    info!(%url, %coin, "hl ws connected + subscribed");

    let deadline = tokio::time::Instant::now() + args.duration;
    let (mut book, mut trade) = (0u64, 0u64);
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, r.next()).await {
        let Ok(Message::Text(txt)) = msg else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        let channel = v.get("channel").and_then(|x| x.as_str()).unwrap_or("");
        let data = v.get("data");
        match channel {
            "l2Book" => {
                // data.levels = [[bids], [asks]] where each level is
                // {"px": "...", "sz": "...", "n": ...}
                let Some(data) = data else { continue };
                let Some(levels) = data.get("levels").and_then(|x| x.as_array()) else {
                    continue;
                };
                if levels.len() != 2 {
                    continue;
                }
                let parse_side = |arr: &serde_json::Value| -> Vec<PriceLevel> {
                    arr.as_array()
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(|e| {
                                    let px = e.get("px")?.as_str()?;
                                    let sz = e.get("sz")?.as_str()?;
                                    let (Ok(p), Ok(q)) = (
                                        Decimal::from_str(px),
                                        Decimal::from_str(sz),
                                    ) else {
                                        return None;
                                    };
                                    if q.is_zero() {
                                        None
                                    } else {
                                        Some(PriceLevel { price: p, qty: q })
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let bids = parse_side(&levels[0]);
                let asks = parse_side(&levels[1]);
                let seq = data.get("time").and_then(|t| t.as_u64()).unwrap_or(0);
                recorder.record(&RecordedEvent::BookSnapshot {
                    timestamp: Utc::now(),
                    bids,
                    asks,
                    sequence: seq,
                })?;
                book += 1;
                if book.is_multiple_of(500) {
                    let _ = recorder.flush();
                }
            }
            "trades" => {
                let Some(arr) = data.and_then(|d| d.as_array()) else {
                    continue;
                };
                for t in arr {
                    let px = t.get("px").and_then(|x| x.as_str()).unwrap_or("0");
                    let sz = t.get("sz").and_then(|x| x.as_str()).unwrap_or("0");
                    let side_str = t.get("side").and_then(|x| x.as_str()).unwrap_or("");
                    let (Ok(p), Ok(q)) = (Decimal::from_str(px), Decimal::from_str(sz))
                    else {
                        continue;
                    };
                    let taker_side = match side_str {
                        "B" => Side::Buy,
                        "A" => Side::Sell,
                        _ => continue,
                    };
                    recorder.record(&RecordedEvent::Trade {
                        timestamp: Utc::now(),
                        price: p as Price,
                        qty: q as Qty,
                        taker_side,
                    })?;
                    trade += 1;
                }
            }
            _ => {}
        }
    }
    Ok((book, trade))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args = parse_args();
    info!(
        venue = ?args.venue,
        symbol = %args.symbol,
        secs = args.duration.as_secs(),
        out = %args.out.display(),
        "starting live recorder"
    );

    let mut recorder = EventRecorder::new(&args.out)?;
    let (book, trade) = match args.venue {
        Venue::Binance => record_binance(&args, &mut recorder).await,
        Venue::Bybit => record_bybit(&args, &mut recorder).await,
        Venue::HyperLiquid => record_hyperliquid(&args, &mut recorder).await,
    }?;

    recorder.flush()?;
    info!(book, trades = trade, "recording complete");
    if book == 0 {
        error!("no book events recorded — is the --venue / --symbol correct?");
    }
    Ok(())
}
