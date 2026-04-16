# Roadmap v2: Production MM Service

> Created Apr 16 2026 after 28-commit production push.
> Status: PLANNING — needs PM review before execution.

## Current State (post Apr 16 session)

- **18 crates, ~55K LoC Rust, 1128 tests**
- **4 venue connectors**: Binance (spot+futures), Bybit, HyperLiquid, Custom
- **5 strategies**: Avellaneda-Stoikov, GLFT, Grid, Basis, CrossVenueBasis
- **24 client + 13 admin API endpoints**
- **Core algo edge**: Hawkes, Johansen, CVaR, EWMA VaR, market impact, Cartea AS, per-side lead-lag
- **Production hardening**: safe unwraps, real reconciliation, dynamic product spec, hot config reload, K8s probes, fill persistence, webhooks

## What's Missing for a Production MM Business

### Epic 1: Multi-Client Isolation (Priority: P0, Blocks Revenue)

**Why:** MM firms serve multiple token projects simultaneously. Each client needs isolated SLA tracking, PnL attribution, fill history, and compliance reporting. Currently everything is mixed.

**Scope:**
- [ ] 1.1 `ClientContext` struct — per-client config (symbols, SLA targets, loan terms, webhook URLs)
- [ ] 1.2 Per-client PnL attribution — tag fills with client_id, aggregate separately
- [ ] 1.3 Per-client SLA tracking — separate SlaTracker per client, not per symbol
- [ ] 1.4 Per-client API authentication — API keys scoped to client's symbols only
- [ ] 1.5 Per-client compliance certificate — `/api/v1/client/{id}/sla/certificate`
- [ ] 1.6 Per-client webhook routing — different URLs per client
- [ ] 1.7 Client onboarding API — `POST /api/admin/clients` with symbols, SLA config

**Dependencies:** None (greenfield)
**Estimate:** 2 weeks
**Risk:** Data model change — needs careful migration path

---

### Epic 2: Token Lending & Loan Management (Priority: P0, Blocks Revenue)

**Why:** Most MM agreements involve token projects lending inventory to the MM. The MM must track utilization, schedule returns, and amortize loan costs in PnL.

**Research needed:**
- Industry-standard loan term structures (lock-up, vesting, options)
- Return schedule patterns (linear, cliff, milestone-based)
- Loan cost accounting (imputed interest vs explicit fee)

**Scope:**
- [ ] 2.1 `LoanAgreement` struct — terms, schedule, qty, start/end dates, cost basis
- [ ] 2.2 Loan utilization tracker — current vs max, alert on approaching limits
- [ ] 2.3 Return schedule engine — automated reminders, pre-staging returns
- [ ] 2.4 Loan cost amortization in PnL — spread loan cost over agreement period
- [ ] 2.5 Client dashboard: loan status, utilization %, upcoming returns
- [ ] 2.6 `POST /api/admin/loans` — create/update loan agreements
- [ ] 2.7 Audit trail for all loan operations (MiCA compliance)

**Dependencies:** Epic 1 (per-client isolation)
**Estimate:** 2 weeks
**Risk:** Legal complexity in loan term modeling

---

### Epic 3: Cross-Symbol Portfolio Risk (Priority: P0, Blocks Scale)

**Why:** Running 50+ symbols with isolated per-engine risk means no global position limits. A correlated crash hits all BTC pairs simultaneously and the per-symbol limits don't protect the aggregate.

**Research needed:**
- Cross-asset correlation estimation (rolling window, EWMA)
- Portfolio VaR vs per-symbol VaR aggregation
- Cross-margin vs isolated-margin implications per venue

**Scope:**
- [ ] 3.1 `PortfolioRiskManager` — global position limits across symbols
- [ ] 3.2 Cross-symbol correlation matrix — rolling estimation from mid-price returns
- [ ] 3.3 Portfolio-level VaR — aggregate VaR using correlation structure
- [ ] 3.4 Global kill switch — "total delta across all BTC pairs > X → widen all"
- [ ] 3.5 Cross-symbol inventory limits — "net BTC delta from BTCUSDT + BTCETH + BTCBUSD < Y"
- [ ] 3.6 Dashboard: portfolio-level risk heatmap
- [ ] 3.7 Wire FactorCovarianceEstimator into portfolio-level decisions (already built, needs routing)

**Dependencies:** Factor covariance estimator (done), Portfolio crate (done)
**Estimate:** 2 weeks
**Risk:** Performance — correlation matrix computation on 50+ symbols

---

### Epic 4: Cross-Venue Execution (Priority: P1, Blocks Multi-Venue)

**Why:** Advisory-only rebalancer isn't enough. Need actual withdraw/transfer/deposit execution + SOR that routes orders, not just recommends.

**Research needed:**
- Per-venue withdrawal API quirks (Binance withdrawal limits, HL L1→L2 bridge, Bybit unified account)
- Settlement times per chain/asset
- Gas cost estimation for on-chain transfers

**Scope:**
- [ ] 4.1 Implement `withdraw()` for Binance connector (REST API)
- [ ] 4.2 Implement `withdraw()` for Bybit connector
- [ ] 4.3 Implement `internal_transfer()` for Bybit (spot↔linear↔funding)
- [ ] 4.4 Implement `internal_transfer()` for Binance (spot↔margin↔futures)
- [ ] 4.5 Auto-rebalancing engine — monitor + execute transfers on threshold breach
- [ ] 4.6 SOR inline dispatch — wire `GreedyRouter` output into actual order execution
- [ ] 4.7 Transfer audit trail + reconciliation
- [ ] 4.8 Dashboard: cross-venue balance overview, transfer history

