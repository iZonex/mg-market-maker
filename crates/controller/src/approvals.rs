//! Controller-side agent admission control.
//!
//! Every agent presents an Ed25519 public key on connect. The
//! controller keys its approval store by the key's fingerprint
//! (`SHA-256(pubkey)[..8]`) rather than the agent's self-reported
//! id — the id is a free-form operator label that anyone can
//! claim, the fingerprint can only be produced by the peer
//! holding the private seed. That's the admission-control
//! surface: operators approve fingerprints, not strings.
//!
//! State machine:
//!   (unknown) → Pending        when an agent registers with a
//!                              fingerprint the store has not
//!                              seen before. Agent stays
//!                              connected but receives no
//!                              LeaseGrant + no credentials until
//!                              an operator accepts it.
//!   Pending   → Accepted       via POST /api/v1/approvals/{fp}/accept.
//!                              Session fires LeaseGrant + credential
//!                              push on the next envelope cycle.
//!   Pending   → Rejected       via POST /api/v1/approvals/{fp}/reject.
//!                              Permanent denial; agent connects but
//!                              never receives authority. Reversible
//!                              by a subsequent accept.
//!   Accepted  → Revoked        via POST /api/v1/approvals/{fp}/revoke.
//!                              Session fires LeaseRevoke and drops
//!                              authority without tearing down
//!                              transport (agent fail-ladders).
//!   Any       → Accepted       re-accept after a reject/revoke;
//!                              agent's next frame (or an explicit
//!                              runtime nudge) triggers lease grant.
//!   "Skip" is a UI-only hide action — the server state is unchanged.
//!
//! Persistence: JSON file next to the credential store. Atomic
//! write on every mutation (temp file + rename) so a crash
//! mid-update never leaves the store half-written. Small-scale
//! fleets (dozens of agents) fit comfortably in one file; a
//! future PR swaps to an operator-editable DB if the fleet
//! outgrows this.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Admission-control state machine.
///
/// Transitions are operator-driven via the HTTP admin surface:
///   Pending → Accepted   "accept"  (grants lease + pushes creds)
///   Pending → Rejected   "reject"  (permanent denial, agent connects but
///                                   gets no authority ever)
///   Accepted → Revoked   "revoke"  (previously authorised, now denied;
///                                   session receives LeaseRevoke)
///   Any → Accepted       re-accept after a reject/revoke decision
///
/// "Skip" is purely a UI action (hide this pending row for now);
/// the server state is unchanged on skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Accepted,
    Rejected,
    Revoked,
}

impl ApprovalState {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub fingerprint: String,
    /// Most recent agent id that registered with this fingerprint.
    /// Informational only — fingerprint is the authority.
    pub agent_id: String,
    /// Full hex-encoded public key. Lets operators eyeball the
    /// same key on the agent's disk when provisioning a new box.
    pub pubkey_hex: String,
    pub state: ApprovalState,
    pub first_seen_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_reason: Option<String>,
    /// UTC millis of the most recent register frame that cited
    /// this fingerprint. Used by the UI to age out stale
    /// pending requests.
    pub last_seen_ms: i64,
    /// Operator-editable profile sub-struct. Intentionally
    /// separate from the admission-control surface above — the
    /// approval state machine (Pending/Accepted/Rejected/Revoked)
    /// is one concern, "what IS this agent" (description, owner,
    /// region, labels) is a different operator concern.
    #[serde(default, skip_serializing_if = "AgentProfile::is_empty")]
    pub profile: AgentProfile,
}

/// Descriptive metadata — the "who / what / where / why" of an
/// agent so operators can identify it at a glance and tie it to
/// an organisational owner. Never affects authority — an agent
/// without a profile filled in still trades normally once it's
/// been Accepted. Editable via the profile HTTP surface at
/// `PUT /api/v1/agents/{fp}/profile`. Stored inside the
/// existing `approvals.json`; no separate file per agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    /// Free-form human label — "Frankfurt HFT box #2, colocated
    /// with Binance gateway".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Client this agent belongs to (multi-tenant deploys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Geographic / provider tag — "eu-fra", "us-nyc", "hetzner-fsn1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Deployment environment — "production", "staging", "dev",
    /// "smoke". UI colours rows by environment so a dev agent
    /// can never be mistaken for a prod box mid-incident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// What this agent is for — "primary BTC/ETH market-maker",
    /// "hedge-only leg", "arb bot". Rendered in the drill-down
    /// so on-call understands the role before reaching for
    /// kill switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// On-call / responsible operator handle — email, Slack,
    /// PagerDuty ID, whatever the ops team keys routing on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_contact: Option<String>,
    /// Free-form ops notes — deployment gotchas, known
    /// behaviours, pointers to runbooks. Multi-line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Operator-editable tags: `env=prod`, `role=hft`, etc. Free
    /// shape — the UI renders them as chips, operator uses them
    /// for filtering. Keep tags for short cross-cutting
    /// classification; `environment` / `purpose` / `owner_contact`
    /// handle the first-class fields so you don't have to reach
    /// into labels for the "every prod deploy needs this".
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

