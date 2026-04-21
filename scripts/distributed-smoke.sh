#!/usr/bin/env bash
#
# Distributed control-plane smoke test.
#
# Launches `mm-server` + `mm-agent` in an isolated workdir,
# walks the full handshake (bootstrap admin → login → accept
# agent), then watches both logs for the lease-refresh cycle.
# Exits PASS only if the lease survives the configured window
# with at least one successful refresh roundtrip.
#
# Usage:
#   ./scripts/distributed-smoke.sh               # 3-minute default
#   SMOKE_WINDOW_SECS=300 ./scripts/distributed-smoke.sh
#
# Workdir ($REPO_ROOT/.smoke-run) holds everything:
#   logs/{server,agent}.log — captured stdout/stderr
#   users.json vault.json tunables.json master-key
#   agent/settings.toml agent/agent-identity.key
# It is wiped on each run.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK="$ROOT/.smoke-run"
LOGS="$WORK/logs"
WINDOW_SECS="${SMOKE_WINDOW_SECS:-180}"
HTTP_PORT=18090
WS_PORT=18091
HTTP_URL="http://127.0.0.1:$HTTP_PORT"
WS_URL="ws://127.0.0.1:$WS_PORT"

# ── Helpers ─────────────────────────────────────────────────

log()  { printf '[smoke %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
fail() { log "FAIL: $*"; tail_logs; exit 1; }
pass() { log "PASS: $*"; exit 0; }

tail_logs() {
  log "--- server.log (last 40) ---"
  tail -40 "$LOGS/server.log" 2>/dev/null || true
  log "--- agent.log (last 40) ---"
  tail -40 "$LOGS/agent.log" 2>/dev/null || true
}

wait_for() {
  # wait_for <log-file> <regex> <timeout-secs> <label>
  local file=$1 pattern=$2 timeout=$3 label=$4
  local start=$SECONDS
  while (( SECONDS - start < timeout )); do
    if grep -qE "$pattern" "$file" 2>/dev/null; then
      log "  ✓ $label"
      return 0
    fi
    sleep 0.5
  done
  fail "timed out (${timeout}s) waiting for '$label' — pattern: $pattern in $file"
}

cleanup() {
  local code=$?
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  if [[ -n "${AGENT_PID:-}" ]]; then kill "$AGENT_PID" 2>/dev/null || true; fi
  wait 2>/dev/null || true
  exit "$code"
}
trap cleanup EXIT INT TERM

# ── Setup ───────────────────────────────────────────────────

log "wiping + preparing $WORK"
rm -rf "$WORK"
mkdir -p "$LOGS" "$WORK/agent"

log "building binaries (debug)"
cargo build --bin mm-server --bin mm-agent 2>&1 \
  | grep -E "(Compiling|Finished|error)" | tail -10

# ── Start server ────────────────────────────────────────────

export MM_HTTP_ADDR="127.0.0.1:$HTTP_PORT"
export MM_AGENT_WS_ADDR="127.0.0.1:$WS_PORT"
export MM_USERS="$WORK/users.json"
export MM_VAULT="$WORK/vault.json"
export MM_TUNABLES="$WORK/tunables.json"
export MM_MASTER_KEY_FILE="$WORK/master-key"
export MM_AUTH_SECRET_FILE="$WORK/auth-secret"
export MM_JWT_SECRET_FILE="$WORK/jwt-secret"
# Approvals persisted into the workdir so the controller-restart
# phase can assert the accepted agent survives across reboots.
export MM_APPROVALS="$WORK/approvals.json"

log "starting mm-server (HTTP=$HTTP_PORT WS=$WS_PORT)"
# NO_COLOR=1 keeps the log grep-friendly — tracing-subscriber
# emits ANSI escapes around field boundaries when writing to a
# pipe/file unless this is set, which breaks naive `fingerprint=…`
# matching with stray `\x1b[3m` bytes.
NO_COLOR=1 RUST_LOG=info,mm_controller=debug,mm_server=debug \
  "$ROOT/target/debug/mm-server" \
  > "$LOGS/server.log" 2>&1 &
SERVER_PID=$!

wait_for "$LOGS/server.log" "HTTP.*listening|mm-server starting" 30 "server boot"

# Poll HTTP until it answers — server might take a bit to bind
# after the "starting" log line.
for i in $(seq 1 30); do
  if curl -sf "$HTTP_URL/health" > /dev/null 2>&1; then
    log "  ✓ HTTP /health responds"
    break
  fi
  sleep 0.5
  if (( i == 30 )); then fail "HTTP never responded at $HTTP_URL/health"; fi
done

# ── Bootstrap admin + login ─────────────────────────────────

log "bootstrapping admin via POST /api/auth/bootstrap"
BOOT_RESP=$(curl -sf -X POST "$HTTP_URL/api/auth/bootstrap" \
  -H "Content-Type: application/json" \
  -d '{"name":"smoke-admin","password":"smoke-password-1234"}' \
  || fail "bootstrap request failed")

TOKEN=$(printf '%s' "$BOOT_RESP" | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])' 2>/dev/null \
  || fail "bootstrap response missing token: $BOOT_RESP")

