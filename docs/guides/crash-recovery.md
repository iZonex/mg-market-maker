# Crash Recovery Runbook

When the engine crashes, hangs, or is killed (OOM, SIGKILL, node
loss), follow this runbook before letting it re-quote. Automated
restart is safe only after the **exchange state matches local
state** — restart without reconciliation can double-book exposure
or leave stranded orders on the venue.

## 0. Freeze

- Verify the engine is actually down (`systemctl status mm.service`
  or `kubectl get pod -n mm`).
- Look at Telegram for the last incident alert. Note the symbol
  and timestamp of the crash.
- If there is ANY ambiguity about whether the process is still
  running, do NOT restart — two engines on the same symbol will
  race for order IDs.

## 1. Snapshot venue truth

On each venue (Binance / Bybit / HyperLiquid), record:

- **Open orders** per symbol (REST: `GET /api/v3/openOrders` on
  Binance, `GET /v5/order/realtime` on Bybit, `info.openOrders`
  on HyperLiquid)
- **Balances** per wallet / asset
- **Positions** (for perps)

Save the raw JSON to `data/incidents/<ISO-date>/venue-<name>.json`.
The audit log references this directory for regulators.

## 2. Inspect local state

- `data/checkpoint.json` — last persisted engine state. The HMAC
  in the envelope must verify against `MM_CHECKPOINT_SECRET`. A
  mismatch means the file was tampered with or signed under a
  different secret (rotate keys? check host compromise).
- `data/audit.jsonl` — last events before the crash. Look for
  `KillSwitchEscalated`, `CircuitBreakerTripped`, `PairBreak`
  within the 60 s leading up to the stop.
- `data/fills.jsonl` — fills the engine observed. Compare against
  venue fill history; any venue fill not in this file is a
  **missed fill** and must be manually applied.

## 3. Reconcile

### Orders
Open orders on the venue but not in `checkpoint.open_order_ids`
are **orphans**. Cancel them manually before restart — the
engine's startup-reconcile path will adopt and cancel them, but
manual cancel is safer when in doubt.

### Positions
Compute net position per symbol from venue truth. Compare with
`checkpoint.symbols[sym].inventory`. Discrepancy:

- **Small (< fee tolerance)**: fee-in-base rounding. Let the
  inventory-drift reconciler absorb it on next reconcile cycle.
- **Large**: missed fill(s). Identify the missing fills, append
  them to the audit log with `OrderFilled`, update checkpoint
  inventory manually (or delete checkpoint and let engine start
  fresh).

### Balances
Internal `BalanceCache` expects venue truth within `max_drift`.
If a withdrawal or transfer happened out-of-band, it's fine — the
first `refresh_balances()` on startup pulls the current state.
Just make sure no `internal_transfer()` was in flight when the
crash happened.

## 4. Choose restart mode

- **Standard restart**: `systemctl start mm.service` or scale the
  Deployment back to 1. Engine loads the checkpoint (rejects if
  HMAC fails), runs `reconcile()` + `cancel_all()` before the
  first quote, then resumes.
- **Fresh start (ignore checkpoint)**: delete or rename
  `data/checkpoint.json`. Use this when the checkpoint is known
  stale (> 5 min old for a fast market) or when you've manually
  flattened on the venue. Engine starts with zero inventory;
  inventory-drift reconciler will surface any residual.
- **Paper mode smoke test**: `MM_MODE=paper` first, confirm the
  strategy ticks and quotes look sane for 5 min, then flip to
  live. Recommended after ANY non-trivial recovery.

## 5. Post-mortem

- Copy the last 30 minutes of logs and the venue snapshots into
  `data/incidents/<ISO>/`.
- File a short incident note (`incident.md` in that dir) with:
  timeline, symptoms, hypothesis, remediation, follow-ups.
- Link from the audit log — this is the MiCA trail regulators
  expect.

## Common failure modes

### "cancel_all left N orders still open"
The engine's shutdown cancel-all reported surviving orders on the
venue. Either the venue was rate-limiting us or a batch cancel
errored out. Manually cancel via the venue's UI or the REST API
before restart.

### "checkpoint HMAC mismatch"
Tampered file or a secret rotation without a corresponding
re-sign. If you rotated `MM_CHECKPOINT_SECRET`: rename the old
checkpoint aside, start fresh, let the first flush re-sign under
the new secret.

### "connector sequence gap flooding"
The `mm_ws_reconnects_total{outcome="backoff_cap"}` gauge is > 0
and sequence-gap warnings are non-stop. Check:
- Network path to the venue
- Account bans / IP allow-lists (contact venue support)
- Local clock skew (NTP)

If sustained, pause quoting (`POST /api/admin/symbols/{sym}/pause`
through the dashboard) while you investigate — the kill switch
will not help if the book stream is unusable.
