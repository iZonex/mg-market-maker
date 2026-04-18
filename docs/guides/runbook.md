# Operations Runbook

Per-failure-mode procedures. For every known incident class: how to
**detect**, what to **do immediately**, how to **recover**, and the
**postmortem template** so the next on-call has a faster time-to-fix.

Runbook is first-read on pager. If the event isn't listed here, add
it after the postmortem completes — undocumented incidents recur.

## Quick reference

| Signal | Likely cause | Go to |
|---|---|---|
| Kill switch L≥3 on any symbol | Circuit breaker tripped OR margin breach | [§2](#2-circuit-breaker-tripped) / [§6](#6-margin-breach--liquidation-approaching) |
| `mm_sla_presence_pct_24h < 95` | SLA violation — venue/engine disconnect | [§3](#3-sla-violation) |
| `WS disconnected` log + `FROZEN` dashboard | WS stream dropped | [§1](#1-websocket-feed-dropped) |
| `clock drift > 2000 ms` preflight fail | NTP / VM clock skew | [§4](#4-clock-skew) |
| Engine panic / OOM / sudden exit | Any — treat as crash | [§7](#7-crash--restart--fill-reconciliation) |
| `mm_order_reject_count` sudden spike | Venue auth failed, rate-limit, or tick-size drift | [§5](#5-venue-auth--rate-limit--tick-size) |
| Maker rebate tier dropped | VIP volume lost, MM-program agreement needs review | [§8](#8-mm-program-rebate-tier-drop) |
| Margin utilisation > 50% on perp | Too much exposure — guard will widen | [§6](#6-margin-breach--liquidation-approaching) |
| Pair break on funding_arb | Taker filled, maker rejected | [§9](#9-funding-arb-pair-break) |

---

## 1. WebSocket feed dropped

### Detect
- Dashboard freshness chip flips to `STALE` (≤5 s) then `FROZEN` (>5 s)
- Log: `WS disconnected, reconnecting in Ns...` repeating
- Prometheus: `mm_ws_connected{venue="..."} == 0`
- SLA tracker starts accumulating one-sided / low-depth strikes

### Immediate action
1. **Do nothing for 10 s.** Connector auto-reconnects. `StaleBook`
   circuit breaker fires after 10 s of no book updates → engine
   automatically cancels and goes L2 StopNewOrders. This is the
   designed behaviour; human intervention during the first tick
   can create order-state divergence.
2. After 30 s of continuous disconnect: check venue status page
   (status.binance.com, bybit-status-page.github.io, etc.).
   Screenshot any venue-side incident banner for the postmortem.
3. Ops Slack: post `🟡 WS DROP ${venue} ${symbol} — engine in L2 auto`
   with venue status link.

### Recover
- **Venue-side outage**: wait; connector reconnects when venue
  returns. Circuit breaker clears itself once 10 consecutive fresh
  book updates arrive.
- **Our-side network**: `curl -I https://api.<venue>.com/time` from
  the engine host. If unreachable: check egress firewall / AWS
  security group. Restart the connector's network path only —
  **do not `pkill mm-server`** unless book has been frozen >2 min
  AND venue status is green. Restart re-reads checkpoint which
  may diverge from venue-side live state; reconciliation cycle
  (60 s after restart) catches drift but introduces ~2 min of
  blind operation.

### Postmortem template
```
Title: WS drop – <venue> – <duration>
When: <start> … <end>
Cause: [venue outage | our egress | DNS | TLS handshake | other]
Impact:
  - SLA uptime delta: <pp>
  - Missed fills: <count> (from fills reconciliation report)
  - PnL impact: $<amount>
Detection delay: <seconds> (from drop to first page)
MTTR: <minutes>
Kill-switch level reached: <0-5>
Follow-up: [ticket links]
```

---

## 2. Circuit breaker tripped

### Detect
- Log: `ERROR mm_risk::circuit_breaker: CIRCUIT BREAKER TRIPPED — reason=<X>`
- Dashboard: Kill level jumps to L2+ without operator action
- Audit: `CircuitBreakerTripped` event with `detail` carrying the reason

### Reason matrix
| `reason=` | What | Go to |
|---|---|---|
| `StaleBook` | No book update for `stale_book_timeout_secs` | §1 |
| `WideSpread` | Venue spread > `max_spread_bps` | §2a |
| `MaxExposure` | Aggregate quote notional > config limit | §2b |
| `MaxDrawdown` | Daily loss exceeded limit | §2c |
| `MaxPositionValue` | Single-symbol position > cap | §2b |

### 2a. Wide-spread trip
Exchange spread blew out — either a known market event (news, funding)
or a venue-side issue. **Engine correctly halted.** Check:
- Log 24 h chart of the affected symbol's spread. Is this normal
  volatility?
- If yes: let it resolve; engine auto-resumes when spread < limit.
- If no: investigate microstructure panel — large print, iceberg,
  maker withdrawal?

### 2b. Exposure / position trip
One of two things:
1. Config is miscalibrated — your `max_exposure_quote` is too tight
   relative to `order_size × num_levels`. Raise limit OR lower
   order_size. Do NOT bypass.
2. Legitimate — we're actually overexposed. Trigger manual flatten:
   `POST /api/v1/ops/flatten/{symbol}` (typed-echo modal). Reset
   kill switch after flattening.

### 2c. Drawdown trip
Daily loss exceeded `kill_switch.daily_loss_limit`. **Do not reset
automatically**. Investigate cause first:
- Was it a single adverse move? Acceptable tail event.
- Repeating losses? Bad calibration / regime change → pause
  quoting, run hyperopt, only then reset.
- Toxic fill sequence? Check VPIN, Kyle λ, adverse selection —
  if all elevated, widen config spreads or swap venue.

### Recover
- Identify and fix root cause (not just the symptom)
- Manual reset: `POST /api/v1/ops/reset/{symbol}` (dashboard button
  or curl with Bearer token)
- Monitor next 30 min at higher attention level

### Postmortem — always required for L3+

---

## 3. SLA violation

### Detect
- Prometheus: `mm_sla_presence_pct_24h{symbol="..."} < 95`
- Dashboard SLA chip: amber (<95%) or red (<90%)
- Audit: `SlaViolation` events accumulating
- Client-side: client reports missed quote obligations

### Immediate action
1. Identify the SLA component that failed:
   - `presence_pct`: we were offline when we should have been quoting
   - `two_sided_pct`: we quoted one-sided
   - `wide_spread_pct`: we quoted but wider than SLA max
   - `low_depth_pct`: we quoted but below min depth
2. Cross-reference with the symbol's event log in `Audit Stream`
3. If <90% and the client pays by SLA: notify client ops
   within the contracted notification window (default 1 h)

### Recover
- SLA is a rolling 24 h metric — time heals it if underlying cause
  resolves.
- If presence is low because engine keeps tripping circuit breaker:
  §2 — fix root cause.
- If low-depth: config `order_size` or `num_levels` too small for
  SLA's `min_depth_quote`. Bump one.
- If wide-spread: config `min_spread_bps` > SLA's `max_spread_bps`.
  These are on a collision course — either renegotiate SLA or
  tighten config.

### Postmortem
```
Title: SLA below <threshold>% on <symbol>
Measurement window: rolling 24 h as of <timestamp>
Client impact: <SLA contract fee penalty / breach notification>
Contributing factors:
  - <circuit breaker trips, count>
  - <WS drops, count + duration>
  - <wide spread ticks, %>
Corrective action: <config bump / SLA renegotiation / venue swap>
Client notified: <yes/no, at <time>>
```

---

## 4. Clock skew

### Detect
- Preflight: `clock_skew` returns `Fail` (>2 s) or `Warn` (>500 ms)
- Log on startup: `local vs venue clock drift N ms (budget ±500 ms)`
- In live: Binance returns `-1021` (`Timestamp for this request is outside of the recvWindow`)

### Immediate action (live)
1. Check host NTP: `chronyc tracking` / `timedatectl status` /
   `ntpq -p`
2. If offset > 2 s: engine will hard-fail on next order. Better to
   restart engine BEFORE next order tick — `pkill mm-server`,
   then sync clock, then restart.

### Recover
- `sudo chronyc makestep` or `sudo ntpdate pool.ntp.org` on
  Linux.
- If containerised: host clock is inherited — fix host, restart
  container.
- In AWS: check instance's PTP/TSC sync — rarely drifts but
  M-class instances can skew under maintenance events.

---

## 5. Venue auth / rate-limit / tick-size

### 5a. Auth rejected
- Log: `AuthRejected` venue error class
- Typical cause: key rotated, IP allowlist changed, read-only key
  used for trading
- Action: re-issue key, update `MM_<VENUE>_API_KEY` env, restart
  engine (auth is loaded at startup only)

### 5b. Rate-limited
- Log: `RateLimit` venue error class; HTTP 418/429 from Binance;
  `retCode=10006` from Bybit
- Our rate limiter (token-bucket, Epic 36.5) should preempt.
  If it didn't: check `mm_rate_limit_remaining{venue=...}` —
  did it saturate?
- Action: back off manually via `min_order_rest_secs` config bump,
  or lower `refresh_interval_ms`.

### 5c. Tick-size drift
- Log: `PairLifecycleTickLotChanged` audit event
- Venue changed tick / lot size mid-trade (rare but happens).
  Engine auto-updates `ProductSpec` (pair_lifecycle module).
- Action: verify our quotes are still post-only compliant. If
  price levels round to venue-tick but post-only got rejected:
  manual `pair_lifecycle_refresh_secs` bump to accelerate
  reconciliation.

---

## 6. Margin breach / liquidation approaching

### Detect
- Log: `margin ratio elevated/high/critical` from kill switch
  `update_margin_ratio` (Epic 40.4)
- Prometheus: `mm_margin_ratio{venue=...}` crossing 0.5 / 0.8 / 0.9
- Venue-side margin-call email

### Immediate action (>80% ratio)
1. Engine has already escalated to L2 StopNewOrders. Do NOT
   reset.
2. Check aggregate delta — we may be over-inventoried. Manual
   flatten the most exposed symbol first.
3. If ratio still climbing: manual reduce-only close via
   `POST /api/v1/ops/flatten/{symbol}` — typed-echo modal
   confirms. Bypass at your risk.

### Recover (<50%)
- Reset kill switch per-symbol after flatten completes
- Raise `margin.widen_ratio` temporarily only if we misestimated
  operational MMR — never above 0.65.

### 6a. Cross-margin contagion
One symbol's loss eating another's margin. **Switch that symbol
to isolated**: set `margin.per_symbol.<SYM>.mode = "isolated"`,
restart. Cross-margin only safe when `hedge_optimizer` active.

---

## 7. Crash / restart / fill reconciliation

### Detect
- `mm-server` exited non-zero, or process killed by OS (OOM,
  orchestrator restart)
- Dashboard URL returns 502/connection-refused

### Immediate action
1. Do NOT auto-restart on crash without reconciliation — checkpoint
   may be stale. Supervisor should run with `restart: on-failure`
   not `always`.
2. Inspect last ~50 log lines: was it a panic, OOM, or clean shutdown?
3. `ls -la data/` — checkpoint present? When was it last flushed?

### Recover
1. `cargo run -p mm-server` (or kube restart)
2. Engine auto-runs:
   - Preflight (aborts startup in live mode if anything fails)
   - Checkpoint restore (opt-in — validate before full restore)
   - Fill replay from `data/audit.jsonl` since last checkpoint
   - Orphaned-order query against venue + auto-cancel anything
     unknown
3. **First 60 s post-restart**: reconciliation cycle runs. Watch
   `InventoryDriftDetected` audit events — small drift is OK
   (rounding / in-flight fills), >1% signals an unaccounted fill.
4. Verify dashboard shows LIVE + non-zero mid + expected inventory
   before considering the restart successful.

### Postmortem — mandatory for every unplanned exit

---

## 8. MM-program rebate tier drop

### Detect
- 7-day volume drops below tier threshold → venue auto-demotes
  effective fee
- `mm_fee_tier_maker_bps{venue=...}` gauge moves toward 0 or
  positive
- PnL attribution `fees_paid` starts > `rebate_income`

### Immediate action
- Check venue's MM program dashboard (Binance Institutional,
  Bybit MMIP) for current tier
- If we're below contracted minimum volume: engage commercial /
  sales to renegotiate or increase symbol coverage

### Recover
- Short-term: stop quoting loss-making pairs where we no longer
  have rebate edge (use the spot-vs-perp matrix in
  `docs/research/spot-vs-perp-mm-apr17.md`)
- Long-term: either scale volume or downscope to spot pairs that
  don't require rebate to be profitable

---

## 9. Funding-arb pair break

### Detect
- Audit: `PairBreak` event with `detail` showing `compensated=<bool>`
- Log: taker filled, maker rejected
- Dashboard: non-zero delta on a symbol expected to be neutral

### Immediate action
1. `compensated=true`: compensating market reversal fired,
   position is flat again. Acceptable. Investigate why maker
   leg rejected (post-only cross? → check venue spread at
   rejection ms).
2. `compensated=false`: **delta-exposed**. Kill switch should
   have escalated to L2. Manually trigger L3 CancelAll on both
   legs if it didn't, then L4 Flatten when safe to eat taker
   slippage.

### Recover
- Reset funding_arb_driver per-pair after investigation
- Tighten `max_slippage_bps` in XemmConfig if compensation is
  firing too often

---

## Escalation matrix

| Severity | Response | Who |
|---|---|---|
| Info (warn, single-symbol, no PnL impact) | Log only | Oncall acknowledges |
| Warn (PnL at risk, SLA in danger) | Slack `#mm-oncall` | Oncall + backup |
| Critical (PnL loss >$1k, multi-symbol, SLA breach imminent) | Page primary oncall | + manager |
| Severe (system-wide outage, regulatory risk, account-wipe near) | Page entire team | + leadership |

## Always record

Every incident must have, within 24 h:
1. A ticket (link in Slack thread)
2. A postmortem using the template in each §
3. A runbook update if the failure class wasn't already documented
4. A preventive follow-up (monitoring, config, code) tracked in
   the main roadmap

## See also

- [Operations guide](operations.md) — daily checklist + config reference
- [Adaptive Calibration](adaptive-calibration.md) — when hyperopt says GO
- [Strategy Catalog](strategy-catalog.md) — troubleshooting per-strategy
- [Spot vs Perp research](../research/spot-vs-perp-mm-apr17.md) — economics
- [Crash Recovery](crash-recovery.md) — detailed restart flow
