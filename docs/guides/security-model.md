# Security Model

End-to-end security surface of MG Market Maker — what protects what, where the trust boundaries are, and how operators, clients, and administrators are authenticated, authorized, and audited.

**TL;DR** — JWT-bearer with three roles (admin / operator / viewer), per-client token scoping on the Client API, encrypted vault for venue credentials, HMAC-signed exports + manifests, append-only SHA-256-chained audit log.

---

## 1. Authentication

### 1.1 Primary: password + JWT

Users log in at `/login` with username + password. The server verifies the password (argon2id hash stored in the user store), then issues a signed JWT bearer token carrying:

```
TokenClaims {
  sub: String,         // user id
  name: String,        // display name
  role: Role,          // Admin | Operator | Viewer | Client
  client_id: Option<String>,  // set when role == Client
  exp: u64,            // expiry (default 12h)
  iat: u64,            // issued at
}
```

- **Signing key** — `MM_AUTH_SECRET` (32+ bytes). Rotating this invalidates every issued token. Required env; the server refuses to boot without it.
- **Token lifetime** — default 12h; configurable via `auth.token_ttl_secs`. Short enough that a stolen token doesn't survive long, long enough that operators don't re-login constantly.
- **Storage** — frontend stores the token in `localStorage.mm_auth`. HttpOnly cookies are NOT used; the API is designed for fetch-based clients.

### 1.2 Optional: TOTP 2FA

When the user store has `totp_secret: Some(_)` for a user, the login flow requires a 6-digit TOTP code in addition to the password.

- **2FA enrollment** — `/api/admin/users/{id}/2fa/enrol` returns a QR-code URI for an authenticator app; user confirms with a code; server persists the secret.
- **Admin enforcement** — a config flag (`auth.require_totp_for_admins`) can harden Admin-role login: every admin MUST have an enrolled TOTP. Server refuses boot if it sees any admin without a secret when the flag is on.

### 1.3 API-key alternative (programmatic access)

