# Metrics Glossary

Every `mm_*` Prometheus metric exported by the engine, with semantics, typical range, and alertable threshold suggestions. Scrape endpoint: `http://<dashboard>:9091/metrics` (Prometheus-federation-friendly).

Most metrics carry a `symbol` label; some (portfolio, archive, scheduler, sentiment, borrow, SOR-routing) are labelled by `asset`, `stream`, `cadence`, `venue`, or other dimensions instead — explicit per-row below. Check `crates/dashboard/src/metrics.rs` as the authoritative source if your PromQL needs an exact label set.

---

## PnL

| Metric | Type | Meaning | Alertable when |
|--------|------|---------|----------------|
| `mm_pnl_total` | gauge | Total PnL (sum of all attribution components), quote asset | Drops below configured loss threshold |
| `mm_pnl_spread` | gauge | Spread-capture PnL (maker rebates + bid-ask advantage) | Negative for sustained period = paying to be there |
| `mm_pnl_inventory` | gauge | Inventory PnL (signed MTM of current position × mid change) | Large negative = adverse selection |
| `mm_pnl_rebates` | gauge | Fee rebate income | Should be positive on rebate-paying venues |
| `mm_pnl_funding_realised` | gauge | Realised funding payments (perp only) | Track vs expected funding rate |
| `mm_pnl_funding_mtm` | gauge | Mark-to-market of pending funding accrual (perp) | Monitor before funding tick |

---

## Inventory + orders

| Metric | Type | Meaning | Notes |
|--------|------|---------|-------|
| `mm_inventory` | gauge | Current position, base asset (signed) | abs(value) > max_inventory → kill switch L3 |
| `mm_inventory_value` | gauge | `abs(inventory) × mid`, quote asset | Use for cross-symbol comparison |
| `mm_live_orders` | gauge | Count of orders currently resting on the venue | Zero for long periods = engine idle or killed |
| `mm_fills_total` | counter | Cumulative fill count, labels `symbol, side` | Rate = fill/min — alert if drops near 0 |

---

## Market microstructure

| Metric | Type | Meaning | Typical range |
|--------|------|---------|--------------|
| `mm_mid_price` | gauge | Current mid price, quote asset | Symbol-specific |
| `mm_spread_bps` | gauge | Current spread in bps | 1-50 bps in liquid pairs; > 100 = venue stress |
| `mm_volatility` | gauge | EWMA realised vol (per-tick return stddev annualised) | 0.001-0.1 typically |
| `mm_vpin` | gauge | VPIN toxicity indicator, [0, 1] | > 0.6 = toxic flow, widen; > 0.8 = exit |
| `mm_kyle_lambda` | gauge | Kyle's λ — price impact per unit volume | Higher = thinner book; breakpoint context-dependent |
| `mm_adverse_selection_bps` | gauge | Post-fill mid drift against our fill side (bps) | > 5 bps = adverse selection pressure |
| `mm_market_resilience` | gauge | Recovery score after a shock, [0, 1] | < 0.3 for 3s → kill switch L1 |
| `mm_order_to_trade_ratio` | gauge | MiCA OTR — orders / trades ratio (rolling) | < 500 typical; MiCA Art. 17 threshold varies |
| `mm_otr_tiered` | gauge | Tiered OTR, labels `symbol, tier, window` | MiCA compliance detail |
| `mm_hma_value` | gauge | Hull Moving Average of mid | Trend indicator, symbol-specific |
| `mm_momentum_ofi_ewma` | gauge | EWMA of Cont-Kukanov-Stoikov OFI | Sign = directional pressure |
| `mm_momentum_learned_mp_drift` | gauge | Stoikov 2018 learned-microprice drift (frac of mid) | \|·\| > 5e-5 = strong signal |
| `mm_as_prob_bid` / `mm_as_prob_ask` | gauge | Adverse-selection probability per side (Cartea) | 0.5 = neutral, > 0.7 = strong skew |
| `mm_market_impact_mean_bps` | gauge | Mean observed impact of own fills (bps) | Context for sizing |
| `mm_market_impact_adverse_pct` | gauge | % of own fills followed by adverse mid drift | > 60% = overexposed to toxic flow |

