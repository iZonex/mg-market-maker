use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use mm_risk::audit::{AuditEventType, AuditLog};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use subtle::ConstantTimeEq;
use tracing::warn;

type HmacSha256 = Hmac<Sha256>;

/// User roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Full control: kill switch, strategy config, all data.
    Admin,
    /// MM operator: can view everything + some controls.
    Operator,
    /// Internal read-only viewer (exchange compliance, auditor
    /// with full-fleet read). Sees all clients, no controls.
    Viewer,
    /// Wave E — tenant-scoped client portal user. Must have
    /// `client_id` set on the `ApiUser`. Sees ONLY their own
    /// client's PnL / SLA / fills / webhook deliveries; cross-
    /// tenant access returns 403 from the scope middleware.
    /// No access to operator/admin controls or internal views.
    #[serde(alias = "client_reader", alias = "client")]
    ClientReader,
}

impl Role {
    pub fn can_control(&self) -> bool {
        matches!(self, Role::Admin | Role::Operator)
    }

    pub fn can_view_internals(&self) -> bool {
        matches!(self, Role::Admin | Role::Operator)
    }

    /// Wave E — this role's requests are tenant-scoped: any
    /// path carrying `{client_id}` must match the token's own
    /// `client_id`, and endpoints without a client id default
    /// to self-scope (`/api/v1/client/self/*`).
    pub fn is_tenant_scoped(&self) -> bool {
        matches!(self, Role::ClientReader)
    }
}

/// A registered user. Carries either an `api_key` (machine
/// auth — exchange CI scripts, prometheus scrapers), a
/// `password_hash` (operator browser login — the standard path),
/// or both. Boot-strap flow creates the first user by password;
/// API keys are optional side tokens the admin issues later.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiUser {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// Optional API key for machine-to-machine auth. Empty
    /// string when the user only has a password login.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Argon2id password hash (PHC string format). `None` when
    /// the user was provisioned with API-key-only auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    /// Activated TOTP shared secret (base32). When `Some`, login
    /// requires a second factor — server returns `needs_totp` and
    /// waits for the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp_secret: Option<String>,
    /// Pending secret from an in-progress enrollment. Kept
    /// separate from `totp_secret` so a half-finished enroll
    /// (QR scanned, code never verified) does NOT lock the
    /// operator out — their login still works off password
    /// alone until verify succeeds and promotes this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp_pending: Option<String>,
    /// UTC millis the account was created. Informational only.
    #[serde(default)]
    pub created_at_ms: i64,
    /// Optional: restrict to specific symbols.
    pub allowed_symbols: Option<Vec<String>>,
    /// Owning client ID (Epic 1). When set, the user can only
    /// access symbols belonging to this client. `None` for
    /// admin/operator users who see everything.
    #[serde(default)]
    pub client_id: Option<String>,
}

impl std::fmt::Debug for ApiUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiUser")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("api_key", &"<redacted>")
            .field("password_hash", &self.password_hash.as_ref().map(|_| "<redacted>"))
            .field("totp_secret", &self.totp_secret.as_ref().map(|_| "<redacted>"))
            .field("allowed_symbols", &self.allowed_symbols)
            .field("client_id", &self.client_id)
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct UserFile {
    #[serde(default)]
    users: Vec<ApiUser>,
}

/// JWT-like token claims (simplified, HMAC-signed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    pub user_id: String,
    pub role: Role,
    /// Owning client ID (Epic 1). When set, API requests are
    /// scoped to this client's symbols. `None` for admin users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub exp: i64,
}

/// Wave E4 — client signup invite. Admin generates a signed
/// invite carrying the target `client_id` + one-shot `invite_id`;
/// the client opens the invite URL, picks their username +
/// password, and ends up with an ApiUser(role=ClientReader,
/// client_id=X). No out-of-band key sharing. `invite_id` is
/// tracked in a used-invites set so re-submission fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteClaims {
    pub invite_id: String,
    pub client_id: String,
    pub exp: i64,
}

/// Wave H1 — password-reset token. Admin mints a signed reset
/// URL for a specific user; the user opens it, picks a new
/// password, and the one-shot `reset_id` is burned in
/// `used_resets` so the same URL can't be replayed. Short 1h
/// expiry — this is an admin-initiated recovery path, not a
/// mailed "forgot password" that sits in an inbox for days.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetClaims {
    pub reset_id: String,
    pub user_id: String,
    pub exp: i64,
}

/// Auth state — manages users and tokens.
#[derive(Clone)]
pub struct AuthState {
    /// Canonical user storage, keyed by `user.id`. API-key and
    /// name lookups read the two index maps and resolve back to
    /// this map.
    users: Arc<RwLock<HashMap<String, ApiUser>>>,
    /// `api_key → user_id`. Populated only for users that have a
    /// non-empty api_key set.
    api_key_index: Arc<RwLock<HashMap<String, String>>>,
    /// `name → user_id` (case-insensitive). Populated for every
    /// user so the password-login path can find them by name.
    name_index: Arc<RwLock<HashMap<String, String>>>,
    /// HMAC secret for token signing. Hidden from Debug output.
    secret: Arc<String>,
    /// Label this controller advertises as the TOTP "issuer" on
    /// enrollment. Shown to users in their authenticator app
    /// (Google Authenticator, 1Password, …). Default is a neutral
    /// "MM" — operators override via `MM_TOTP_ISSUER` so a
    /// multi-tenant deployment doesn't label every account with
    /// the same generic string.
    totp_issuer: Arc<String>,
    /// Optional path for on-disk user persistence. When present,
    /// `add_user` / `create_password_user` / `delete_user` also
    /// write the full user set to the file atomically.
    users_path: Option<Arc<PathBuf>>,
    /// Optional audit sink — when present, login successes,
    /// failures, and logouts emit rows into the MiCA audit log
    /// so credential-stuffing attempts and operator intent leave
    /// a tamper-evident trail (Epic 38).
    audit: Option<Arc<AuditLog>>,
    /// Wave E4 — set of `invite_id`s that have already been
    /// consumed by a successful signup. A resubmit with the same
    /// token returns 410 Gone instead of creating a second user.
    /// In-memory only; restarts reset the set, so invites become
    /// replayable within a 24h window across restarts — mitigated
    /// by the 24h `exp` on InviteClaims.
    used_invites: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Wave H1 — burned password-reset token ids. Short 1h TTL
    /// on the signature bounds cross-restart replay; same in-mem
    /// set pattern as `used_invites`.
    used_resets: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Wave H3 — when true, password login for `Role::Admin`
    /// requires an active TOTP secret. Admins without 2FA
    /// armed are rejected with a 403 + `must_enroll_totp` hint
    /// so the UI can route them to the enrollment flow. Does
    /// not affect operator/viewer/clientreader. Flipped by
    /// `MM_REQUIRE_TOTP_FOR_ADMIN=true` at the server binary.
    require_totp_for_admin: bool,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("users", &"<hidden>")
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl AuthState {
    pub fn new(secret: &str) -> Self {
        if secret.len() < 32 {
            warn!(
                secret_len = secret.len(),
                "MM_AUTH_SECRET is shorter than 32 bytes — token forgery is easier; \
                 set a 32+ byte random secret in production"
            );
        }
        if secret == "change-me-in-production" {
            warn!(
                "MM_AUTH_SECRET is the default placeholder — set a real random \
                 secret before exposing the dashboard to any network"
            );
        }
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            api_key_index: Arc::new(RwLock::new(HashMap::new())),
            name_index: Arc::new(RwLock::new(HashMap::new())),
            secret: Arc::new(secret.to_string()),
            totp_issuer: Arc::new("MG | Market Maker".to_string()),
            users_path: None,
            audit: None,
            used_invites: Arc::new(RwLock::new(std::collections::HashSet::new())),
            used_resets: Arc::new(RwLock::new(std::collections::HashSet::new())),
            require_totp_for_admin: false,
        }
    }

