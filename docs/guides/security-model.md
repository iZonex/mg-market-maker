# Security Model

End-to-end security surface of MG Market Maker — what protects what, where the trust boundaries are, and how operators, clients, and administrators are authenticated, authorized, and audited.

**TL;DR** — JWT-bearer with four roles (admin / operator / viewer / client-reader), per-client token scoping on the Client API, AES-256-GCM vault for venue credentials, HMAC-SHA256-signed exports + manifests, append-only SHA-256-chained audit log, Ed25519-signed control-plane envelopes.

---

## 1. Authentication

### 1.1 Primary: password + JWT

Users log in at `/login` with username + password. The server verifies the password (argon2id hash stored in the user store), then issues a signed JWT bearer token carrying:

```rust
pub struct TokenClaims {
    pub user_id: String,
    pub role: Role,
    pub client_id: Option<String>,  // set when role == ClientReader
    pub exp: i64,                   // unix seconds
}
```

- **Signing key** — `MM_AUTH_SECRET` (32+ bytes). Rotating this invalidates every issued token. Required env; the server refuses to boot without it. Tokens are HMAC-SHA256 signed JWT-style (there is no server-side session table — the HMAC *is* the proof).
- **Token lifetime** — 24h (hardcoded in `generate_session_token`); password-reset tokens are 1h.
- **Storage** — frontend stores the token in `localStorage.mm_auth`. HttpOnly cookies are NOT used; the API is designed for fetch-based clients.

### 1.2 Optional: TOTP 2FA

When the user store has `totp_secret: Some(_)` for a user, the login flow requires a 6-digit TOTP code in addition to the password.

- **2FA enrollment** — the user starts a TOTP setup via the Profile page, scans the QR with an authenticator, confirms with a code; server persists the secret.
- **Admin enforcement** — `MM_REQUIRE_TOTP_FOR_ADMIN=1` hardens Admin-role login: every admin MUST have an enrolled TOTP. `MM_REQUIRE_TOTP_ADMIN_BYPASS` is an escape hatch for the first-boot bootstrap admin only.
- **Issuer label in QR** — `MM_TOTP_ISSUER` sets the issuer text the authenticator app displays.

---

## 2. Authorization (RBAC)

Four roles, in order of power:

| Role | Can read | Can control | Admin surface |
|------|----------|-------------|---------------|
| **ClientReader** | Their own client's symbols only (per-client API) | None | None |
| **Viewer** | Everything on Overview / Fleet / metrics / reports | None | None |
| **Operator** | Viewer + config + rules | Deploy strategies, ack incidents, pause symbols, approve calibration | None |
| **Admin** | Everything | Everything Operator can | Users, vault, platform config, kill-switch reset, login audit |

### 2.1 Router wiring

`router_full_authed()` in `crates/controller/src/http.rs` builds three nested routers, each with its own middleware:

1. **Public** — `/login`, `/health` (no auth)
2. **Authed** — requires a valid token; checks role at handler-level for mutations
3. **Admin-gated** — `control_role_middleware` refuses anything but `Admin` at the router level; covers `/api/admin/*`

A regression test `auth_matrix.rs` pins the matrix: every route × every role → expected status code (200 / 401 / 403). Adding a new endpoint without a matrix row is a CI failure.

### 2.2 Per-client scoping (Client API)

Routes under `/api/v1/client/self/*` extract the token's `client_id` and filter data to that client's symbols only. The scoping happens at query time via `DashboardState::get_client_symbols(client_id)` — there is no "client header" that the client can spoof, because the scope comes from the token.

Cross-client leakage is prevented by the `VaultEntry.client_id` field (Wave 2B) — even venue credentials are per-tenant, so one client's order flow never touches another client's API keys.

---

## 3. Vault (venue credentials)

Venue API keys + secrets NEVER live in config TOML in production. They live in the encrypted vault.

### 3.1 Shape

```rust
pub struct VaultEntry {
    pub name: String,                       // stable identifier
    pub kind: String,                       // "exchange" | "telegram" | "generic"
    pub description: Option<String>,
    pub values: BTreeMap<String, String>,   // secret fields — encrypted on disk
    pub metadata: BTreeMap<String, String>, // non-secret labels (exchange, product, chat_id, ...)
    pub allowed_agents: Vec<String>,        // whitelist — empty = push to every accepted agent
}
```

### 3.2 Encryption at rest