---

## SOR (Smart Order Router)

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_sor_dispatch_success_total` | counter | Successful multi-leg dispatches |
| `mm_sor_dispatch_errors_total` | counter | Failed dispatches, labels `symbol, venue` |
| `mm_sor_dispatch_filled_qty` | gauge | Last dispatch's total dispatched qty |
| `mm_sor_route_cost_bps` | gauge | Per-venue effective cost in bps, label `venue` |
| `mm_sor_fill_attribution` | gauge | Per-venue recommended fill quantity, label `venue` |

---

## Kill switch + SLA

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_kill_switch_level` | gauge | 0-5 current kill level per symbol | > 0 sustained = operator attention required |
| `mm_sla_uptime_pct` | gauge | 24h uptime percentage | < MM agreement threshold → client-facing alert |
| `mm_sla_presence_pct_24h` | gauge | Per-minute presence % rolled up | Complement to uptime for MiCA |
| `mm_regime` | gauge | Regime label encoded: 0=Quiet, 1=Trending, 2=Volatile, 3=MeanReverting | Decoder in the agent-side `regime_label` table |

---

## Fees + borrow

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_maker_fee_bps` | gauge | Active maker fee tier in bps |
| `mm_taker_fee_bps` | gauge | Active taker fee tier in bps |
| `mm_borrow_rate_bps_hourly` | gauge | Per-hour borrow rate (spot margin), label `asset` |
| `mm_borrow_carry_bps` | gauge | Accumulated borrow carry cost (bps) |
| `mm_fill_slippage_avg_bps` | gauge | Average slippage of filled orders vs NBBO at placement |

---

## Cross-venue

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_cross_venue_basis_bps` | gauge | `perp_mid − spot_mid` in bps of spot mid | > max_divergence → basis config guard |

---

## Portfolio

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_portfolio_total_equity` | gauge | Aggregated equity across all symbols, quote asset |
| `mm_portfolio_realised_pnl` | gauge | Realised PnL, portfolio-wide |
| `mm_portfolio_unrealised_pnl` | gauge | Unrealised PnL (MTM), portfolio-wide |
| `mm_portfolio_asset_qty` | gauge | Per-asset position quantity (signed), labelled by symbol |
| `mm_portfolio_asset_unrealised_reporting` | gauge | Per-asset unrealised MTM for client reports |
| `mm_portfolio_factor_delta` | gauge | Signed factor-model exposure, label `asset` |
| `mm_portfolio_strategy_pnl` | gauge | Per-strategy PnL, label `strategy` |

---

## Strategy graph

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_strategy_graph_deploys_total` | counter | Deploy attempts, label `outcome` (accepted / rejected) |
| `mm_strategy_graph_nodes` | gauge | Node count of currently deployed graph, label `graph` |

---

## Calibration (adaptive)

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_calibration_a` | gauge | Calibrated A parameter (GLFT) |
| `mm_calibration_k` | gauge | Calibrated k parameter (GLFT) |
| `mm_calibration_samples` | gauge | Sample count used in latest calibration |

---

## Funding arb

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_funding_arb_active` | gauge | 1 when funding-arb has an open pair, else 0 |
| `mm_funding_arb_transitions_total` | counter | State transitions, labels `symbol, outcome` (entered / exited / hold / pair_break / ...) |

---

