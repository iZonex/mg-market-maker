//! Command + telemetry payloads exchanged over the transport.
//!
//! Shape is deliberately enum-of-variants — every new capability
//! the controller wants to push (deploy strategy, rotate credentials,
//! force kill, push updated fail-ladder) lands as a new
//! [`CommandPayload`] variant. Adding variants is backward-
//! compatible; renaming or removing fields inside an existing
//! variant is NOT and triggers a [`crate::PROTOCOL_VERSION`] bump.
//!
//! Intentionally sparse for PR-1: only the handful of payloads
//! needed to exercise the lease / heartbeat loop are defined.
//! Strategy-deploy / credential-push / telemetry-fill payloads
//! arrive in follow-up PRs.

use serde::{Deserialize, Serialize};

use crate::fail_ladder::FailLadder;
use crate::lease::LeaderLease;

/// Stable identifier the controller uses to route commands and the
/// agent uses to register itself. Operators pick the string at
/// provisioning time (`"eu-hft-01"`, `"ap-tokyo-01"`). Not a UUID
/// on purpose — when something goes wrong in a colo, humans need
/// a name they can recognise at a glance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Declarative "I want this strategy running on this agent" row
/// the controller publishes. Agent reconciles its live set against the
/// full desired slice on every apply.
///
/// Credential shape note: this struct carries a flat allow-list of
/// credential IDs the deployment may touch. The controller does
/// NOT classify them by role (`primary`, `hedge`, `extras` are
/// strategy-internal concepts — a cross-venue maker knows which
/// one is hedge, a spot-only maker has just one). Role assignment
/// happens inside [`variables`] where the template reads named
/// keys (`"quote_venue_credential": "binance_spot"` etc.) and
/// decides what each ID is for. Controller's job is access
/// control, not domain modelling.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesiredStrategy {
    /// Stable identifier operators see in the dashboard.
    pub deployment_id: String,
    /// Which template to materialise (see `crates/strategy-graph/templates`).
    pub template: String,
    /// Symbol the strategy operates on.
    pub symbol: String,
    /// Flat allow-list of credential IDs this deployment is
    /// permitted to reference. Controller validates each ID
    /// exists in its store + that the target agent is authorised
    /// to receive it; the strategy template internally picks
    /// which one plays which role via `variables`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
    /// Template variables the operator tuned at deploy time
    /// (gamma, spread_bps, venue role → credential mapping,
    /// etc.). Keyed by variable name; value is kept as JSON so
    /// the control plane stays schemaless — the template's
    /// strategy node validates shape when the agent instantiates
    /// it.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub variables: serde_json::Map<String, serde_json::Value>,
    /// Ladder to execute on control-plane silence. Absent means
    /// "use the class default from [`FailLadder::default_for`]".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_ladder_override: Option<FailLadder>,
}

impl DesiredStrategy {
    /// Every non-empty credential ID this deployment references.
    /// Used by the controller's pre-deploy validation to check
    /// each referenced ID exists + is authorised for the target
    /// agent.
    pub fn credential_ids(&self) -> impl Iterator<Item = &str> {
        self.credentials.iter().map(|s| s.as_str()).filter(|s| !s.is_empty())
    }
}