The controller holds a `MasterKey` — a 32-byte symmetric key used for AES-256-GCM (`aes-gcm` crate). Each field inside a `VaultEntry.values` map is encrypted individually: nonce (12 bytes, `OsRng`) + ciphertext+tag. `metadata` stays plaintext so the UI can list entries without decrypting.

Master key sourcing (checked in order):
1. `MM_MASTER_KEY=<64-hex>` env var — 32 raw bytes hex-encoded
2. `MM_MASTER_KEY_FILE=/path/to/master-key` — file containing raw 32 bytes
3. Generated on first boot and persisted (file mode `0600`) via `MasterKey::load_or_generate(path)` when neither env is set — acceptable for single-node dev, NOT for production.

### 3.3 Access flow

On agent registration + on rotation:
1. Controller decrypts the relevant `VaultEntry` values
2. Dispatches `PushedCredential` to the agent over the signed control channel
3. Agent instantiates connectors with the decrypted credentials in memory; no disk persistence

### 3.4 Rotation

Admin rotates via the Vault UI or POST against `/api/admin/vault/{name}`. Behaviour:
1. Controller overwrites the entry's encrypted values with the newly-encrypted versions
2. Pushes fresh `PushedCredential` to every whitelisted agent
3. Each affected connector reconnects with the new credentials (WS drops + reopens, REST signer swaps)

---

## 4. Audit trail (MiCA compliance)

Every control-plane action + every risk event + every manual operator decision writes to an append-only JSONL audit log.

### 4.1 Chain integrity

Each row is:
```json
{"ts": "2026-04-24T12:34:56.789Z",
 "event_type": "StrategyGraphDeployed",
 "symbol": "BTCUSDT",
 "client_id": "acme",
 "data": "graph=cross-asset-regime hash=abc123...",
 "prev_hash": "<sha256 of previous row's raw bytes>",
 "row_hash": "<sha256 of this row's payload>"}
```

`prev_hash` → `row_hash` form a chain. Tampering with any past row breaks verification from that row onward.

### 4.2 Verification

- **Agent-local verify** — `audit_chain_verify` details topic walks the agent's local file and confirms every `prev_hash` equals the SHA-256 of the previous line's raw bytes. Exposed in the Compliance page's "Verify audit chain" button.
- **Over-the-wire** — the controller can't re-verify remotely because re-serialisation would change the bytes; the verify has to run on the agent.

### 4.3 Export

MiCA Article 17 requires a monthly report with signed manifest. The Compliance page's "MiCA report" button:
1. Server selects the relevant audit rows (date range, client scope)
2. Serializes to JSON / CSV / XLSX / PDF
3. Computes HMAC-SHA256 over the bundle. The signing secret is passed at the call site (`mica_report::sign_body(body, secret)` in `crates/dashboard/src/mica_report.rs`) — typically sourced from the controller's master-key material, not a separate env var
4. Returns the bundle + a manifest with `signature_algo: "hmac-sha256-hex"` and the signed digest
5. Regulators can re-verify the bundle against the manifest without trusting the exporting party

---

## 5. Control-plane trust boundary

The agent ↔ controller wire uses **signed envelopes** (`crates/control/src/envelope.rs` + `crates/control/src/identity.rs`):

```rust
pub struct IdentityKey { /* wraps ed25519_dalek::SigningKey */ }
pub struct SignedEnvelope { inner: Envelope, signature: [u8; 64] }
```

- **Identity keys** — raw 32-byte Ed25519 seeds. Loaded from a file path at startup (`IdentityKey::load_from_file(path)`); if the file is absent, a new key is generated and persisted to disk. Agents and controllers each have their own identity file.
  - Agent bootstrap: `mm-agent --identity <path>` (or similar config key) — see `crates/agent/src/main.rs::load_or_generate_identity`
  - Controller bootstrap: same pattern on the controller binary
- **Signing** — `IdentityKey::sign(bytes)` returns a 64-byte Ed25519 signature; the receiver calls `PublicKey::verify(bytes, sig)`. Unknown-pubkey envelopes are rejected.
- **Approvals** — controller maintains an approval store of agent pubkeys. A new agent's first register is queued in the approval ring; an admin approves it before the agent's envelopes are accepted.
- **Lease protocol** — the agent holds a `LeaderLease` with an expiry; refresh at 1/3 of lifetime. Lease revocation terminates the agent's authority; it walks the fail-ladder (cancel all orders, disconnect).