    /// Wave H3 — harden admin login with mandatory TOTP. When
    /// `enabled` is true and an `Admin` user tries to log in
    /// without `totp_secret` armed, the login handler rejects
    /// with 403 + `must_enroll_totp` so the UI can route them
    /// into enrollment. Idempotent / defaults false, so flipping
    /// the flag on an existing deployment doesn't lock out
    /// admins mid-session — only the next login attempt is
    /// gated. Bootstrap flow is untouched.
    pub fn with_require_totp_for_admin(mut self, enabled: bool) -> Self {
        self.require_totp_for_admin = enabled;
        self
    }

    pub fn require_totp_for_admin(&self) -> bool {
        self.require_totp_for_admin
    }

    /// Override the TOTP issuer label. Called from the binary
    /// with the operator-configured `MM_TOTP_ISSUER` value (e.g.
    /// `"Alpha Capital MM"`, `"Bob's Bot"`). Short, recognisable
    /// strings work best — authenticator apps truncate on small
    /// screens.
    pub fn with_totp_issuer(mut self, issuer: impl Into<String>) -> Self {
        let raw = issuer.into();
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            self.totp_issuer = Arc::new(trimmed.to_string());
        }
        self
    }

    pub fn totp_issuer(&self) -> &str {
        &self.totp_issuer
    }

    /// Attach a path for on-disk user persistence. When the file
    /// exists it's loaded; subsequent user-mutating operations
    /// write the full user set atomically. First-run detection
    /// (`needs_bootstrap`) flips on when no users are loaded.
    pub fn with_users_path(mut self, path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let p = path.as_ref().to_path_buf();
        if p.exists() {
            let raw = std::fs::read_to_string(&p)?;
            if !raw.trim().is_empty() {
                let file: UserFile = serde_json::from_str(&raw).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?;
                let mut users = self.users.write().unwrap();
                let mut keys = self.api_key_index.write().unwrap();
                let mut names = self.name_index.write().unwrap();
                for u in file.users {
                    if !u.api_key.is_empty() {
                        keys.insert(u.api_key.clone(), u.id.clone());
                    }
                    names.insert(u.name.to_ascii_lowercase(), u.id.clone());
                    users.insert(u.id.clone(), u);
                }
            }
        }
        self.users_path = Some(Arc::new(p));
        Ok(self)
    }

    /// Returns `true` when no users are loaded — the caller
    /// should render the bootstrap flow instead of a normal
    /// login form.
    pub fn needs_bootstrap(&self) -> bool {
        self.users.read().map(|g| g.is_empty()).unwrap_or(true)
    }

    /// H5 GOBS — boot-time preflight for the TOTP hard-gate.
    /// Returns true if at least one admin user has an active
    /// `totp_secret` enrolled. The server uses this to refuse
    /// booting under `MM_REQUIRE_TOTP_FOR_ADMIN=true` when
    /// nobody can satisfy the 2FA check, preventing an
    /// auth-layer lockout.
    pub fn any_admin_has_totp(&self) -> bool {
        let Ok(guard) = self.users.read() else { return false };
        guard
            .values()
            .any(|u| matches!(u.role, Role::Admin) && u.totp_secret.is_some())
    }

    /// Attach an audit sink — login / logout events will be
    /// mirrored into the `AuditLog` so the MiCA trail captures
    /// who logged in from where and when. Optional: when not
    /// attached, auth events only go to `tracing`.
    pub fn with_audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Emit an auth audit event if the sink is attached.
    fn audit(&self, ty: AuditEventType, detail: &str) {
        if let Some(a) = &self.audit {
            a.auth_event(ty, detail);
        }
    }

    /// Register a user (legacy / test path).
    pub fn add_user(&self, user: ApiUser) {
        {
            let mut users = self.users.write().unwrap();
            let mut keys = self.api_key_index.write().unwrap();
            let mut names = self.name_index.write().unwrap();
            if !user.api_key.is_empty() {
                keys.insert(user.api_key.clone(), user.id.clone());
            }
            names.insert(user.name.to_ascii_lowercase(), user.id.clone());
            users.insert(user.id.clone(), user);
        }
        let _ = self.persist();
    }

    /// Remove a user by id. Clears both indexes. Persists.
    pub fn remove_user(&self, id: &str) -> bool {
        let removed = {
            let mut users = self.users.write().unwrap();
            let Some(u) = users.remove(id) else {
                return false;
            };
            let mut keys = self.api_key_index.write().unwrap();
            let mut names = self.name_index.write().unwrap();
            if !u.api_key.is_empty() {
                keys.remove(&u.api_key);
            }
            names.remove(&u.name.to_ascii_lowercase());
            true
        };
        let _ = self.persist();
        removed
    }

    /// Create a password-backed user. `password` is argon2id-
    /// hashed before storage; the plaintext is never retained.
    /// Returns an error on duplicate name or argon2 failure.
    pub fn create_password_user(
        &self,
        name: &str,
        password: &str,
        role: Role,
    ) -> Result<ApiUser, String> {
        if password.len() < 8 {
            return Err("password must be at least 8 characters".into());
        }
        if name.trim().is_empty() {
            return Err("name must not be empty".into());
        }
        {
            let names = self.name_index.read().unwrap();
            if names.contains_key(&name.to_ascii_lowercase()) {
                return Err(format!("user '{name}' already exists"));
            }
        }
        let hash = hash_password(password)?;
        let user = ApiUser {
            id: format!("u-{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            role,
            api_key: String::new(),
            password_hash: Some(hash),
            totp_secret: None,
            totp_pending: None,
            created_at_ms: Utc::now().timestamp_millis(),
            allowed_symbols: None,
            client_id: None,
        };
        self.add_user(user.clone());
        Ok(user)
    }

    /// Authenticate by API key. Returns user if valid.
    pub fn auth_by_key(&self, api_key: &str) -> Option<ApiUser> {
        let id = {
            let keys = self.api_key_index.read().unwrap();
            keys.get(api_key).cloned()?
        };
        let users = self.users.read().unwrap();
        users.get(&id).cloned()
    }

    /// Authenticate by name + password. Argon2id verify — runs
    /// in constant time relative to hash length.
    pub fn auth_by_password(&self, name: &str, password: &str) -> Option<ApiUser> {
        let id = {
            let names = self.name_index.read().unwrap();
            names.get(&name.to_ascii_lowercase()).cloned()
        };
        // Always compute a hash-verify against SOMETHING so the
        // not-found path takes comparable time to the found-but-
        // wrong-password path. Avoids a trivial username-oracle.
        let user = id.and_then(|id| self.users.read().unwrap().get(&id).cloned());
        let hash_str = user
            .as_ref()
            .and_then(|u| u.password_hash.clone())
            .unwrap_or_else(|| DUMMY_HASH.to_string());
        let parsed = PasswordHash::new(&hash_str).ok()?;
        match Argon2::default().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => user.filter(|u| u.password_hash.is_some()),
            Err(_) => None,
        }
    }

    /// Generate a session token (valid for 24h).
    pub fn generate_token(&self, user: &ApiUser) -> String {
        let claims = TokenClaims {
            user_id: user.id.clone(),
            role: user.role,
            client_id: user.client_id.clone(),
            exp: (Utc::now() + Duration::hours(24)).timestamp(),
        };
        let payload = serde_json::to_string(&claims).unwrap_or_default();
        let signature = self.sign(&payload);
        let encoded_payload = base64_encode(&payload);
        format!("{encoded_payload}.{signature}")
    }

    /// Verify a token. Returns claims if valid.
    pub fn verify_token(&self, token: &str) -> Option<TokenClaims> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let payload = base64_decode(parts[0])?;
        let expected_sig = self.sign(&payload);
        // Constant-time comparison to prevent timing attacks.
        if parts[1].as_bytes().ct_eq(expected_sig.as_bytes()).unwrap_u8() != 1 {
            return None;
        }
        let claims: TokenClaims = serde_json::from_str(&payload).ok()?;
        if claims.exp < Utc::now().timestamp() {
            return None;
        }
        Some(claims)
    }

    /// List all users (for admin).
    pub fn list_users(&self) -> Vec<ApiUser> {
        let users = self.users.read().unwrap();
        users.values().cloned().collect()
    }

    /// Look up user by ID.
    pub fn get_user_by_id(&self, user_id: &str) -> Option<ApiUser> {
        let users = self.users.read().unwrap();
        users.values().find(|u| u.id == user_id).cloned()
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Wave E4 — issue a single-use invite for a client. Returned
    /// string is `base64(json).signature`; same shape as auth
    /// tokens, different payload type so an auth token cannot be
    /// accidentally replayed as an invite (verify checks the
    /// shape). Valid for 24 hours.
    pub fn issue_invite(&self, client_id: &str) -> String {
        let invite = InviteClaims {
            invite_id: uuid::Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            exp: (Utc::now() + Duration::hours(24)).timestamp(),
        };
        let payload = serde_json::to_string(&invite).unwrap_or_default();
        let signature = self.sign(&payload);
        let encoded = base64_encode(&payload);
        format!("{encoded}.{signature}")
    }

    /// Wave E4 — verify an invite. Returns `Some(claims)` if
    /// signature valid + not expired + not previously consumed.
    /// Does NOT mark the invite consumed — the caller does that
    /// inside `consume_invite` after the signup succeeds, so a
    /// failed signup attempt (password too weak, name taken)
    /// doesn't burn the invite.
    pub fn verify_invite(&self, token: &str) -> Option<InviteClaims> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let payload = base64_decode(parts[0])?;
        let expected_sig = self.sign(&payload);
        if parts[1].as_bytes().ct_eq(expected_sig.as_bytes()).unwrap_u8() != 1 {
            return None;
        }
        let claims: InviteClaims = serde_json::from_str(&payload).ok()?;
        if claims.exp < Utc::now().timestamp() {
            return None;
        }
        if self
            .used_invites
            .read()
            .ok()?
            .contains(&claims.invite_id)
        {
            return None;
        }
        Some(claims)
    }

    /// Mark an invite consumed. Call from the signup handler
    /// after the new user is created. Idempotent — a double
    /// insert is a no-op (set semantics).
    pub fn consume_invite(&self, invite_id: &str) {
        if let Ok(mut set) = self.used_invites.write() {
            set.insert(invite_id.to_string());
        }
    }

    /// Wave H1 — issue a signed password-reset token for a
    /// specific user. Token shape `base64(json).signature`, valid
    /// for 1 hour, one-shot via `used_resets`. Caller is admin —
    /// the handler enforces that.
    pub fn issue_password_reset(&self, user_id: &str) -> String {
        let reset = ResetClaims {
            reset_id: uuid::Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            exp: (Utc::now() + Duration::hours(1)).timestamp(),
        };
        let payload = serde_json::to_string(&reset).unwrap_or_default();
        let signature = self.sign(&payload);
        let encoded = base64_encode(&payload);
        format!("{encoded}.{signature}")
    }

    /// Wave H1 — verify a reset token. Returns `Some(claims)` if
    /// signature valid + not expired + not previously consumed.
    /// Does NOT mark consumed — the handler burns after the
    /// password write succeeds.
    pub fn verify_password_reset(&self, token: &str) -> Option<ResetClaims> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let payload = base64_decode(parts[0])?;
        let expected_sig = self.sign(&payload);
        if parts[1].as_bytes().ct_eq(expected_sig.as_bytes()).unwrap_u8() != 1 {
            return None;
        }
        let claims: ResetClaims = serde_json::from_str(&payload).ok()?;
        if claims.exp < Utc::now().timestamp() {
            return None;
        }
        if self
            .used_resets
            .read()
            .ok()?
            .contains(&claims.reset_id)
        {
            return None;
        }
        Some(claims)
    }

    pub fn consume_password_reset(&self, reset_id: &str) {
        if let Ok(mut set) = self.used_resets.write() {
            set.insert(reset_id.to_string());
        }
    }

    /// Wave H1 — set a user's password without the old-password
    /// challenge. Used by the reset handler after verifying a
    /// signed admin-issued reset token. Keeps the TOTP secret
    /// intact — a password reset should not silently drop 2FA.
    pub fn set_password(&self, user_id: &str, new_password: &str) -> Result<(), String> {
        if new_password.len() < 8 {
            return Err("new password must be at least 8 characters".into());
        }
        let exists = self.users.read().unwrap().contains_key(user_id);
        if !exists {
            return Err("user not found".into());
        }
        let new_hash = hash_password(new_password)?;
        {
            let mut users = self.users.write().unwrap();
            if let Some(u) = users.get_mut(user_id) {
                u.password_hash = Some(new_hash);
            }
        }
        let _ = self.persist();
        Ok(())
    }

    /// Wave E4 — create a ClientReader user from a verified
    /// invite. Separate from `create_password_user` because it
    /// forces `role = ClientReader` and `client_id = <invite>`;
    /// the signup handler can't be tricked into upgrading the
    /// role by passing a different body.
    pub fn create_client_reader(
        &self,
        name: &str,
        password: &str,
        client_id: &str,
    ) -> Result<ApiUser, String> {
        if password.len() < 8 {
            return Err("password must be at least 8 characters".into());
        }
        if name.trim().is_empty() {
            return Err("name must not be empty".into());
        }
        {
            let names = self.name_index.read().unwrap();
            if names.contains_key(&name.to_ascii_lowercase()) {
                return Err(format!("user '{name}' already exists"));
            }
        }
        let hash = hash_password(password)?;
        let user = ApiUser {
            id: format!("u-{}", uuid::Uuid::new_v4().simple()),
            name: name.trim().to_string(),
            role: Role::ClientReader,
            api_key: String::new(),
            password_hash: Some(hash),
            totp_secret: None,
            totp_pending: None,
            created_at_ms: Utc::now().timestamp_millis(),
            allowed_symbols: None,
            client_id: Some(client_id.to_string()),
        };
        self.add_user(user.clone());
        Ok(user)
    }

    /// Change a user's password after verifying the old one.
    /// Returns `Err` on: unknown user, wrong old password, new
    /// password too short.
    pub fn change_password(
        &self,
        user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), String> {
        if new_password.len() < 8 {
            return Err("new password must be at least 8 characters".into());
        }
        let user = {
            let users = self.users.read().unwrap();
            users.get(user_id).cloned()
        };
        let user = user.ok_or_else(|| "user not found".to_string())?;
        let hash = user
            .password_hash
            .as_deref()
            .ok_or_else(|| "user has no password set".to_string())?;
        let parsed = PasswordHash::new(hash).map_err(|e| format!("corrupt hash: {e}"))?;
        Argon2::default()
            .verify_password(old_password.as_bytes(), &parsed)
            .map_err(|_| "old password is wrong".to_string())?;
        let new_hash = hash_password(new_password)?;
        {
            let mut users = self.users.write().unwrap();
            if let Some(u) = users.get_mut(user_id) {
                u.password_hash = Some(new_hash);
            }
        }
        let _ = self.persist();
        Ok(())
    }

    /// Start a TOTP enrollment. Generates a fresh secret, stores
    /// it on the user record in "pending" form (we re-use the
    /// same field — verify flips state on successful code match).
    /// Returns the otpauth URI the client renders as a QR code.
    pub fn totp_enroll(&self, user_id: &str, issuer: &str) -> Result<TotpEnrollment, String> {
        use totp_rs::{Algorithm, Secret, TOTP};
        let user = self
            .users
            .read()
            .unwrap()
            .get(user_id)
            .cloned()
            .ok_or_else(|| "user not found".to_string())?;
        // Generate a 160-bit secret — RFC 4226 recommendation.
        let secret = Secret::generate_secret();
        let secret_base32 = secret
            .to_encoded()
            .to_string();
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            secret.to_bytes().map_err(|e| format!("secret encode: {e}"))?,
            Some(issuer.to_string()),
            user.name.clone(),
        )
        .map_err(|e| format!("totp build: {e}"))?;
        let otpauth = totp.get_url();
        // Stash the pending secret on the user record. It does
        // NOT activate 2FA at login time until `totp_verify`
        // confirms the client can produce a valid code from the
        // same secret — prevents locking an operator out when a
        // QR-scan mid-step goes wrong.
        {
            let mut users = self.users.write().unwrap();
            if let Some(u) = users.get_mut(user_id) {
                u.totp_pending = Some(secret_base32.clone());
            }
        }
        let _ = self.persist();
        Ok(TotpEnrollment { secret_base32, otpauth })
    }

    /// Verify a 6-digit TOTP code against the pending secret and,
    /// on success, promote it to the active `totp_secret`. From
    /// now on login requires the second factor.
    pub fn totp_verify(&self, user_id: &str, code: &str) -> Result<(), String> {
        let pending = {
            let users = self.users.read().unwrap();
            users.get(user_id).and_then(|u| u.totp_pending.clone())
        };
        let pending = pending.ok_or_else(|| "no enrollment in progress".to_string())?;
        if !verify_totp(&pending, code)? {
            return Err("code did not match".into());
        }
        {
            let mut users = self.users.write().unwrap();
            if let Some(u) = users.get_mut(user_id) {
                u.totp_secret = Some(pending);
                u.totp_pending = None;
            }
        }
        let _ = self.persist();
        Ok(())
    }

    /// Turn 2FA off. Requires the current password — prevents a
    /// stolen session token from undoing the second factor.
    pub fn totp_disable(&self, user_id: &str, password: &str) -> Result<(), String> {
        let user = self
            .users
            .read()
            .unwrap()
            .get(user_id)
            .cloned()
            .ok_or_else(|| "user not found".to_string())?;
        let hash = user
            .password_hash
            .as_deref()
            .ok_or_else(|| "user has no password set".to_string())?;
        let parsed = PasswordHash::new(hash).map_err(|e| format!("corrupt hash: {e}"))?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| "password is wrong".to_string())?;
        {
            let mut users = self.users.write().unwrap();
            if let Some(u) = users.get_mut(user_id) {
                u.totp_secret = None;
                u.totp_pending = None;
            }
        }
        let _ = self.persist();
        Ok(())
    }

    /// Check whether a user has 2FA armed (not pending, fully
    /// verified). Login flow consults this before issuing a
    /// token — when true the client must also present a code.
    pub fn user_requires_totp(&self, user_id: &str) -> bool {
        self.users
            .read()
            .ok()
            .and_then(|g| g.get(user_id).cloned())
            .map(|u| u.totp_secret.is_some())
            .unwrap_or(false)
    }

    /// Verify a TOTP code against the user's stored secret. Used
    /// in the login path when the user has 2FA enabled.
    pub fn verify_totp_for(&self, user_id: &str, code: &str) -> bool {
        let secret = {
            let users = self.users.read().unwrap();
            users.get(user_id).and_then(|u| u.totp_secret.clone())
        };
        let Some(secret) = secret else { return false };
        verify_totp(&secret, code).unwrap_or(false)
    }

    fn persist(&self) -> Result<(), std::io::Error> {
        let Some(path) = self.users_path.as_ref() else {
            return Ok(());
        };
        let snapshot: Vec<ApiUser> = self
            .users
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        let file = UserFile { users: snapshot };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("users"),
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

/// Precomputed argon2 hash used by the "user not found" path of
/// `auth_by_password` so attackers can't distinguish unknown-
/// username from wrong-password via response timing. Hash of a
/// random string that no legitimate user will ever guess.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$UHJvZHVjdGlvblNhbHQxMjM$KBeaCpMdCbA8HU3QJv6KKMSjOdFWkFmAaYvQJgyeOw4";

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("argon2 hash failed: {e}"))
}

