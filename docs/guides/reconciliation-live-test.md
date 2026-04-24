# Reconciliation loop live test — runbook

Validates **HARD-3** from TODO: the reconciliation loop (orders +
balances + position-delta) is exercised by 12+ unit tests, but
has never been run against a real venue account. This runbook is
what an operator runs with live paper / testnet keys to prove
end-to-end behaviour before trusting the loop on production funds.

⚠ **Use a testnet / sub-account with ≤$100 exposure.** The
reconciliation loop writes audit rows + fires incidents on drift
— a real mismatch on a mainnet account will page you.

---

## Pre-flight

1. **Testnet keys**. Binance testnet (testnet.binancefuture.com),
   Bybit testnet (api-testnet.bybit.com), HyperLiquid
   mock-deployment — any venue whose REST + WS is real but the
   balance is sandbox.

   **Do not export as env vars** — venue credentials live in the
   encrypted vault. Add an entry via Admin → Vault with
   `kind=exchange`, `values={api_key, api_secret}`, and metadata
   `{exchange: "binance", product: "linear_perp"}`. Controller will
   push them to the agent on register.

2. **Config**. Start a single-symbol config pointing at the testnet
   REST/WS URLs. Minimal:

   ```toml
   [market_maker]
   symbols = ["BTCUSDT"]

   [[exchanges]]
   symbol = "BTCUSDT"
   exchange_type = "binance"
   rest_url = "https://testnet.binancefuture.com"
   ws_url = "wss://stream.binancefuture.com/ws"

   [risk]
   inventory_drift_tolerance = 0.0001
   ```

3. **Tighten the reconcile cadence for the test run**. In
   `[engine]`:

   ```toml
   [engine]
   reconcile_interval_secs = 20
   ```

   Default is 60 s; 20 s means you see the loop fire 3× during a
   1-minute scenario.

---

## Scenario A — agreement (happy path)

Goal: prove the reconciler doesn't false-positive when state is
in sync.

1. Boot: `MM_MODE=live cargo run --release --bin mm-server`
2. Wait 2 minutes for the engine to open a few maker orders +
   record at least one fill.
3. Tail the audit log:

   ```bash
   tail -F data/audit.jsonl | grep reconcile
   ```

4. Expect at least 3 rows of shape:

   ```json
   {"kind":"reconcile_ok","orders":{"matched":N,"missing_on_exchange":0,...},...}
   ```

   Every reconcile row should show `missing_on_exchange = 0` and
   `missing_internal = 0`.

### Pass

- [ ] `reconcile_ok` row with zero mismatches within 60 s of
      boot
- [ ] `reconcile_balance_ok` row shows drift < tolerance
- [ ] `reconcile_position_delta_ok` row (perp only)

---

## Scenario B — induced drift (failure path)

Goal: prove the reconciler **catches** real mismatches and fires
a high-severity incident.

1. While the engine is running Scenario A, open a terminal and
   hand-cancel one of the engine's open orders on the venue UI
   (Binance testnet web UI → Orders → Cancel).
2. Within 20 s the next reconcile tick should publish an audit
   row:

   ```json
   {"kind":"reconcile_mismatch","missing_on_exchange":["<order_id>"],...}
   ```

3. Dashboard → Incidents panel should show the mismatch at
   severity `warning` (orders path) or `high` (position-delta
   drift past tolerance).
4. Engine should **not** kill-switch on a single mismatch; it
   logs, publishes, and keeps running so the operator can
   diagnose. Kill-switch escalation is on the operator.

### Pass

- [ ] `reconcile_mismatch` row in audit log within 30 s of the
      hand cancel
- [ ] Dashboard Incidents panel shows the event
- [ ] Engine keeps quoting (no spurious auto-kill)

---

## Scenario C — induced balance drift (perp only)

1. Open a small position manually on the testnet (e.g. +0.01 BTC
   long via the web UI).
2. Engine's `InventoryManager` doesn't see the manual fill.
3. Next reconcile tick:

   ```json
   {"kind":"reconcile_position_delta_drift",
    "venue":"binance","asset":"BTC",
    "expected_inventory":"0","actual_inventory":"0.01",...}
   ```

4. `InventoryDriftReconciler` fires a `high`-severity incident.

### Pass

- [ ] `reconcile_position_delta_drift` row with the manual 0.01
      BTC delta reflected
- [ ] Incident severity = `high`

---

## Teardown

1. Flatten manual test position on the venue UI.
2. `Ctrl+C` the engine; final checkpoint flush runs.
3. Archive `data/audit.jsonl` for the incident evidence chain.

If any scenario failed, file against TODO.md's P0 / P1 band with
exact repro + the audit rows that DID fire (even if wrong).

---

## Why this isn't automated in CI

The reconciliation loop depends on a real venue's REST / WS round
trips. Testnet accounts need manual funding + order placement, and
the sandbox can't be faked with mocks (we'd lose coverage of the
exact code path this runbook exists to exercise).

Tracked as operator task until testnet sub-accounts can be
provisioned per-CI-run, at which point it becomes an hourly job.