/// Controller → agent commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandPayload {
    /// First message after the agent registers. Contains the
    /// freshly-issued lease the agent must refresh before expiry.
    LeaseGrant { lease: LeaderLease },
    /// Extended (or re-issued) lease in response to an agent's
    /// refresh request, or proactively when the controller decides to
    /// bump expiry.
    LeaseRefresh { lease: LeaderLease },
    /// Controller explicitly withdraws authority. Agent executes the
    /// fail-ladder immediately regardless of nominal expiry.
    LeaseRevoke { reason: String },
    /// Push a resolved venue credential to the agent. Secrets
    /// live on the controller; they're transmitted once over the
    /// authenticated TLS channel and stored in agent memory
    /// only (never written to disk). The agent acknowledges
    /// receipt like any other command. Controller re-pushes on
    /// agent reconnect so the in-memory catalog stays warm.
    PushCredential { credential: PushedCredential },
    /// Full replacement of the agent's desired-strategy slice.
    /// Level-triggered: agent diffs and reconciles, no per-item
    /// events needed. Controller ensures referenced `credential_id`s
    /// have been pushed before the SetDesiredStrategies lands.
    SetDesiredStrategies { strategies: Vec<DesiredStrategy> },
    /// Request a topic-scoped detail payload from a specific
    /// deployment. The agent replies with
    /// `TelemetryPayload::DetailsReply` carrying the same
    /// `request_id` so the controller can correlate. Topics
    /// recognised today:
    ///   * `"funding_arb_recent_events"` — last N DriverEvent
    ///     entries from the engine's ring buffer.
    /// Unknown topics yield an empty payload + error string so
    /// the controller can surface a 400 to the caller.
    FetchDeploymentDetails {
        deployment_id: String,
        topic: String,
        request_id: uuid::Uuid,
        /// Topic-specific arguments (date ranges, limits,
        /// filters). Agent topic handlers read what they
        /// understand and ignore the rest. `#[serde(default)]`
        /// keeps the wire backwards-compatible with older
        /// controllers that don't carry args.
        #[serde(default)]
        args: serde_json::Map<String, serde_json::Value>,
    },
    /// Patch a specific deployment's `variables` map in place.
    /// The agent merges the patch into the running deployment's
    /// variable snapshot (later PATCHes with the same key
    /// overwrite). Telemetry reflects the updated map
    /// immediately. Actual engine hot-reload is strategy-type
    /// dependent — some templates react to variable changes on
    /// their next tick (γ, spread_bps), some require a
    /// deployment restart (credential id, template kind). In
    /// phase B this command lands the storage + wire update;
    /// per-strategy reconcile hooks add incrementally.
    PatchDeploymentVariables {
        deployment_id: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    /// Wave B5 — hot-register a client (tenant) on every agent's
    /// `DashboardState` so per-client report endpoints
    /// (`/api/v1/client/{id}/*`) start collecting fills, SLA, PnL
    /// as soon as matching deployments start trading. No agent
    /// restart required. Idempotent — a duplicate registration
    /// updates the symbols set but keeps existing state (fills,
    /// PnL accumulators) intact. Sent by the controller's
    /// `create_client` admin handler to every accepted agent.
    AddClient {
        client_id: String,
        symbols: Vec<String>,
    },
    /// Liveness ping with no state change. Agent ACKs with a
    /// heartbeat telemetry so the controller can measure one-way
    /// latency + detect silent links.
    Heartbeat,
}

/// Credential payload format on the wire. Mirrors
/// [`mm_common::settings::ResolvedCredential`] but without the
/// mm-common dep — control crate stays at the protocol layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushedCredential {
    pub id: String,
    pub exchange: String,
    pub product: String,
    pub api_key: String,
    pub api_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_notional_quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_symbol: Option<String>,
}