fn verify_totp(secret_base32: &str, code: &str) -> Result<bool, String> {
    use totp_rs::{Algorithm, Secret, TOTP};
    let secret = Secret::Encoded(secret_base32.to_string())
        .to_bytes()
        .map_err(|e| format!("totp secret decode: {e}"))?;
    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret, None, String::new())
        .map_err(|e| format!("totp build: {e}"))?;
    totp.check_current(code)
        .map_err(|e| format!("totp check: {e}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct TotpEnrollment {
    /// Base32-encoded secret the client may copy as a fallback
    /// if it can't scan the QR code.
    pub secret_base32: String,
    /// `otpauth://` URI the client renders as a QR image. Contains
    /// the issuer, account label, and secret in the URL encoding
    /// authenticator apps expect.
    pub otpauth: String,
}

fn base64_encode(s: &str) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder = Base64Writer::new(&mut buf);
        encoder.write_all(s.as_bytes()).unwrap();
    }
    String::from_utf8(buf).unwrap_or_default()
}

fn base64_decode(s: &str) -> Option<String> {
    let bytes = Base64Reader::decode(s)?;
    String::from_utf8(bytes).ok()
}

// Simple base64 (URL-safe, no padding) — avoids extra dependency.
struct Base64Writer<'a> {
    buf: &'a mut Vec<u8>,
}

