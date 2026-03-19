# Risk Management

## Kill Switch (5 Levels)

The kill switch is an automated emergency system. It can only **escalate** — reset requires manual action.

| Level | Name | Trigger | Action |
|-------|------|---------|--------|
| 0 | Normal | — | Full quoting |
| 1 | Widen Spreads | Daily PnL < -warning | 2x spread, 0.5x size |
| 2 | Stop New Orders | Max position value exceeded, API errors | No new orders, existing expire |
| 3 | Cancel All | Daily PnL < -limit, runaway algo | Cancel all open orders |
| 4 | Flatten All | Escalation from L3 | Cancel all + TWAP to zero inventory |
| 5 | Disconnect | Manual only | Sever all exchange connections |

**Runaway algo detection:** If message rate exceeds `max_message_rate` per second, kill switch escalates to L3. This prevents the Knight Capital scenario ($440M loss from runaway algo).

---

## VPIN (Volume-Synchronized Probability of Informed Trading)

Measures order flow toxicity. High VPIN means informed traders are aggressively taking liquidity.

**How it works:**
1. Accumulate trades into volume buckets (e.g., $50K per bucket)
2. For each bucket, compute |buy_volume - sell_volume| (imbalance)
3. VPIN = sum(imbalances) / sum(volumes) over N buckets

**Range:** [0, 1]. 0 = balanced flow, 1 = completely one-sided.

**Action:** When VPIN > threshold (default 0.7), auto-tuner widens spreads by up to 3x.

---

## Kyle's Lambda (Price Impact)

Estimates how much price moves per unit of signed order flow.

```
λ = Cov(ΔP, OFI) / Var(OFI)
```

High λ = low liquidity or informed trading → widen spreads.

---

## Adverse Selection Tracker

After each fill, tracks where the mid price goes:
- If mid consistently moves **against** us after fills → we're being adversely selected
- Measured in bps over a lookback window (default 3 seconds)
- If adverse selection > 5 bps → warning logged

---

## Balance Pre-Check

Before placing any order, the engine checks:
1. `balance_cache.can_afford(side, price, qty)` — is there enough balance?
2. `balance_cache.reserve()` — lock the amount for this pending order
3. On fill or cancel → `balance_cache.release()`

This prevents submitting orders that would be rejected by the exchange (wasting rate limits).

---

## Reconciliation

Every 60 seconds:
1. Query balances from exchange → compare with internal cache
2. Query open orders → detect:
   - **Orphaned orders**: on exchange but not tracked (cancel them)
   - **Phantom orders**: tracked but not on exchange (likely filled, update state)
3. Log any balance mismatches above tolerance threshold

---

## SLA Compliance

Tracks exchange obligations tick-by-tick (every second):
- **Uptime %**: fraction of time with valid two-sided quotes
- **Spread compliance**: quotes within max_spread_bps
- **Depth compliance**: bid_depth and ask_depth above min_depth_quote
- **Requote speed**: time to refresh after a fill

Violations are counted and logged. The SLA status is exposed via `/api/v1/sla`.

---

## Audit Trail

Append-only JSONL file (`data/audit/{symbol}.jsonl`) logging every action:
- Order placed / cancelled / filled / rejected
- Circuit breaker trips
- Kill switch escalations
- Engine start / shutdown
- Reconnections
- Reconciliation results

MiCA requires 5-year retention of all order lifecycle data.
