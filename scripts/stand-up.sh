#!/usr/bin/env bash
#
# Persistent dev stand for browser + Playwright smoke tests.
#
# Mirrors the boot sequence in `scripts/distributed-smoke.sh`
# (env vars, NO_COLOR log filters, wait patterns) but keeps the
# server + agent running after the initial deploy and writes
# connection state to `.stand-run/stand.env`:
#
#     HTTP_URL=http://127.0.0.1:18092
#     ADMIN_TOKEN=eyJ…
#     AGENT_ID=stand-agent
#     DEPLOYMENT_ID=stand-dep-1
#     SERVER_PID=12345
#     AGENT_PID=12346
#
# Deploys `rug-detector-composite` by default so the graph has
# real sources (`Surveillance.RugScore`) and the UI renders
# non-trivial overlays. Override with STAND_TEMPLATE=<name>.
# Companion `tear-down.sh` reads the env file and kills PIDs.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK="$ROOT/.stand-run"
LOGS="$WORK/logs"
HTTP_PORT="${STAND_HTTP_PORT:-18092}"
WS_PORT="${STAND_WS_PORT:-18093}"
HTTP_URL="http://127.0.0.1:$HTTP_PORT"
WS_URL="ws://127.0.0.1:$WS_PORT"
TEMPLATE="${STAND_TEMPLATE:-rug-detector-composite}"
AGENT_ID="stand-agent"
DEP_ID="stand-dep-1"

log() { printf '[stand %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

wait_for() {
  local file=$1 pattern=$2 timeout=$3 label=$4
  local start=$SECONDS
  while (( SECONDS - start < timeout )); do
    if grep -qE "$pattern" "$file" 2>/dev/null; then
      log "  ✓ $label"
      return 0
    fi
    sleep 0.4
  done
  log "  ✗ timeout waiting for '$label'"
  log "--- $file (last 30) ---"
  tail -30 "$file" >&2 2>/dev/null || true
  exit 1
}

log "preparing $WORK"
rm -rf "$WORK"
mkdir -p "$LOGS" "$WORK/agent"

log "building binaries"
( cd "$ROOT" && cargo build --bin mm-server --bin mm-agent 2>&1 \
    | grep -E "(Compiling|Finished|error)" | tail -5 )

if [[ -z "${STAND_SKIP_FRONTEND:-}" ]]; then
  if [[ ! -d "$ROOT/frontend/node_modules" ]]; then
    log "installing frontend deps (first boot)"
    ( cd "$ROOT/frontend" && npm install --silent )
  fi
  log "building frontend"
  ( cd "$ROOT/frontend" && npm run build 2>&1 | tail -3 )
fi

# ── Server ────────────────────────────────────────────────────
# Env var names are copied verbatim from the working
# distributed-smoke.sh — any drift here bricks the boot.

export MM_HTTP_ADDR="127.0.0.1:$HTTP_PORT"
export MM_AGENT_WS_ADDR="127.0.0.1:$WS_PORT"
export MM_USERS="$WORK/users.json"
export MM_VAULT="$WORK/vault.json"
export MM_TUNABLES="$WORK/tunables.json"
export MM_MASTER_KEY_FILE="$WORK/master-key"
export MM_AUTH_SECRET_FILE="$WORK/auth-secret"
export MM_JWT_SECRET_FILE="$WORK/jwt-secret"
export MM_APPROVALS="$WORK/approvals.json"

log "starting mm-server (HTTP=$HTTP_PORT WS=$WS_PORT)"
NO_COLOR=1 RUST_LOG=info,mm_controller=info,mm_server=info \
  "$ROOT/target/debug/mm-server" \
  > "$LOGS/server.log" 2>&1 &
SERVER_PID=$!

wait_for "$LOGS/server.log" "HTTP.*listening|mm-server starting" 30 "server boot"

for i in $(seq 1 30); do
  if curl -sf "$HTTP_URL/health" > /dev/null 2>&1; then
    log "  ✓ HTTP /health responds"
    break
  fi
  sleep 0.5
  (( i == 30 )) && fail "HTTP never responded at $HTTP_URL/health"
done

# ── Admin bootstrap ───────────────────────────────────────────

log "bootstrapping admin"
BOOT_RESP=$(curl -sf -X POST "$HTTP_URL/api/auth/bootstrap" \
  -H "Content-Type: application/json" \
  -d '{"name":"stand-admin","password":"stand-password-1234"}' \
  || fail "bootstrap request failed")
ADMIN_TOKEN=$(printf '%s' "$BOOT_RESP" | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])' \
  || fail "bootstrap response missing token: $BOOT_RESP")