impl<'a> Base64Writer<'a> {
    fn new(buf: &'a mut Vec<u8>) -> Self {
        Self { buf }
    }
}

impl std::io::Write for Base64Writer<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as usize;
            let b1 = if chunk.len() > 1 {
                chunk[1] as usize
            } else {
                0
            };
            let b2 = if chunk.len() > 2 {
                chunk[2] as usize
            } else {
                0
            };
            self.buf.push(CHARS[b0 >> 2]);
            self.buf.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)]);
            if chunk.len() > 1 {
                self.buf.push(CHARS[((b1 & 0xf) << 2) | (b2 >> 6)]);
            }
            if chunk.len() > 2 {
                self.buf.push(CHARS[b2 & 0x3f]);
            }
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Base64Reader;

impl Base64Reader {
    fn decode(s: &str) -> Option<Vec<u8>> {
        const DECODE: [u8; 128] = {
            let mut t = [255u8; 128];
            let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut i = 0;
            while i < 64 {
                t[chars[i] as usize] = i as u8;
                i += 1;
            }
            // Also accept + and / for standard base64.
            t[b'+' as usize] = 62;
            t[b'/' as usize] = 63;
            t
        };

        let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
        let mut out = Vec::new();

        for chunk in bytes.chunks(4) {
            let vals: Vec<u8> = chunk
                .iter()
                .map(|&b| {
                    if (b as usize) < 128 {
                        DECODE[b as usize]
                    } else {
                        255
                    }
                })
                .collect();
            if vals.contains(&255) {
                return None;
            }
            out.push((vals[0] << 2) | (vals.get(1).unwrap_or(&0) >> 4));
            if chunk.len() > 2 {
                out.push((vals[1] << 4) | (vals.get(2).unwrap_or(&0) >> 2));
            }
            if chunk.len() > 3 {
                out.push((vals[2] << 6) | vals.get(3).unwrap_or(&0));
            }
        }
        Some(out)
    }
}

