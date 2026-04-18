# Spot vs Perpetual Market Making — Production Reference

Research pass April 2026. Target: `mm-market-maker` engine.
Covers economics, microstructure, risk, strategy choice, regulatory
differences. Concrete code-change list at the end.

## Key numbers (2026 venue tariffs)

| Venue / product | Base maker | Top public tier | MM-program rebate | Taker (base) |
|---|---|---|---|---|
| Binance **spot** | 0.1000% | 0.020–0.025% (VIP 5) / 0% (VIP 9) | +25% BNB disc; VIP 9 can be 0 | 0.10% |
| Binance **USDT-M perp** | 0.0200% | 0.000% VIP 9 (negative for MM program) | Negative via MM program | 0.05% → 0.017% VIP9 |
| Bybit **linear perp** | 0.020% | 0.000% at top VIP | up to **−0.015%** (MMIP rebate) | 0.055% → 0.030% |
| OKX **perp** | 0.020% | near zero / negative at LP tiers | negative maker at LP tiers | 0.050% → ~0.015% |
| Deribit **perp** | **−0.025%** (already rebate) | further negative for weekly BTC/ETH | weekly BTC/ETH maker negative | 0.075% |
| Hyperliquid **perp** | 0.0150% | 0.000% at top | up to **−0.003%** at Tier 3 | 0.045% → 0.024% |
| Hyperliquid **spot** | 0.0400% | scales (spot counts 2× toward tier) | aligned-quote +50% rebate | 0.070% |

**Concrete bottom line**: Binance spot retail pays **20 bps round trip**.
That is more than the entire daily realised vol on a stablecoin pair
and about 3–4× the quoted spread on BTCUSDT. **Pure spread-capture on
spot retail is mathematically dead.**

Binance USDT-M perp retail is **7 bps round trip**. Deribit perp maker
is already **−2.5 bps rebate without an MM agreement** — that is why so
many delta-hedged options books hedge on Deribit perp.

## Economics detail

### Funding rate (perp only)

- Paid every 8h on Binance/Bybit/OKX/Hyperliquid.
- Formula: `F = Premium Index P + clamp(I − P, −0.05%, +0.05%)`. Cap
  per settlement typically ±0.75%. Position at snapshot pays or
  receives `notional × F`.
- Historical: BTC/ETH funding over last 3 y ~0.01%/8h average (≈11%
  APR), spikes to 0.30%/8h in euphoria, negative 0.05%/8h in
  capitulations.
- For an MM holding directional inventory on a perp hedge leg, funding
  P&L is a **first-class** attribution line, not a footnote.

### Basis / funding arb threshold

- Amberdata / Chainstack quote a practical threshold of
  **≥ 0.11% / 8h** with maker orders to beat round-trip fees.
- He-Manela-Ross (arXiv 2212.06888, 2024): **Sharpe 1.8 at retail
  cost; 3.5 at MM cost** on perp basis arb. ~2× uplift comes
  entirely from the fee side.

### Capital efficiency

- Spot leg = 100% notional locked in base asset (earn
  staking/lending 0–5% APR minus borrow to short).
- Perp leg at 5× cross-margin = 20% notional as IM.
- Delta-neutral MM on perp hedge side is **4–10× more capital-
  efficient**, at the cost of liquidation + funding + venue-custody
  risk.

## Microstructure differences

- **Volume ratio**: 2025–2026 market = perp 3.4× spot (Mar 2026
  briefly 4×). Extreme moments: 20–46× spikes. Price discovery now
  happens on perp; spot frequently lags.
- **Tick size**:
  - Binance BTCUSDT spot = $0.01
  - Binance BTCUSDT perp = $0.10 (raised 2022)
  - Binance ETHUSDT perp = spot tick = $0.01
- **Typical quoted spread** on perp = 1–3 ticks; alpha per tick much
  bigger than on spot where tick constrains the book and queue
  position dominates.
- **GLFT `k, A` calibration must be per-venue per-product** — cross-
  product copy-paste gives wrong optimal spread.
- **Depth**: Binance spot BTC $15–25M / 2 % of mid; perp ~$30–60M
  same band. 30–40% of visible depth is spoofy (cancels in <200 ms
  when price approaches).