## Atomic bundles

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_atomic_bundles_inflight` | gauge | Bundles currently being placed |
| `mm_atomic_bundles_completed_total` | counter | Successfully completed multi-leg bundles |
| `mm_atomic_bundles_rolled_back_total` | counter | Bundles that failed mid-placement and rolled back |

---

## Social / sentiment

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_sentiment_ticks_total` | counter | Sentiment ticks ingested, label `asset` |
| `mm_sentiment_articles_total` | counter | Articles processed, label `scorer` |
| `mm_sentiment_mentions_rate` | gauge | Current mentions/min rate, label `asset` |
| `mm_social_spread_mult` | gauge | Spread multiplier driven by sentiment engine |
| `mm_social_size_mult` | gauge | Size multiplier driven by sentiment engine |
| `mm_social_kill_triggers_total` | counter | Times sentiment escalated kill switch |

---

## Surveillance

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_surveillance_score` | gauge | Per-pattern score [0, 1], labels `pattern, symbol` |
| `mm_surveillance_alerts_total` | counter | Surveillance-score breaches that fired alert, labels `pattern, symbol` |
| `mm_manipulation_combined` | gauge | Aggregate manipulation score |
| `mm_manipulation_pump_dump` | gauge | Pump-and-dump orchestrator-style score |
| `mm_manipulation_thin_book` | gauge | Thin-book / layering composite |
| `mm_manipulation_wash` | gauge | Wash-trading detector score |

---

## Decision ledger

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_decision_realized_cost_bps` | gauge | Rolling realised cost per decision (bps) |
| `mm_decision_vs_expected_bps` | gauge | Realised − Expected spread (bps), labels `symbol, side` |

---

## Book health

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_book_update_latency_ms` | gauge | WS book-update processing latency, labels `symbol, venue, kind` |

---

## Operational / infra

| Metric | Type | Meaning |
|--------|------|---------|
| `mm_archive_uploads_total` | counter | Audit archive uploads attempted, label `stream` |
| `mm_archive_upload_errors_total` | counter | Failed archive uploads, label `stream` |
| `mm_archive_upload_bytes_total` | counter | Bytes uploaded to archive, label `stream` |
| `mm_archive_last_success_ts` | gauge | Unix ts of last successful archive upload, label `stream` |
| `mm_scheduler_runs_total` | counter | Scheduled job runs, label `cadence` |
| `mm_scheduler_failures_total` | counter | Scheduled job failures, label `cadence` |
| `mm_scheduler_last_success_ts` | gauge | Last successful scheduled run, label `cadence` |

---

## Recommended alert set (starter)

Paste into your Prometheus alerting rules:

```yaml
groups:
- name: mm-maker-critical
  rules:
  - alert: MMKillSwitchL3OrAbove
    expr: mm_kill_switch_level >= 3
    for: 30s
    labels: { severity: critical }
    annotations:
      summary: "Kill switch escalated to L3+ for {{ $labels.symbol }}"

  - alert: MMPnLCatastrophicLoss
    expr: mm_pnl_total < -10000
    for: 1m
    labels: { severity: critical }

  - alert: MMSLABreach
    expr: mm_sla_uptime_pct < 95
    for: 5m
    labels: { severity: warn }

  - alert: MMFillRateStalled
    expr: rate(mm_fills_total[5m]) < 0.01
    for: 10m
    labels: { severity: warn }
    annotations:
      summary: "Fills have stopped for {{ $labels.symbol }} — engine stuck?"

  - alert: MMArchiveUploadStale
    expr: time() - mm_archive_last_success_ts > 3600
    labels: { severity: warn }
    annotations:
      summary: "Audit archive hasn't uploaded in over an hour"

  - alert: MMReconcileMismatchPersistent
    expr: mm_atomic_bundles_rolled_back_total - mm_atomic_bundles_rolled_back_total offset 5m > 0
    labels: { severity: critical }

  - alert: MMToxicityExtreme
    expr: mm_vpin > 0.9
    for: 1m
    labels: { severity: warn }
    annotations:
      summary: "VPIN extremely high — likely toxic venue"
```

Adjust thresholds per your market / SLA agreement. The above is a conservative starter.