/// Axum middleware: extract auth from `Authorization: Bearer <token>`
/// or `X-API-Key: <key>` header. Rejects requests with tokens in
/// URL query strings — those leak into logs, browser history, and
/// proxy caches (WebSocket upgrade uses a separate auth path that
/// accepts `?token=` via `verify_token_param` in the handler).
pub async fn auth_middleware(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Try Bearer token first.
    if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(val) = auth_header.to_str() {
            if let Some(token) = val.strip_prefix("Bearer ") {
                if let Some(claims) = auth.verify_token(token) {
                    req.extensions_mut().insert(claims);
                    return next.run(req).await;
                }
            }
        }
    }

    // Try API key header.
    if let Some(key_header) = req.headers().get("x-api-key") {
        if let Ok(key) = key_header.to_str() {
            if let Some(user) = auth.auth_by_key(key) {
                let claims = TokenClaims {
                    user_id: user.id.clone(),
                    role: user.role,
                    client_id: user.client_id.clone(),
                    exp: Utc::now().timestamp() + 3600,
                };
                req.extensions_mut().insert(claims);
                return next.run(req).await;
            }
        }
    }

    // 2026-04 stabilization — when an operator's JWT expires
    // while the dashboard is open, every polling component hits
    // this middleware around the same time (Overview page polls
    // ~10 endpoints at 4-10s cadence). Without dedup that's one
    // WARN per endpoint per cycle for every stale-token user —
    // the "auth flood" the TODO called out. Dedupe by
    // (path, client_ip) with a 60s window: first miss warns,
    // subsequent ones drop to debug. Genuine attack churn
    // (many paths, many IPs) still surfaces because the cache
    // fills independently per tuple.
    let path = req.uri().path().to_string();
    // axum sets X-Forwarded-For via ConnectInfo in the server
    // layer; here we take the first value of either header as a
    // cheap cache key. Falling back to "unknown" is fine — the
    // dedup grouping still works.
    let client = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    if unauthorized_should_warn(&path, &client) {
        warn!(path = %path, client = %client, "unauthorized request");
    } else {
        tracing::debug!(path = %path, client = %client, "unauthorized request (dedup)");
    }
    StatusCode::UNAUTHORIZED.into_response()
}