/// Agent → controller upstream events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TelemetryPayload {
    /// Sent once on connect, names the agent and advertises its
    /// last-applied cursor so the controller can resume from exactly
    /// the right command seq. `pubkey` is the agent's Ed25519
    /// verifying key — controller pins it per-agent on receive, then
    /// verifies every subsequent telemetry envelope against it.
    /// `None` is transitional: agents built before the signing
    /// wire-up still connect, controller accepts them as unsigned.
    ///
    /// `agent_version` is the crate version of the agent binary
    /// (`CARGO_PKG_VERSION`). Controller enforces a configured
    /// compatibility range and refuses to issue a lease to
    /// agents outside it. Keeps protocol-version + binary-version
    /// in lockstep so long-running deployments don't silently
    /// drift after a partial rollout.
    Register {
        agent_id: AgentId,
        last_applied: crate::seq::Seq,
        protocol_version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pubkey: Option<crate::identity::PublicKey>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        agent_version: String,
    },
    /// Agent-initiated lease extension. Controller may honour or
    /// refuse; on refuse it responds with [`CommandPayload::LeaseRevoke`].
    LeaseRefreshRequest { current_lease_id: uuid::Uuid },
    /// Reply to [`CommandPayload::Heartbeat`] + unsolicited
    /// liveness pings on the agent's own cadence.
    Heartbeat { agent_clock_ms: i64 },
    /// Acknowledgement of a command at seq `applied_seq`. Lets
    /// the controller advance its per-agent cursor.
    Ack { applied_seq: crate::seq::Seq },
    /// Per-deployment snapshot the agent pushes on its own
    /// cadence (typically every 1-2 seconds). Controller caches the
    /// latest frame per `deployment_id` and exposes via HTTP so
    /// operators see live state across the fleet. Best-effort:
    /// dropped frames are fine, the controller just shows the last
    /// received snapshot until a fresher one lands.
    DeploymentState {
        deployments: Vec<DeploymentStateRow>,
    },
    /// Reply to a prior [`CommandPayload::FetchDeploymentDetails`].
    /// `request_id` correlates to the pending controller
    /// request; `payload` carries the topic-shaped JSON (empty
    /// object when the deployment has no data yet). `error` is
    /// populated when the agent refused — unknown topic, stale
    /// deployment, etc.
    DetailsReply {
        request_id: uuid::Uuid,
        deployment_id: String,
        topic: String,
        #[serde(default)]
        payload: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// One per-deployment snapshot row. Intentionally small so the
/// full fleet's frames fit in one WS text message even on large
/// deployments. Finer-grained metrics (per-order, per-venue book
/// pressure) live in a future separate streaming channel — this
/// row is the "fleet dashboard" view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentStateRow {
    pub deployment_id: String,
    pub symbol: String,
    /// `true` while the deployment's task is live in the
    /// registry. Flips false on reconcile-stop + on authority
    /// loss (before fail-ladder fires).
    pub running: bool,
    /// Net inventory in base-asset units. String-formatted to
    /// avoid f64 rounding when the controller re-serves it.
    pub inventory: String,
    /// Unrealised PnL in quote-asset units at the latest mid.
    /// Empty string when the deployment has no position yet.
    pub unrealized_pnl_quote: String,
    /// UTC millis at which the agent sampled this row.
    pub sampled_at_ms: i64,

    // ── Strategy-level state for the per-deployment drilldown.
    // All fields are optional — older agents that don't
    // populate them simply leave them as None / empty, the
    // dashboard renders "—".

    /// Which template the agent spawned this deployment from.
    /// Useful for the drilldown header.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
    /// Which venue + product the strategy actually binds to.
    /// Populated from the primary credential at runtime.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub venue: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product: String,
    /// Engine mode — `"paper"` / `"live"` / `"smoke"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// Volatility regime the classifier reports right now
    /// (`"Quiet"`, `"Volatile"`, `"News"`, `"Liquidity"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub regime: String,
    /// Kill ladder level, 0 = NORMAL, 5 = DISCONNECT.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub kill_level: u8,
    /// Live γ the adaptive tuner landed on (decimal string).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adaptive_gamma: String,
    /// Short one-line reason the adaptive tuner cites for its
    /// last γ adjustment (regime change, adverse-select spike,
    /// toxicity breach, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adaptive_reason: String,
    /// Runtime feature flags — operator sees which strategy
    /// sub-systems are live for this deployment. Values are
    /// `true` / `false`; keys are template-defined
    /// (`"momentum_ofi"`, `"bvc_classifier"`, `"sor_inline"`,
    /// …).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub features: std::collections::BTreeMap<String, bool>,
    /// Snapshot of the effective `variables` this deployment is
    /// running with. Drilldown ParamTuner shows these + lets
    /// operator PATCH them live.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub variables: serde_json::Map<String, serde_json::Value>,
    /// UI-DEPLOY-1 (2026-04-22) — the credential allow-list the
    /// operator deployed with. Echoed back so DeployDialog can
    /// reconstruct the full `DesiredStrategy` list when adding a
    /// new deployment: `SetDesiredStrategies` is REPLACE-by-set,
    /// so the UI must union the existing slice with the new
    /// strategy before POST, or sibling deployments stop
    /// silently. Empty in the subscribe-only runner path
    /// (credentials don't exist there).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
    /// Count of currently-resting orders this deployment has on
    /// the book.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub live_orders: u32,

    // ── Execution-layer scalars (Wave 1 R — item 3).
    // Scrape-from-Prometheus-gauge snapshots so the drilldown has
    // a pulse per topic without pushing full arrays every tick.
    // Detailed per-decision / per-bundle arrays stay on an
    // on-demand endpoint (follow-up work).

    /// Last SOR dispatch's filled quantity (gauge
    /// `mm_sor_dispatch_filled_qty`). Empty when this deployment
    /// never dispatched a route.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sor_filled_qty: String,
    /// Running total of successful SOR dispatches (counter
    /// `mm_sor_dispatch_success_total`).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub sor_dispatch_success: u64,
    /// In-flight atomic maker/hedge bundles (gauge
    /// `mm_atomic_bundles_inflight`).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub atomic_bundles_inflight: u32,
    /// Running total of completed atomic bundles (counter
    /// `mm_atomic_bundles_completed_total`).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub atomic_bundles_completed: u64,

    // ── Calibration (GLFT) ──────────────────────────────────
    /// GLFT `a` constant — empty when strategy doesn't
    /// calibrate (e.g. Avellaneda without GLFT sub-module).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub calibration_a: String,
    /// GLFT `k` coefficient.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub calibration_k: String,
    /// Number of fill samples currently in the calibration window.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub calibration_samples: u32,

    // ── Manipulation detector scores ────────────────────────
    /// Pump-dump detector score `[0, 1]`. Empty when no sample.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manipulation_pump_dump: String,
    /// Wash-trading score `[0, 1]`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manipulation_wash: String,
    /// Thin-book score `[0, 1]`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manipulation_thin_book: String,
    /// Combined / aggregator score `[0, 1]`. Same number the
    /// Overview page surfaces in the toxicity column.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manipulation_combined: String,

    // ── Funding-arb driver state ────────────────────────────
    /// `true` when this deployment's funding-arb driver has the
    /// symbol actively engaged (between `Entered` and
    /// `Exited`/`PairBreak`). Empty when no funding-arb driver
    /// is attached.
    #[serde(default, skip_serializing_if = "is_false")]
    pub funding_arb_active: bool,
    /// Running total of `Entered` driver events.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub funding_arb_entered: u64,
    /// Running total of `Exited` driver events.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub funding_arb_exited: u64,
    /// Running total of `TakerRejected` driver events.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub funding_arb_taker_rejected: u64,
    /// Running total of compensated `PairBreak` events.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub funding_arb_pair_break: u64,
    /// Running total of UNcompensated `PairBreak` events — the
    /// one that tripped L2 kill + dropped the driver.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub funding_arb_pair_break_uncompensated: u64,

    // ── Book + toxicity scalars (from engine's SymbolState)
    // All decimal-strings so the controller re-serialises
    // without f64 rounding. Empty string = "no sample yet"
    // (distinct from literal 0).

    /// Current mid price (bid+ask)/2. Empty while the book is
    /// warming up.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mid_price: String,
    /// Last spread in bps. Empty while book is one-sided.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spread_bps: String,
    /// Annualised realised volatility (EWMA on log returns).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub volatility: String,
    /// VPIN toxicity reading in [0, 1]. Empty until the bucket
    /// aggregator hits its minimum sample count.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vpin: String,
    /// Kyle's lambda price-impact coefficient.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kyle_lambda: String,
    /// Adverse-selection bps (Cartea ρ inputs the strategy
    /// consumes).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adverse_bps: String,

    // ── SLA scalars (MiCA presence / uptime) ────────────────
    /// SLA uptime percentage (connection + quote coverage) over
    /// the rolling window.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sla_uptime_pct: String,
    /// Share of the trailing 24h where the maker has been
    /// present on the book.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub presence_pct_24h: String,
    /// Share of the trailing 24h where both sides were active.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub two_sided_pct_24h: String,

    // ── Richer structures carried opaquely (engine's
    // SymbolState substructures that the dashboard consumes
    // as-is). Zero-cost when absent: serde skips the field.

    /// Live open-order snapshot list. Shape matches
    /// `mm_dashboard::state::OrderSnapshot` — carried as opaque
    /// JSON so the control crate stays dashboard-type-free.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_orders: Vec<serde_json::Value>,
    /// Hourly presence 24-bucket histogram (MiCA SLA). Each
    /// entry is an opaque `HourlyPresenceSummary` JSON
    /// (hour / presence_pct / two_sided_pct / minutes_with_data
    /// / worst_spread_bps).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hourly_presence: Vec<serde_json::Value>,
    /// Minutes with data over the trailing 24h (MiCA SLA).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub minutes_with_data_24h: u32,
    /// Engine's `MarketImpactReport` snapshot — opaque JSON.
    /// Absent until the impact tracker has completed ≥1 fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_impact: Option<serde_json::Value>,
    /// Engine's `PerformanceReport` snapshot — opaque JSON.
    /// Absent until the PnL tracker has enough history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<serde_json::Value>,
    /// Active strategy graph metadata — opaque
    /// `ActiveGraphSnapshot` JSON (name, hash, scope, deploy
    /// timestamp). Absent when the deployment is quoting
    /// through the default Strategy slot (no graph deployed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_graph: Option<serde_json::Value>,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}
fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_roundtrips_as_plain_string() {
        let id = AgentId::new("eu-hft-01");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"eu-hft-01\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn command_variants_roundtrip() {
        let c = CommandPayload::Heartbeat;
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"heartbeat\""));
        let back: CommandPayload = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, CommandPayload::Heartbeat));
    }

    #[test]
    fn desired_strategy_omits_ladder_when_none() {
        let d = DesiredStrategy {
            deployment_id: "dep-1".into(),
            template: "avellaneda-via-graph".into(),
            symbol: "BTCUSDT".into(),
            credentials: Vec::new(),
            variables: serde_json::Map::new(),
            fail_ladder_override: None,
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(!s.contains("fail_ladder_override"));
        assert!(!s.contains("variables"));
        assert!(!s.contains("credentials"));
    }

    #[test]
    fn credential_ids_skips_empty_slots() {
        let d = DesiredStrategy {
            deployment_id: "x".into(),
            template: "t".into(),
            symbol: "S".into(),
            credentials: vec!["a".into(), "".into(), "b".into()],
            variables: serde_json::Map::new(),
            fail_ladder_override: None,
        };
        let got: Vec<&str> = d.credential_ids().collect();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn desired_strategy_with_variables_roundtrips() {
        let mut vars = serde_json::Map::new();
        vars.insert("gamma".into(), serde_json::json!("0.12"));
        vars.insert("spread_bps".into(), serde_json::json!("4"));
        vars.insert("primary_credential".into(), serde_json::json!("binance_spot_main"));
        vars.insert("hedge_credential".into(), serde_json::json!("bybit_perp_hedge"));
        let d = DesiredStrategy {
            deployment_id: "dep-xex".into(),
            template: "cross-exchange-basic".into(),
            symbol: "BTCUSDT".into(),
            credentials: vec!["binance_spot_main".into(), "bybit_perp_hedge".into()],
            variables: vars.clone(),
            fail_ladder_override: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: DesiredStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.credentials, vec!["binance_spot_main", "bybit_perp_hedge"]);
        assert_eq!(
            back.variables.get("primary_credential"),
            Some(&serde_json::json!("binance_spot_main"))
        );
    }
}
