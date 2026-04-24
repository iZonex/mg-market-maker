//! Fit Avellaneda-Stoikov parameters from a recorded event stream.
//!
//! Computes:
//! - **σ (sigma)** — per-second realised volatility from mid-price
//!   log returns. Reported at the 1-second cadence the AS model
//!   assumes; the engine scales it by time horizon internally.
//! - **κ (kappa)** — order arrival intensity fit from trade
//!   inter-arrival time. We use the average `1 / Δt` across trade
//!   events as a first-order MLE estimate. The paper's κ is
//!   conditioned on distance-from-mid; this gives a reasonable
//!   baseline.
//! - **μ_mid** — median mid-price so the operator can sanity-check
//!   the recording window.
//!
//! The script emits a toml fragment that can be pasted into
//! `config/default.toml` or saved to a per-venue config.
//!
//! Usage:
//! ```bash
//! cargo run -p mm-backtester --bin mm-calibrate -- \
//!     --input data/recorded/binance-btcusdt.jsonl \
//!     --out config/btcusdt-binance-live.toml
//! ```

use anyhow::{bail, Context, Result};
use mm_backtester::data::{load_events, RecordedEvent};
use rust_decimal::prelude::ToPrimitive;
use std::path::PathBuf;

struct Args {
    input: PathBuf,
    out: Option<PathBuf>,
    strategy: String,
}

fn parse_args() -> Args {
    let mut input = PathBuf::from("data/recorded/binance-btcusdt.jsonl");
    let mut out: Option<PathBuf> = None;
    let mut strategy = "avellaneda_stoikov".to_string();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                input = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--strategy" => {
                strategy = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    Args {
        input,
        out,
        strategy,
    }
}

fn main() -> Result<()> {
    let args = parse_args();
    let events = load_events(&args.input).context("load recorded events")?;
    if events.is_empty() {
        bail!("no events in input");
    }

    // Pass 1 — collect mid-price time series + trade timestamps.
    let mut mids: Vec<(chrono::DateTime<chrono::Utc>, f64)> = Vec::new();
    let mut trade_ts: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();
    let mut trade_qty_sum = 0.0f64;
    let mut trade_count = 0u64;
    let mut window_start = None;
    let mut window_end = None;

    for ev in &events {
        match ev {
            RecordedEvent::BookSnapshot {
                timestamp,
                bids,
                asks,
                ..
            } => {
                window_start.get_or_insert(*timestamp);
                window_end = Some(*timestamp);
                let (Some(bb), Some(ba)) = (bids.first(), asks.first()) else {
                    continue;
                };
                let bb = bb.price.to_f64().unwrap_or(0.0);
                let ba = ba.price.to_f64().unwrap_or(0.0);
                if bb > 0.0 && ba > 0.0 {
                    mids.push((*timestamp, 0.5 * (bb + ba)));
                }
            }
            RecordedEvent::Trade { timestamp, qty, .. } => {
                trade_ts.push(*timestamp);
                trade_qty_sum += qty.to_f64().unwrap_or(0.0);
                trade_count += 1;
            }
        }
    }

    if mids.len() < 2 {
        bail!("not enough mid-price observations to compute σ");
    }

    // σ — per-second realised vol of log-returns. We bin mids to
    // 1-second buckets (take the last in each second) so the σ
    // estimate matches the "per second" scale the AS model uses.
    let mut per_sec: Vec<f64> = Vec::new();
    {
        let mut current_sec = mids[0].0.timestamp();
        let mut current_mid = mids[0].1;
        for (t, m) in &mids {
            let sec = t.timestamp();
            if sec != current_sec {
                per_sec.push(current_mid);
                current_sec = sec;
            }
            current_mid = *m;
        }
        per_sec.push(current_mid);
    }

    let log_returns: Vec<f64> = per_sec
        .windows(2)
        .map(|w| (w[1] / w[0]).ln())
        .filter(|x| x.is_finite())
        .collect();
    if log_returns.is_empty() {
        bail!("no usable log-returns");
    }

    let mean_ret = log_returns.iter().sum::<f64>() / log_returns.len() as f64;
    let var_ret = log_returns
        .iter()
        .map(|r| (r - mean_ret).powi(2))
        .sum::<f64>()
        / log_returns.len() as f64;
    let sigma_per_sec = var_ret.sqrt();

    // κ — trade arrival rate, trades per second.
    let (kappa, mean_trade_size) = if trade_count >= 2 {
        let total_secs = (trade_ts.last().unwrap().timestamp_millis()
            - trade_ts.first().unwrap().timestamp_millis()) as f64
            / 1000.0;
        let rate = if total_secs > 0.0 {
            trade_count as f64 / total_secs
        } else {
            0.0
        };
        let avg_size = trade_qty_sum / trade_count as f64;
        (rate, avg_size)
    } else {
        (0.0, 0.0)
    };

    let median_mid = {
        let mut mm: Vec<f64> = mids.iter().map(|(_, m)| *m).collect();
        mm.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        mm[mm.len() / 2]
    };

    let window_secs = match (window_start, window_end) {
        (Some(a), Some(b)) => (b.timestamp_millis() - a.timestamp_millis()) as f64 / 1000.0,
        _ => 0.0,
    };

    // γ is a risk preference, not a venue statistic. We keep it
    // at a conservative default and let the operator tune it per
    // their capital. 0.1 is the reference in the original A-S
    // paper for a small-account desk — flag this in the emitted
    // toml so nobody forgets.
    let gamma = 0.1;

    println!("\n=== Calibration summary ===");
    println!("  window:      {:.1} s", window_secs);
    println!("  book obs:    {}", mids.len());
    println!("  trades:      {}", trade_count);
    println!("  median mid:  {:.2}", median_mid);
    println!("  σ (per s):   {:.6}", sigma_per_sec);
    println!("  κ (1/s):     {:.4}", kappa);
    println!("  avg trade:   {:.6}", mean_trade_size);
    println!("  γ (picked):  {:.4} (judgment call)\n", gamma);

    let toml_fragment = format!(
        "# Auto-generated by mm-calibrate from {input}\n\
         # Recording window: {window_secs:.0} s, {book_obs} book obs, {trades} trades.\n\
         # Median mid during recording: {median_mid:.2}.\n\
         #\n\
         # σ and κ are point estimates — rerun the recorder + calibrator\n\
         # periodically (e.g. nightly) and update this file. γ is a risk\n\
         # preference; tune it against your capital and drawdown tolerance.\n\
         \n\
         [market_maker]\n\
         strategy = \"{strategy}\"\n\
         gamma = \"{gamma:.4}\"\n\
         kappa = \"{kappa:.4}\"\n\
         sigma = \"{sigma:.6}\"\n\
         time_horizon_secs = 300\n\
         num_levels = 3\n\
         # A first-order order_size: 1/10 of the average observed trade qty.\n\
         order_size = \"{order_size:.6}\"\n\
         refresh_interval_ms = 500\n\
         min_spread_bps = \"3\"\n\
         max_distance_bps = \"80\"\n",
        input = args.input.display(),
        window_secs = window_secs,
        book_obs = mids.len(),
        trades = trade_count,
        median_mid = median_mid,
        strategy = args.strategy,
        gamma = gamma,
        kappa = kappa,
        sigma = sigma_per_sec,
        order_size = (mean_trade_size / 10.0).max(0.00001),
    );

    if let Some(out) = args.out {
        std::fs::write(&out, &toml_fragment).context("write toml")?;
        println!("wrote {}", out.display());
    } else {
        println!("{toml_fragment}");
    }

    Ok(())
}
