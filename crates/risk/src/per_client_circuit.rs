//! Per-client daily-loss circuit breaker.
//!
//! Epic 1 wired multi-client isolation (per-client SLA, fills,
//! reports, audit trail). Epic 6 completes the risk side: every
//! client gets its own daily-loss budget, aggregated across all
//! symbols that client owns. When the aggregate drops below the
//! configured floor the circuit **trips** and every engine that
//! belongs to that client stops quoting.
//!
//! Design notes
//!
//! - One [`PerClientLossCircuit`] per process, shared via
//!   `Arc<…>` across all engine tasks. Each engine reports its
//!   own PnL delta and then checks whether its client is tripped
//!   before issuing new orders.
//! - Cheap: a `HashMap<String, …>` under a `Mutex`. Engines hit
//!   it once per refresh tick.
//! - Reset is **manual**. Operator calls `reset_client` from an
//!   ops endpoint once they've reviewed what blew up. A wall-
//!   clock midnight reset is intentionally NOT implemented:
//!   compliance expects a human ack on every circuit breach.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Mutex;

/// Read-only aggregate snapshot for dashboards / MiCA reports.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientLossState {
    /// Summed daily PnL across every symbol owned by the client.
    pub daily_pnl: Decimal,
    /// Configured floor — an absolute positive magnitude from
    /// [`mm_common::config::ClientConfig::daily_loss_limit_usd`].
    /// `None` means "no per-client limit"; the circuit still
    /// tracks aggregate PnL for display but never trips.
    pub limit: Option<Decimal>,
    /// Set when aggregate ≤ -limit and awaiting manual reset.
    pub tripped: bool,
}

/// Internal book — keeps the per-symbol map so repeated calls
/// from a single engine remain idempotent.
#[derive(Debug, Default)]
struct Inner {
    limit: Option<Decimal>,
    per_symbol: HashMap<String, Decimal>,
    tripped: bool,
}

impl Inner {
    fn aggregate(&self) -> Decimal {
        self.per_symbol.values().copied().sum()
    }
}

/// Process-wide per-client loss circuit.
pub struct PerClientLossCircuit {
    clients: Mutex<HashMap<String, Inner>>,
}

impl Default for PerClientLossCircuit {
    fn default() -> Self {
        Self::new()
    }
}

impl PerClientLossCircuit {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Register a client with its configured daily loss limit.
    /// Call once at startup per `ClientConfig` that carries a
    /// `daily_loss_limit_usd`. Passing `None` still registers
    /// the client so aggregate PnL shows up in the dashboard
    /// snapshot, but no trip ever fires.
    pub fn register(&self, client_id: &str, limit: Option<Decimal>) {
        let mut map = self.clients.lock().unwrap();
        let entry = map.entry(client_id.to_string()).or_default();
        entry.limit = limit;
    }

    /// Absolute PnL report — replaces the last recorded value
    /// for `(client_id, symbol)` instead of adding to it. Using
    /// absolute values means a repeating call from one engine is
    /// idempotent; the alternative — delta — drifts whenever an
    /// engine restart loses its last-seen baseline.
    pub fn report_symbol_pnl(&self, client_id: &str, symbol: &str, daily_pnl: Decimal) {
        let mut map = self.clients.lock().unwrap();
        let state = map.entry(client_id.to_string()).or_default();
        state.per_symbol.insert(symbol.to_string(), daily_pnl);
        let agg = state.aggregate();
        if let Some(limit) = state.limit {
            if agg <= -limit {
                state.tripped = true;
            }
        }
    }

    /// Has this client's circuit tripped? `false` for unknown
    /// client ids (treat as "not enforced yet") so a fresh
    /// engine on a new client does not get blocked before the
    /// startup registration completes.
    pub fn is_tripped(&self, client_id: &str) -> bool {
        self.clients
            .lock()
            .unwrap()
            .get(client_id)
            .map(|s| s.tripped)
            .unwrap_or(false)
    }

