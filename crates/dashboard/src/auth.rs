use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiUser {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// API key for authentication.
    pub api_key: String,
    /// Optional: restrict to specific symbols.
    pub allowed_symbols: Option<Vec<String>>,
}

/// JWT-like token claims (simplified, HMAC-signed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    pub user_id: String,
    pub role: Role,
    pub exp: i64,
}

/// Auth state — manages users and tokens.
#[derive(Clone)]
pub struct AuthState {
    /// API key → user mapping.
    users: Arc<RwLock<HashMap<String, ApiUser>>>,
    /// HMAC secret for token signing.
    secret: String,
}

impl AuthState {
    pub fn new(secret: &str) -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            secret: secret.to_string(),
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
        if parts[1] != expected_sig {
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
            let b1 = if chunk.len() > 1 { chunk[1] as usize } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as usize } else { 0 };
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

/// Axum middleware: extract auth from `Authorization: Bearer <token>` or `X-API-Key: <key>`.
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
                    exp: Utc::now().timestamp() + 3600,
                };
                req.extensions_mut().insert(claims);
                return next.run(req).await;
            }
        }
    }

    // Try query parameter (for WebSocket connections from browser).
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=") {
                if let Some(claims) = auth.verify_token(token) {
                    req.extensions_mut().insert(claims);
                    return next.run(req).await;
                }
            }
        }
    }

    warn!("unauthorized request to {}", req.uri());
    StatusCode::UNAUTHORIZED.into_response()
}

/// Login endpoint: POST /api/auth/login { "api_key": "..." } → { "token": "...", "role": "..." }
pub async fn login_handler(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    axum::Json(body): axum::Json<LoginRequest>,
) -> Response {
    if let Some(user) = auth.auth_by_key(&body.api_key) {
        let token = auth.generate_token(&user);
        axum::Json(LoginResponse {
            token,
            user_id: user.id,
            name: user.name,
            role: user.role,
        })
        .into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
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
