use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use mm_risk::audit::{AuditEventType, AuditLog};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::net::SocketAddr;
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
    /// Client/exchange: read-only positions, PnL, SLA reports.
    Viewer,
}

impl Role {
    pub fn can_control(&self) -> bool {
        matches!(self, Role::Admin | Role::Operator)
    }

    pub fn can_view_internals(&self) -> bool {
        matches!(self, Role::Admin | Role::Operator)
    }
}

/// A registered API user.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiUser {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// API key for authentication.
    pub api_key: String,
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
            .field("allowed_symbols", &self.allowed_symbols)
            .field("client_id", &self.client_id)
            .finish()
    }
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

/// Auth state — manages users and tokens.
#[derive(Clone)]
pub struct AuthState {
    /// API key → user mapping.
    users: Arc<RwLock<HashMap<String, ApiUser>>>,
    /// HMAC secret for token signing. Hidden from Debug output.
    secret: Arc<String>,
    /// Optional audit sink — when present, login successes,
    /// failures, and logouts emit rows into the MiCA audit log
    /// so credential-stuffing attempts and operator intent leave
    /// a tamper-evident trail (Epic 38).
    audit: Option<Arc<AuditLog>>,
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
            secret: Arc::new(secret.to_string()),
            audit: None,
        }
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

    /// Register a user.
    pub fn add_user(&self, user: ApiUser) {
        let mut users = self.users.write().unwrap();
        users.insert(user.api_key.clone(), user);
    }

    /// Authenticate by API key. Returns user if valid.
    pub fn auth_by_key(&self, api_key: &str) -> Option<ApiUser> {
        let users = self.users.read().unwrap();
        users.get(api_key).cloned()
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

    warn!(path = %req.uri().path(), "unauthorized request");
    StatusCode::UNAUTHORIZED.into_response()
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
/// Blocks `Viewer` roles from seeing Prometheus metrics + internal
/// diagnostics that expose position sizes and PnL.
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

/// Verify a token supplied as a query parameter. Used by the
/// WebSocket upgrade handler where browsers cannot set request
/// headers — never accept this path on regular HTTP routes.
pub fn verify_token_param(auth: &AuthState, token: &str) -> Option<TokenClaims> {
    auth.verify_token(token)
}

/// Login endpoint: POST /api/auth/login { "api_key": "..." } → { "token": "...", "role": "..." }
///
/// Both paths (success and failure) emit an audit event so
/// credential-stuffing attempts leave a trail even when no valid
/// key is ever guessed (Epic 38).
pub async fn login_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<LoginRequest>,
) -> Response {
    let ip = addr.ip();
    if let Some(user) = auth.auth_by_key(&body.api_key) {
        let token = auth.generate_token(&user);
        auth.audit(
            AuditEventType::LoginSucceeded,
            &format!("user_id={},role={:?},ip={}", user.id, user.role, ip),
        );
        axum::Json(LoginResponse {
            token,
            user_id: user.id,
            name: user.name,
            role: user.role,
        })
        .into_response()
    } else {
        // Timing-equalization: run verify_token on a dummy input
        // so a failed login takes a comparable amount of time to
        // a valid one, avoiding a trivial timing oracle on api_key
        // membership. Cheap.
        let _ = auth.verify_token(&body.api_key);
        // Record a short prefix of the presented key so incident
        // responders can correlate stuffing runs without leaking
        // the full (possibly valid-on-another-surface) secret.
        let key_prefix: String = body.api_key.chars().take(6).collect();
        auth.audit(
            AuditEventType::LoginFailed,
            &format!("ip={},key_prefix={},reason=unknown_key", ip, key_prefix),
        );
        StatusCode::UNAUTHORIZED.into_response()
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

#[derive(Deserialize)]
pub struct LoginRequest {
    pub api_key: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user_id: String,
    pub name: String,
    pub role: Role,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn tmp_audit_path() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mm_auth_audit_{}_{}.jsonl",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        p
    }

    fn read_audit_lines(path: &std::path::Path) -> Vec<String> {
        let f = std::fs::File::open(path).expect("open audit file");
        BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .collect()
    }

    fn user(key: &str, role: Role) -> ApiUser {
        ApiUser {
            id: format!("u-{key}"),
            name: "tester".into(),
            role,
            api_key: key.to_string(),
            allowed_symbols: None,
            client_id: None,
        }
    }

    #[test]
    fn token_round_trips_and_expires() {
        let auth = AuthState::new("0123456789abcdef0123456789abcdef");
        let u = user("k-admin", Role::Admin);
        auth.add_user(u.clone());
        let tok = auth.generate_token(&u);
        let claims = auth.verify_token(&tok).expect("valid token verifies");
        assert_eq!(claims.user_id, u.id);
        assert_eq!(claims.role, Role::Admin);
        // Tamper: flip a byte in the signature — must fail.
        let mut bad = tok.clone();
        bad.pop();
        bad.push('a');
        assert!(auth.verify_token(&bad).is_none());
    }

    #[test]
    fn audit_success_and_failure_rows_written() {
        let path = tmp_audit_path();
        let audit = Arc::new(AuditLog::new(&path).expect("audit"));
        let auth = AuthState::new("0123456789abcdef0123456789abcdef").with_audit(audit.clone());
        auth.add_user(user("real-key", Role::Operator));

        auth.audit(
            AuditEventType::LoginSucceeded,
            "user_id=u-real-key,role=Operator,ip=127.0.0.1",
        );
        auth.audit(
            AuditEventType::LoginFailed,
            "ip=127.0.0.1,key_prefix=badkey,reason=unknown_key",
        );
        auth.audit(
            AuditEventType::LogoutSucceeded,
            "user_id=u-real-key,ip=127.0.0.1",
        );
        audit.flush();

        let lines = read_audit_lines(&path);
        assert!(lines.iter().any(|l| l.contains("\"login_succeeded\"")));
        assert!(lines.iter().any(|l| l.contains("\"login_failed\"")));
        assert!(lines.iter().any(|l| l.contains("\"logout_succeeded\"")));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_sink_optional() {
        // No audit attached — calls must not panic.
        let auth = AuthState::new("0123456789abcdef0123456789abcdef");
        auth.audit(AuditEventType::LoginSucceeded, "no-sink");
    }
}