    /// Snapshot the aggregate for dashboards / MiCA reports.
    pub fn snapshot(&self, client_id: &str) -> Option<ClientLossState> {
        self.clients
            .lock()
            .unwrap()
            .get(client_id)
            .map(|s| ClientLossState {
                daily_pnl: s.aggregate(),
                limit: s.limit,
                tripped: s.tripped,
            })
    }

    /// Snapshot every registered client — used by the dashboard
    /// overview endpoint and the MiCA aggregate export.
    pub fn snapshot_all(&self) -> HashMap<String, ClientLossState> {
        self.clients
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ClientLossState {
                        daily_pnl: v.aggregate(),
                        limit: v.limit,
                        tripped: v.tripped,
                    },
                )
            })
            .collect()
    }

    /// Manual reset — operator ack. Zeroes the aggregate + the
    /// per-symbol book so the next reports start clean. Limit
    /// is preserved.
    pub fn reset_client(&self, client_id: &str) {
        let mut map = self.clients.lock().unwrap();
        if let Some(state) = map.get_mut(client_id) {
            state.tripped = false;
            state.per_symbol.clear();
            let _ = dec!(0); // silence unused-import on rust_decimal_macros in some configs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_client_never_tripped() {
        let c = PerClientLossCircuit::new();
        assert!(!c.is_tripped("unknown"));
    }

    #[test]
    fn aggregate_across_symbols_trips_limit() {
        let c = PerClientLossCircuit::new();
        c.register("acme", Some(dec!(1000)));

        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-400));
        assert!(!c.is_tripped("acme"));

        c.report_symbol_pnl("acme", "ETHUSDT", dec!(-700));
        assert!(c.is_tripped("acme"), "aggregate -1100 must trip limit 1000");
    }

    #[test]
    fn positive_pnl_on_one_leg_offsets_loss_on_another() {
        // Under the sticky-trip design, feeding -600 first would
        // fire the breaker before the +200 recovery arrived.
        // Reality: engines report continuously — the rolling
        // aggregate is what matters. So we report both sides in
        // order and verify the aggregate stays within the limit.
        let c = PerClientLossCircuit::new();
        c.register("acme", Some(dec!(500)));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-300));
        c.report_symbol_pnl("acme", "ETHUSDT", dec!(200));
        let snap = c.snapshot("acme").unwrap();
        assert_eq!(snap.daily_pnl, dec!(-100));
        assert!(!snap.tripped, "aggregate -100 within -500 must not trip");
    }

    #[test]
    fn trip_is_sticky_across_recovery() {
        // Once tripped, the circuit stays tripped until operator
        // reset. A transient dip below the limit must not reopen
        // quoting just because the next tick recovered — that
        // would re-expose a blown-up client to more loss.
        let c = PerClientLossCircuit::new();
        c.register("acme", Some(dec!(500)));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-600));
        assert!(c.is_tripped("acme"));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-100));
        assert!(
            c.is_tripped("acme"),
            "recovery must not auto-clear the breach"
        );
    }

    #[test]
    fn report_symbol_pnl_is_idempotent() {
        let c = PerClientLossCircuit::new();
        c.register("acme", Some(dec!(1000)));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-300));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-300));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-300));
        assert_eq!(c.snapshot("acme").unwrap().daily_pnl, dec!(-300));
    }

    #[test]
    fn reset_clears_trip_and_aggregate() {
        let c = PerClientLossCircuit::new();
        c.register("acme", Some(dec!(100)));
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-500));
        assert!(c.is_tripped("acme"));
        c.reset_client("acme");
        assert!(!c.is_tripped("acme"));
        assert_eq!(c.snapshot("acme").unwrap().daily_pnl, dec!(0));
    }

    #[test]
    fn unlimited_client_tracks_but_never_trips() {
        let c = PerClientLossCircuit::new();
        c.register("acme", None);
        c.report_symbol_pnl("acme", "BTCUSDT", dec!(-99999));
        assert!(!c.is_tripped("acme"));
        assert_eq!(c.snapshot("acme").unwrap().daily_pnl, dec!(-99999));
    }
}
