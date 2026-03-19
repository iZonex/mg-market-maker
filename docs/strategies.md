# Strategies

## Avellaneda-Stoikov

**Paper:** "High-frequency trading in a limit order book" (Avellaneda & Stoikov, 2008)

The foundational optimal market making model. Computes bid/ask prices that maximize expected terminal wealth while penalizing inventory risk.

### Core Formulas

```
reservation_price = mid - q * γ * σ² * (T - t)
optimal_spread = γ * σ² * (T - t) + (2/γ) * ln(1 + γ/κ)
```

**Parameters:**
- `γ` (gamma) — risk aversion. Higher = wider spread, less inventory risk
- `σ` (sigma) — volatility estimate (annualized)
- `κ` (kappa) — order arrival intensity. Higher = tighter spread
- `q` — current inventory (positive = long)
- `T - t` — time remaining in the horizon

**When to use:** General-purpose MM. Good starting point for any pair.

---

## GLFT (Guéant-Lehalle-Fernandez-Tapia)

**Paper:** "Dealing with the Inventory Risk" (Guéant et al., 2012)

Extension of A-S with execution risk and bounded inventory constraints. Uses closed-form solutions with calibrated order flow intensity.

### Core Formulas

```
half_spread = C1 * σ
skew = σ * C2
C1 = (1/(ξδ)) * ln(1 + ξδ/k)
C2 = sqrt((γ/(2Aδk)) * (1 + ξδ/k)^(k/(ξδ)+1))
```

Where A, k are calibrated from `λ = A * exp(-k * δ)` (order arrival intensity as function of quote depth).

**Calibration:** The strategy automatically recalibrates A and k from observed fill depths every 50 fills.

**When to use:** When you have enough fill data to calibrate (>50 fills). More accurate than A-S for active pairs.

---

## Grid

Simple symmetric grid strategy. Places N levels at equal intervals around mid price with basic inventory skew.

**When to use:** Very liquid pairs where you want simple, predictable behavior. Good for testing.

---

## Cross-Exchange

Make on venue A (collect maker rebates), hedge immediately on venue B (pay taker fees).

### How It Works

1. Observe best bid/ask on hedge venue
2. Quote on maker venue with prices that guarantee profit after hedge:
   - `our_ask > hedge_mid + total_fees + min_profit`
   - `our_bid < hedge_mid - total_fees - min_profit`
3. When filled → immediately hedge on the other venue

**When to use:** When you have accounts on 2+ venues with price discrepancies.

---

## TWAP (Time-Weighted Average Price)

Not a market-making strategy per se — used for **execution**. Splits a large order into equal time slices to minimize market impact.

**Used internally by:**
- Kill switch level 4 (Flatten All) — TWAP sells inventory over 60 seconds
- Manual inventory unwinding

---

## Momentum Alpha (Cartea-Jaimungal)

Not a standalone strategy — an **overlay** that shifts the reservation price based on short-term price predictions:

```
adjusted_mid = mid + alpha * mid * time_remaining
```

**Alpha signal combines:**
- Book imbalance (40% weight): `(bid_depth - ask_depth) / total_depth`
- Trade flow imbalance (40% weight): net signed volume over recent window
- Micro-price deviation (20% weight): weighted mid vs simple mid

**Effect:** When alpha is positive (predicted up-move), both bid and ask shift up. This makes you more aggressive buying (filled more) and less aggressive selling.

---

## Auto-Tuning (Regime Detection)

Automatically adjusts strategy parameters based on detected market regime:

| Regime | Gamma | Size | Spread | Refresh |
|--------|-------|------|--------|---------|
| Quiet | 0.8x | 1.2x | 0.8x | 1.0x |
| Trending | 2.0x | 0.5x | 2.0x | 0.5x |
| Volatile | 3.0x | 0.3x | 3.0x | 0.3x |
| Mean-Reverting | 0.6x | 1.5x | 0.6x | 1.5x |

**Detection method:** Variance of returns (high/low volatility) + lag-1 autocorrelation (trending vs mean-reverting).

VPIN toxicity adds an additional spread multiplier on top of regime adjustments.
