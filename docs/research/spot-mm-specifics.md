# Spot Market Making — specifics for implementers

**Status:** canonical reference
**Created:** 2026-04-14

This document is **required reading** before touching any spot or
cross-product code. Spot MM and perp MM look identical at 30,000 ft
— you quote both sides, capture spread, manage inventory. Every
assumption underneath that surface is different. Read all 15
sections before modifying connectors, risk guards, or strategies
that touch spot symbols.

Where a section has a direct code implication it is called out at
the bottom of the section with the exact file path. Grep for that
path before changing the matching subsystem so you know what the
existing code assumes.

---

## 1. Fee & rebate structure

### Perp venues
Revenue splits three ways: `maker_fee`, `taker_fee`, and funding.
Funding settles every 8h on Binance, Bybit, and HyperLiquid; hourly
on OKX and dYdX. Maker rebates are typically small — HL perps is
`−1.5 bps`, Bybit linear is `−2 bps`, Binance USDⓈ-M is `−2 bps` for
VIP 0 and up to `−5 bps` at top VIP tiers.

### Spot venues
No funding. VIP tier maker rebates are significantly richer. Concrete
numbers as of late 2025 (verify before production — these drift):

| Venue | Tier | Maker rebate | Taker fee |
|---|---|---|---|
| Binance spot | VIP 0 | +10 bps | +10 bps |
| Binance spot | VIP 9 | **−5 bps** | +1.2 bps |
| Coinbase Advanced | VIP 8 | **−6 bps** | +1 bps |
| Kraken Pro | Tier 9 | **−2 bps** | +1 bps |
| Bybit spot | VIP Pro | **−1 bps** | +2 bps |
| HyperLiquid spot | VIP tier | 0 bps | +3.5 bps |

At high volume tiers the rebate is a **primary revenue stream**, not
a rounding error. A spot MM at VIP 9 on Binance captures 5 bps per
filled lot as pure rebate income, before any spread capture. That's
`$50 per $1M traded` — and a serious spot MM pipes $100M/day.

### Implication for our code
- `PnlTracker` at `crates/risk/src/pnl.rs` holds a single `maker_fee`
  + `taker_fee` pair. Correct for single-product engines but the
  aggregated dashboard tracks rebate income **separately per
  product type** so the operator can see whether spot rebates or perp
  spread capture is actually paying the rent.
- `mm-portfolio` exposes per-product PnL rollups. When a `Portfolio`
  instance is wired to the engine, rebates become a Prometheus gauge
  (`mm_portfolio_realised_pnl{currency}`) in addition to the existing
  per-symbol `mm_pnl_rebates`.

---

## 2. Settlement

### Spot is T+0 with actual asset delivery
A filled buy moves wallet `base` up and wallet `quote` down. The
exchange does **not** track a "position" — there is only the wallet
balance. Starting from zero BTC, a sell is either:
- **Margin borrow** — the venue lends you the asset, charges interest,
  and you owe it back. This is the `/sapi/v1/margin/*` API on Binance
  and is a completely separate risk model.
- **Outright rejected** — if you don't have margin enabled the order
  simply fails with "insufficient balance".

### Perp is cash-settled
Fills do not move wallet balances directly. They move `positions`.
Unrealised PnL accrues against `margin`. Realised PnL is a margin
credit/debit at the moment of close. Wallet balance changes only
happen on funding (every 8h) and margin-mode transfers.

### Implication for our code
Our engine derives inventory from `InventoryManager::on_fill` at
`crates/risk/src/inventory.rs`. On perp this matches the `positions`
API. On spot we must **also** reconcile against `get_balances`
because the wallet is the ground truth — if we miss a `Fill` event,
the wallet still moved and the next reconciliation cycle catches the
drift. The engine's reconciliation loop in
`crates/engine/src/market_maker.rs::reconcile` calls
`refresh_balances` on every cycle for exactly this reason.

---

## 3. No leverage, no liquidation

Spot positions are bounded by wallet balance. Maximum loss on a long
position held to zero is the capital spent — no liquidation cascade,
no maintenance margin, no position-mode concept. You either hold the
asset or you don't.

