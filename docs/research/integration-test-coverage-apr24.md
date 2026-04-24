# Integration test coverage audit — R10.2

Sprint 14 flagged the gap between ~1600 unit tests and the
much smaller integration / E2E surface. This is the
enumeration + gap analysis the task asked for.

## Inventory

### Rust integration tests (`crates/*/tests/*.rs`)

77 `#[test]` + `#[tokio::test]` cases across 16 files:

| Area                   | File                                                 | Count |
|------------------------|------------------------------------------------------|------:|
| Engine core            | `engine/tests/integration.rs`                        |    15 |
| WS-RPC protocol        | `protocols/ws_rpc/tests/client.rs`                   |    10 |
| Exchange core trait    | `exchange/core/tests/mock_connector_contracts.rs`    |     8 |
| Credentials CRUD       | `controller/tests/credentials_crud.rs`               |     8 |
| Engine chaos           | `engine/tests/chaos.rs`                              |     7 |
| Pentest templates      | `strategy-graph/tests/pentest_templates_e2e.rs`      |     6 |
| Dashboard HTTP         | `dashboard/tests/http_handlers_e2e.rs`               |     6 |
| Lease handshake        | `controller/tests/lease_handshake.rs`                |     3 |
| Epic R (surveillance)  | `engine/tests/epic_r_e2e.rs`                         |     2 |
| Controller reconnect   | `controller/tests/reconnect.rs`                      |     2 |
| Fleet HTTP             | `controller/tests/fleet_http.rs`                     |     2 |
| Deployment telemetry   | `controller/tests/deployment_telemetry.rs`           |     2 |
| Reconcile lifecycle    | `agent/tests/reconcile_lifecycle.rs`                 |     2 |
| Auth matrix            | `controller/tests/auth_matrix.rs`                    |     1 |
| Controller WS E2E      | `controller/tests/ws_e2e.rs`                         |     1 |
| Deploy HTTP            | `controller/tests/deploy_http.rs`                    |     1 |

### HyperLiquid

No integration tests under `exchange/hyperliquid/tests/` — all coverage is inline `#[cfg(test)]` in src. **Gap.**

### Binance

No integration tests at all under `exchange/binance/`. **Gap** — every binance-specific path (HMAC signing, listen-key rotation, retry) is unit-only.

### Bybit

No `tests/` directory. All coverage inline. **Gap** — the UX-VENUE-3 silent-WS bug sat undetected for weeks and was only found by operator log review. A tests/ws_contracts.rs harness would catch it.

### Persistence

No `crates/persistence/tests/` directory. The checkpoint + fill-replay crash-recovery paths are unit-only. **Gap** — the 23-P1-1 write-loop bug was caught by audit, not test.

### Playwright E2E (`frontend/tests/e2e/*.spec.ts`)

10 spec files covering the graph-observability + save-diff
surface:
- api-graph-observability · 3 tests
- authz-graph-topics · 3 tests
- error-states · 4 tests
- m3-validation · 3 tests
- m4-4-incident-link · 1 test
- m4-timeline · 3 tests
- m5-replay · 3 tests
- multi-deploy · 1 test
- ui-live-mode · 4 tests
- graph-save-versioning · 2 tests

**27 Playwright cases total.** No E2E coverage outside the
strategy-graph surface (no Clients page, Fleet page, Compliance
page, Reconciliation drift fan-out, etc.).

## Coverage ratios

| Layer                      | Unit  | Integration | Ratio |
|---------------------------|-------|-------------|-------|
| engine                     | ~700  | 24          | 3.4%  |
| exchange/bybit             | ~100  | 0           | 0%    |
| exchange/binance           | ~80   | 0           | 0%    |
| exchange/hyperliquid       | ~50   | 0           | 0%    |
| controller + agent + dashboard | ~400 | 25      | 6.3%  |
| strategy-graph             | ~110  | 6           | 5.5%  |
| persistence                | ~50   | 0           | 0%    |

## Gaps by severity

### P0 — production paths without integration coverage
- **Bybit WS contract** (`tests/ws_contracts.rs`) — UX-VENUE-3
  silent-stream class of bug. Mock a Bybit-shaped WS server,
  verify uppercase-topic subscribe, ACK parse, snapshot +
  delta ingestion, reconnect on disconnect.
- **Binance HMAC signing** (`tests/auth_contracts.rs`) — spot
  + futures request signing against recorded-from-testnet
  fixtures. We sign every REST call; a drift in signature
  params silently 401s every order.
- **Persistence crash recovery**
  (`persistence/tests/crash_recovery.rs`) — simulate a
  mid-write crash (truncate checkpoint), re-open, verify
  restore either loads the prior-good snapshot or refuses
  with a diagnostic.

### P1 — high-churn paths with single-digit coverage
- **Controller reconnect under load** — only 2 cases; real
  deployment has stalling network hiccups not modelled.
- **Fleet HTTP** — 2 cases, but there are ~20 endpoints in
  `internal_view`. Most landed in this quarter's sprints.
- **Deployment telemetry fan-out** — 2 cases cover the
  happy-path GET. No coverage for the trace ring overflow
  or the `graph_trace_recent` + `graph_analysis` topics added
  this sprint (those are in Playwright though, so net OK).

### P2 — value-add when someone has a day
- **Engine chaos** — 7 cases simulate disconnect / stale-book
  / exception-raising strategy. Good baseline; would benefit
  from a flaky-connector fuzz pass.
- **Pentest templates** — 6 cases verify each bundled
  manipulation template compiles + hits expected sinks on
  synthetic inputs. Fine as-is; operators use these templates
  in pentest mode and they haven't regressed.

## Recommendations

1. **Do not attempt to reach 10% integration coverage in a
   single sprint.** Pick 3 P0 files, write each as a 2-3
   day effort, land them separately so each has a focused
   review.
2. **Prioritise WS contract tests** (Bybit + HyperLiquid). The
   class of bug is "subscribe accepted, no data" which is
   invisible to unit tests and expensive to root-cause
   without the contract harness.
3. **Revisit this doc quarterly.** The Playwright E2E suite
   has grown 0 → 27 in one quarter; if the same pace lands
   on Rust integration side the ratio will be comfortable.

## Follow-ups (not blocked by this audit)

- [ ] Write `crates/exchange/bybit/tests/ws_contracts.rs`
  following the pattern in
  `crates/exchange/core/tests/mock_connector_contracts.rs`.
- [ ] Write `crates/exchange/binance/tests/auth_contracts.rs`
  against recorded testnet signatures.
- [ ] Write `crates/persistence/tests/crash_recovery.rs`.

R10.2 itself — **the audit** — is landed with this document.
Filling the P0 gaps is explicitly scoped here so each becomes
its own checkable item without the ambiguous "integration
coverage audit" umbrella.
