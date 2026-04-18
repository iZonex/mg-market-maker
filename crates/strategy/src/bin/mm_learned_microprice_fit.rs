//! Offline fit tool for the Stoikov 2018 learned-microprice
//! G-function. Reads a JSONL tape of L1 book snapshots, fits the
//! `(imbalance, spread) -> Δmid` histogram, and writes the
//! finalised model out as TOML.
//!
//! # CLI
//!
//! ```text
//! mm-learned-microprice-fit \
//!     --input  path/to/tape.jsonl \
//!     --output path/to/model.toml \
//!     [--horizon 10] \
//!     [--imbalance-buckets 20] \
//!     [--spread-buckets 5] \
//!     [--min-bucket-samples 100]
//! ```
//!
//! # Input schema
//!
//! Each line of the input file must be a JSON object of the
//! shape:
//!
//! ```json
//! { "ts": 123456, "bid": 49990.5, "bid_qty": 1.2, "ask": 50010.5, "ask_qty": 0.8, "mid": 50000.5 }
//! ```
//!
//! `ts` and `mid` are the only fields strictly required by the
//! fitter — `bid` / `bid_qty` / `ask` / `ask_qty` are used to
//! compute the top-of-book imbalance
//! `(bid_qty − ask_qty) / (bid_qty + ask_qty)`.
//!
//! This tape format is intentionally decoupled from the
//! `mm-backtester` event format so the fit tool stays trivially
//! replayable from research notebooks that spit out flat L1
//! tapes. Callers that want to replay a full backtester tape can
//! convert on the fly in ~10 lines of awk / jq.
//!
//! # Two-pass fit
//!
//! Because `n_spread_buckets > 1` requires the spread quantile
//! edges to be known before `accumulate_with_edges` is called,
//! the fit runs two passes over the input:
//!
//! 1. **Pass 1:** walk the tape, record spreads, compute the
//!    horizon-`k` forward mid delta for each observation, and
//!    stash the full `(imbalance, spread, Δmid)` triples in
//!    memory.
//! 2. **Edge computation:** sort spreads, pick quantile edges.
//! 3. **Pass 2:** re-walk the triples (in memory), seed
//!    `with_spread_edges`, and call `accumulate_with_edges`.
//! 4. **Finalise + write TOML.**
//!
//! The in-memory triple stash is bounded by the size of the
//! input tape — for research-scale data (millions of rows) this
//! is fine. A streaming two-pass that re-reads the file is a
//! stage-3 optimisation if it becomes a problem.

use anyhow::{anyhow, Context, Result};
use mm_strategy::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct BookSnapshot {
    #[allow(dead_code)]
    ts: i64,
    bid: f64,
    bid_qty: f64,
    ask: f64,
    ask_qty: f64,
    mid: f64,
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    imbalance: Decimal,
    spread: Decimal,
    delta_mid: Decimal,
}

/// Parsed CLI args — stdlib only, no `clap`.
struct CliArgs {
    input: PathBuf,
    output: PathBuf,
    horizon: usize,
    imbalance_buckets: usize,
    spread_buckets: usize,
    min_bucket_samples: usize,
}

impl CliArgs {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut input: Option<PathBuf> = None;
        let mut output: Option<PathBuf> = None;
        let mut horizon: usize = 10;
        let mut imbalance_buckets: usize = 20;
        let mut spread_buckets: usize = 5;
        let mut min_bucket_samples: usize = 100;

        let mut iter = args.into_iter().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--input" => input = Some(PathBuf::from(take_value(&mut iter, "--input")?)),
                "--output" => output = Some(PathBuf::from(take_value(&mut iter, "--output")?)),
                "--horizon" => horizon = take_value(&mut iter, "--horizon")?.parse()?,
                "--imbalance-buckets" => {
                    imbalance_buckets = take_value(&mut iter, "--imbalance-buckets")?.parse()?
                }
                "--spread-buckets" => {
                    spread_buckets = take_value(&mut iter, "--spread-buckets")?.parse()?
                }
                "--min-bucket-samples" => {
                    min_bucket_samples = take_value(&mut iter, "--min-bucket-samples")?.parse()?
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown flag: {other}")),
            }
        }

        Ok(Self {
            input: input.ok_or_else(|| anyhow!("--input is required"))?,
            output: output.ok_or_else(|| anyhow!("--output is required"))?,
            horizon,
            imbalance_buckets,
            spread_buckets,
            min_bucket_samples,
        })
    }
}

fn take_value<I: Iterator<Item = String>>(iter: &mut I, flag: &str) -> Result<String> {
    iter.next()
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn print_usage() {
    eprintln!(
        "mm-learned-microprice-fit \\\n\
         \t--input <path>  \\\n\
         \t--output <path> \\\n\
         \t[--horizon 10] [--imbalance-buckets 20] \\\n\
         \t[--spread-buckets 5] [--min-bucket-samples 100]"
    );
}

fn dec(v: f64) -> Decimal {
    Decimal::from_f64(v).unwrap_or(Decimal::ZERO)
}

/// Parse a JSONL stream into a vector of `BookSnapshot`s.
/// Blank lines and lines starting with `#` are skipped so
/// callers can annotate their tapes.
fn parse_jsonl<R: BufRead>(reader: R) -> Result<Vec<BookSnapshot>> {
    let mut out = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read error at line {}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let snap: BookSnapshot = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse JSONL at line {}", line_no + 1))?;
        out.push(snap);
    }
    Ok(out)
}