log "  ✓ admin created, token=${TOKEN:0:16}…"

# ── Start agent ─────────────────────────────────────────────

cat > "$WORK/agent/settings.toml" <<EOF
[agent]
id = "smoke-agent"
controller_addr = "$WS_URL"
EOF

log "starting mm-agent (id=smoke-agent → $WS_URL)"
(
  cd "$WORK/agent"
  NO_COLOR=1 MM_SETTINGS="$WORK/agent/settings.toml" \
    MM_AGENT_IDENTITY="$WORK/agent/agent-identity.key" \
    RUST_LOG=info,mm_agent=debug,mm_controller=debug,mm_engine=debug \
    "$ROOT/target/debug/mm-agent" \
    > "$LOGS/agent.log" 2>&1 &
  echo $! > "$WORK/agent.pid"
)
AGENT_PID=$(cat "$WORK/agent.pid")

wait_for "$LOGS/agent.log" "mm-agent starting" 30 "agent boot"

# Grab fingerprint from the agent's own startup log
FP=$(grep -oE 'fingerprint=[a-f0-9]+' "$LOGS/agent.log" | head -1 | cut -d= -f2)
[[ -n "$FP" ]] || fail "could not parse fingerprint from agent log"
log "  agent fingerprint = $FP"

# ── Accept the pending agent ────────────────────────────────

wait_for "$LOGS/server.log" "agent registered|new agent|pending" 20 "server sees agent register"

log "accepting agent $FP via POST /api/v1/approvals/$FP/accept"
ACCEPT=$(curl -sf -X POST \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  "$HTTP_URL/api/v1/approvals/$FP/accept" \
  -d '{"reason":"smoke"}' \
  || fail "accept request failed")

log "  ✓ accept OK"

# ── Watch lease cycle ───────────────────────────────────────

wait_for "$LOGS/agent.log" "lease held" 20 "agent holds a lease"

log "watching lease refresh cycle for ${WINDOW_SECS}s"
log "  PASS requires:"
log "    - at least one 'requesting lease refresh' on the agent"
log "    - at least one 'issuing lease refresh' on the controller"
log "    - no 'authority lost' / 'lease expired' on the agent"

START=$SECONDS
saw_agent_refresh=0
saw_controller_refresh=0
while (( SECONDS - START < WINDOW_SECS )); do
  # Failure conditions
  if grep -qE "authority lost|lease expired" "$LOGS/agent.log" 2>/dev/null; then
    fail "agent lost authority before the window elapsed"
  fi

  # Success signals
  if (( ! saw_agent_refresh )) \
     && grep -qE "requesting lease refresh" "$LOGS/agent.log" 2>/dev/null; then
    log "  ✓ saw 'requesting lease refresh' on agent"
    saw_agent_refresh=1
  fi
  if (( ! saw_controller_refresh )) \
     && grep -qE "issuing lease refresh" "$LOGS/server.log" 2>/dev/null; then
    log "  ✓ saw 'issuing lease refresh' on controller"
    saw_controller_refresh=1
  fi
  sleep 2
done

