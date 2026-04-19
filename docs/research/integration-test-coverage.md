# Integration Test Coverage Matrix — Apr 19

> Sprint 15 R8.7. The audit in Sprint 14 found that E2E test gaps
> are where latent bugs hide (env-var drift across dashboard +
> evaluator would have been caught by one integration test). This
> doc enumerates every critical operator-visible path and marks
> what we've actually tested E2E vs relied on unit-level coverage.

## Scoring

- **✅ E2E** — integration test exercises the full producer →
  transport → consumer path; changing any link without updating
  the test breaks it.
- **🟡 Unit** — unit tests cover each link but not the glue
  between them; gate drift + inter-crate contract bugs can hide.
- **❌ None** — no test at all beyond compile-check.

## User-visible HTTP endpoints

| Endpoint | Publisher | Coverage | Notes |
| --- | --- | --- | --- |
| `/api/v1/status` | Engine tick publishes SymbolState | 🟡 Unit | Existing tests on `get_all` + publish; no HTTP-layer E2E |
| `/api/v1/metrics` | Prometheus crate | 🟡 Unit | Histograms tested in isolation |
| `/api/v1/rebalance/recommendations` | `Rebalancer` + `DashboardState` | 🟡 Unit | `rebalance_recommendations_surface_deficit` covers state; no HTTP E2E |
| `/api/v1/rebalance/execute` | Dashboard handler → connector | ✅ E2E (state-level, R8.5) | State roundtrip + kill-switch gate + no-log 503 path all pinned |
| `/api/v1/rebalance/log` | `read_all(transfer_log.jsonl)` | ✅ E2E (R8.5) | Log write + read exercised |
| `/api/v1/manipulation/scores` | Engine → SymbolState → handler | ✅ E2E (state-level, R8.6) | Publish cycle + missing-skip pinned |
| `/api/v1/onchain/scores` | Poller → DashboardState → handler | 🟡 Unit | `publish_onchain` tested; no HTTP E2E |
| `/api/v1/funding-arb/pairs` | DriverEventSink → DashboardState | 🟡 Unit | `funding_arb_events_accumulate_per_pair` covers state |
| `/api/v1/sor/decisions/recent` | `record_sor_decision` | 🟡 Unit | Ring buffer tested |
| `/api/v1/atomic-bundles/inflight` | `register_atomic_bundle_leg` | 🟡 Unit | Cross-engine ack sweep has its own E2E |
| `/api/v1/calibration/status` | GLFT `on_tick` → DashboardState | 🟡 Unit | `calibration_snapshots_replace_and_sort` |
| `/api/v1/active-graphs` | SymbolState.active_graph | 🟡 Unit | `active_graph_snapshot_round_trips` |
| `/api/v1/adverse-selection` | SymbolState fields | ❌ None | Never tested post-Sprint 5 |
| `/api/v1/plans/active` | Engine publishes per symbol | 🟡 Unit | Shape tested |
| `/api/v1/decisions/recent` | DecisionLedger | 🟡 Unit | Ledger tested |
| `/api/v1/otr/tiered` | OTR tracker | 🟡 Unit | Metric tested |
| `/api/v1/portfolio/cross_venue` | `CrossVenuePortfolio` | 🟡 Unit | `cross_venue_inventory_aggregates_by_base_asset` |
| `/api/v1/venues/latency_p95` | Prometheus scrape | ❌ None | Only smoke-tested during development |
| `/api/v1/venues/status` | Engine publishes per venue | ❌ None | |

## Strategy graph E2E

| Template | Coverage | Notes |
| --- | --- | --- |
| `pentest-liquidation-cascade` | ✅ E2E (Sprint 14 R8.2) | Trigger loop + guard loop both pinned |
| `pentest-rave-cycle` | ✅ E2E (Sprint 14 R8.2) | Guard fires on RugScore |
| `rug-detector-composite` | ✅ E2E (Sprint 14 R8.2) | Defender kill-escalate pinned |
| `pentest-spoof-classic` | 🟡 Unit | Detectors + strategy individually tested |
| `pentest-pump-and-dump` | 🟡 Unit | Same |
| `pentest-rave-full-campaign` | 🟡 Unit | CampaignOrchestrator FSM has its own unit tests |
| `glft-via-graph` | 🟡 Unit | Round-trips through `every_safe_template_compiles` |
| `avellaneda-via-graph` | 🟡 Unit | Same |
| `grid-via-graph` | 🟡 Unit | Same |
| `cross-exchange-basic` | 🟡 Unit | Same |
| `basis-carry-spot-perp` | 🟡 Unit | Same |
| `funding-aware-quoter` | 🟡 Unit | Same |
| `liquidity-burn-guard` | 🟡 Unit | Same |
| `cost-gated-quoter` | 🟡 Unit | Same |
| `major-spot-basic` / `meme-spot-guarded` / `cross-asset-regime` | 🟡 Unit | Same |