### Implication for our code
`KillSwitch` at `crates/risk/src/kill_switch.rs` uses:
- L2 `StopNewOrders` when `max_position_value` is breached
- L3 `CancelAll` on `max_consecutive_errors`
- L4 `FlattenAll` when `daily_loss_limit` is breached

All three levels are meaningful on spot but mean **different things**
from perp:
- `max_position_value` on spot is an **allocation cap** (don't put
  more than $X in this symbol), not a liquidation guard.
- `FlattenAll` on spot is a sell of the wallet balance, not a
  position close. If the spot book is thin, flatten may take minutes
  of TWAP slicing. Acceptable for spot because you can't get
  liquidated out of a long position — you just hold until you decide
  to sell.

---

## 4. Delta-neutrality is not free on spot

### Perp
Going flat means closing the position — one trade, no borrow.

### Spot-only
Going "short" requires either:
1. **Margin trading** — borrow the base asset, pay interest, sell it.
2. **A paired derivative** — short a perp with the same underlying.
3. **Running long-biased** — accept that you are always net long and
   manage inventory risk through skew, not shorts.

Most real-world spot MMs run long-biased. The asymmetry is built
into the strategy: the bid side skew is steeper than the ask side
skew because accumulating more inventory is the natural drift and
you want to pay for it with a better entry price.

### Implication for our code
`inventory_skew.rs` already applies quadratic skew on the quote side
based on `net_inventory`. On spot this is the **only** tool — there
is no "just hedge it" escape hatch unless a paired connector exists.

The `cross_exchange` and `xemm` strategies assume venue-A / venue-B
parity: **both legs trade the same product type**. Venue-A spot
against venue-B spot is fine (two spot books, same asset). Venue-A
spot against venue-B perp is a **basis trade** — different math,
different risk profile, different unwind rules. `BasisStrategy` and
`FundingArbExecutor` in `crates/strategy/` are the code paths that
handle this correctly.

---

## 5. Wallet topology

On every major venue, `spot`, `margin`, `USDⓈ-M futures`,
`COIN-M futures`, `options`, and `funding` are **separate
sub-accounts** with separate balances. The API paths differ:

| Venue | Spot | USDⓈ-M Futures | Margin |
|---|---|---|---|
| Binance | `/api/v3/account` | `/fapi/v2/balance` | `/sapi/v1/margin/account` |
| Bybit | `/v5/account/wallet-balance?accountType=SPOT` | `?accountType=CONTRACT` | `?accountType=MARGIN` |
| HyperLiquid | `/info {type: spotClearinghouseState}` | `/info {type: clearinghouseState}` | n/a |

On Bybit V5 the **Unified** account partially consolidates — all
sub-products share one collateral pool — but `category: spot` and
`category: linear` still report distinct balance envelopes in the
response.

Transfers between sub-accounts are **explicit API calls**. An MM
running spot + futures on Binance cannot simply "move BTC from spot
to futures" — it must call `/sapi/v1/asset/transfer` or use the
UI. This matters for our `balance_cache` because a spot buy that
lands in the spot wallet does not make the futures wallet richer.

### Implication for our code
`Balance` at `crates/common/src/types.rs` carries a `wallet:
WalletType` field. `BalanceCache` keys on `(asset, WalletType)` so
running spot BTCUSDT and USDⓈ-M BTCUSDT on the same engine does not
silently overwrite one wallet's balance with the other. The
regression test `wallet_types_do_not_collide` in
`crates/engine/src/balance_cache.rs` pins this invariant.

---

## 6. Order types

### Spot lacks
- `reduce_only` — no position to reduce; spot orders are outright
  buys/sells against the wallet.
- `position_side` — no long/short position concept; every buy grows
  the balance and every sell shrinks it.
- `trigger_by_mark_price` — there's no mark price on spot, only the
  last trade.
- Funding-fee offset and other perp-specific toggles.

### Spot has
- Native **iceberg** on Binance spot (`icebergQty` param), hidden on
  most perps.
- **OCO brackets** (one-cancels-other stop+takeprofit pair) as a
  first-class order type on Binance spot.
- Server-side `DAY` time-in-force that expires at session close.
- `STOP_LOSS_LIMIT` and `TAKE_PROFIT_LIMIT` as separate order types,
  distinct from the perp "trigger order" concept.