- **Queue turnover**: perp books churn faster because HFT
  arbitrageurs continuously rebase perp to spot on funding drift.
  Amend (P1.1 in our engine) matters more on perp; stale perp quotes
  are picked off in <1 s.
- **Toxicity**: informed flow heavier on perp (leverage + systematic
  models live there). VPIN / Kyle's λ run hotter on perp — our
  toxicity response should auto-widen ~1.3–1.5× more aggressively.

## Risk

- **Spot inventory**: never expires, no funding, no liquidation —
  but capital locked, custody risk if held on-exchange, **no easy
  short** without a borrow channel.
- **Perp inventory**: no custody, instant short, but carries (a)
  funding P&L drift, (b) liquidation if margin depletes, (c)
  cross-margin contagion — a loss on A can liquidate B.
- **Liquidation cascades** (March 2020 / May 2021 / Aug 2024 / Feb
  2025): cross-margin MMs saw all positions liquidated on one
  concentrated loss in a correlated crash. **Recommend: run perp
  MM in isolated margin per symbol unless a live cross-asset
  hedge** — which our `hedge_optimizer` does provide. If trusted,
  cross-margin delivers 30–50% capital-efficiency uplift.
- **Leverage decision**: for a market-neutral MM, never use
  leverage *directionally*. Use leverage only to (a) free capital
  for more symbols, (b) post margin on hedge legs.
- **Kill-switch L4 stress**: a flatten that sends taker orders into
  a thin book can itself trigger own-account liquidation — must be
  tested.
- **Delta-hedging asymmetry**:
  - long-spot + short-perp = clean
  - short-spot + long-perp = requires borrow (operationally
    harder, 5–20% APR + recall risk)
  - XEMM / cross-exchange **should default to perp-short** as
    hedge leg, not spot-short.

## Strategy choice — mapping onto our crates

| Strategy | Spot-only | Perp-only | Both | Notes |
|---|---|---|---|---|
| `avellaneda` | fees ≤ 2 bps | ✓ | both | Retail spot fees = unprofitable |
| `glft` | same | ✓ (smaller tick = better approx) | both | Recalibrate `k, A` per venue/product |
| `grid` | ranging spot | avoid on perp (funding swamps grid income) | spot-pref | |
| `basis` | — | — | **both required** | Spot + perp pair |
| `cross_exchange` | venue-A spot vs venue-B spot | A-perp vs B-perp | both | |
| `xemm` | **make-spot, hedge-perp = production default** | same | both | Our crate structurally aligned with Hummingbot #4900 |
| `funding_arb` / driver | — | perp both legs or perp + spot | both | Threshold per pair, count transfer latency |
| `stat_arb` | spot pairs | perp pairs | both | Keep venue homogeneous per pair |
| `paired_unwind` | needed on non-trivial inventory | needed on perp margin | both | |

**Pure spread-capture rule**: target product where
`base_maker_rebate + expected_queue_fill_edge ≥ realised_vol × inventory_half_life`.
On retail Binance spot this is almost never true. On Binance/Bybit
perp with MM program it is true for BTC, ETH, top-10 pairs
continuously.

## Regulatory / operational

- **MiCA** (EU, full deadline Jul 1 2026) applies to spot crypto-
  asset service providers. Derivatives fall under **MiFID II** —
  older, better understood, with explicit market-maker regimes
  (designated MM agreements, presence obligations, quote-size
  rules). Our `risk/sla` per-minute presence maps cleanly onto
  MiFID II MM obligations.
- **Geographic**: US persons blocked on Binance/Bybit/OKX/HL perp.
  US-accessible perp = CME, Kraken Derivatives, Coinbase
  International, CBOE Digital. Spot broadly accessible except
  sanctioned jurisdictions. If we onboard US clients via
  `admin_clients`, flag `perp: false` per jurisdiction.
- **Settlement**: spot T+0 at match. Perp accrues funding every 8h
  + marks-to-market continuously. PnL accounting must differentiate
  realised spot, realised perp, funding accrual, unrealised mark.
- **Custody**: spot = bearer crypto on-exchange (FTX-class risk).
  Perp = margin-book entries only, no wallet to lose. Our
  `persistence/transfer_log` should minimise idle spot balances.