/// Cache of (path, client) → last warn timestamp. Entries older
/// than 60s are ignored so the cache stays bounded; a full GC
/// is unnecessary because session-expiry flood has a small
/// working-set (one operator × handful of endpoints).
fn unauthorized_should_warn(path: &str, client: &str) -> bool {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    static CACHE: OnceLock<Mutex<std::collections::HashMap<(String, String), Instant>>> =
        OnceLock::new();
    const WINDOW: Duration = Duration::from_secs(60);
    const CACHE_CAP: usize = 1024;

    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let Ok(mut guard) = cache.lock() else { return true };
    let key = (path.to_string(), client.to_string());
    let now = Instant::now();
    // Cheap GC — if the map got huge, drop old entries
    // opportunistically. Bounded by CACHE_CAP so pathological
    // attack churn doesn't leak memory.
    if guard.len() > CACHE_CAP {
        guard.retain(|_, ts| now.duration_since(*ts) < WINDOW);
    }
    match guard.get(&key) {
        Some(last) if now.duration_since(*last) < WINDOW => false,
        _ => {
            guard.insert(key, now);
            true
        }
    }
}

/// Role gate: require `Role::Admin`. Place after `auth_middleware`
/// via `.route_layer(admin_middleware).route_layer(auth_middleware)`
/// so claims are populated by the time this runs.
pub async fn admin_middleware(req: Request, next: Next) -> Response {
    match req.extensions().get::<TokenClaims>() {
        Some(c) if c.role == Role::Admin => next.run(req).await,
        Some(c) => {
            warn!(
                user_id = %c.user_id,
                role = ?c.role,
                path = %req.uri().path(),
                "admin-only endpoint blocked"
            );
            StatusCode::FORBIDDEN.into_response()
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Role gate: require internal-view permission (Admin or Operator).
/// Blocks `Viewer` + `ClientReader` roles from seeing Prometheus
/// metrics + internal diagnostics that expose position sizes and
/// PnL across clients.
pub async fn internal_view_middleware(req: Request, next: Next) -> Response {
    match req.extensions().get::<TokenClaims>() {
        Some(c) if c.role.can_view_internals() => next.run(req).await,
        Some(c) => {
            warn!(
                user_id = %c.user_id,
                role = ?c.role,
                path = %req.uri().path(),
                "internal-view endpoint blocked"
            );
            StatusCode::FORBIDDEN.into_response()
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Wave E — tenant-scope gate for `/api/v1/client/{id}/*`.
/// Three cases:
///   (1) Admin / Operator / Viewer with `client_id = None` —
///       full access, pass through.
///   (2) Any token with a concrete `client_id` (ClientReader
///       by design, or a tenant-tagged Operator) hitting a
///       path that carries `{id}` — must match exactly or 403.
///   (3) A tenant-scoped role (ClientReader) hitting a path
///       that doesn't carry `{id}` AND isn't the `/self/`
///       alias — blocked 403. ClientReader must not see
///       fleet / admin / cross-tenant endpoints at all.
pub async fn tenant_scope_middleware(req: Request, next: Next) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // Case (1): untenanted admin/operator/viewer.
    if claims.client_id.is_none() && !claims.role.is_tenant_scoped() {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let path_id = extract_client_id_from_path(&path);
    let token_id = claims.client_id.as_deref();
    // Allow the `/api/v1/client/self/*` alias for tenant-scoped
    // tokens — the self-endpoint handler rewrites the path id
    // from the token.
    let is_self_alias = path.starts_with("/api/v1/client/self/")
        || path == "/api/v1/client/self";
    match (token_id, path_id) {
        (Some(tok), Some(p)) if tok == p => next.run(req).await,
        (Some(tok), Some(p)) => {
            warn!(
                user_id = %claims.user_id,
                token_client = %tok,
                path_client = %p,
                path = %path,
                "cross-tenant access blocked"
            );
            StatusCode::FORBIDDEN.into_response()
        }
        (Some(_), None) if is_self_alias => next.run(req).await,
        (Some(tok), None) => {
            // Tenant-scoped token on a non-client, non-self path.
            // ClientReader hitting /api/v1/fleet, /api/v1/pnl, …
            warn!(
                user_id = %claims.user_id,
                token_client = %tok,
                path = %path,
                "unscopable endpoint blocked for tenant-scoped token"
            );
            StatusCode::FORBIDDEN.into_response()
        }
        // Untenanted token but role is tenant_scoped — invalid
        // config (ClientReader provisioned without client_id).
        (None, _) if claims.role.is_tenant_scoped() => {
            warn!(
                user_id = %claims.user_id,
                role = ?claims.role,
                path = %path,
                "tenant-scoped role without client_id — rejecting"
            );
            StatusCode::FORBIDDEN.into_response()
        }
        _ => next.run(req).await,
    }
}

/// Extract the `{id}` segment from a path like
/// `/api/v1/client/{id}/pnl` or `/api/admin/clients/{id}`. Returns
/// None for paths that don't carry a client id.
fn extract_client_id_from_path(path: &str) -> Option<&str> {
    let segments: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // /api/v1/client/{id}/... → segments = ["api","v1","client",id,...]
    // /api/admin/clients/{id} → ["api","admin","clients",id]
    for (i, seg) in segments.iter().enumerate() {
        if (seg == &"client" || seg == &"clients") && i + 1 < segments.len() {
            let id = segments[i + 1];
            // "self" is the tenant-scoped self-alias, not a literal id.
            if id != "self" {
                return Some(id);
            }
        }
    }
    None
}

/// Verify a token supplied as a query parameter. Used by the
/// WebSocket upgrade handler where browsers cannot set request
/// headers — never accept this path on regular HTTP routes.
pub fn verify_token_param(auth: &AuthState, token: &str) -> Option<TokenClaims> {
    auth.verify_token(token)
}

/// Login endpoint: `POST /api/auth/login`.
///
/// Accepts `{ "name": ..., "password": ... }` (primary path,
/// first-factor browser auth) OR `{ "api_key": ... }` (legacy /
/// machine auth). Either produces a JWT on success.
///
/// Both success and failure emit audit events so credential-
/// stuffing attempts leave a trail even when no valid secret is
/// ever guessed (Epic 38).
pub async fn login_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<LoginRequest>,
) -> Response {
    let ip = addr.ip();
    // Prefer name+password when both are provided.
    let (user, method) = if let (Some(name), Some(password)) =
        (body.name.as_deref(), body.password.as_deref())
    {
        (auth.auth_by_password(name, password), "password")
    } else if let Some(api_key) = body.api_key.as_deref() {
        (auth.auth_by_key(api_key), "api_key")
    } else {
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={ip},reason=missing_credentials"),
        );
        return (StatusCode::BAD_REQUEST, "provide name+password or api_key").into_response();
    };

    if let Some(user) = user {
        // Wave H3 — hard gate: if the deployment requires TOTP
        // for admins and this admin has not yet enrolled, refuse
        // the login with a structured 403. The frontend routes
        // such users through the out-of-band enrollment process
        // (reset password from another admin session, or flip
        // the flag off temporarily during the migration window).
        if auth.require_totp_for_admin
            && matches!(user.role, Role::Admin)
            && user.totp_secret.is_none()
        {
            auth.audit(
                AuditEventType::LoginFailed,
                &format!(
                    "user_id={},role=Admin,ip={},reason=totp_required_not_enrolled",
                    user.id, ip
                ),
            );
            return (
                StatusCode::FORBIDDEN,
                axum::Json(LoginResponseTotpRequired {
                    must_enroll_totp: true,
                    message:
                        "admin accounts must have TOTP enrolled before login is allowed",
                }),
            )
                .into_response();
        }
        // Second-factor gate: if 2FA is armed, the client must
        // supply a TOTP code. Missing code → respond with the
        // signal that lets the UI render the code prompt without
        // burning the password attempt.
        if user.totp_secret.is_some() {
            match body.totp_code.as_deref() {
                None | Some("") => {
                    return (
                        StatusCode::ACCEPTED,
                        axum::Json(LoginResponse2FAPending { needs_totp: true }),
                    )
                        .into_response();
                }
                Some(code) => {
                    if !auth.verify_totp_for(&user.id, code) {
                        auth.audit(
                            AuditEventType::LoginFailed,
                            &format!(
                                "user_id={},ip={},reason=bad_totp_code",
                                user.id, ip
                            ),
                        );
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                }
            }
        }
        let token = auth.generate_token(&user);
        auth.audit(
            AuditEventType::LoginSucceeded,
            &format!(
                "user_id={},role={:?},method={},ip={}",
                user.id, user.role, method, ip
            ),
        );
        axum::Json(LoginResponse {
            token,
            user_id: user.id,
            name: user.name,
            role: user.role,
        })
        .into_response()
    } else {
        let hint = body
            .name
            .as_deref()
            .or(body.api_key.as_deref())
            .map(|s| s.chars().take(6).collect::<String>())
            .unwrap_or_default();
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={ip},method={method},hint={hint},reason=bad_credentials"),
        );
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// `GET /api/auth/status` — public, unauthenticated. Returns
/// whether the UI should render the bootstrap form (no users
/// yet) or the normal login form.
pub async fn auth_status_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
) -> Response {
    axum::Json(AuthStatusResponse {
        needs_bootstrap: auth.needs_bootstrap(),
    })
    .into_response()
}

/// `POST /api/auth/bootstrap` — public, unauthenticated. Only
/// works when no users exist; creates the first root admin with
/// the supplied name + password. Subsequent calls return 409.
pub async fn bootstrap_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<BootstrapRequest>,
) -> Response {
    let ip = addr.ip();
    if !auth.needs_bootstrap() {
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={ip},reason=bootstrap_already_done"),
        );
        return (
            StatusCode::CONFLICT,
            "bootstrap already completed — use /api/auth/login",
        )
            .into_response();
    }
    let user = match auth.create_password_user(&body.name, &body.password, Role::Admin) {
        Ok(u) => u,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let token = auth.generate_token(&user);
    auth.audit(
        AuditEventType::LoginSucceeded,
        &format!(
            "user_id={},role=Admin,method=bootstrap,ip={}",
            user.id, ip
        ),
    );
    axum::Json(LoginResponse {
        token,
        user_id: user.id,
        name: user.name,
        role: user.role,
    })
    .into_response()
}

/// Wave E4 — admin generates an invite URL for a tenant.
/// `POST /api/admin/clients/{id}/invite` → returns the token
/// + a ready-to-send URL. Single-use inside the 24h window
/// (`used_invites` set on AuthState).
#[derive(Debug, serde::Serialize)]
pub struct InviteResponse {
    pub invite_token: String,
    pub invite_url: String,
    pub expires_at: String,
}

pub async fn create_invite_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    axum::extract::Path(client_id): axum::extract::Path<String>,
) -> Response {
    if client_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "client_id must not be empty").into_response();
    }
    let token = auth.issue_invite(&client_id);
    let url = format!("/client-signup/{token}");
    let expires_at = (Utc::now() + Duration::hours(24)).to_rfc3339();
    axum::Json(InviteResponse {
        invite_token: token,
        invite_url: url,
        expires_at,
    })
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct ClientSignupRequest {
    pub invite_token: String,
    pub name: String,
    pub password: String,
}

/// Wave E4 — public signup endpoint. Client visits the URL
/// from their admin, picks a name + password, and ends up with
/// a ClientReader login. We verify the invite first, create
/// the user, consume the invite on success, and issue an auth
/// token so the new user lands directly on the portal without
/// a second round-trip to /api/auth/login.
pub async fn client_signup_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<ClientSignupRequest>,
) -> Response {
    let ip = addr.ip();
    let Some(claims) = auth.verify_invite(&body.invite_token) else {
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={ip},reason=invalid_invite"),
        );
        return (StatusCode::UNAUTHORIZED, "invalid or expired invite").into_response();
    };
    let user = match auth.create_client_reader(&body.name, &body.password, &claims.client_id) {
        Ok(u) => u,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    auth.consume_invite(&claims.invite_id);
    let token = auth.generate_token(&user);
    auth.audit(
        AuditEventType::LoginSucceeded,
        &format!(
            "user_id={},role=ClientReader,client_id={},method=invite_signup,ip={}",
            user.id, claims.client_id, ip
        ),
    );
    axum::Json(LoginResponse {
        token,
        user_id: user.id,
        name: user.name,
        role: user.role,
    })
    .into_response()
}

/// Wave H1 — admin mints a one-shot password-reset URL for a
/// target user. `POST /api/admin/users/{id}/reset-password`
/// returns `{ reset_token, reset_url, expires_at }`. Admin
/// delivers the URL out-of-band (Signal, in-person, secure
/// paste). Public consumer is `/api/auth/password-reset`.
#[derive(Debug, serde::Serialize)]
pub struct ResetResponse {
    pub reset_token: String,
    pub reset_url: String,
    pub expires_at: String,
}

pub async fn create_password_reset_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
    req: Request,
) -> Response {
    if user_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "user_id must not be empty").into_response();
    }
    if auth.get_user_by_id(&user_id).is_none() {
        return (StatusCode::NOT_FOUND, "user not found").into_response();
    }
    let by = req
        .extensions()
        .get::<TokenClaims>()
        .map(|c| c.user_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let token = auth.issue_password_reset(&user_id);
    let url = format!("/password-reset/{token}");
    let expires_at = (Utc::now() + Duration::hours(1)).to_rfc3339();
    auth.audit(
        AuditEventType::PasswordResetIssued,
        &format!("target={user_id},by={by}"),
    );
    axum::Json(ResetResponse {
        reset_token: token,
        reset_url: url,
        expires_at,
    })
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct PasswordResetRequest {
    pub reset_token: String,
    pub new_password: String,
}

/// Wave H1 — public endpoint. Verifies a signed reset token,
/// sets the user's password, and burns the token so a replay
/// returns 401. Rate-limited under `login_rl` because brute-
/// forcing a random reset_id is the only attack surface.
pub async fn password_reset_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<PasswordResetRequest>,
) -> Response {
    let ip = addr.ip();
    let Some(claims) = auth.verify_password_reset(&body.reset_token) else {
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={ip},reason=invalid_reset_token"),
        );
        return (StatusCode::UNAUTHORIZED, "invalid or expired reset token").into_response();
    };
    match auth.set_password(&claims.user_id, &body.new_password) {
        Ok(()) => {
            auth.consume_password_reset(&claims.reset_id);
            auth.audit(
                AuditEventType::PasswordResetCompleted,
                &format!("user_id={},ip={}", claims.user_id, ip),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Logout endpoint: POST /api/auth/logout → 204.
///
/// Tokens are stateless HMAC so there is no server-side session
/// to clear — the client drops the token, the 24 h `exp` bound
/// remains the only server-side enforcement. The event exists to
/// mark operator intent in the MiCA audit trail.
pub async fn logout_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    let ip = addr.ip();
    let user_id = req
        .extensions()
        .get::<TokenClaims>()
        .map(|c| c.user_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    auth.audit(
        AuditEventType::LogoutSucceeded,
        &format!("user_id={user_id},ip={ip}"),
    );
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize, Default)]
pub struct LoginRequest {
    /// Primary path: operator types their name + password.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Legacy / machine-auth path: a long API key issued
    /// out-of-band. Keep working for scripted integrations.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Second factor — populated on the follow-up request when
    /// the server responds with `needs_totp: true`.
    #[serde(default)]
    pub totp_code: Option<String>,
}

/// Response shape when the first factor succeeded but the user
/// has 2FA armed — client must resubmit with `totp_code`. HTTP
/// status is 202 Accepted so the client can distinguish from a
/// fully-issued token (200) and from a failure (401).
#[derive(Serialize)]
pub struct LoginResponse2FAPending {
    pub needs_totp: bool,
}

/// Wave H3 — response body on 403 when an admin tries to log
/// in without TOTP armed while `require_totp_for_admin` is on.
/// The frontend reads `must_enroll_totp=true` and redirects the
/// user into the profile/2FA enrollment flow without burning
/// another password attempt in the rate limiter.
#[derive(Serialize)]
pub struct LoginResponseTotpRequired {
    pub must_enroll_totp: bool,
    pub message: &'static str,
}

#[derive(Deserialize)]
pub struct BootstrapRequest {
    pub name: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct TotpVerifyRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct TotpDisableRequest {
    pub password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub user_id: String,
    pub name: String,
    pub role: Role,
    pub totp_enabled: bool,
    pub created_at_ms: i64,
}

/// `GET /api/auth/me` — protected. Returns the current user's
/// profile summary so the Profile UI can render without a
/// second round-trip.
pub async fn me_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(user) = auth.get_user_by_id(&claims.user_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    axum::Json(MeResponse {
        user_id: user.id,
        name: user.name,
        role: user.role,
        totp_enabled: user.totp_secret.is_some(),
        created_at_ms: user.created_at_ms,
    })
    .into_response()
}

/// `POST /api/auth/password` — protected. Changes the current
/// user's password after verifying the old one. Active session
/// token is kept (not rotated) — operator can decide when to
/// log out via the usual path.
pub async fn change_password_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // Manually extract the JSON body — the middleware-injected
    // claims took priority; axum extractors can only run once.
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 1024 * 8).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let payload: ChangePasswordRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad body: {e}")).into_response(),
    };
    match auth.change_password(&claims.user_id, &payload.old_password, &payload.new_password) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `POST /api/auth/totp/enroll` — protected. Generates a fresh
/// pending secret + `otpauth://` URI for QR rendering. The
/// client verifies a 6-digit code from its authenticator app
/// before the secret becomes active.
pub async fn totp_enroll_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let issuer = auth.totp_issuer().to_string();
    match auth.totp_enroll(&claims.user_id, &issuer) {
        Ok(enroll) => axum::Json(enroll).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `POST /api/auth/totp/verify` — protected. Takes the 6-digit
/// code and, on match, promotes the pending secret to the
/// active `totp_secret`. From now on login requires the code.
pub async fn totp_verify_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 1024 * 8).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let payload: TotpVerifyRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad body: {e}")).into_response(),
    };
    match auth.totp_verify(&claims.user_id, &payload.code) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `POST /api/auth/totp/disable` — protected. Turns 2FA off;
/// requires the current password so a stolen session token
/// can't undo the second factor by itself.
pub async fn totp_disable_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
) -> Response {
    let Some(claims) = req.extensions().get::<TokenClaims>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 1024 * 8).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let payload: TotpDisableRequest = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad body: {e}")).into_response(),
    };
    match auth.totp_disable(&claims.user_id, &payload.password) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub needs_bootstrap: bool,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user_id: String,
    pub name: String,
    pub role: Role,
}

#[cfg(test)]
mod tests;
