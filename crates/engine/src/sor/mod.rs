//! Smart Order Router — Epic A.
//!
//! Cost-aware cross-venue dispatcher. Takes a
//! `(side, qty, urgency)` target plus a `ConnectorBundle`
//! and recommends how to split the fill across the
//! available venues to minimise total cost (taker fee +
//! maker queue wait + slippage).
//!
//! # Module layout
//!
//! - [`cost`] — per-venue cost model
//!   ([`cost::VenueCostModel`], [`cost::RouteCost`])
//! - [`venue_state`] — per-venue snapshot aggregator
//!   ([`venue_state::VenueSnapshot`],
//!   [`venue_state::VenueStateAggregator`],
//!   [`venue_state::VenueSeed`])
//! - `router` — greedy cost-minimising router (Sprint A-3)
//!
//! ## Stage-1 (advisory)
//! `MarketMakerEngine::recommend_route` returns a
//! `router::RouteDecision` and emits a `RouteRecommendation`
//! audit row — the engine does NOT place leg orders. Operators
//! read the row through the dashboard / CLI to validate the
//! cost model before arming dispatch.
//!
//! ## Stage-2 (inline dispatch — MM-6)
//! `MarketMakerEngine::dispatch_route` (and the scheduled
//! `run_sor_dispatch_tick`) place real leg orders through each
//! venue's connector. Gated by
//! `market_maker.sor_inline_enabled` (config) with an env
//! override `MM_SOR_INLINE_DISPATCH=1` (ops hotfix path) —
//! default is off so upgrading the binary does not silently
//! start routing real flow. Dispatched ticks emit a
//! `RouteDispatched` audit row (distinct from the advisory
//! `RouteRecommendation`) plus per-venue Prometheus counters
//! (`mm_sor_dispatch_{success,errors}_total`).

pub mod cost;
pub mod dispatch;
pub mod router;
pub mod trade_rate;
pub mod venue_state;

/// MM-6 — env override for `sor_inline_enabled`. Returns
/// `Some(true)` when `MM_SOR_INLINE_DISPATCH=1` or `=true`
/// (case-insensitive), `Some(false)` when `=0` / `=false`,
/// `None` when unset so the caller falls back to the config
/// value. Keeps the operator-facing flag live under one name
/// regardless of config source.
pub fn inline_dispatch_env_override() -> Option<bool> {
    let raw = std::env::var("MM_SOR_INLINE_DISPATCH").ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env override parses the usual truthy / falsy spellings.
    #[test]
    fn env_override_accepts_common_spellings() {
        // HARD-4 — `std::env::set_var` is `unsafe` under the
        // Rust 2024 edition because concurrent reads/writes
        // across threads race on POSIX `setenv`. This test
        // runs single-threaded via `#[test]` (cargo test
        // threads tests out, but no other thread is touching
        // the env variable during this block — verified by
        // grep: this env var has no setter outside this
        // test + one read in `inline_dispatch_env_override`),
        // so the SAFETY contract "no other thread reads or
        // writes the env" is upheld. Preserving the ambient
        // value restores the pre-test state for subsequent
        // suites.
        let prev = std::env::var("MM_SOR_INLINE_DISPATCH").ok();
        let cases_true = ["1", "true", "TRUE", "yes", "on"];
        let cases_false = ["0", "false", "FALSE", "no", "off"];
        for v in cases_true {
            // SAFETY: see test-level comment above.
            unsafe { std::env::set_var("MM_SOR_INLINE_DISPATCH", v) };
            assert_eq!(inline_dispatch_env_override(), Some(true), "truthy: {v}");
        }
        for v in cases_false {
            // SAFETY: see test-level comment above.
            unsafe { std::env::set_var("MM_SOR_INLINE_DISPATCH", v) };
            assert_eq!(inline_dispatch_env_override(), Some(false), "falsy: {v}");
        }
        // SAFETY: see test-level comment above.
        unsafe { std::env::set_var("MM_SOR_INLINE_DISPATCH", "hmm") };
        assert_eq!(inline_dispatch_env_override(), None);
        // SAFETY: see test-level comment above.
        unsafe { std::env::remove_var("MM_SOR_INLINE_DISPATCH") };
        assert_eq!(inline_dispatch_env_override(), None);
        if let Some(v) = prev {
            // SAFETY: see test-level comment above.
            unsafe { std::env::set_var("MM_SOR_INLINE_DISPATCH", v) };
        }
    }
}