/// Pass 1: walk the snapshots and produce one `Observation` per
/// input row whose horizon-`k` forward mid delta exists.
fn build_observations(snaps: &[BookSnapshot], horizon: usize) -> Vec<Observation> {
    if snaps.len() <= horizon {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(snaps.len() - horizon);
    for i in 0..(snaps.len() - horizon) {
        let s = &snaps[i];
        let future = &snaps[i + horizon];
        let qb = s.bid_qty;
        let qa = s.ask_qty;
        let denom = qb + qa;
        if denom == 0.0 {
            continue;
        }
        let imbalance = (qb - qa) / denom;
        let spread = (s.ask - s.bid).max(0.0);
        let delta_mid = future.mid - s.mid;
        out.push(Observation {
            imbalance: dec(imbalance),
            spread: dec(spread),
            delta_mid: dec(delta_mid),
        });
    }
    out
}

/// Compute `n_edges` quantile edges over a sorted slice.
fn quantile_edges(sorted: &[Decimal], n_spread_buckets: usize) -> Vec<Decimal> {
    if n_spread_buckets <= 1 || sorted.is_empty() {
        return Vec::new();
    }
    let n = sorted.len();
    let n_edges = n_spread_buckets - 1;
    let mut edges = Vec::with_capacity(n_edges);
    for k in 1..=n_edges {
        let idx = (k * n) / n_spread_buckets;
        edges.push(sorted[idx.min(n - 1)]);
    }
    edges
}

/// Run the fit against an in-memory observation vector.
/// Factored out of `main` so the smoke test can drive it
/// without a file system.
fn fit_from_observations(
    observations: &[Observation],
    config: LearnedMicropriceConfig,
) -> LearnedMicroprice {
    let n_spread_buckets = config.n_spread_buckets;
    let mut model = LearnedMicroprice::empty(config);

    if n_spread_buckets > 1 {
        let mut spreads: Vec<Decimal> = observations.iter().map(|o| o.spread).collect();
        spreads.sort();
        let edges = quantile_edges(&spreads, n_spread_buckets);
        model.with_spread_edges(edges);
    }

    for obs in observations {
        model.accumulate_with_edges(obs.imbalance, obs.spread, obs.delta_mid);
    }

    model.finalize();
    model
}

fn run(args: CliArgs) -> Result<()> {
    let file = File::open(&args.input)
        .with_context(|| format!("cannot open input {}", args.input.display()))?;
    let reader = BufReader::new(file);
    let snaps = parse_jsonl(reader)?;
    eprintln!("parsed {} snapshots", snaps.len());

    let observations = build_observations(&snaps, args.horizon);
    eprintln!(
        "built {} observations at horizon={}",
        observations.len(),
        args.horizon
    );

    let config = LearnedMicropriceConfig {
        n_imbalance_buckets: args.imbalance_buckets,
        n_spread_buckets: args.spread_buckets,
        min_bucket_samples: args.min_bucket_samples,
        ..Default::default()
    };

    let model = fit_from_observations(&observations, config);
    model.to_toml(&args.output)?;
    eprintln!(
        "wrote fitted model to {} ({} buckets x {} spread buckets)",
        args.output.display(),
        args.imbalance_buckets,
        args.spread_buckets
    );
    Ok(())
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let parsed = match CliArgs::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e:#}");
            print_usage();
            std::process::exit(2);
        }
    };
    if let Err(e) = run(parsed) {
        eprintln!("fit failed: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Smoke test: build a synthetic in-memory tape where
    /// high-imbalance snapshots predict a positive forward mid
    /// delta, run the fitter, and confirm the resulting model
    /// produces a non-zero prediction on the high-imbalance
    /// bucket.
    #[test]
    fn cli_fit_synthetic_tape_produces_nonzero_high_imbalance_prediction() {
        // Build a deterministic tape: 200 snapshots, mid walks
        // upward every other step, imbalance flipped to high
        // when the mid is about to jump.
        let mut snaps = Vec::with_capacity(200);
        let mut mid = 100.0;
        for i in 0..200 {
            let about_to_jump = i % 2 == 0;
            let (bid_qty, ask_qty) = if about_to_jump {
                (9.0, 1.0)
            } else {
                (5.0, 5.0)
            };
            snaps.push(BookSnapshot {
                ts: i as i64,
                bid: mid - 0.5,
                bid_qty,
                ask: mid + 0.5,
                ask_qty,
                mid,
            });
            if about_to_jump {
                mid += 1.0;
            }
        }

        let observations = build_observations(&snaps, 1);
        assert!(
            !observations.is_empty(),
            "synthetic tape should produce observations"
        );

        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 5,
            ..Default::default()
        };
        let model = fit_from_observations(&observations, config);
        assert!(model.is_finalized());

        let high_pred = model.predict(dec!(0.8), dec!(1.0));
        assert!(
            high_pred > Decimal::ZERO,
            "high-imbalance bucket should predict positive Δmid, got {high_pred}",
        );
    }

    #[test]
    fn quantile_edges_splits_evenly() {
        let sorted: Vec<Decimal> = (1..=10).map(Decimal::from).collect();
        let edges = quantile_edges(&sorted, 5);
        assert_eq!(edges.len(), 4);
        // Quantile picks: 2, 4, 6, 8 (1-indexed edge positions).
        assert_eq!(edges[0], Decimal::from(3));
        assert_eq!(edges[3], Decimal::from(9));
    }

    #[test]
    fn parse_jsonl_skips_blank_and_comment_lines() {
        let text = "\n# header\n{\"ts\":1,\"bid\":1.0,\"bid_qty\":2.0,\"ask\":1.5,\"ask_qty\":2.0,\"mid\":1.25}\n";
        let reader = std::io::Cursor::new(text);
        let out = parse_jsonl(reader).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ts, 1);
    }
}
