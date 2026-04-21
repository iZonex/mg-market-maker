# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do NOT** open a public GitHub issue
2. Email: [security contact — add your email here]
3. Include: description, reproduction steps, potential impact
4. We will acknowledge within 48 hours and provide a fix timeline

## Secrets inventory

Every credential the market-maker handles, where it comes from,
and what rotating it breaks. Keep this table current — it is the
reference for every runbook below.

| Name | Purpose | Source | Rotate cadence |
|------|---------|--------|----------------|
| `MM_BINANCE_API_KEY` / `MM_BINANCE_API_SECRET` | Binance spot + USDM futures auth | Exchange UI | 30–90 d |
| `MM_BYBIT_API_KEY` / `MM_BYBIT_API_SECRET` | Bybit V5 auth (spot + linear + inverse) | Exchange UI | 30–90 d |
| `MM_HL_PRIVATE_KEY` | HyperLiquid EIP-712 signing (address derived) | Wallet export (hex, 32 bytes) | 180 d + on operator turnover |
| `MM_READ_KEY` / `MM_READ_SECRET` | Optional read-only key pair for /balance + fee-tier polls | Exchange UI | 90 d |
| `MM_AUTH_SECRET` | HMAC key for dashboard session tokens | `openssl rand -base64 48` | 30 d + on operator turnover |
| `MM_CHECKPOINT_SECRET` | HMAC key signing checkpoint file integrity | `openssl rand -base64 48` | 180 d (rotating truncates restore capability — see runbook) |
| `MM_TELEGRAM_TOKEN` / `MM_TELEGRAM_CHAT` | Telegram alert bot | BotFather | On demand |
| `MM_SENTRY_DSN` | Sentry error aggregation endpoint | Sentry UI | On project rotate |
| `TELEGRAM_BOT_TOKEN_CRITICAL` / `_OPS` | Alertmanager Telegram receivers (separate bots for page vs ops) | BotFather | On demand |
| `PAGERDUTY_SERVICE_KEY` | Optional Alertmanager PagerDuty receiver | PagerDuty UI | On service rotate |

## Security Best Practices

### API Keys

- **NEVER** commit API keys to the repository
- Use environment variables: `MM_BINANCE_API_KEY`, `MM_BINANCE_API_SECRET`,
  `MM_BYBIT_API_KEY`, `MM_BYBIT_API_SECRET`, `MM_HL_PRIVATE_KEY`
  (venue-scoped — the engine picks the right pair from
  `config.exchange.exchange_type`)
- Use **trade-only** keys — withdrawals disabled at the exchange
- Enable **IP whitelisting** on every exchange API key
- Rotate keys every **30–90 days** (see runbooks below)

### Deployment

- Run as non-root (`runAsUser: 1000` in the Docker image + Helm chart)
- TLS for every exchange connection (`https://` / `wss://`)
- Dashboard port restricted: firewall, VPN, or `127.0.0.1` + TLS reverse proxy
- Monitor the audit trail (`data/audit.jsonl`) for anomalies —
  `mm_risk::audit::verify_chain` runs on every boot; operator
  can re-check any time with `cargo run -p mm-server --bin mm-verify-audit`
- Alertmanager **critical** channel fires on kill-switch L3+ /
  archive broken / atomic rollbacks — keep on-call wired

### Configuration

- `config/default.toml` MUST NOT contain secrets — enforced by
  the secrets inventory and reviewer discipline
- Production: external secret backing store (External Secrets
  Operator, SealedSecrets, SOPS+KSOPS, Vault Agent) materialises
  a K8s Secret; Helm references it via `secret.existingSecretName`
- Dev: inline `secret.create: true` in `values-dev.yaml` is OK
  (values end up in `helm history` / etcd — not prod-safe)
- Validate config at startup (server hard-fails on live mode
  with placeholder `MM_AUTH_SECRET`)

---

## Rotation Runbooks