### Implication for our code
`NewOrder` at `crates/exchange/core/src/connector.rs` is a minimal
struct — no `reduce_only`, no `position_side`, no `iceberg_qty`.
**That's the right minimum.** Keep it that way.

Product-specific extras should live in a per-product param enum
passed alongside `NewOrder` when the product needs it. Avoid a
kitchen-sink struct — if only one product needs a field, it goes in
the per-product param enum, not the shared order struct.

---

## 7. Listen keys / user data streams (Binance-specific)

Binance spot delivers **execution reports and balance updates** over
a listen-key WebSocket obtained from `POST /api/v3/userDataStream`.
Without this stream, the engine never sees spot fills that arrive
via:
- Manual intervention from the trader
- An RFQ product or OTC deal
- A non-WS-API fallback REST submission from our own engine (when
  the WS API path is degraded)
- A maker rebate adjustment posted after the trade

The WS-API trader at `ws_trade.rs` delivers responses to its own
`place_order` requests — that's a request/response envelope, not a
user-data stream. If an order is placed via REST fallback, the fill
never flows through the engine unless the user-data stream is
running.

USDⓈ-M futures has the same pattern but with different paths:
`POST /fapi/v1/listenKey` and
`wss://fstream.binance.com/ws/<listenKey>`.

### Implication for our code
`crates/exchange/binance/src/user_stream.rs` owns the listen-key
lifecycle for both spot and USDⓈ-M futures:
- Obtain via POST.
- Keepalive every 30 minutes via PUT.
- Close via DELETE on shutdown.
- Re-obtain on expiry after 60 minutes.

The module parses `executionReport` → `MarketEvent::Fill` (mapping
the client order id back to a UUID via `OrderIdMap`) and
`outboundAccountPosition` / `ACCOUNT_UPDATE` → `MarketEvent::BalanceUpdate`.
Reconnects automatically on drop or expiry.

---

## 8. Rate limits

Spot and futures on the same Binance account have **separate
rate-limit buckets**:
- Spot REST: **1,200 weight/min**
- Futures REST: **2,400 weight/min**
- Spot orders: 50/10s, 160k/day
- Futures orders: per-sub-account quota, usually 300/10s

Our `RateLimiter` at `crates/exchange/core/src/rate_limiter.rs` is
per-connector. Spot and futures each hold their own connector
instance — `BinanceConnector` and `BinanceFuturesConnector` are two
separate structs in `crates/exchange/binance/` — so the buckets are
correctly independent.

We explicitly avoid multiplexing both products through a single
connector; that would require two limiters in one place and is harder
to reason about.

---

## 9. Asset metadata and symbol normalisation

| Venue | Spot symbol | Perp symbol | Notes |
|---|---|---|---|
| Binance | `BTCUSDT` | `BTCUSDT` (different base URL) | Same string, disambiguated by API path |
| Bybit V5 | `BTCUSDT` | `BTCUSDT` (same URL, `category` param) | Disambiguated by `category` field |
| HyperLiquid | `@0`, `@107` | `BTC`, `ETH` | Spot uses `@N` indices, perp uses coin names |
| OKX | `BTC-USDT` | `BTC-USDT-SWAP` | Suffix disambiguates |
| Kraken | `XXBTZUSD` | `PI_XBTUSD` | Completely different conventions |

### Implication
The engine uses `ProductSpec.symbol` as the venue-native identifier.
That is already the right choice — we never try to normalise across
venues inside the engine. The `VenueProduct` enum in
`mm-exchange-core::connector` lets the connector disambiguate spot
vs perp **within** a venue.

`InstrumentPair` in `mm-common::types` pairs a spot symbol on
venue-A with a futures symbol on venue-B **without** assuming they're
the same string. That future-proofs us against Kraken-style renames
and OKX's suffix convention.

---

## 10. Liquidity profile

Spot books on BTCUSDT Binance are typically **~3× deeper** at the
top-of-book vs Binance USDⓈ-M during quiet hours but **lag perps by
50–200ms on volatility**. The price discovery happens on perps — the
tape reveals the move first on `wss://fstream.binance.com` and then
propagates to spot 100–150ms later.