This boundary is what stops a compromised agent from authoring a rogue deployment on another agent's behalf, or from forging a "kill switch reset" telemetry ping.

### 5.1 Transport

The control-plane transport is pluggable (`Transport` trait). Production uses a TLS-terminated WebSocket between controller and agent (`MM_AGENT_WS_ADDR` on the server side); operator provides TLS certs via `MM_TLS_CERT` / `MM_TLS_KEY`.

---

## 6. Kill switch (operator-last-resort)

5-level escalation, automatic UP, manual-only DOWN from L5:

| Level | Trigger | Behaviour |
|-------|---------|-----------|
| L0 Normal | — | Quoting |
| L1 WidenSpreads | VPIN > threshold OR Market Resilience < 0.3 for 3s+ | Autotuner widens |
| L2 StopNewOrders | Drawdown breach, VaR breach, news Critical | No new quotes |
| L3 CancelAll | Hard inventory / exposure breach | Cancel every live order |
| L4 FlattenAll | Uncompensated pair-break, disaster drawdown | TWAP out the inventory |
| L5 Disconnect | Escalated L4 with stuck inventory | Manual reset required |

**Manual reset** (L5 → L0) is Admin-only, requires 2FA if enrolled, audits the reset action with the operator's user id.

---

## 7. Secrets inventory

Secrets that must be provided at boot (env vars):

| Variable | Purpose | Impact if leaked |
|----------|---------|------------------|
| `MM_AUTH_SECRET` | JWT HMAC signing | All issued tokens forgeable; rotate ⇒ every user must re-login |
| `MM_MASTER_KEY` / `MM_MASTER_KEY_FILE` | Vault AES-256-GCM | Attacker with vault file + this key reads every venue credential |
| `MM_CHECKPOINT_SECRET` | Checkpoint integrity | Attacker can forge crash-recovery state |
| `MM_TLS_CERT` / `MM_TLS_KEY` | Dashboard + control-plane TLS | Standard cert-theft impact |
| Identity key files (controller + each agent) | Ed25519 control-plane | Attacker can forge commands to agents (controller key) or spoof an agent's identity to the controller |
| `MM_TELEGRAM_TOKEN` | Telegram bot | Attacker can spam alerts; limited impact |
| Venue credentials (in vault) | Real API keys | Direct trading compromise |

Leaks of rows 1-3 + identity key files are disaster-tier → treat as an incident: rotate the leaked key, invalidate all derived material, audit the window.

---

## 8. Network surface

| Binding | Bound to | Protocol | Env / config |
|---------|----------|----------|--------------|
| Dashboard HTTP + WS | Axum server | HTTPS (prod, with `MM_TLS_CERT` + `MM_TLS_KEY`) / HTTP (dev) | `MM_HTTP_ADDR` |
| Control-plane WS | Agent ⇄ controller | TLS WS (same TLS env pair) | `MM_AGENT_WS_ADDR` |
| Prometheus scrape | `/metrics` on the dashboard HTTP server | HTTP | Same as dashboard |

TLS certs live outside the repo; operator's responsibility to provision + rotate. See `docs/deployment.md`.

---

## 9. Common compliance asks

- **"Show me who deployed graph X at time T"** — audit log filter by `event_type=StrategyGraphDeployed`, the row carries `actor=<user_id>`.
- **"Prove an order flow wasn't tampered"** — audit chain verify + HMAC on the fills export.
- **"Rotate credentials without downtime"** — vault rotate triggers the reconnect flow transparently; fills in-flight complete against the old session then the new one picks up.
- **"Revoke a compromised operator"** — admin UI → Users → disable → their token still works until expiry (max 24h); for immediate kill, rotate `MM_AUTH_SECRET` (invalidates everyone's tokens, forces re-login).

---

## 10. Known limits (be honest)

- **No HSM / KMS integration** today; vault key is a file/env-sourced 32-byte seed. Acceptable for small deployments, NOT for regulated custody of client funds at scale.
- **No secret scanning in CI** (git-secrets / trufflehog not wired). Developer discipline required: never commit a real key.
- **No rate-limiting on login** — a brute-force probe against `/login` is currently unthrottled. Put the dashboard behind a WAF / Cloudflare for production.
- **Kill-switch escalations aren't signed from the user side** — an operator click to "reset kill" relies on the JWT. A stolen token with Admin role = they can reset. 2FA+fresh-login-for-critical-actions should gate this for high-stakes deployments.