## Engine tick critical paths

| Path | Producer | Consumer | Coverage |
| --- | --- | --- | --- |
| Book WS → `BookKeeper` → `SymbolState.mid_price` | Binance/Bybit/HL parsers | UI + graph source `Book.L1` | 🟡 Unit |
| Trade WS → `manipulation` aggregator → SymbolState.manipulation_score | Connector parsers | `/api/v1/manipulation/scores` | ✅ E2E (R8.6 state-level) |
| Liquidation WS → `LiquidationHeatmap` → `Surveillance.LiquidationHeatmap` graph source | Connector parsers | Graph consumers | 🟡 Unit (parsers + heatmap each tested) |
| `MarketEvent::Fill` → `InventoryManager` → drift reconciler | Engine | Risk + dashboard | 🟡 Unit |
| Strategy pool tick → `last_strategy_quotes_per_node` → graph overlay → `Out.Quotes` | `build_strategy_pool` | Sink | ✅ E2E (Sprint 14 R8.2 for Strategy.* source nodes) |
| Funding-rate refresh → `get_funding_rate` → `get_open_interest` → `get_long_short_ratio` | Connector REST | Engine state | 🟡 Unit (Sprint 17 R11.4 — MockConnector contracts) |
| `swap_strategy_graph` → `spawn_leverage_setup` → `set_leverage` | Dashboard config-override | Connector | ❌ None |
| Config override → `refresh_quotes` → new strategy live | `register_config_channel` | Engine loop | 🟡 Unit |
| Kill-switch L4 → `TwapExecutor` flatten | `kill_switch` | `OrderManager` | 🟡 Unit |

## Restricted-gate flow

| Path | Coverage | Notes |
| --- | --- | --- |
| Graph evaluator `MM_ALLOW_RESTRICTED` check | ✅ E2E (Sprint 14) | `with_restricted` helper in pentest_templates_e2e |
| Dashboard deploy handler env-var check | ❌ None | Sprint 14 found drift vs evaluator; fixed but not tested |
| Tracing warn! on restricted compile | ❌ None | Warn message content not asserted anywhere |

## What the matrix tells us

Three clusters of weakness:

1. **HTTP-layer E2E is near-zero across the board.** Every
   endpoint has state-level or unit tests but no "client POSTs +
   expects response" integration test. A bug in axum routing,
   middleware ordering, or serde shape wouldn't be caught. Priority:
   wire a single Axum `TestClient` harness and hit the 6 highest-
   risk endpoints (rebalance/execute, manipulation/scores, deploy,
   logout, /metrics, /health).

2. **REST-poll paths on connectors (funding, OI, L/S ratio) have
   no E2E.** Unit tests for each impl exist but no test proves the
   engine's `refresh_funding_rate` poll → state → graph source loop
   works. Sprint 14 would have caught two bugs if we'd had it.
   Priority: mock connector + engine tick → verify state updates.

3. **Dashboard deploy handler has no env-var gate test.** This is
   exactly where Sprint 14's BUG #1 hid. Priority: add a handler
   unit test that sets `MM_ALLOW_RESTRICTED` both correctly and
   incorrectly, asserts 202 vs 403.

## Sprint 16+ backlog

- [x] **R10.2a** Axum TestClient harness (Sprint 16 —
  `crates/dashboard/tests/http_handlers_e2e.rs`)
- [x] **R10.2b** Top-6 endpoints hit via harness (Sprint 16)
- [x] **R10.2d** Dashboard deploy handler env-var gate test
  (Sprint 16 R11.3 —
  `restricted_env_gate_only_accepts_exact_literal`)
- [x] **R11.4** MockConnector fixture + REST-poll contract
  tests (Sprint 17 —
  `crates/exchange/core/tests/mock_connector_contracts.rs`).
  Pins default `Ok(None)` for `get_open_interest` +
  `get_long_short_ratio` on spot; override path for perps;
  `set_leverage` call recording + failure injection.
- [ ] **R10.2c** Engine tick integration: spin MockConnector +
  drive 10 s of fake WS events → verify SymbolState publish.
  The MockConnector fixture now exists (Sprint 17) —
  remaining work is the engine-side harness. Added to
  Sprint 18 backlog.
- [ ] **R10.2e** Wire CI to run integration tests in addition
  to unit tests — currently `--lib` + `--tests` runs
  everything locally but we haven't verified the CI workflow
  picks up the new `tests/*.rs` files.
