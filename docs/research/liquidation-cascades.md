# Liquidation Cascades — Public Investigation Reference

> ⚠⚠⚠ **Pentest reference only.** This document catalogues public
> forensic write-ups of real liquidation-cascade events so that
> exchange operators pentesting their own surveillance stack can
> reproduce the observable attack shape. Running any of the
> attack patterns described here against a venue you do not own
> or are not explicitly authorized to pentest is market
> manipulation under MAR Article 12 (EU), CEA §9(a) (US), MiCA
> Article 92 (EU), and a ToS violation at every exchange we are
> aware of. Read `docs/guides/pentest.md` first.

## What a liquidation cascade is

Perpetual-futures venues force-liquidate positions whose margin
falls below a maintenance threshold. A cascade is what happens
when one liquidation's market-sell order pushes the price far
enough that it triggers the *next* tranche of over-leveraged
positions, which force-sells again, etc. The slope of the
cascade is dominated by:

1. **Book depth per bps band** — thin books mean a few-$MM of
   force-sells slide price tens or hundreds of bps.
2. **OI density per price level** — a cluster of leveraged
   positions all with liquidation near the same price is the
   cascade fuel.
3. **Cross-venue propagation** — many perps are priced off an
   index built from multiple spot venues. Pushing the lowest-
   liquidity index constituent shifts the perp mark, triggering
   liquidations on venues that never saw the spot flow directly.

The attack pattern is to identify (2) and (3), build an
asymmetric exposure that profits from the cascade, then
mechanically light the fuse.

## Documented events

### 2021-05-19 — BTC flash crash (Kaiko / Glassnode reports)

- ~$10 B in perp liquidations across Binance / FTX / Bybit /
  OKX in ~20 minutes as BTC fell from $42 k to $30 k.
- Kaiko post-mortem: the initial 3 % drop was absorbed by spot
  buying; the cascade started when a concentrated set of
  Binance perp longs at 25× leverage hit their liq band, and
  Binance's auto-liquidation filled into a thinner-than-normal
  book because Asia sessions were still sparse. The resulting
  mark move dragged FTX and OKX indexes along; their own
  leverage was higher on average (up to 50× on FTX then), so
  those venues cascaded harder.
- Public references:
  - Kaiko Q2 2021 report on perp cascades
  - Glassnode "Capitulation & Reset" (24 May 2021)
  - CoinMetrics State of the Network Issue 101

### 2022-06 — LUNA / UST death spiral

- Not a classic perp cascade but a related long-squeeze
  pattern: UST depeg → spot LUNA dumps → LUNA-perp longs
  blown out → feedback into further LUNA spot selling via
  the mint/burn mechanism.
- Chainalysis + Nansen published wallet traces showing the
  attacker wallets had shorted LUNA perp AND opened UST short
  exposures concurrently — profit was captured on BOTH legs
  of the cascade, not just one.

### 2022-11 — Alameda / FTX leverage-squeeze discovery

- Court filings revealed FTX ran proprietary concentration
  heatmaps internally that Alameda could query. Analogous
  public data now partially available via CoinGlass
  `liquidation-heatmap`.
- The pattern: Alameda would accumulate a short perp, push
  spot down via a low-liquidity venue, watch the long
  cascade, then cover. SBF testimony + bankruptcy filings
  document several "manually triggered cascades" during late
  2022 distress windows.

### 2026-04 — RAVE / SIREN / MYX (ZachXBT)

- Spot-side cycles rather than perp-cascade specifically but
  the offensive playbook is the same: operator builds
  concentration, pushes via a low-liquidity venue, collects
  in the cascade. See `docs/guides/pentest.md` § "Exploit
  suite" for the paired defensive tooling.

## Attack shape — data the operator needs

The RAVE / 2021-BTC / Alameda patterns all share the same
observable inputs:

| Input | Where it comes from | What we call it |
| --- | --- | --- |
| Current mid | Connector WS (L1/L2 book) | engine-local |
| Own / observed liquidations | `MarketEvent::Liquidation` | `LiquidationHeatmap` |
| Long/Short ratio of retail | Binance / Bybit `/long-short-ratio` | `Signal.LongShortRatio` |
| Average leverage estimate | Operator config (no public API) | `Signal.LiquidationLevelEstimate.avg_leverage` |
| Expected cluster distance | Derived from mid + avg_leverage | `Signal.LiquidationLevelEstimate` |
| Cascade-complete trigger | LiquidationHeatmap total > threshold | `Signal.CascadeCompleted` |

## Defensive takeaways (always-on, not restricted)

An honest MM running near a concentration cluster should:

- Widen spreads when `Signal.LongShortRatio.ratio > 1.5 OR
  < 0.67` — crowd-one-sided books revert more.
- Reduce size when `Surveillance.LiquidationHeatmap.nearest_*`
  is within 100 bps of mid — the book you're quoting into can
  evaporate.
- Pause maker quoting for `min_idle_secs` after
  `Signal.CascadeCompleted = true` — fill rates spike with
  adverse selection during a cascade.

These are non-restricted signals. A legitimate MM graph can
reference all four sources without `MM_ALLOW_RESTRICTED=yes-pentest-mode`.

## Offensive components (RESTRICTED — pentest only)

- `Strategy.LiquidationHunt` — crossing push to trigger a
  heatmap cluster (Sprint 10 / R4.3).
- `Strategy.LeverageBuilder` — asymmetric exposure setup
  (Sprint 10 / R4.5, set_leverage wired in Sprint 12 R6.2).
- `Strategy.CascadeHunter` — gated one-shot trigger (this
  sprint / R7.4).
- `Strategy.CampaignOrchestrator` — multi-phase timeline
  (Sprint 12 / R6.1 real FSM).

## Bundled pentest template

`pentest-liquidation-cascade` wires the full loop end-to-end:

```
  Signal.LiquidationLevelEstimate.long_liq_bps ─┐
                                                 ├─► Strategy.CascadeHunter ─► Out.Quotes
  Signal.LongShortRatio.ratio ─► Cast.ToBool ───┘

  Surveillance.RugScore ─► Cast.ToBool(≥0.5) ─┐
                                               ├─► Out.KillEscalate(L4)  (self-guard)
  Math.Const(4) ─────────────────────────────── ┘
```

Deploy it only on a venue you are explicitly authorized to
pentest. The graph's `MM_ALLOW_RESTRICTED=yes-pentest-mode` gate plus the
loud `tracing::warn!` on every restricted compile is the
minimum audit trail; add operator-side logging per your
venue's compliance framework.

## Future research items

- **Cross-venue cascade** — triggering a perp cascade via a
  spot push on a different venue. Requires cross-venue
  coordination from one graph (`Out.VenueQuotes` already
  exists; orchestration across sub-graphs is TBD).
- **Funding-rate weaponization** — using extreme funding to
  force leverage unwinds without needing a price push. Open
  question: is this achievable without large counterparty
  inventory?
- **Index-composition gaming** — moving a weighted index by
  pushing its thinnest constituent. Needs per-index metadata
  source nodes the venue would have to expose.
