//! Controller-level runtime tunables.
//!
//! Single source of truth for operator-editable knobs that
//! affect the controller's own behaviour (lease policy, version
//! pinning, approval policy, deploy dialog defaults). The store
//! is an `Arc<RwLock<Tunables>>` backed by a JSON file; on
//! mutation we write the whole state atomically, same pattern
//! as the vault + approval stores.
//!
//! What this is NOT: engine-internal / strategy tunables
//! (`momentum_ofi_enabled`, `gamma`, `spread_bps`, …). Those
//! live inside each `DesiredStrategy.variables` map at deploy
//! time — the Deploy dialog handles them. A future phase
//! migrates the legacy `config.toml` big bag of engine settings
//! out of TOML into per-deployment variables + per-agent
//! overrides; for now they coexist.
//!
//! Subsystem integration: callers hold `Arc<TunablesStore>` and
//! call `.current()` whenever they need a value. Readers that
//! want to react to changes call `.watch()` → `watch::Receiver`
//! and re-read on each tick (rare; most tunables are read on
//! a decision path where polling is cheap).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Debug, thiserror::Error)]
pub enum TunablesError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("validation: {0}")]
    Invalid(String),
}

/// The tunables struct. `#[serde(default)]` on every field +
/// `impl Default` below means a newly-added field doesn't break
/// old JSON files — missing fields get the code default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Tunables {
    // ── Lease policy ────────────────────────────────────
    /// How long a lease issued to an agent is valid (seconds).
    /// Agents refresh at 1/3 of this interval.
    pub lease_ttl_secs: u32,
    /// Hard cap on how often an agent may request a refresh.
    /// Cheap guard against a misbehaving agent burning CPU.
    pub lease_min_refresh_interval_secs: u32,

    // ── Agent version pinning ───────────────────────────
    /// Minimum agent binary version (semver) the controller
    /// accepts on register. Empty / missing = no lower bound.
    pub min_agent_version: String,
    /// Exclusive upper bound — agents at or above this semver
    /// are refused. Empty / missing = no upper bound.
    pub max_agent_version: String,

    // ── Deploy dialog defaults ──────────────────────────
    /// Symbol the Deploy dialog pre-fills. Operators still
    /// edit per-deployment; this is just the "most common"
    /// starting point.
    pub deploy_default_symbol: String,

    // ── Admission policy ────────────────────────────────
    /// Pending agents older than this many days are flagged as
    /// stale in the UI (no auto-reject — operator still has to
    /// click). `0` disables the flag.
    pub pending_stale_after_days: u32,

    // ── Violation auto-actions (Fix #5 + Wave G3) ─────────────────
    /// Umbrella toggle — when `false`, all per-category flags
    /// below are ignored even if set. When `true`, the loop
    /// runs and each category flag gates its own auto-widen.
    /// Default off — automatic kill escalation is a policy
    /// decision that should be explicit. Telegram bridge
    /// (Fix #6) still fires regardless of this flag.
    pub auto_widen_on_violation: bool,
    /// Wave G3 — auto-widen on SLA breach (uptime < 90%). Safe
    /// to turn on for most deploys since widening isn't a kill
    /// switch, it just backs the MM out of the book.
    pub auto_widen_sla: bool,
    /// Wave G3 — auto-widen on manipulation score ≥ 0.95.
    /// Safer than auto-pause because a false-positive detector
    /// just costs you a few ticks of fills, not the whole
    /// session.
    pub auto_widen_manip: bool,
    /// Wave G3 — auto-widen on reconciliation drift. Typically
    /// off — drift signals are often transient (venue pagination
    /// lag) and widening repeatedly is noisy. Turn on when a
    /// specific strategy is known to exhibit real drift events.
    pub auto_widen_recon: bool,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            // Matches `LeasePolicy::default()` in the controller
            // lib — 120s of headroom accommodates the boot-burst
            // commands (PushCredential + SetDesiredStrategies +
            // engine spawn) without the first lease expiring in
            // the middle of reconcile.
            lease_ttl_secs: 120,
            lease_min_refresh_interval_secs: 3,
            min_agent_version: String::new(),
            max_agent_version: String::new(),
            deploy_default_symbol: "BTCUSDT".into(),
            pending_stale_after_days: 7,
            auto_widen_on_violation: false,
            // Wave G3 — sensible per-category defaults. SLA +
            // manipulation auto-widen are low-risk (spread wider,
            // no kill); recon auto-widen is conservative-off.
            auto_widen_sla: true,
            auto_widen_manip: true,
            auto_widen_recon: false,
        }
    }
}