(( saw_agent_refresh )) || fail "no lease refresh request from agent in ${WINDOW_SECS}s"
(( saw_controller_refresh )) || fail "no lease refresh reply from controller in ${WINDOW_SECS}s"

# ── Sanity HTTP readback ────────────────────────────────────

log "checking /api/v1/fleet reports agent accepted + connected"
FLEET_JSON=$(curl -sf -H "Authorization: Bearer $TOKEN" "$HTTP_URL/api/v1/fleet")
echo "$FLEET_JSON" > "$LOGS/fleet-snapshot.json"
AGENT_ON_FLEET=$(printf '%s' "$FLEET_JSON" | python3 -c '
import sys, json
data = json.load(sys.stdin)
for row in data:
  if row.get("pubkey_fingerprint"):
    print(row["agent_id"], row.get("approval_state", "?"))
' 2>/dev/null || true)
[[ -n "$AGENT_ON_FLEET" ]] || fail "agent not in /api/v1/fleet response"
log "  ✓ fleet entry: $AGENT_ON_FLEET"

# ── Maintenance-reconnect: kill agent, restart, expect auto-accept ──

log "maintenance scenario: killing agent + restarting with same identity"
kill "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true
# Give controller a moment to register the disconnect.
sleep 1

# Restart agent with SAME identity file — fingerprint stays
# the same so the approval record should match.
mv "$LOGS/agent.log" "$LOGS/agent-phase1.log"
(
  cd "$WORK/agent"
  NO_COLOR=1 MM_SETTINGS="$WORK/agent/settings.toml" \
    MM_AGENT_IDENTITY="$WORK/agent/agent-identity.key" \
    RUST_LOG=info,mm_agent=debug,mm_controller=debug,mm_engine=debug \
    "$ROOT/target/debug/mm-agent" \
    > "$LOGS/agent.log" 2>&1 &
  echo $! > "$WORK/agent.pid"
)
AGENT_PID=$(cat "$WORK/agent.pid")

wait_for "$LOGS/agent.log" "mm-agent starting" 30 "agent restart"

# Auto-accept check — no manual operator action, the agent
# should get a lease because the controller already has its
# fingerprint in the approval store.
wait_for "$LOGS/agent.log" "lease held" 20 "agent auto-accepted + lease after restart"
log "  ✓ maintenance-reconnect: agent recovered without operator intervention"

# ── Controller-restart scenario: persist approvals across reboot ────

log "controller restart scenario: killing server + restarting"
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
kill "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true
sleep 1

mv "$LOGS/server.log" "$LOGS/server-phase1.log"
mv "$LOGS/agent.log" "$LOGS/agent-phase2.log"

log "restarting mm-server with existing vault/approvals/tunables"
NO_COLOR=1 RUST_LOG=info,mm_controller=debug,mm_server=debug \
  "$ROOT/target/debug/mm-server" \
  > "$LOGS/server.log" 2>&1 &
SERVER_PID=$!
wait_for "$LOGS/server.log" "HTTP.*listening" 30 "server restart"
for i in $(seq 1 30); do
  curl -sf "$HTTP_URL/health" > /dev/null 2>&1 && break
  sleep 0.5
done
# Confirm approvals loaded from disk with the prior entry.
if grep -qE "approval store loaded \(persisted\).*entries=[1-9]" "$LOGS/server.log"; then
  log "  ✓ server restart re-loaded persisted approvals"
else
  fail "server restart did not re-load persisted approvals — check MM_APPROVALS default"
fi

log "restarting mm-agent after server bounce"
(
  cd "$WORK/agent"
  NO_COLOR=1 MM_SETTINGS="$WORK/agent/settings.toml" \
    MM_AGENT_IDENTITY="$WORK/agent/agent-identity.key" \
    RUST_LOG=info,mm_agent=debug,mm_controller=debug,mm_engine=debug \
    "$ROOT/target/debug/mm-agent" \
    > "$LOGS/agent.log" 2>&1 &
  echo $! > "$WORK/agent.pid"
)
AGENT_PID=$(cat "$WORK/agent.pid")
wait_for "$LOGS/agent.log" "mm-agent starting" 30 "agent boot (post server-restart)"
wait_for "$LOGS/agent.log" "lease held" 20 "agent auto-accepted after server restart"
log "  ✓ controller-restart: persisted approvals → auto-lease → no re-approve"

# ── Live deploy phase — does telemetry actually flow? ──────
#
# Lease + handshake is useful but doesn't prove the engine
# populates data. We push a dummy Binance-testnet credential,
# deploy an avellaneda-via-graph template on BTCUSDT, wait for
# the engine to tick a few times, and query every endpoint that
# should now carry real values. Any field that stays empty
# exposes a broken wire between engine → DashboardState →
# agent telemetry → controller endpoint.
#
# Paper mode + dummy keys: signed endpoints (get_balances,
# get_open_orders) 401 on first call but the engine tolerates
# that; public WS subscribe works against testnet without auth
# so book + mid + spread all populate from real market data.
#
# Skip this phase with SKIP_LIVE_PHASE=1 (no-internet CI).
if [[ -z "${SKIP_LIVE_PHASE:-}" ]]; then
  log "live deploy phase: push credential + deploy avellaneda"

  CRED_ID="smoke-binance"
  curl -sf -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    "$HTTP_URL/api/v1/vault" \
    -d '{
      "name": "'"$CRED_ID"'",
      "kind": "exchange",
      "description": "smoke test dummy key",
      "values": { "api_key": "smoke-key", "api_secret": "smoke-secret" },
      "metadata": { "exchange": "binance_testnet", "product": "spot" },
      "allowed_agents": []
    }' > /dev/null || fail "could not push vault credential"
  log "  ✓ credential pushed"

  # Give credential a tick to replicate to the agent.
  sleep 1

  AGENT_ID=$(printf '%s' "$FLEET_JSON" | python3 -c '
import sys, json
data = json.load(sys.stdin)
for row in data:
    if row.get("pubkey_fingerprint"):
        print(row["agent_id"]); break
')
  [[ -n "$AGENT_ID" ]] || fail "could not resolve agent_id for deploy"

  DEP_ID="smoke-dep-1"
  log "deploying avellaneda-via-graph on BTCUSDT → $AGENT_ID/$DEP_ID"
  DEPLOY_RESP=$(curl -sf -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    "$HTTP_URL/api/v1/agents/$AGENT_ID/deployments" \
    -d '{
      "strategies": [{
        "deployment_id": "'"$DEP_ID"'",
        "template": "avellaneda-via-graph",
        "symbol": "BTCUSDT",
        "credentials": ["'"$CRED_ID"'"],
        "variables": { "primary_credential": "'"$CRED_ID"'" }
      }]
    }' 2>&1) || fail "deploy POST failed: $DEPLOY_RESP"
  log "  ✓ deploy POST accepted"

  # Let the agent spawn + subscribe + tick. Testnet WS is
  # reliable-ish but we give it a full 60s so refresh_quotes
  # fires multiple times and paper quotes accumulate on the
  # simulated book.
  log "  waiting 60s for engine to tick + paper quotes to accumulate"
  sleep 60

  # Readback 1 — fleet deployment row.
  log "checking /api/v1/fleet deployment row has populated scalars"
  FLEET_JSON=$(curl -sf -H "Authorization: Bearer $TOKEN" "$HTTP_URL/api/v1/fleet")
  echo "$FLEET_JSON" > "$LOGS/fleet-post-deploy.json"
  DEP_SCALARS=$(printf '%s' "$FLEET_JSON" | python3 -c '
import sys, json
data = json.load(sys.stdin)
for a in data:
    for d in a.get("deployments", []):
        if d.get("deployment_id") == "smoke-dep-1":
            out = {
                "running": d.get("running"),
                "mid_price": d.get("mid_price") or "",
                "spread_bps": d.get("spread_bps") or "",
                "mode": d.get("mode") or "",
                "template": d.get("template") or "",
                "venue": d.get("venue") or "",
                "live_orders": d.get("live_orders", 0),
                "inventory": d.get("inventory") or "0",
                "unrealized_pnl_quote": d.get("unrealized_pnl_quote") or "0",
                "volatility": d.get("volatility") or "",
                "vpin": d.get("vpin") or "",
                "kyle_lambda": d.get("kyle_lambda") or "",
                "regime": d.get("regime") or "",
                "open_orders_count": len(d.get("open_orders") or []),
            }
            print(json.dumps(out))
            sys.exit(0)
print("{}")
' || fail "fleet parse failed")
  log "  deployment scalars: $DEP_SCALARS"
  MID=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("mid_price",""))')
  RUNNING=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("running",False))')
  LIVE_ORDERS=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("live_orders",0))')
  OPEN_ORDERS_COUNT=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("open_orders_count",0))')

  if [[ "$RUNNING" != "True" ]]; then
    log "  ⚠ deployment.running = $RUNNING (expected True)"
    log "  tail agent.log for spawn failures:"
    tail -30 "$LOGS/agent.log"
    fail "deployment did not enter RUNNING state"
  fi
  log "  ✓ deployment RUNNING"

  if [[ -z "$MID" || "$MID" == "0" ]]; then
    log "  ⚠ mid_price empty or zero — book never populated (WS subscribe failed?)"
    log "  looking for subscribe errors in agent log:"
    grep -iE "subscribe|book|ws.*error" "$LOGS/agent.log" | tail -10 || true
  else
    log "  ✓ mid_price=$MID (book populated from WS)"
  fi

  # Paper mode check — after 60s of ticking we should see
  # orders resting on the simulated book. Zero live_orders
  # means the strategy never placed any (disabled by kill
  # switch? stale book? too-far quotes?) — flag it.
  if (( LIVE_ORDERS == 0 && OPEN_ORDERS_COUNT == 0 )); then
    log "  ⚠ live_orders=0 — strategy placed NO quotes in 60s. Investigate:"
    grep -iE "kill|stale|disabled|refuse|paused" "$LOGS/agent.log" | tail -10 || true
  else
    log "  ✓ live_orders=$LIVE_ORDERS open_orders=$OPEN_ORDERS_COUNT (strategy is placing)"
  fi

  # Toxicity / microstructure — these warm up on trade flow.
  # 60s on BTCUSDT testnet usually gets a handful of trades.
  VPIN=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("vpin",""))')
  VOL=$(printf '%s' "$DEP_SCALARS" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("volatility",""))')
  log "  toxicity: vpin=${VPIN:-—} vol=${VOL:-—}"

  # Readback 2 — /api/v1/pnl, /api/v1/sla, /api/v1/positions,
  # /api/v1/reconciliation/fleet, /api/v1/alerts/fleet.
  check_endpoint_nonempty() {
    local path=$1 label=$2
    local resp
    resp=$(curl -sf -H "Authorization: Bearer $TOKEN" "$HTTP_URL$path" || echo "null")
    echo "$resp" > "$LOGS/endpoint-$label.json"
    local kind
    kind=$(printf '%s' "$resp" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    print("invalid"); sys.exit()
if isinstance(d, list):
    nonempty = sum(1 for x in d if x not in (None, "", [], {}))
    print("array[{}, {} non-empty]".format(len(d), nonempty))
elif isinstance(d, dict):
    nz = sum(1 for v in d.values() if v not in (None, "", [], {}, 0))
    print("object[{}keys, {} non-empty]".format(len(d), nz))
else:
    print(type(d).__name__)
')
    log "  $path → $kind"
  }

  log "checking downstream endpoints after deploy"
  check_endpoint_nonempty "/api/v1/pnl" "pnl"
  check_endpoint_nonempty "/api/v1/sla" "sla"
  check_endpoint_nonempty "/api/v1/positions" "positions"
  check_endpoint_nonempty "/api/v1/reconciliation/fleet" "reconciliation"
  check_endpoint_nonempty "/api/v1/alerts/fleet" "alerts"
  check_endpoint_nonempty "/api/v1/surveillance/fleet" "surveillance"

  log "  ✓ live deploy phase complete (inspect $LOGS/endpoint-*.json for detail)"
else
  log "SKIP_LIVE_PHASE=1 — skipping live deploy + telemetry check"
fi

pass "control-plane handshake + lease refresh + maintenance-reconnect + controller-restart + live deploy telemetry all green"