log "  ✓ admin token issued"

# ── Agent ─────────────────────────────────────────────────────

cat > "$WORK/agent/settings.toml" <<EOF
[agent]
id = "$AGENT_ID"
controller_addr = "$WS_URL"
EOF

log "starting mm-agent (id=$AGENT_ID → $WS_URL)"
(
  cd "$WORK/agent"
  NO_COLOR=1 MM_SETTINGS="$WORK/agent/settings.toml" \
    MM_AGENT_IDENTITY="$WORK/agent/agent-identity.key" \
    RUST_LOG=info,mm_agent=info,mm_controller=info,mm_engine=info \
    "$ROOT/target/debug/mm-agent" \
    > "$LOGS/agent.log" 2>&1 &
  echo $! > "$WORK/agent.pid"
)
AGENT_PID=$(cat "$WORK/agent.pid")

wait_for "$LOGS/agent.log" "mm-agent starting" 30 "agent boot"

FP=$(grep -oE 'fingerprint=[a-f0-9]+' "$LOGS/agent.log" | head -1 | cut -d= -f2)
[[ -n "$FP" ]] || fail "could not parse fingerprint"
log "  agent fingerprint = $FP"

wait_for "$LOGS/server.log" "agent registered|new agent|pending" 20 "server sees agent"

log "accepting agent"
curl -sf -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  "$HTTP_URL/api/v1/approvals/$FP/accept" \
  -d '{"reason":"stand-up"}' > /dev/null \
  || fail "accept failed"
log "  ✓ accepted"

wait_for "$LOGS/agent.log" "lease held" 20 "agent lease"

# ── Credential (paper venue) ─────────────────────────────────

CRED_ID="stand-binance"
log "pushing paper credential id=$CRED_ID"
curl -sf -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  "$HTTP_URL/api/v1/vault" \
  -d '{
    "name": "'"$CRED_ID"'",
    "kind": "exchange",
    "description": "stand dev dummy key",
    "values": { "api_key": "stand-paper-key", "api_secret": "stand-paper-secret" },
    "metadata": { "exchange": "binance_testnet", "product": "spot" }
  }' > /dev/null || fail "credential push failed"
log "  ✓ credential stored"

# ── Deploy ────────────────────────────────────────────────────

log "deploying template=$TEMPLATE on BTCUSDT → $AGENT_ID/$DEP_ID"
DEPLOY_RESP=$(curl -sf -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  "$HTTP_URL/api/v1/agents/$AGENT_ID/deployments" \
  -d '{
    "strategies": [{
      "deployment_id": "'"$DEP_ID"'",
      "template": "'"$TEMPLATE"'",
      "symbol": "BTCUSDT",
      "credentials": ["'"$CRED_ID"'"],
      "variables": {"primary_credential": "'"$CRED_ID"'"}
    }]
  }' 2>&1) || fail "deploy POST failed: $DEPLOY_RESP"
log "  ✓ deploy POST accepted"

log "waiting 45s for engine to tick + graph traces to accumulate"
sleep 45

# ── Persist state ─────────────────────────────────────────────

cat > "$WORK/stand.env" <<EOF
HTTP_URL=$HTTP_URL
WS_URL=$WS_URL
ADMIN_TOKEN=$ADMIN_TOKEN
AGENT_ID=$AGENT_ID
DEPLOYMENT_ID=$DEP_ID
TEMPLATE=$TEMPLATE
SERVER_PID=$SERVER_PID
AGENT_PID=$AGENT_PID
EOF

log "stand is LIVE"
log "  URL: $HTTP_URL/?live=$AGENT_ID/$DEP_ID"
log "  admin token (seed into localStorage.mm_auth): $ADMIN_TOKEN"
log "  state file: $WORK/stand.env"
log "  tear down: scripts/tear-down.sh"
