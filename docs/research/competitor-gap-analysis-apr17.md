# Competitive Gap Analysis — April 2026

Research pass comparing MG against Hummingbot, Tribeca, CCXT-pro, and
public discussion from prop desks (Wintermute, Jump, DRW/Cumberland).
Items omitted where they could not be firmly verified.

## Sources surveyed

| Source | What |
|---|---|
| `hummingbot/hummingbot` | OSS MM, Python, strategies under `hummingbot/strategy/` |
| `michaelgrosner/tribeca` (archived) | TS MM, mostly stale |
| `ccxt/ccxt` + ccxt-pro | Venue adapters, unified symbol/precision |
| Cartea, Jaimungal, Penalva (2015) | Optimal MM baseline |
| Guéant (2016) | GLFT reference |
| Stoikov (2018) | Micro-price |
| Sirignano & Cont (2019+) | Deep LOB |
| Spooner (2020), Ganesh et al. (2019) | RL-MM |
| Jump/Cumberland/Wintermute blogs | Infra (kdb+, co-lo, risk) |

## CRITICAL — gaps that bleed PnL today

### 1. Self-match / wash-trade prevention (STP)
- **What**: Exchange-native `selfTradePreventionMode` (EXPIRE_MAKER / EXPIRE_TAKER / EXPIRE_BOTH). Without it, our maker + taker can cross inside one account → taker fees to ourselves + surveillance flags.
- **Who**: Binance, Bybit V5 `smpType`, Hummingbot sets STP by default.
- **Why**: Silent fee bleed on any make+hedge strategy (XEMM, funding_arb primary, stat_arb). MiCA Art. 17 risk.
- **Effort**: **S** — add `stp_mode` to `binance` + `bybit` order params.

### 2. Venue-side OCO / bracket orders
- **What**: Exchange-hosted `OCO` / `OTOCO` that survives process death.
- **Who**: Binance spot `/api/v3/orderList/oco`, Bybit TP/SL conditional, Hyperliquid TP/SL.
- **Why**: Our `order_emulator` holds stops client-side. Process dies mid-shock → stops never trigger. Real live-PnL risk.
- **Effort**: **M** per venue.

### 3. FIX drop-copy / trade capture session
- **What**: Second session mirroring every fill independent of primary order-entry channel (FIX 4.4 `MsgType=AE`, or venue drop-copy WS).
- **Who**: Binance `userDataStream`, Bybit Execution WS, CME/Coinbase Prime FIX drop-copy. Wintermute/GSR public hiring references.
- **Why**: Single-channel fill capture is the #1 reconciliation failure mode. 60 s polling is a gap; drop-copy makes fill loss impossible.
- **Effort**: **M** — we already have `protocols/fix` + WS infra.

### 4. Queue-position model in live quoting
- **What**: Estimate FIFO queue position of resting orders; use for (a) don't-cancel-near-front, (b) fill probability in AS reservation price.
- **Who**: Hummingbot V2 `order_book_tracker`; prop-shop table stakes. We have `backtester/queue_model.rs` but not wired into live.
- **Why**: Cancel+repost loses 10–100 ms of priority earned over minutes. Dominant live-vs-backtest divergence on tight-spread majors.
- **Effort**: **M** — lift `queue_model.rs` into `book_keeper`, expose `queue_ahead_qty` to diff logic.

### 5. Tick-size / lot-size auto-rounding with "worst-safe" direction
- **What**: Price/size rounding that rounds *away* from book for sells, *toward* for buys, so we never accidentally cross. Plus `minNotional` check.
- **Who**: CCXT `exchange.price_to_precision`, Hummingbot `quantize_order_price`.
- **Why**: PostOnly failures are the #1 "why didn't my order land" source. Accidental marketable post = fees + inventory shock.
- **Effort**: **S** — audit `exchange/*/client.rs`, add proptest.

### 6. Explicit maker-rebate tier tracking + fee-tier polling
- **What**: Poll `/sapi/v1/asset/tradeFee` (Binance) or `/v5/account/fee-rate` (Bybit) hourly → live fee (VIP/BNB/maker rebate) into strategy reservation price.
- **Who**: CCXT exposes it; prop desks track realised vs expected fees as PnL attribution line.
- **Why**: Moving VIP tiers mid-month shifts reservation price 0.5–1 bp. Our strategies use fixed fee constants.
- **Effort**: **S** — fee-tier poller in `exchange/*/rest.rs`, shared cache.

## IMPORTANT — meaningful edge or risk

### 7. Cross-margin / portfolio-margin aware position sizing
- **What**: Binance PM / Bybit UTA cross-asset maintenance margin for sizing; auto-borrow/auto-repay toggles.
- **Why**: PM frees 30–60% capital on perps; direct PnL via capital efficiency; cross-asset liquidation risk we don't model.
- **Effort**: **L**.

### 8. Adverse-selection-aware quote shading
- **What**: Per-side quote shading from short-horizon predicted return (depth-weighted OFI + micro-price + trade-sign AR(1)). We have VPIN/Kyle triggering widen, not shading.
- **Who**: Cartea-Jaimungal "signal-driven"; academic SOTA; Sirignano-Cont, Kolm-Ritter.
- **Why**: RL-MM papers show toxicity-conditioned shading beats symmetric widening by 20–40% PnL.
- **Effort**: **M** — extend `momentum.rs` with depth-weighted imbalance, wire to reservation_price shift.