**Dependencies:** Connector trait methods (done — `withdraw`, `internal_transfer` stubs exist)
**Estimate:** 3 weeks
**Risk:** Venue API changes, chain congestion, gas spikes

---

### Epic 5: Compliance & Reporting (Priority: P1, Blocks Enterprise Clients)

**Why:** Enterprise clients and regulated venues require formatted reports, not JSON APIs. MiCA compliance needs signed audit exports.

**Research needed:**
- MiCA Article 17 reporting requirements for algorithmic trading
- Exchange-specific compliance formats (Binance Institutional, Bybit API compliance)
- PDF generation in Rust (printpdf, wkhtmltopdf subprocess, or template-based)

**Scope:**
- [ ] 5.1 PDF daily report generator — formatted with charts, signed
- [ ] 5.2 Excel/CSV monthly compliance export
- [ ] 5.3 Automated report email delivery (SMTP integration)
- [ ] 5.4 MiCA Article 17 algorithmic trading report template
- [ ] 5.5 Audit log export with date-range filtering and digital signature
- [ ] 5.6 Per-client branded report templates
- [ ] 5.7 Scheduled report generation (daily/weekly/monthly cron)

**Dependencies:** Epic 1 (per-client isolation)
**Estimate:** 2 weeks
**Risk:** PDF generation complexity in Rust

---

### Epic 6: Strategy Optimization & A/B Testing (Priority: P1, Blocks Edge)

**Why:** hyperopt module exists but isn't wired. Need online parameter optimization and A/B testing to maintain competitive edge.

**Research needed:**
- Bayesian optimization vs grid search vs random search for live tuning
- A/B testing frameworks for financial strategies (split traffic vs time-based)
- Regime-aware optimization (different params for different market regimes)

**Scope:**
- [ ] 6.1 Wire hyperopt into server as background optimization task
- [ ] 6.2 A/B split engine — run two parameter sets side-by-side
- [ ] 6.3 Online parameter tuning — update gamma/spread/size based on recent performance
- [ ] 6.4 Regime-aware parameter selection — per-regime optimal params from backtest
- [ ] 6.5 Optimization results API — `/api/v1/optimization/results`
- [ ] 6.6 Admin: trigger optimization run, compare A/B results, promote winner

**Dependencies:** Performance tracker (done), AutoTuner (done)
**Estimate:** 3 weeks
**Risk:** Overfitting, regime detection accuracy

---

### Epic 7: Disaster Recovery & Operational Resilience (Priority: P1, Blocks Production)

**Why:** Crash recovery path isn't tested end-to-end. Need to prove that after a crash, the system recovers cleanly without orphaned orders or missed fills.

**Scope:**
- [ ] 7.1 End-to-end crash recovery test — kill process, restart, verify state
- [ ] 7.2 Checkpoint restore validation — load checkpoint, verify inventory/PnL
- [ ] 7.3 Orphaned order detection on restart — query venue, cancel stale orders
- [ ] 7.4 Fill replay from audit log — reconstruct PnL from JSONL after crash
- [ ] 7.5 Health degradation modes — graceful degradation when one venue is down
- [ ] 7.6 Circuit breaker for venue connectivity — auto-pause on persistent errors
- [ ] 7.7 Runbook documentation — operator procedures for common failure modes

**Dependencies:** Checkpoint manager (done), audit log (done)
**Estimate:** 1.5 weeks
**Risk:** Edge cases in multi-venue partial failure

---

### Epic 8: Paper Trading Parity (Priority: P2, Blocks Sales Demos)

**Why:** Sales demos run in paper mode. All production features must work identically in paper mode, or the demo is misleading.

**Scope:**
- [ ] 8.1 Audit: list all features that don't work in paper mode
- [ ] 8.2 Paper mode market impact simulation
- [ ] 8.3 Paper mode fill quality simulation (realistic slippage)
- [ ] 8.4 Paper mode performance tracking
- [ ] 8.5 Paper mode webhook delivery (test URLs)
- [ ] 8.6 Demo data generator — synthetic market data for demos

**Dependencies:** None
**Estimate:** 1 week
**Risk:** Low

---

## Dependency Graph

```
Epic 1 (Multi-Client) ──┬──→ Epic 2 (Token Lending)
                        └──→ Epic 5 (Compliance)
Epic 3 (Cross-Symbol Risk) ──→ standalone
Epic 4 (Cross-Venue Exec) ──→ standalone
Epic 6 (Strategy Opt) ──→ standalone
Epic 7 (DR) ──→ standalone
Epic 8 (Paper Parity) ──→ standalone
```

## Suggested Execution Order

| Phase | Epics | Duration | Outcome |
|-------|-------|----------|---------|
| **Phase A** | 1 + 3 (parallel) | 2 weeks | Multi-client + portfolio risk |
| **Phase B** | 2 + 7 (parallel) | 2 weeks | Token lending + DR |
| **Phase C** | 4 + 5 (parallel) | 3 weeks | Cross-venue exec + compliance |
| **Phase D** | 6 + 8 (parallel) | 3 weeks | Strategy opt + paper parity |

**Total: ~10 weeks to full competitive parity with tier-1 MM firms.**

## External Dependencies (Cannot Do In-Code)

- [ ] Coinbase Prime sandbox credentials (for FIX 4.4 connector)
- [ ] Tardis subscription (for historical replay stress tests)
- [ ] Linux staging server (for io_uring runtime validation)
- [ ] SMTP relay credentials (for email report delivery)
- [ ] Legal review of loan agreement templates
- [ ] MiCA compliance legal opinion on audit log format