/// Field-level schema the UI consumes to render a dynamic
/// settings form. Keyed by the JSON field name. Keep order
/// stable — the UI renders in the order returned.
#[derive(Debug, Clone, Serialize)]
pub struct TunableField {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    /// One of `"bool"`, `"u32"`, `"string"`, `"semver_opt"`.
    /// `semver_opt` tells the UI to render a string input with
    /// a semver regex hint + allow empty ("no bound").
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

pub fn schema() -> Vec<TunableField> {
    vec![
        TunableField {
            key: "lease_ttl_secs",
            label: "Lease TTL (seconds)",
            description: "How long a lease issued to an agent remains valid. Agent refreshes at 1/3 of this. Shorter = tighter dead-man's-switch, more control-plane chatter.",
            category: "Lease policy",
            kind: "u32",
            min: Some(6),
            max: Some(600),
        },
        TunableField {
            key: "lease_min_refresh_interval_secs",
            label: "Min refresh interval (seconds)",
            description: "Hard cap on how often an agent may ask for a lease refresh. Guard against a misbehaving agent flooding the controller.",
            category: "Lease policy",
            kind: "u32",
            min: Some(1),
            max: Some(60),
        },
        TunableField {
            key: "min_agent_version",
            label: "Min agent version",
            description: "Reject agents older than this semver at register time. Empty = no lower bound.",
            category: "Version pinning",
            kind: "semver_opt",
            min: None,
            max: None,
        },
        TunableField {
            key: "max_agent_version",
            label: "Max agent version (exclusive)",
            description: "Reject agents at or above this semver. Empty = no upper bound. Use to pin a rollout window during staged upgrades.",
            category: "Version pinning",
            kind: "semver_opt",
            min: None,
            max: None,
        },
        TunableField {
            key: "pending_stale_after_days",
            label: "Pending-agent stale flag (days)",
            description: "Pending agents untouched longer than this show a \"stale\" badge in the Fleet UI. Does NOT auto-reject. 0 disables.",
            category: "Admission",
            kind: "u32",
            min: Some(0),
            max: Some(365),
        },
        TunableField {
            key: "deploy_default_symbol",
            label: "Deploy dialog: default symbol",
            description: "Symbol pre-filled in the Deploy dialog when an operator creates a new deployment. Operator can always override per deployment.",
            category: "Deploy defaults",
            kind: "string",
            min: None,
            max: None,
        },
        TunableField {
            key: "auto_widen_on_violation",
            label: "Auto-widen — umbrella toggle",
            description: "Master switch for the per-category auto-widen flags below. When off, no auto-actions fire regardless of category flags. Default off — automatic kill escalation is a deliberate policy choice.",
            category: "Violations",
            kind: "bool",
            min: None,
            max: None,
        },
        TunableField {
            key: "auto_widen_sla",
            label: "Auto-widen on SLA breach",
            description: "Fires L1 widen on deployments whose uptime < 90%. Safe — widening is reversible. Default on (within the umbrella).",
            category: "Violations",
            kind: "bool",
            min: None,
            max: None,
        },
        TunableField {
            key: "auto_widen_manip",
            label: "Auto-widen on manipulation score ≥ 0.95",
            description: "Fires L1 widen when the fleet's manipulation detector scores a combined ≥ 0.95. Default on (within the umbrella) — false-positives cost only a few ticks of fills.",
            category: "Violations",
            kind: "bool",
            min: None,
            max: None,
        },
        TunableField {
            key: "auto_widen_recon",
            label: "Auto-widen on reconciliation drift",
            description: "Fires L1 widen when orders/balance reconciliation reports drift. Default off — transient pagination lag often looks like drift; turn on for strategies with known drift profile.",
            category: "Violations",
            kind: "bool",
            min: None,
            max: None,
        },
    ]
}

/// Shared handle. Clone is cheap; all clones see the same state.
#[derive(Clone)]
pub struct TunablesStore {
    inner: Arc<RwLock<Tunables>>,
    tx: watch::Sender<Tunables>,
    path: Option<Arc<PathBuf>>,
}

impl std::fmt::Debug for TunablesStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunablesStore")
            .field("path", &self.path)
            .finish()
    }
}

impl Default for TunablesStore {
    fn default() -> Self {
        let t = Tunables::default();
        let (tx, _) = watch::channel(t.clone());
        Self {
            inner: Arc::new(RwLock::new(t)),
            tx,
            path: None,
        }
    }
}

impl TunablesStore {
    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, TunablesError> {
        let p = path.as_ref().to_path_buf();
        let t: Tunables = if p.exists() {
            let raw = std::fs::read_to_string(&p)?;
            if raw.trim().is_empty() {
                Tunables::default()
            } else {
                serde_json::from_str(&raw)?
            }
        } else {
            Tunables::default()
        };
        validate(&t)?;
        let (tx, _) = watch::channel(t.clone());
        Ok(Self {
            inner: Arc::new(RwLock::new(t)),
            tx,
            path: Some(Arc::new(p)),
        })
    }