### Implications for strategy parameters
- Spot quotes can afford **tighter `min_spread_bps`** during quiet
  regimes because rebates are richer.
- Spot reservation price should be **skewed toward the perp mid**
  during trending regimes — that's what `BasisStrategy` in
  `crates/strategy/src/basis.rs` does via its `shift` parameter.
- Spot VPIN and toxicity scores lag perp VPIN. A trader who is toxic
  on perps will usually trade spot a beat later. Our existing
  `toxicity` module updates per-venue independently; cross-product
  toxicity remains an open research question.

---

## 11. Maker/taker asymmetry on spot

As noted in §1: Binance spot VIP 9 maker rebate is `−5 bps`, taker
fee is `+1.2 bps`. That's a **6.2 bp edge** per maker fill captured
purely from fee tier.

For comparison, spread capture on BTCUSDT at a 2 bp spread is `+1 bp
per fill` (half the spread). So **the rebate is 6× the spread
capture** at top VIP tier. The MM business on spot is primarily a
rebate business, not a spread business, once you're above VIP 7.

### Implication for our code
- `PnlAttribution::rebate_income` in `crates/risk/src/pnl.rs` tracks
  rebate income as a separate line item.
- `mm_pnl_rebates{symbol}` in `dashboard/metrics.rs` exposes it as
  a Prometheus gauge so the operator can see the rebate rate
  directly without having to subtract `total_pnl − spread_pnl`.

---

## 12. Cross-margin vs isolated

Perp venues support `CROSS` (shared margin across all positions) or
`ISOLATED` (per-symbol margin). Affects liquidation math — cross mode
can drag down unrelated positions during a liquidation cascade.
Spot has no analogue.

### Implication
The `ExchangeConnector` trait stays perp-agnostic. Margin mode is a
config knob on the futures connector, never exposed in the shared
trait. Anyone writing a spot-only strategy never sees it.

`BinanceFuturesConnector` currently defaults to one-way position
mode. Hedge mode (holding long and short simultaneously on the same
symbol) is intentionally not wired — basis strategies pair legs
across instruments, not within, so one-way is sufficient.

---

## 13. Withdrawal / deposit flows

Out of scope for the quote-maker engine. Flagged because a spot
inventory manager eventually has to rebalance wallets across venues
(or even across chain layers). A pure quote-maker's job stops at
the venue boundary.

Implementation notes for future work:
- Binance: `POST /sapi/v1/capital/withdraw/apply`
- HL: on-chain signed withdrawal transaction
- Bybit: `POST /v5/asset/withdraw/create`

All three have manual approval queues, whitelisted addresses, and
2FA requirements — not automatable in a production flow without
explicit operator sign-off per withdrawal.

---

## 14. Tax / wash-trade rules

Some spot venues enforce wash-trade restrictions — if you try to
match your own maker order as a taker (same account, opposite
side), the venue rejects the match. Binance spot explicitly rejects
"self-trade" with the `-2021` error code.

### Implication
Our engine already assigns unique cloids via
`Self::uuid_to_cloid(uuid)` so this doesn't hit us in practice —
each place_order goes out with a fresh client id. But if we ever
implement a cross-exchange strategy where both legs are routed
through the same API key on the same venue, the **venue will reject
the second leg** with `-2021`. The `cross_exchange` and `xemm`
strategies assume distinct API credentials per leg for this reason.

Regulatory note: tax-lot accounting for spot (FIFO vs LIFO vs
weighted-average for gains reporting) is a compliance concern, not
a code change. Our `Portfolio` uses weighted-average by default
which is acceptable for most jurisdictions but ask accounting
before trading in a US-taxable entity.

---

## 15. Dust and minimum notionals

| Venue | Spot MIN_NOTIONAL | Perp MIN_NOTIONAL |
|---|---|---|
| Binance | $10 | $5 |
| Bybit | $1 | $5 |
| HyperLiquid | $10 | $10 |
| OKX | $1 | $10 |
| Kraken | $10 | $20 |

`ProductSpec.min_notional` already handles this per-symbol via the
existing `meets_min_notional` method at
`crates/common/src/types.rs`.

The venue-level dust handling (automatic conversion to BNB/USDT
after you accumulate a position below the minimum) is a post-trade
concern and not handled by the engine.
