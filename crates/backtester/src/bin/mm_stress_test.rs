//! `mm-stress-test` — Epic C sub-component #5 CLI.
//!
//! Runs one or more canonical stress scenarios through the
//! synthetic stress runner (`mm_backtester::stress::run_stress`)
//! and emits a markdown report covering max drawdown, time to
//! recovery, inventory peak, kill-switch trips, VaR throttle
//! activations, and hedge basket recommendations.
//!
//! # Usage
//!
//! ```bash
//! cargo run -p mm-backtester --bin mm-stress-test -- \
//!     --scenario=usdc-depeg-2023
//! cargo run -p mm-backtester --bin mm-stress-test -- --all
//! cargo run -p mm-backtester --bin mm-stress-test -- \
//!     --all --output=stress-report.md
//! ```
//!
//! Stage-1 uses synthetic shock profiles (no external data,
//! no Tardis subscription) so the run is fully reproducible
//! and offline. See
//! `docs/sprints/epic-c-portfolio-risk-view.md` for the
//! Sprint C-1 decision that pinned the synthetic-first
//! approach.

use std::env;
use std::fs;
use std::process::ExitCode;

use mm_backtester::stress::{
    run_stress, scenario_by_slug, StressReport, StressRunConfig, CANONICAL_SCENARIOS,
};

const USAGE: &str = "\
Usage:
    mm-stress-test --scenario=<slug> [--output=<path>]
    mm-stress-test --all              [--output=<path>]
    mm-stress-test --list

Flags:
    --scenario=<slug>  Run exactly one canonical scenario by slug
                       (e.g. `ftx-2022`, `usdc-depeg-2023`).
    --all              Run every canonical scenario and emit an
                       aggregated markdown table.
    --output=<path>    Write the markdown report to the given
                       path instead of stdout.
    --list             Print the catalogue of available scenario
                       slugs and exit.
    -h, --help         Show this help text.
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let mut scenario_slug: Option<String> = None;
    let mut run_all = false;
    let mut output: Option<String> = None;
    let mut list_only = false;
    for arg in &args {
        if let Some(v) = arg.strip_prefix("--scenario=") {
            scenario_slug = Some(v.to_string());
        } else if arg == "--all" {
            run_all = true;
        } else if let Some(v) = arg.strip_prefix("--output=") {
            output = Some(v.to_string());
        } else if arg == "--list" {
            list_only = true;
        } else {
            eprintln!("unknown argument: {arg}");
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    }

    if list_only {
        println!("Canonical stress scenarios:");
        for s in CANONICAL_SCENARIOS {
            println!("  {:<20} {}", s.slug, s.label);
        }
        return ExitCode::SUCCESS;
    }

    let reports: Vec<StressReport> = if run_all {
        CANONICAL_SCENARIOS
            .iter()
            .map(|s| run_stress(s, &StressRunConfig::defaults_for(s)))
            .collect()
    } else if let Some(slug) = scenario_slug {
        let Some(scenario) = scenario_by_slug(&slug) else {
            eprintln!("unknown scenario slug: {slug}\nRun `--list` for available slugs.");
            return ExitCode::FAILURE;
        };
        vec![run_stress(
            scenario,
            &StressRunConfig::defaults_for(scenario),
        )]
    } else {
        eprintln!("either --scenario=<slug> or --all is required");
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let markdown = render_markdown(&reports);
    match output {
        Some(path) => {
            if let Err(e) = fs::write(&path, &markdown) {
                eprintln!("failed to write {path}: {e}");
                return ExitCode::FAILURE;
            }
            println!("wrote {} scenario report(s) to {path}", reports.len());
        }
        None => {
            print!("{markdown}");
        }
    }

    ExitCode::SUCCESS
}

/// Render a list of reports as one big markdown document with
/// a header per scenario. When there is only one report, the
/// header is omitted so the single-scenario output is clean.
fn render_markdown(reports: &[StressReport]) -> String {
    if reports.len() == 1 {
        return reports[0].to_markdown();
    }
    let mut out = String::new();
    out.push_str("# Stress test report\n\n");
    for r in reports {
        out.push_str(&format!("## Scenario: {}\n\n", r.scenario));
        out.push_str(&r.to_markdown());
        out.push('\n');
    }
    out
}