impl AgentProfile {
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.client_id.is_none()
            && self.region.is_none()
            && self.environment.is_none()
            && self.purpose.is_none()
            && self.owner_contact.is_none()
            && self.notes.is_none()
            && self.labels.is_empty()
    }
}

/// Patch body for `PUT /api/v1/agents/{fp}/profile`. Any `Some`
/// field overwrites; empty strings clear the corresponding
/// optional field. `labels = Some(HashMap)` replaces the full map.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentProfilePatch {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub owner_contact: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub labels: Option<HashMap<String, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalStoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ApprovalFile {
    #[serde(default)]
    records: Vec<ApprovalRecord>,
}

/// Cheaply cloneable shared handle — HTTP handlers, accept-loop
/// sessions, and tests all hold an `Arc` to the same map. All
/// mutations hit disk atomically.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    inner: Arc<RwLock<HashMap<String, ApprovalRecord>>>,
    path: Option<Arc<PathBuf>>,
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl ApprovalStore {
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            path: None,
        }
    }

    /// Load records from a JSON file. Creates the file on first
    /// write if it doesn't exist — operators don't have to seed
    /// an empty file manually.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ApprovalStoreError> {
        let p = path.as_ref().to_path_buf();
        let map = if p.exists() {
            let raw = std::fs::read_to_string(&p)?;
            if raw.trim().is_empty() {
                HashMap::new()
            } else {
                let parsed: ApprovalFile = serde_json::from_str(&raw)?;
                parsed
                    .records
                    .into_iter()
                    .map(|r| (r.fingerprint.clone(), r))
                    .collect()
            }
        } else {
            HashMap::new()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(map)),
            path: Some(Arc::new(p)),
        })
    }

    /// Record an agent register and return the effective state.
    /// - Unknown fingerprint → inserts as Pending, returns Pending.
    /// - Known fingerprint → updates `last_seen_ms` + `agent_id`
    ///   (in case operator renamed the box), returns current state.
    pub fn record_register(
        &self,
        fingerprint: &str,
        agent_id: &str,
        pubkey_hex: &str,
    ) -> ApprovalState {
        let now = Utc::now().timestamp_millis();
        let mut write_needed = false;
        let state = {
            let mut guard = match self.inner.write() {
                Ok(g) => g,
                Err(_) => return ApprovalState::Pending,
            };
            let entry = guard.entry(fingerprint.to_string()).or_insert_with(|| {
                write_needed = true;
                ApprovalRecord {
                    fingerprint: fingerprint.to_string(),
                    agent_id: agent_id.to_string(),
                    pubkey_hex: pubkey_hex.to_string(),
                    state: ApprovalState::Pending,
                    first_seen_ms: now,
                    approved_at_ms: None,
                    approved_by: None,
                    revoked_at_ms: None,
                    revoked_by: None,
                    revoke_reason: None,
                    last_seen_ms: now,
                    profile: AgentProfile::default(),
                }
            });
            // Sync live fields so the UI shows the most recent
            // values — but never mutate `state` here.
            if entry.agent_id != agent_id {
                entry.agent_id = agent_id.to_string();
                write_needed = true;
            }
            if entry.pubkey_hex.is_empty() && !pubkey_hex.is_empty() {
                // Wave F2 — pre-approved record had no pubkey
                // yet; bind it now. Fingerprint alone won't let
                // an attacker bind their own key: to guess a
                // fingerprint they already had to get it from
                // the operator, so the trust chain is
                // operator → fingerprint → pubkey (first to
                // present wins within the pre-approve window).
                entry.pubkey_hex = pubkey_hex.to_string();
                write_needed = true;
                tracing::info!(
                    fingerprint,
                    agent = agent_id,
                    "pre-approved fingerprint bound to pubkey on first connect"
                );
            } else if entry.pubkey_hex != pubkey_hex {
                // Pubkey mismatch for a known fingerprint is a
                // SHA-256 collision and should never happen; log
                // loudly + leave the original record alone.
                tracing::error!(
                    fingerprint,
                    old = %entry.pubkey_hex,
                    new = pubkey_hex,
                    "approval store: pubkey mismatch for known fingerprint — ignoring new key"
                );
            }
            entry.last_seen_ms = now;
            entry.state
        };
        if write_needed {
            let _ = self.persist();
        }
        state
    }

    pub fn get(&self, fingerprint: &str) -> Option<ApprovalRecord> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(fingerprint).cloned())
    }

    pub fn list(&self) -> Vec<ApprovalRecord> {
        let mut out: Vec<_> = self
            .inner
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        out.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        out
    }

    /// Wave F2 — pre-approve a fingerprint before the agent has
    /// ever connected. Admin pastes the fingerprint from the
    /// agent's boot log (or scans it off the trading box) and
    /// the record lands in state=Accepted immediately. When the
    /// agent later connects, `record_register` binds the pubkey
    /// onto the empty slot and the handshake clears instantly —
    /// no "pending, please eyeball" step. Returns the created
    /// record; fails with Conflict if the fingerprint is already
    /// known.
    pub fn pre_approve(
        &self,
        fingerprint: &str,
        by: &str,
        notes: Option<&str>,
    ) -> Result<ApprovalRecord, ApprovalStoreError> {
        let now = Utc::now().timestamp_millis();
        let rec = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            if guard.contains_key(fingerprint) {
                return Err(ApprovalStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("fingerprint {fingerprint} already known — use accept"),
                )));
            }
            let mut profile = AgentProfile::default();
            if let Some(n) = notes {
                profile.notes = Some(n.to_string());
            }
            let rec = ApprovalRecord {
                fingerprint: fingerprint.to_string(),
                // agent_id stays empty until the agent actually
                // registers — it self-reports its id on connect.
                agent_id: String::new(),
                pubkey_hex: String::new(),
                state: ApprovalState::Accepted,
                first_seen_ms: now,
                approved_at_ms: Some(now),
                approved_by: Some(by.to_string()),
                revoked_at_ms: None,
                revoked_by: None,
                revoke_reason: None,
                last_seen_ms: now,
                profile,
            };
            guard.insert(fingerprint.to_string(), rec.clone());
            rec
        };
        self.persist()?;
        Ok(rec)
    }

    pub fn accept(
        &self,
        fingerprint: &str,
        by: &str,
    ) -> Result<ApprovalRecord, ApprovalStoreError> {
        let updated = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            let Some(rec) = guard.get_mut(fingerprint) else {
                return Err(ApprovalStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("fingerprint {fingerprint} not in approval store"),
                )));
            };
            rec.state = ApprovalState::Accepted;
            rec.approved_at_ms = Some(Utc::now().timestamp_millis());
            rec.approved_by = Some(by.to_string());
            rec.revoke_reason = None;
            rec.clone()
        };
        self.persist()?;
        Ok(updated)
    }

    pub fn revoke(
        &self,
        fingerprint: &str,
        by: &str,
        reason: &str,
    ) -> Result<ApprovalRecord, ApprovalStoreError> {
        let updated = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            let Some(rec) = guard.get_mut(fingerprint) else {
                return Err(ApprovalStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("fingerprint {fingerprint} not in approval store"),
                )));
            };
            rec.state = ApprovalState::Revoked;
            rec.revoked_at_ms = Some(Utc::now().timestamp_millis());
            rec.revoked_by = Some(by.to_string());
            rec.revoke_reason = Some(reason.to_string());
            rec.clone()
        };
        self.persist()?;
        Ok(updated)
    }

    /// Apply an operator-edited profile patch. The approval
    /// state + decision history is never touched by this call —
    /// profile (description / client_id / region / labels) and
    /// approval are separate operator concerns.
    pub fn update_profile(
        &self,
        fingerprint: &str,
        patch: AgentProfilePatch,
    ) -> Result<ApprovalRecord, ApprovalStoreError> {
        let updated = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            let Some(rec) = guard.get_mut(fingerprint) else {
                return Err(ApprovalStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("fingerprint {fingerprint} not in approval store"),
                )));
            };
            // Each patch field follows the "empty string = clear"
            // convention so a text input that loses focus with an
            // empty value actually removes the previously-set
            // label. `labels = Some(map)` replaces the whole map.
            if let Some(d) = patch.description {
                rec.profile.description = if d.is_empty() { None } else { Some(d) };
            }
            if let Some(c) = patch.client_id {
                rec.profile.client_id = if c.is_empty() { None } else { Some(c) };
            }
            if let Some(r) = patch.region {
                rec.profile.region = if r.is_empty() { None } else { Some(r) };
            }
            if let Some(env) = patch.environment {
                rec.profile.environment = if env.is_empty() { None } else { Some(env) };
            }
            if let Some(p) = patch.purpose {
                rec.profile.purpose = if p.is_empty() { None } else { Some(p) };
            }
            if let Some(o) = patch.owner_contact {
                rec.profile.owner_contact = if o.is_empty() { None } else { Some(o) };
            }
            if let Some(n) = patch.notes {
                rec.profile.notes = if n.is_empty() { None } else { Some(n) };
            }
            if let Some(l) = patch.labels {
                rec.profile.labels = l;
            }
            rec.clone()
        };
        self.persist()?;
        Ok(updated)
    }

    /// Reject a pending (or previously-accepted) fingerprint.
    /// Permanent denial until an operator calls `accept` again.
    /// Different from `revoke` in that `revoke` is specifically
    /// for pulling authority from an already-accepted agent
    /// (triggers LeaseRevoke); `reject` is for pending agents we
    /// decided not to admit in the first place.
    pub fn reject(
        &self,
        fingerprint: &str,
        by: &str,
        reason: &str,
    ) -> Result<ApprovalRecord, ApprovalStoreError> {
        let updated = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            let Some(rec) = guard.get_mut(fingerprint) else {
                return Err(ApprovalStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("fingerprint {fingerprint} not in approval store"),
                )));
            };
            rec.state = ApprovalState::Rejected;
            rec.revoked_at_ms = Some(Utc::now().timestamp_millis());
            rec.revoked_by = Some(by.to_string());
            rec.revoke_reason = Some(reason.to_string());
            rec.clone()
        };
        self.persist()?;
        Ok(updated)
    }

    pub fn remove(&self, fingerprint: &str) -> Result<bool, ApprovalStoreError> {
        let removed = {
            let mut guard = self.inner.write().map_err(|_| {
                ApprovalStoreError::Io(std::io::Error::other("approval store poisoned"))
            })?;
            guard.remove(fingerprint).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn persist(&self) -> Result<(), ApprovalStoreError> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let records: Vec<_> = self
            .inner
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        let file = ApprovalFile { records };
        let json = serde_json::to_string_pretty(&file)?;
        // Atomic write: temp-file in the same dir, then rename.
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("approvals"),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path.as_path())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fingerprint_is_pending_on_first_register() {
        let s = ApprovalStore::in_memory();
        let state = s.record_register("abc123", "eu-01", "deadbeef");
        assert_eq!(state, ApprovalState::Pending);
        assert_eq!(s.len(), 1);
        assert_eq!(s.get("abc123").unwrap().state, ApprovalState::Pending);
    }

    #[test]
    fn second_register_keeps_state_updates_last_seen() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        s.accept("abc", "operator").unwrap();
        // Re-register with different agent_id — must NOT reset state.
        let st = s.record_register("abc", "eu-01-renamed", "pk");
        assert_eq!(st, ApprovalState::Accepted);
        assert_eq!(s.get("abc").unwrap().agent_id, "eu-01-renamed");
    }

    #[test]
    fn accept_moves_to_accepted() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        let r = s.accept("abc", "admin").unwrap();
        assert_eq!(r.state, ApprovalState::Accepted);
        assert_eq!(r.approved_by.as_deref(), Some("admin"));
        assert!(r.approved_at_ms.is_some());
    }

    #[test]
    fn revoke_sets_reason_and_state() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        s.accept("abc", "admin").unwrap();
        let r = s.revoke("abc", "admin", "compromised").unwrap();
        assert_eq!(r.state, ApprovalState::Revoked);
        assert_eq!(r.revoke_reason.as_deref(), Some("compromised"));
    }

    #[test]
    fn accept_unknown_errors() {
        let s = ApprovalStore::in_memory();
        let e = s.accept("nope", "admin").unwrap_err();
        assert!(matches!(e, ApprovalStoreError::Io(_)));
    }

    #[test]
    fn persists_and_reloads_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("approvals.json");
        let s = ApprovalStore::load_from_path(&path).unwrap();
        s.record_register("abc", "eu-01", "pk");
        s.accept("abc", "admin").unwrap();
        drop(s);
        let reloaded = ApprovalStore::load_from_path(&path).unwrap();
        let r = reloaded.get("abc").unwrap();
        assert_eq!(r.state, ApprovalState::Accepted);
        assert_eq!(r.approved_by.as_deref(), Some("admin"));
    }

    #[test]
    fn reject_marks_state_and_reason() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        let r = s.reject("abc", "admin", "wrong key").unwrap();
        assert_eq!(r.state, ApprovalState::Rejected);
        assert_eq!(r.revoke_reason.as_deref(), Some("wrong key"));
    }

    #[test]
    fn re_accept_after_reject_flips_back() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        s.reject("abc", "admin", "second thought").unwrap();
        let r = s.accept("abc", "admin").unwrap();
        assert_eq!(r.state, ApprovalState::Accepted);
        assert!(r.revoke_reason.is_none(), "accept clears revoke reason");
    }

    #[test]
    fn profile_patch_applies_and_preserves_state() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "pk");
        s.accept("abc", "admin").unwrap();
        let mut labels = HashMap::new();
        labels.insert("env".into(), "prod".into());
        let patch = AgentProfilePatch {
            description: Some("Frankfurt box 3".into()),
            client_id: Some("alice".into()),
            region: Some("eu-fra".into()),
            environment: Some("production".into()),
            purpose: Some("primary BTC/ETH maker".into()),
            owner_contact: Some("oncall@example.com".into()),
            notes: Some("Colocated with Binance FRA gateway".into()),
            labels: Some(labels),
        };
        let updated = s.update_profile("abc", patch).unwrap();
        assert_eq!(
            updated.state,
            ApprovalState::Accepted,
            "profile patch must not touch approval"
        );
        assert_eq!(
            updated.profile.description.as_deref(),
            Some("Frankfurt box 3")
        );
        assert_eq!(updated.profile.client_id.as_deref(), Some("alice"));
        assert_eq!(updated.profile.region.as_deref(), Some("eu-fra"));
        assert_eq!(updated.profile.environment.as_deref(), Some("production"));
        assert_eq!(
            updated.profile.purpose.as_deref(),
            Some("primary BTC/ETH maker")
        );
        assert_eq!(
            updated.profile.owner_contact.as_deref(),
            Some("oncall@example.com")
        );
        assert_eq!(
            updated.profile.notes.as_deref(),
            Some("Colocated with Binance FRA gateway")
        );
        assert_eq!(
            updated.profile.labels.get("env").map(String::as_str),
            Some("prod")
        );
    }

    #[test]
    fn pre_approve_then_register_binds_pubkey_silently() {
        let s = ApprovalStore::in_memory();
        // Admin pre-approves ahead of agent boot.
        let pre = s.pre_approve("abc123", "admin", Some("eu-01 box")).unwrap();
        assert_eq!(pre.state, ApprovalState::Accepted);
        assert!(pre.pubkey_hex.is_empty());
        assert_eq!(pre.approved_by.as_deref(), Some("admin"));
        // Agent connects with a real pubkey. State stays Accepted,
        // pubkey gets bound onto the empty slot.
        let state = s.record_register("abc123", "eu-01", "real-pubkey-hex");
        assert_eq!(state, ApprovalState::Accepted);
        let got = s.get("abc123").unwrap();
        assert_eq!(got.pubkey_hex, "real-pubkey-hex");
        assert_eq!(got.agent_id, "eu-01");
    }

    #[test]
    fn pre_approve_refuses_duplicate() {
        let s = ApprovalStore::in_memory();
        s.pre_approve("abc", "admin", None).unwrap();
        let dup = s.pre_approve("abc", "admin", None);
        assert!(dup.is_err());
    }

    #[test]
    fn pubkey_mismatch_for_known_fingerprint_is_logged_not_overwritten() {
        let s = ApprovalStore::in_memory();
        s.record_register("abc", "eu-01", "original-pk");
        s.record_register("abc", "eu-01", "spoofed-pk");
        assert_eq!(s.get("abc").unwrap().pubkey_hex, "original-pk");
    }
}