    /// Snapshot clone of the current tunables. Hot path — held
    /// for the duration of a single decision (lease issuance,
    /// version check). Do not cache for long periods; call
    /// again each time.
    pub fn current(&self) -> Tunables {
        self.inner
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| Tunables::default())
    }

    /// Watch channel — subsystems that want to react to edits
    /// immediately subscribe. The channel pushes a new value on
    /// every successful mutation.
    pub fn watch(&self) -> watch::Receiver<Tunables> {
        self.tx.subscribe()
    }

    /// Replace the entire tunables blob. The normal UI path —
    /// the form submits a full `Tunables` JSON, server
    /// validates + persists.
    pub fn replace(&self, next: Tunables) -> Result<Tunables, TunablesError> {
        validate(&next)?;
        {
            let mut guard = self
                .inner
                .write()
                .map_err(|_| TunablesError::Io(std::io::Error::other("tunables poisoned")))?;
            *guard = next.clone();
        }
        let _ = self.tx.send(next.clone());
        self.persist(&next)?;
        Ok(next)
    }

    fn persist(&self, t: &Tunables) -> Result<(), TunablesError> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(t)?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("tunables"),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path.as_path())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path.as_path()) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path.as_path(), perms);
            }
        }
        Ok(())
    }
}

fn validate(t: &Tunables) -> Result<(), TunablesError> {
    if !(6..=600).contains(&t.lease_ttl_secs) {
        return Err(TunablesError::Invalid(
            "lease_ttl_secs must be in [6, 600]".into(),
        ));
    }
    if !(1..=60).contains(&t.lease_min_refresh_interval_secs) {
        return Err(TunablesError::Invalid(
            "lease_min_refresh_interval_secs must be in [1, 60]".into(),
        ));
    }
    if t.lease_min_refresh_interval_secs * 3 > t.lease_ttl_secs {
        return Err(TunablesError::Invalid(
            "refresh interval × 3 must be ≤ lease TTL (otherwise agents can't keep a lease alive)".into(),
        ));
    }
    if !t.min_agent_version.is_empty() {
        semver::Version::parse(&t.min_agent_version)
            .map_err(|e| TunablesError::Invalid(format!("min_agent_version: {e}")))?;
    }
    if !t.max_agent_version.is_empty() {
        semver::Version::parse(&t.max_agent_version)
            .map_err(|e| TunablesError::Invalid(format!("max_agent_version: {e}")))?;
    }
    if t.pending_stale_after_days > 365 {
        return Err(TunablesError::Invalid(
            "pending_stale_after_days must be ≤ 365".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tunables_are_valid() {
        validate(&Tunables::default()).unwrap();
    }

    #[test]
    fn in_memory_replace_and_current() {
        let s = TunablesStore::in_memory();
        let mut next = Tunables::default();
        next.lease_ttl_secs = 60;
        s.replace(next.clone()).unwrap();
        assert_eq!(s.current().lease_ttl_secs, 60);
    }

    #[test]
    fn bad_lease_ttl_rejected() {
        let s = TunablesStore::in_memory();
        let mut bad = Tunables::default();
        bad.lease_ttl_secs = 3; // below minimum
        assert!(s.replace(bad).is_err());
    }

    #[test]
    fn refresh_interval_too_close_to_ttl_rejected() {
        let s = TunablesStore::in_memory();
        let mut bad = Tunables::default();
        bad.lease_ttl_secs = 30;
        bad.lease_min_refresh_interval_secs = 15; // 15*3 > 30
        assert!(s.replace(bad).is_err());
    }

    #[test]
    fn bad_semver_rejected() {
        let s = TunablesStore::in_memory();
        let mut bad = Tunables::default();
        bad.min_agent_version = "not-semver".into();
        assert!(s.replace(bad).is_err());
    }

    #[test]
    fn watch_observes_changes() {
        let s = TunablesStore::in_memory();
        let mut rx = s.watch();
        assert_eq!(rx.borrow().lease_ttl_secs, 120);
        let mut next = Tunables::default();
        next.lease_ttl_secs = 60;
        s.replace(next).unwrap();
        // watch::Sender drops the borrow from the initial read
        // above; channel marks changed on `send`.
        assert!(rx.has_changed().unwrap_or(false));
        assert_eq!(rx.borrow_and_update().lease_ttl_secs, 60);
    }

    #[test]
    fn disk_roundtrip_preserves_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tunables.json");
        {
            let s = TunablesStore::load_from_path(&path).unwrap();
            let mut t = Tunables::default();
            t.deploy_default_symbol = "ETHUSDT".into();
            t.lease_ttl_secs = 45;
            s.replace(t).unwrap();
        }
        let reloaded = TunablesStore::load_from_path(&path).unwrap();
        let cur = reloaded.current();
        assert_eq!(cur.deploy_default_symbol, "ETHUSDT");
        assert_eq!(cur.lease_ttl_secs, 45);
    }

    #[test]
    fn schema_lists_every_tunable_field() {
        let keys: Vec<&str> = schema().iter().map(|f| f.key).collect();
        // One assertion per field — catches a forgotten schema
        // entry when a new tunable lands in the struct.
        let struct_fields = [
            "lease_ttl_secs",
            "lease_min_refresh_interval_secs",
            "min_agent_version",
            "max_agent_version",
            "deploy_default_symbol",
            "pending_stale_after_days",
            "auto_widen_on_violation",
            "auto_widen_sla",
            "auto_widen_manip",
            "auto_widen_recon",
        ];
        for f in struct_fields {
            assert!(
                keys.contains(&f),
                "schema missing field {f} — add it to `schema()`"
            );
        }
    }
}
