# Two-venue paper smoke test — runbook

30-minute validation that the multi-venue pipeline functions
end-to-end in paper mode. Addresses **PAPER-2** from the TODO:
run a pair of paper engines against two different venues, deploy
a graph that touches cross-venue state, and verify every output
hook (DecisionLedger, TieredOtr, CrossVenuePortfolio, FillProbability)
fires with sensible values.

No live keys required. Pure paper mode — `MM_MODE=paper`
short-circuits every connector's order path before it hits the
wire (`OrderManager::new_paper` gate).

---

## Pre-flight

1. **Build clean**: `cargo build --release --bin mm-server`
2. **Config**: point `MM_CONFIG` at a file declaring two symbols
   on different venues. Minimal shape:

   ```toml
   [market_maker]
   symbols = ["BTCUSDT", "BTC-USDT"]

   [[exchanges]]
   symbol = "BTCUSDT"
   exchange_type = "binance"

   [[exchanges]]
   symbol = "BTC-USDT"
   exchange_type = "bybit"
   ```

   Both connectors run with synthetic WS feeds because `MM_MODE=paper`
   replaces the live WS with the backtester fill simulator.

3. **Audit chain**: on first run the boot path triggers HARD-2's
   `verify_chain()`. If the log is fresh, it prints
   `rows = 0 last_hash = None`. If an older run left a broken
   chain and you want to proceed past it, set
   `MM_AUDIT_RESUME_ON_BROKEN=yes` before starting.

---

## Run

```bash
MM_MODE=paper MM_CONFIG=./config/paper-two-venue.toml \
  cargo run --release --bin mm-server
```

Leave it running. Dashboard comes up at `http://127.0.0.1:8080`.

### Graph deploy

Via HTTP (operator token required):

```bash
curl -X POST http://127.0.0.1:8080/api/admin/strategy/graph \
     -H "Authorization: Bearer $MM_ADMIN_TOKEN" \
     -H "Content-Type: application/json" \
     -d @crates/strategy-graph/templates/major-spot-basic.json
```

Or via the UI: `Strategy → Deploy` and pick
`major-spot-basic`.

The graph uses `Portfolio.CrossVenueNetDelta`,
`Cost.Sweep`, and `Book.FillProbability` — all INV-4 / BOOK-1+2
sources so the test exercises the fresh wiring.

---

## What to check (30 min runtime)

Every check below has an `/api/v1/*` endpoint and a dashboard
panel. Pass = all green.

| # | Check | How to verify |
|---|-------|---------------|
| 1 | Both engines quoting | Overview → per-symbol cards show non-zero `bid/ask` |
| 2 | Cross-venue portfolio aggregates | Overview → Cross-Venue Portfolio panel shows one asset row per base, both legs populated, `net_delta` updates over ticks |
| 3 | DecisionLedger resolves | `GET /api/v1/decisions/recent?limit=10` returns rows whose `decision_id` is bound to order ids and whose `outcome` flips from `Pending` → `Filled/Cancelled` |
| 4 | Tiered OTR publishes | `GET /api/v1/otr/tiered` returns per-symbol rows; each symbol has `tob` and `top20` buckets with monotone-increasing counts |
| 5 | FillProbability source emits | Deploy a graph with `Book.FillProbability(side=buy)` wired into `Out.SpreadMult`. Check dashboard → Strategy → Node outputs: the source shows a number in [0, 1], not `Missing` (after ~30 sec of synthetic trade flow) |
| 6 | Queue tracker attaches | `cargo run --release --bin mm-server -- --dump-queue-tracker` *(optional stretch — not wired yet, manually inspect via log)*: search stdout for `QueueTracker` initialisation lines, one per fresh maker order |
| 7 | Venues health | `GET /api/v1/venues/health` returns both venues with `status=healthy` |
| 8 | Audit chain writes | `tail -n 20 data/audit.jsonl` — every row's `prev_hash` matches the previous row's SHA-256 |

Stop-the-bus signals:

- **Kill switch L5** should only fire on explicit operator input — `GET /api/v1/status` should report `kill_level=Normal` throughout
- **Margin guard** returns `Stale` (no perp connector in this config — expected if the primary is spot only)

---

## Teardown + cleanup

1. `Ctrl+C` — graceful shutdown cancels all orders, flushes the
   checkpoint + daily report, closes the audit log.
2. `rm data/*.paper-snapshot.json` if you want a fresh run next time.
3. Commit the runbook outcome — if anything failed, file it against
   `TODO.md`'s P1 band with exact reproduction steps.

---

## Known caveats

- Paper mode uses `ProbabilisticFiller` defaults — tweak
  `[paper] fill_prob_on_touch` in config if you want more / fewer
  fills during the 30-minute window.
- `Risk.LiquidationDistance` returns `Missing` on spot-only
  configs (no margin guard instantiated). Intentional.
- The sprint4 metrics test (`sprint4_strategy_graph_deploy_metrics`)
  is a known-flaky test when the full workspace suite runs in
  parallel — passes 3/3 in isolation. Not a smoke-test blocker.