All rotations assume a Kubernetes deployment via the Helm chart
in `deploy/helm/mm/`. Docker-compose and bare-systemd paths are
noted inline where the steps diverge.

### Exchange API key (Binance / Bybit / HyperLiquid)

**When**: scheduled 30–90 d cadence, immediately on operator
offboarding, immediately on any suspected compromise.

**Steps**:

1. Generate a new key pair at the exchange UI. Apply the same
   restrictions as the outgoing pair: trade-only, IP-whitelisted,
   no withdrawals. **Do not delete the old pair yet.**
2. Write the new key+secret into your secret store
   (`mm-prod-secrets` for the Helm default):

   ```bash
   # External Secrets Operator flow — refresh from source truth
   kubectl -n external-secrets annotate externalsecret mm-prod-secrets \
     force-sync=$(date +%s) --overwrite

   # Raw Secret edit (emergency only — breaks GitOps):
   kubectl -n mm create secret generic mm-prod-secrets \
     --from-literal=MM_BINANCE_API_KEY=<new-key> \
     --from-literal=MM_BINANCE_API_SECRET=<new-secret> \
     --dry-run=client -o yaml | kubectl apply -f -
   ```
3. Restart the pod to pick up the new env. The Helm template's
   `checksum/secret` annotation triggers this automatically for
   the next `helm upgrade`, but a force-reload works:

   ```bash
   kubectl -n mm rollout restart deployment/mm-prod
   kubectl -n mm rollout status deployment/mm-prod
   ```
4. Confirm the engine authenticated: dashboard shows
   `venue_ready=true`, `/api/v1/venues/status` reports no
   `auth_rejected` errors, SLA counter ticks.
5. **Only after** step 4 is green, delete the old key pair at
   the exchange UI.
6. Audit: `grep "API key loaded" logs/mm.log` should now only
   reference the new key in the Secret's annotation stamp.

**Rollback**: if the new pair misbehaves, revert the Secret to
the old values and `rollout restart`. The old key is still
valid until step 5.

### `MM_AUTH_SECRET`

**Blast radius**: rotating invalidates **every** outstanding
dashboard session token. Every operator re-logs in. Running
engines do NOT restart on this rotate — only the dashboard
session layer cares.

**When**: 30-day cadence, immediately on operator offboarding.

**Steps**:

1. Generate a new 32-byte random value:
   ```bash
   openssl rand -base64 48
   ```
2. Update the Secret (same flow as exchange keys above).
3. `kubectl -n mm rollout restart deployment/mm-prod` — the
   server reads `MM_AUTH_SECRET` at boot into `AuthState::new`.
4. Every operator opens the dashboard, re-logs in. No other
   action required — tokens are stateless HMAC, so the rotate
   transparently cuts every pre-rotate token.

**Rollback**: revert the Secret and rollout-restart. Pre-rotate
tokens become valid again.

### `MM_CHECKPOINT_SECRET`

**Blast radius**: the on-disk checkpoint is HMAC-signed with
this secret. Rotating it means **existing checkpoints become
unreadable** — the engine cold-starts (empty inventory / PnL
baseline).

**When**: 180-day cadence, on suspected disk compromise. Schedule
during a flat-inventory window.

**Steps**:

1. Walk the pair to flat inventory (operator action — widen
   spreads or manual unwind).
2. Confirm `mm_inventory` metric reads zero for all symbols.
3. Stop the engine:
   ```bash
   kubectl -n mm scale deployment/mm-prod --replicas=0
   ```
4. Delete the existing checkpoint file on the PVC so the engine
   can't attempt to restore with the new secret:
   ```bash
   kubectl -n mm run cleanup --rm -it --image=busybox \
     --overrides='{"spec":{"containers":[{"name":"cleanup","image":"busybox","command":["sh"],"stdin":true,"tty":true,"volumeMounts":[{"name":"data","mountPath":"/data"}]}],"volumes":[{"name":"data","persistentVolumeClaim":{"claimName":"mm-prod-data"}}]}}'
   # inside: rm /data/checkpoint.json
   ```
