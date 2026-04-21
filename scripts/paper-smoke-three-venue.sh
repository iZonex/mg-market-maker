#!/usr/bin/env bash
#
# Three-venue paper smoke launcher.
#
# Starts three engine instances side-by-side — one per venue
# surface — each in `MM_MODE=paper` so no real orders ever hit
# the wire. Observes a market-making triangle: Binance spot +
# Binance USDM perp + Bybit linear perp, all on BTCUSDT.
#
# Dashboards land on ports 9090 / 9091 / 9092 — open all three
# in separate tabs. Logs stream into `logs/paper-*.log` so you
# can tail them individually without console cross-talk.
#
# Usage:
#   # From the repo root
#   ./scripts/paper-smoke-three-venue.sh
#
#   # Stop: Ctrl+C — the trap cancels orders + flushes the
#   # checkpoint on every engine before exit.
#
# Binaries:
#   cargo build --release --bin mm-server   # run first
#
# Credentials:
#   Paper mode needs no real API keys, but Binance futures +
#   Bybit sign every REST request. The WS feed on all three
#   is public. Set these env vars if you have keys, or leave
#   unset for public-data-only behaviour:
#     MM_BINANCE_API_KEY / MM_BINANCE_API_SECRET
#     MM_BYBIT_API_KEY   / MM_BYBIT_API_SECRET
#
# Set explicitly to avoid the venue-scoped env picker silently
# using the wrong pair when multiple envs are populated.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BIN="${MM_BIN:-$ROOT_DIR/target/release/mm-server}"
if [ ! -x "$BIN" ]; then
  echo "mm-server binary not found at $BIN"
  echo "build it first: cargo build --release --bin mm-server"
  exit 1
fi

mkdir -p logs

declare -A CONFIGS=(
  ["binance-spot"]="config/binance-paper.toml"
  ["binance-perp"]="config/binance-perp-paper.toml"
  ["bybit-perp"]="config/bybit-perp-paper.toml"
)

declare -a PIDS=()

# Trap signals so Ctrl-C cleanly stops every child. The engine's
# graceful-shutdown path cancels all orders and flushes the
# checkpoint on SIGTERM, so no orphaned state on Ctrl-C.
cleanup() {
  echo ""
  echo "[stopping] sending SIGTERM to ${#PIDS[@]} paper engine(s)..."
  for pid in "${PIDS[@]}"; do
    # `kill -0` probes the process without signalling — only
    # SIGTERM pids that are still alive to avoid a cascade of
    # "No such process" stderr lines on shutdown.
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
    fi
  done
  # Give each engine up to 45s to cancel its orders + flush
  # (matches the Helm chart's terminationGracePeriodSeconds).
  local deadline=$((SECONDS + 45))
  for pid in "${PIDS[@]}"; do
    while kill -0 "$pid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do
      sleep 1
    done
  done
  # Escalate any stragglers.
  for pid in "${PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      echo "[stopping] pid $pid still alive after 45s — SIGKILL"
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
  echo "[stopped] all engines exited"
}
trap cleanup EXIT INT TERM

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  3-venue paper smoke — Binance spot / Binance perp / Bybit perp"
echo "  Binary: $BIN"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

for name in binance-spot binance-perp bybit-perp; do
  cfg="${CONFIGS[$name]}"
  log="logs/paper-${name}.log"
  if [ ! -f "$cfg" ]; then
    echo "config missing: $cfg"
    exit 1
  fi
  echo "[start] $name  cfg=$cfg  log=$log"
  MM_CONFIG="$cfg" MM_MODE=paper RUST_LOG="${RUST_LOG:-info,mm_engine=info}" \
    "$BIN" > "$log" 2>&1 &
  PIDS+=("$!")
  # Stagger by 2s so the engines don't race each other for the
  # same file-lock on data/ during first-run bootstrap.
  sleep 2
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  pids: ${PIDS[*]}"
echo "  dashboards:"
echo "    Binance spot → http://127.0.0.1:9090"
echo "    Binance perp → http://127.0.0.1:9091"
echo "    Bybit perp   → http://127.0.0.1:9092"
echo ""
echo "  live logs:"
echo "    tail -f logs/paper-binance-spot.log"
echo "    tail -f logs/paper-binance-perp.log"
echo "    tail -f logs/paper-bybit-perp.log"
echo ""
echo "  Ctrl+C to stop — graceful shutdown cancels all orders."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Block until any child exits unexpectedly. `wait -n` returns
# when the first child dies; the trap handles cleanup of the
# survivors.
wait -n
echo "[crash] one engine exited unexpectedly — stopping the rest"
exit 1
