//! `mm-probe` — offline driver for the Market Resilience,
//! Order-to-Trade Ratio and HMA signal pipelines.
//!
//! Reads a JSONL event recording produced by the backtester's
//! `EventRecorder` and streams the recorded `BookSnapshot` /
//! `Trade` events through the three signal calculators
//! **without** running the full engine. Prints a human-readable
//! time series to stdout so operators can exercise the
//! pipelines, calibrate thresholds and sanity-check behaviour
//! on real market data offline.
//!
//! Usage:
//!
//! ```bash
//! cargo run -p mm-backtester --bin mm-probe -- \
//!     --events data/replays/btcusdt.jsonl \
//!     --stride 10
//! ```
//!
//! The `--stride` flag controls how often a row is emitted —
//! e.g. `--stride 10` prints every 10th event. Defaults to 1
//! (every event).

use std::path::PathBuf;

use anyhow::{Context, Result};
use mm_backtester::data::{load_events, RecordedEvent};
use mm_common::types::PriceLevel;
use mm_indicators::Hma;
use mm_risk::otr::OrderToTradeRatio;
use mm_strategy::market_resilience::{MarketResilienceCalculator, MrConfig};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

/// Parsed command-line flags.
struct Args {
    events: PathBuf,
    stride: usize,
    mr_warmup: usize,
    hma_window: usize,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut events: Option<PathBuf> = None;
    let mut stride = 1_usize;
    let mut mr_warmup = 5_usize;
    let mut hma_window = 9_usize;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--events" => {
                let p = args.next().context("--events requires a path argument")?;
                events = Some(PathBuf::from(p));
            }
            "--stride" => {
                let s = args.next().context("--stride requires a number")?;
                stride = s.parse().context("--stride must be a positive integer")?;
                if stride == 0 {
                    anyhow::bail!("--stride must be >= 1");
                }
            }
            "--mr-warmup" => {
                let s = args.next().context("--mr-warmup requires a number")?;
                mr_warmup = s
                    .parse()
                    .context("--mr-warmup must be a positive integer")?;
            }
            "--hma-window" => {
                let s = args.next().context("--hma-window requires a number")?;
                hma_window = s.parse().context("--hma-window must be >= 4")?;
                if hma_window < 4 {
                    anyhow::bail!("--hma-window must be >= 4");
                }
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    let events = events.context("--events is required")?;
    Ok(Args {
        events,
        stride,
        mr_warmup,
        hma_window,
    })
}

fn print_usage() {
    eprintln!(
        "mm-probe — offline driver for MR / OTR / HMA signals\n\
         \n\
         Usage: mm-probe --events <path.jsonl> [--stride N] [--mr-warmup N] [--hma-window N]\n\
         \n\
         Flags:\n  \
         --events <path>   JSONL recording produced by the backtester\n  \
         --stride N        print every Nth event (default 1)\n  \
         --mr-warmup N     Market Resilience warmup sample count (default 5)\n  \
         --hma-window N    HMA window (default 9)\n"
    );
}

fn main() -> Result<()> {
    let args = parse_args()?;

    eprintln!(
        "mm-probe: loading {}, stride={}, mr_warmup={}, hma_window={}",
        args.events.display(),
        args.stride,
        args.mr_warmup,
        args.hma_window
    );
    let events = load_events(&args.events)
        .with_context(|| format!("failed to load events from {}", args.events.display()))?;
    eprintln!("mm-probe: loaded {} events", events.len());

    let mr_config = MrConfig {
        warmup_samples: args.mr_warmup,
        ..MrConfig::default()
    };
    let mut mr = MarketResilienceCalculator::new(mr_config);
    let mut otr = OrderToTradeRatio::new();
    let mut hma = Hma::new(args.hma_window);

    // Header line. Tab-separated so callers can pipe into
    // `column -t` or import as TSV.
    println!("idx\tt_ms\ttype\tmid\tmr\totr\thma");

    let mut emitted = 0_usize;
    for (idx, ev) in events.iter().enumerate() {
        match ev {
            RecordedEvent::BookSnapshot {
                timestamp,
                bids,
                asks,
                ..
            } => {
                otr.on_update();
                let ts_ns = timestamp.timestamp_nanos_opt().unwrap_or(0);
                let (top_bids, top_asks) = top_n(bids, asks, 10);
                mr.on_book(&top_bids, &top_asks, ts_ns);
                if let Some(mid) = mid_price(&top_bids, &top_asks) {
                    hma.update(mid);
                    if idx % args.stride == 0 {
                        print_row(
                            idx,
                            timestamp.timestamp_millis(),
                            "book",
                            Some(mid),
                            mr.score(ts_ns),
                            otr.ratio(),
                            hma.value(),
                        );
                        emitted += 1;
                    }
                }
            }
            RecordedEvent::Trade {
                timestamp,
                price,
                qty,
                ..
            } => {
                otr.on_trade();
                let ts_ns = timestamp.timestamp_nanos_opt().unwrap_or(0);
                mr.on_trade(*qty, ts_ns);
                if idx % args.stride == 0 {
                    print_row(
                        idx,
                        timestamp.timestamp_millis(),
                        "trade",
                        Some(*price),
                        mr.score(ts_ns),
                        otr.ratio(),
                        hma.value(),
                    );
                    emitted += 1;
                }
            }
        }
    }
    eprintln!("mm-probe: printed {emitted} rows");
    Ok(())
}

/// Return best-first top-`n` slices for each side. `RecordedEvent`
/// bids come sorted high-to-low already.
fn top_n(bids: &[PriceLevel], asks: &[PriceLevel], n: usize) -> (Vec<PriceLevel>, Vec<PriceLevel>) {
    (
        bids.iter().take(n).cloned().collect(),
        asks.iter().take(n).cloned().collect(),
    )
}

fn mid_price(bids: &[PriceLevel], asks: &[PriceLevel]) -> Option<Decimal> {
    let bb = bids.first()?;
    let ba = asks.first()?;
    Some((bb.price + ba.price) / Decimal::from(2))
}

fn print_row(
    idx: usize,
    t_ms: i64,
    kind: &str,
    mid: Option<Decimal>,
    mr: Decimal,
    otr: Decimal,
    hma: Option<Decimal>,
) {
    let mid_str = mid
        .map(|m| m.to_f64().map(|f| format!("{f:.4}")).unwrap_or_default())
        .unwrap_or_default();
    let hma_str = hma
        .and_then(|h| h.to_f64().map(|f| format!("{f:.4}")))
        .unwrap_or_else(|| "-".to_string());
    let mr_f = mr.to_f64().unwrap_or(1.0);
    let otr_f = otr.to_f64().unwrap_or(0.0);
    println!("{idx}\t{t_ms}\t{kind}\t{mid_str}\t{mr_f:.4}\t{otr_f:.4}\t{hma_str}");
}