5. Rotate the Secret (same flow as above).
6. Scale back: `kubectl -n mm scale deployment/mm-prod --replicas=1`.
7. Monitor the first 10 min — engine logs should show
   `no checkpoint available for symbol` (not a restore failure).

**Rollback**: revert the Secret AND restore the deleted
`checkpoint.json` from the most recent PVC snapshot. This is
why step 3 happens during a flat-inventory window — if you
can't roll back, the worst case is a cold start, not inconsistent
state.

### Telegram bot tokens

**When**: on demand (compromise, bot re-created), or operator
turnover.

**Steps**:

1. Generate a new token via `@BotFather`. You can re-use the
   existing bot or create a new one per severity tier (the
   Alertmanager config already supports separate `_CRITICAL`
   and `_OPS` tokens).
2. Update the Secret keys:
   - `MM_TELEGRAM_TOKEN` + `MM_TELEGRAM_CHAT` for the engine's
     direct alert hook
   - `TELEGRAM_BOT_TOKEN_CRITICAL` / `TELEGRAM_CHAT_ID_CRITICAL`
     + `_OPS` variants for Alertmanager
3. Rollout-restart both the engine and Alertmanager:
   ```bash
   kubectl -n mm rollout restart deployment/mm-prod
   # Alertmanager container reads env via entrypoint envsubst,
   # so a restart is enough.
   docker compose restart alertmanager   # compose stack
   ```
4. Smoke: trigger a WidenSpreads from the ops panel; the
   `_OPS` channel should receive the notification within 30s.

### `MM_SENTRY_DSN`

**When**: on Sentry project rotate or operator turnover.

1. Create a new DSN in the Sentry project (or rotate the
   existing one — Sentry UI supports this without losing
   historical issues).
2. Update the Secret, rollout-restart the engine.
3. On boot stderr should show `Sentry enabled (MM_SENTRY_DSN set)`.
4. Trigger a deliberate error (malformed graph POST — see
   `docs/guides/obs-sanity.md` § Part A) and confirm the event
   lands in the Sentry project within 30s.

---

## Operator offboarding

When an operator leaves the team, rotate these **within 24h**:

- [ ] `MM_AUTH_SECRET` — kills every dashboard session
- [ ] Any exchange API key the departing operator issued
- [ ] Telegram bot tokens if the operator had BotFather access
- [ ] Their entry under `[[users]]` in the ConfigMap / config TOML
- [ ] Their SSH / kubeconfig access to the cluster

Log the offboarding as an audit event:

```bash
# From an admin session that still has a live token:
curl -X POST https://mm.internal/api/admin/audit/manual \
  -H "Authorization: Bearer $MM_ADMIN_TOKEN" \
  -d '{"reason":"operator-offboarding","user":"alice","rotated":["MM_AUTH_SECRET","binance-trade-key"]}'
```

---

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.4.x   | Yes       |
| < 0.4   | No — upgrade (breaking changes in secret layout) |

## Known Limitations

- Paper trading mode simulates fills via `ProbabilisticFiller`
  (`[paper_fill]` config). Slippage + latency are modelled but
  any queue-position effect beyond the backtester parity is
  advisory — real live runs against a testnet are the
  authoritative reference.
- Backtester fill models are approximations — real fill rates
  depend on queue position and the venue's matching priority
  (time / pro-rata).
- The reconciliation system queries open orders periodically
  (default 60s) but may miss rapid state changes between cycles —
  fills arriving via listen-key / user-stream close the window
  to sub-second, but the cycle is the authoritative source.
- Kill-switch L5 Disconnect requires a manual reset
  (`ManualKillSwitchReset` via `/api/v1/ops/reset/{symbol}`) —
  deliberate: automated recovery from a disconnect is a
  foot-gun when the cause is adversarial.