## 10 concrete engine changes required for full perp support

1. **Config schema** — add `product: {spot, linear_perp, inverse_perp}` per symbol. Wire into `common::config`. Drive fee table + margin flags off it.
2. **Fee model** — allow `maker_fee < 0` (rebate). Audit `risk/pnl` for implicit `>= 0` assumptions.
3. **Funding** — add `FundingLeg` to position tracker in `portfolio`. Accrue per-second (mark-style) or at exact 8h snapshots. Produce `funding_pnl` attribution + Prometheus gauge.
4. **Liquidation guard** — before perp order place, `risk/kill_switch` margin-ratio check. L1 spread-widen should trigger when margin utilisation > 50%.
5. **Basis / funding dispatchers** — already present; expose `min_funding_bps_per_8h = 15`-style threshold keys per pair + per-venue borrow-cost table.
6. **Hedge direction default** — XEMM / cross-exchange hedge leg = perp short. Flag in `ClientConfig`.
7. **Margin mode** — `margin_mode: {isolated, cross}` per symbol. Default isolated unless `hedge_optimizer` wired.
8. **Toxicity sensitivity** — multiply VPIN / Kyle widen factor by `1.3–1.5` on perp. Config; tune from backtest.
9. **Borrow cost** (`risk/borrow`, already present) — applies only to spot-short leg.
10. **Regulatory routing** — `admin_clients` blocks `perp` products for `jurisdiction ∈ {US, …}` at API layer.

## Bottom line for production

- **Spot MM alone** only works on an MM program or VIP 5+. Retail
  10 bps kills it. Best targets: stablecoin pairs, highly ranging
  alts, venues with rebates (Binance MM, OKX LP).
- **Perp MM alone** is economically most attractive — every top
  venue pays makers (Deribit outright, others via MM programs).
  Loads funding / liquidation / margin-contagion risk.
- **Spot maker + perp hedge (XEMM / basis)** is the dominant
  institutional setup. Capital-efficient, delta-neutral, funding
  as extra carry. Our engine is already structured for this
  (`xemm`, `basis`, `funding_arb`, cross-venue transfer).

## References

- [Binance spot fees](https://www.binance.com/en/fee/spotMaker)
- [Binance USDⓈ-M futures fees](https://www.binance.com/en/fee/futureFee)
- [Bybit MMIP](https://www.bybit.com/en/help-center/article/Introduction-to-the-Market-Maker-Incentive-Program)
- [OKX fees](https://www.okx.com/en-us/fees)
- [Deribit fees](https://www.deribit.com/kb/fees) / [Deribit market-structure analysis](https://insights.deribit.com/market-research/maker-taker-fees-on-crypto-exchanges-a-market-structure-analysis/)
- [Hyperliquid fees](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees)
- [He, Manela, Ross 2024 — *Fundamentals of Perpetual Futures* (arXiv 2212.06888)](https://arxiv.org/pdf/2212.06888)
- [Guéant, Lehalle, Fernandez-Tapia — arXiv 1105.3115](https://arxiv.org/abs/1105.3115)
- [Guéant 2016 — *Optimal Market Making* (arXiv 1605.01862)](https://arxiv.org/pdf/1605.01862)
- [Cartea, Jaimungal papers index](https://sites.google.com/site/alvarocartea/home/papers)
- [Amberdata — Funding Rate Arbitrage Guide](https://blog.amberdata.io/the-ultimate-guide-to-funding-rate-arbitrage-amberdata)
- [CoinGlass — perp/spot volume ratio](https://www.coinglass.com/pro/perpteual-spot-volume)
- [Hummingbot XEMM strategy](https://hummingbot.org/strategies/v1-strategies/cross-exchange-market-making/) / [Issue #4900 spot→perp hedge](https://github.com/hummingbot/hummingbot/issues/4900)
- [Binance tick size update 2025-12-22](https://www.binance.com/en/support/announcement/detail/1cd08ee9bafd40a2a94e8dfc58408f82)
- [ESMA MiCA hub](https://www.esma.europa.eu/esmas-activities/digital-finance-and-innovation/markets-crypto-assets-regulation-mica)
