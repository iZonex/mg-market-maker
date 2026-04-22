#!/usr/bin/env bash
#
# Companion to `stand-up.sh` — kills the server + agent PIDs
# recorded in `.stand-run/stand.env` and removes the workdir.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK="$ROOT/.stand-run"
ENV_FILE="$WORK/stand.env"

log() { printf '[stand %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

if [[ ! -f "$ENV_FILE" ]]; then
  log "no stand.env — nothing to tear down"
  exit 0
fi

# shellcheck disable=SC1090
source "$ENV_FILE"

for pid_var in SERVER_PID AGENT_PID; do
  pid="${!pid_var:-}"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    log "  killing $pid_var=$pid"
    kill "$pid" 2>/dev/null || true
  fi
done

# Give tasks a moment to flush.
sleep 1

# Force-kill any stragglers.
for pid_var in SERVER_PID AGENT_PID; do
  pid="${!pid_var:-}"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    log "  force-killing $pid_var=$pid"
    kill -9 "$pid" 2>/dev/null || true
  fi
done

log "stand torn down"