### 9. Funding-rate forecast
- **What**: Predict next funding rate from premium index + OI changes; close before funding tick if predicted flip.
- **Why**: Funding every 8h on CEX, 1h on HL. Catching a flip saves 1 tick (often > daily maker PnL).
- **Effort**: **M** — premium-index TWAP + OI delta; gate `funding_arb_driver`.

### 10. Deterministic replay from audit log (bit-exact)
- **What**: Re-run `strategy.step()` on yesterday's events → identical decisions. Requires seeded RNG, logical time, no wall-clock in strategy.
- **Who**: Jump/HRT public talks.
- **Why**: When a live loss happens, "rerun, was it strategy or exchange weirdness". Post-mortem unlock.
- **Effort**: **L** — audit every `SystemTime::now`, `thread_rng`, `HashMap` iter; trait-inject time; replay test.

### 11. Shadow trading / canary parallel run
- **What**: N strategy variants in parallel on live data; variant 0 places real orders; others paper. Compare realised PnL to catch config regressions.
- **Why**: Catches "bad config push" in minutes. Our `ab_split.rs` is traffic-split, not shadow.
- **Effort**: **M** — extend `ab_split` with shadow variant routing to `paper.rs`.

### 12. Per-order latency histograms (microsecond)
- **What**: HdrHistogram of decide-to-send, send-to-ack, ack-to-fill, wire RTT per venue.
- **Why**: Latency regressions are silent PnL killers. 20 ms RTT increase on Binance measurably lowers fill rates.
- **Effort**: **S** if we're on `metrics-rs` — add histograms at entry/exit points.

### 13. Historical tick-database for research
- **What**: Tick-level L2 + trades in kdb+/QuestDB/ClickHouse, symbol+time partitioned.
- **Why**: Hypothesis iteration speed. "Fill rate vs queue position by hour-of-day, last 3 months" should be a query.
- **Effort**: **L** — QuestDB ILP path simplest.

### 14. Iceberg order detection
- **What**: Detect counterparty iceberg by repeated same-price prints; avoid quoting inside detected icebergs.
- **Why**: Direct adverse-selection defence.
- **Effort**: **M** — `iceberg_detector.rs` in `strategy/features`, feed to `toxicity`.

### 15. Cross-symbol hedging with basket beta (verify live wiring)
- **What**: Hedge BTC-PERP inventory partly with ETH-PERP by realised beta. We have `hedge_optimizer` (Markowitz) — confirm it's live-wired.
- **Effort**: **M** — audit, extend if offline.

### 16. WS sequence-gap detection and resync (verify)
- **What**: `U`/`u` seq on Binance depth; Bybit V5 `seq`; HL channel sequence. On gap → force snapshot reload.
- **Effort**: **S–M** — depends on current state; grep `book_keeper` first.

## NICE-TO-HAVE

### 17. Transformer / RL alpha signals
- Transformer on L2 (Zhang 2022 "Deep-LOB"), PPO quote placement (Spooner 2020).
- Marginal alpha above classical OFI/micro-price; high productionisation cost.
- Effort: **XL** — training, ONNX/`burn`/`candle` serving, latency budget, drift monitoring.

### 18. MEV-aware execution on EVM DEXes
- Private mempool / Flashbots bundles, slippage-optimal cross-pool routing.
- Skip until we add an EVM DEX.

### 19. Co-location
- AWS Tokyo (Binance), Singapore (Bybit), Zurich (Deribit).
- If not already: **S** (change region).

### 20. Telegram → ChatOps parity (PagerDuty / Opsgenie)
- Severity-routed paging, escalation, on-call.
- **S** — add PagerDuty webhook.

### 21. UI: per-order lifecycle drilldown
- Click order → NEW → ACK (latency) → PARTIAL → CANCEL with timestamps + venue seq.
- We already log this to audit; just a view.
- **M**.

### 22. CCXT-style unified symbol normalisation
- Canonical `BASE/QUOTE:SETTLE`; avoid `BTCUSDT` vs `BTC-USDT` vs `BTC/USDT:USDT` bugs.
- **S** — `SymbolRegistry` in `common`.

## Items already present

`lookahead` detector, `stress`, `paper.rs`, `audit` + HMAC + hash chain, `kill_switch` (5-level), `portfolio_var`, `hedge_optimizer`, `var_guard`, `lead_lag_guard`, `news_retreat`, `otr`, `sla`, `inventory_drift`, `reconciliation`, `circuit_breaker`, `toxicity` (VPIN/Kyle), `learned_microprice`, `cks_ofi`, `cartea_spread`, `market_resilience`, `per_client_circuit`, `volume_limit`, `ab_split`. Hummingbot lacks most of these.

## Recommended order of attack (next 2 weeks)

1. STP flags (S) — same-day
2. Venue-native OCO on Binance (M) — removes single biggest "process-down" risk
3. Queue-position in live (M) — biggest live-vs-backtest alpha leak
4. Drop-copy FIX/WS (M) — makes fill loss impossible
5. Fee-tier poller + strategy awareness (S)
6. Tick/lot rounding proptest audit (S)
7. Quote shading from depth-weighted imbalance (M) — real alpha
8. Deterministic replay (L) — biggest operational unlock once landed
9. Funding forecaster (M)
10. QuestDB tick storage (L)

## Caveats

- Competitor feature list from training data; verify before committing time — especially Wintermute/Jump/GSR paraphrases.
- Items 14–16 may already be implemented; grep `seq_gap`, `iceberg_detector`, `basket_hedge` first.