For programmatic clients (e.g. a client's automated reporting pipeline) the server supports API-key HMAC auth:

```
Authorization: Hmac-SHA256 keyId=<key_id>,sig=<hmac(method+path+body+ts)>,ts=<unix_ms>
```

- **Keys** live in the user store under the owning user, scoped to the owner's role
- **Replay protection** — `ts` must be within ±60 s of server clock
- **Key rotation** — `/api/admin/users/{id}/keys` CRUD; revocation is immediate

---

## 2. Authorization (RBAC)

Four roles, in order of power:

| Role | Can read | Can control | Admin surface |
|------|----------|-------------|---------------|
| **Client** | Their own symbols only (per-client API) | None | None |
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
VaultEntry {
  id: String,             // stable identifier
  client_id: Option<String>,  // None = platform-level; Some = per-tenant
  venue: String,          // "binance" | "bybit" | "hyperliquid" | "custom"
  kind: String,           // "spot" | "linear_perp" | "inverse_perp" | "margin"
  secrets: BTreeMap<String, String>,  // flat bag: api_key, api_secret, passphrase, ...
  created_at: i64,
  rotated_at: Option<i64>,
}
```

### 3.2 Encryption at rest

Vault entries encrypt with a master key derived from `MM_VAULT_KEY` (32+ bytes) via HKDF-SHA256. Each entry's `secrets` map serializes → AES-256-GCM encrypts → stored as `<ciphertext, nonce, tag>` triple. Master key rotation re-encrypts every entry under the new DEK.

### 3.3 Access flow

On engine startup:
1. Agent reads `(client_id, venue, kind)` from its config
2. Controller fetches the matching vault entry, decrypts, passes the secrets to the agent via the control-plane `CredentialPush` message (over a TLS-terminated control channel)
3. Agent instantiates the exchange connector with the decrypted credentials, never writes them to disk, zeroizes on drop

The engine binary never reads a plaintext secret from a config file or env var.

### 3.4 Rotation

Admin rotates via the Vault UI or POST `/api/admin/vault/{id}/rotate` with the new secrets. Behaviour:
1. Entry's `secrets` replaced, `rotated_at` stamped
2. Controller broadcasts `CredentialPush` to every agent using the entry
3. Each affected connector reconnects with the new credentials (WS drops + reopens, REST signer swaps)
4. `VaultRotated` audit event written with the entry id (NOT the secret)

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
3. Computes HMAC-SHA256 over the bundle with `MM_EXPORT_SIGNING_KEY`
4. Returns the bundle + a `manifest.json` with `{ hmac, bundle_sha256, byte_count, row_count, date_range }`
5. Regulators can re-verify the bundle against the manifest without trusting the exporting party

---

## 5. Control-plane trust boundary

The agent ↔ controller wire uses **signed envelopes**:

```rust
SignedEnvelope {
  inner: Envelope { command_or_telemetry: ... },
  signature: Ed25519(<controller or agent identity key>, hash(inner))
}
```

- **Controller identity key** — `MM_CONTROLLER_IDENTITY_KEY` (Ed25519 32-byte seed). Agents reject any envelope whose signature doesn't verify against the controller's pubkey.
- **Agent identity key** — same pattern in reverse. The controller rejects telemetry from an agent with an unknown pubkey.
- **Lease protocol** — the agent holds a `LeaderLease` with an expiry; refresh at 1/3 of lifetime. Lease revocation terminates the agent's authority; it walks the fail-ladder (cancel all orders, disconnect).

This boundary is what stops a compromised agent from authoring a rogue deployment on another agent's behalf, or from forging a "kill switch reset" telemetry ping.

### 5.1 Transport

The control-plane transport is pluggable (`Transport` trait). Production uses a TLS-terminated WebSocket between controller and agent. Agent startup includes a cert-pin check; mismatch = immediate exit.

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
| `MM_AUTH_SECRET` | JWT signing | All issued tokens forgeable; rotate ⇒ every user must re-login |
| `MM_VAULT_KEY` | Vault encryption | Attacker with vault file + this key reads every venue credential |
| `MM_EXPORT_SIGNING_KEY` | HMAC on exports | Attacker can forge MiCA manifests |
| `MM_CONTROLLER_IDENTITY_KEY` | Ed25519 control-plane | Attacker can forge commands to agents |
| `MM_API_KEY` / `MM_API_SECRET` | Legacy startup bootstrap of a single venue | Must be promoted to vault on first boot |
| `MM_TELEGRAM_TOKEN` | Telegram bot | Attacker can spam alerts; limited impact |

Leaks of any row 1-4 are disaster-tier → treat as an incident: rotate the leaked key, invalidate all derived material, audit the window.

---

## 8. Network surface

| Port | Bound to | Protocol | Notes |
|------|----------|----------|-------|
| `:9090` | Dashboard HTTP + WS | HTTPS (prod) / HTTP (dev) | Public; JWT-bearer gated; TLS terminates here |
| `:9091` | Prometheus scrape | HTTP | Bind to localhost; scrape via federated Prom |
| `:9443` | Control-plane | TLS WS | Agent ⇄ controller; cert-pinned |

TLS certs live outside the repo; operator's responsibility to provision + rotate (see `docs/deployment.md`).

---

## 9. Common compliance asks

- **"Show me who deployed graph X at time T"** — audit log filter by `event_type=StrategyGraphDeployed`, the row carries `actor=<user_id>`.
- **"Prove an order flow wasn't tampered"** — audit chain verify + HMAC on the fills export.
- **"Rotate credentials without downtime"** — vault rotate triggers the reconnect flow transparently; fills in-flight complete against the old session then the new one picks up.
- **"Revoke a compromised operator"** — admin UI → Users → disable → their token still works until expiry (max 12h); for immediate kill, rotate `MM_AUTH_SECRET` (invalidates everyone's tokens, forces re-login).

---

## 10. Known limits (be honest)

- **No HSM / KMS integration** today; vault key is a file/env-sourced 32-byte seed. Acceptable for small deployments, NOT for regulated custody of client funds at scale.
- **No secret scanning in CI** (git-secrets / trufflehog not wired). Developer discipline required: never commit a real key.
- **No rate-limiting on login** — a brute-force probe against `/login` is currently unthrottled. Put the dashboard behind a WAF / Cloudflare for production.
- **Kill-switch escalations aren't signed from the user side** — an operator click to "reset kill" relies on the JWT. A stolen token with Admin role = they can reset. 2FA+fresh-login-for-critical-actions should gate this for high-stakes deployments.
