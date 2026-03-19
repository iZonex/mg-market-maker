# Professional Crypto Market Making: Comprehensive Research

## Table of Contents
1. [Market Making Algorithms](#1-market-making-algorithms)
2. [Techniques and Tricks](#2-techniques-and-tricks)
3. [Exchange Contractual Obligations](#3-exchange-contractual-obligations)
4. [Risk Management](#4-risk-management)
5. [Market Microstructure](#5-market-microstructure)
6. [Modern Features](#6-modern-features)

---

## 1. Market Making Algorithms

### 1.1 Avellaneda-Stoikov (AS) Model

The foundational model for optimal market making under inventory risk.

**Reservation Price:**
```
r(s, q, t, sigma) = s - q * gamma * sigma^2 * (T - t)
```
- `s` = current mid price
- `q` = inventory position (positive = long)
- `gamma` = risk aversion parameter
- `sigma` = volatility
- `T - t` = time remaining in trading session

**Optimal Spread:**
```
delta_a + delta_b = gamma * sigma^2 * (T - t) + (2/gamma) * ln(1 + gamma/kappa)
```
- `kappa` = order book liquidity parameter (estimated from order arrival rates)

**Bid/Ask Prices:**
```
bid = r - (delta_a + delta_b) / 2
ask = r + (delta_a + delta_b) / 2
```

**Practical Parameter Calibration for Crypto:**

| Parameter | How to Calibrate | Typical Range |
|-----------|-----------------|---------------|
| `gamma` (risk aversion) | `gamma_max = (max_spread - min_spread) / (2 * abs(q) * sigma^2)` then `gamma = gamma_max * IRA` where IRA in [0,1] | 0.01 - 1.0 |
| `sigma` (volatility) | Rolling window of recent ticks (e.g., 60-300 seconds of mid-price changes) | Asset-dependent |
| `kappa` (liquidity) | Regression on log order arrival rate vs depth: `log(lambda) = -k*delta + log(A)`, then `kappa = gamma / [exp((spread*gamma - sigma^2*gamma^2)/2) - 1]` | Calibrate from LOB |
| `T` (horizon) | For 24/7 crypto, use recurring cycles (e.g., 1 hour) and reset `t` each cycle | 3600s typical |

**Crypto-Specific Adaptations:**
- Use infinite horizon variant (no session close) -- set `T - t` to a constant
- Add volatility threshold trigger to recalibrate parameters during regime changes
- Implement order amount shape factor `eta = 1 / (total_inventory / IRA)` to scale order sizes
- When `IRA -> 0`, reservation price collapses to mid-price, spread becomes `2 * max_spread`

**Limitations in Practice:**
- Assumes constant volatility and Poisson order arrivals -- both false in crypto
- No adverse selection modeling
- No multi-asset consideration
- Requires significant customization to work in real markets

### 1.2 Gueant-Lehalle-Fernandez-Tapia (GLFT) Model

Extension of AS with closed-form solutions and inventory boundary conditions.

**Optimal Bid Depth:**
```
delta_b(q) = c1 + (Delta/2) * sigma * c2 + q * sigma * c2
```

**Optimal Ask Depth:**
```
delta_a(q) = c1 + (Delta/2) * sigma * c2 - q * sigma * c2
```

Where:
```
c1 = (1 / (xi * Delta)) * ln(1 + xi * Delta / k)
c2 = sqrt(gamma / (2 * A * Delta * k) * (1 + xi * Delta / k)^(k/(xi*Delta) + 1))
```

**Decomposition:**
```
Half Spread = c1 + (Delta/2) * sigma * c2
Inventory Skew = sigma * c2
```

**Final Prices:**
```
Bid = fair_price - (half_spread + skew * q)
Ask = fair_price + (half_spread - skew * q)
```

**Key Parameters:**

| Parameter | Definition | Typical Value |
|-----------|-----------|---------------|
| `sigma` | Volatility (ticks/sqrt(second)) | Measured from data |
| `gamma` | Risk aversion / adjustment parameter | 0.05 |
| `xi` | Risk aversion (= gamma in simplified form) | = gamma |
| `Delta` | Time step | 1 |
| `A` | Trading intensity coefficient | Calibrated from LOB |
| `k` | Trading intensity decay rate | Calibrated: `log(lambda) = -k*delta + log(A)` |

**Trading Intensity Calibration:**
```
lambda(delta) = A * exp(-k * delta)
```
Fit via linear regression on log-transformed order arrival rates at various depths.

**Practical Adjustments:**
```
half_spread_adj = half_spread * adj1  (adj1 ~ 1.0)
skew_adj = skew * adj2  (adj2 ~ 0.05)
```
The skew adjustment factor `adj2` is typically much smaller than 1.0 to prevent over-aggressive inventory management.

**Advantages over AS:**
- No terminal time condition -- suitable for 24/7 crypto
- Closed-form solution (faster computation)
- Explicit inventory bounds (stops quoting when inventory hits max)
- More stable in practice

### 1.3 Optimal Execution Algorithms

These are used for unwinding inventory or executing large orders with minimal market impact.

**TWAP (Time-Weighted Average Price):**
```
order_size_per_interval = total_quantity / num_intervals
schedule: execute order_size_per_interval every (total_time / num_intervals)
```
- Divides total order evenly across time
- Simple, predictable, but ignores volume patterns
- Best when volume data is unavailable or unreliable
- Crypto adaptation: weight by expected volume patterns (higher around US/Asia market hours)

**VWAP (Volume-Weighted Average Price):**
```
target_volume_fraction(t) = historical_volume_profile(t) / total_historical_volume
order_size(t) = total_quantity * target_volume_fraction(t)
```
- Tracks historical volume curve
- Crypto challenge: volume profiles are less predictable than equities
- Recent research uses deep learning to bypass volume curve prediction entirely, directly optimizing VWAP slippage via automatic differentiation
- Implementation: maintain rolling volume profiles per hour-of-day, day-of-week

**POV (Percentage of Volume):**
```
order_size(t) = participation_rate * market_volume(t)
```
- Typically 5-20% participation rate
- Adapts automatically to market conditions
- Risk: in low liquidity, can become dominant flow and move market
- Implementation: track real-time volume and submit proportional orders

**Almgren-Chriss Optimal Execution:**
```
x(t) = X * sinh(kappa * (T-t)) / sinh(kappa * T)
kappa = sqrt(lambda * sigma^2 / eta)
```
- `X` = total quantity, `lambda` = risk aversion, `eta` = temporary impact
- Balances market impact cost vs timing risk
- Front-loads execution when risk aversion is high

### 1.4 Statistical Arbitrage Components

**Pairs Trading / Cointegration:**
```
spread(t) = price_A(t) - beta * price_B(t) - mu
z_score(t) = (spread(t) - mean(spread)) / std(spread)
```
- Test for cointegration using Engle-Granger two-step or Johansen test
- Entry signal: |z_score| > 2.0 (trade mean reversion)
- Exit signal: |z_score| < 0.5 (spread reverted)
- Crypto examples: BTC/ETH, stablecoin pairs, same-asset cross-exchange
- BTC-ETH pairs trading historically shows ~16% annualized return, Sharpe ~2.45

**Cross-Exchange Arbitrage:**
```
profit = (bid_exchange_A - ask_exchange_B) - fees_A - fees_B - transfer_cost
```
- Exploit price discrepancies across venues
- Must account for: transfer latency, withdrawal fees, deposit confirmations
- Perpetual funding rate arbitrage: long spot + short perp (or vice versa)

**Triangular Arbitrage:**
```
rate_implied = (BTC/USDT) * (ETH/BTC)
profit = rate_implied - (ETH/USDT) - fees
```

### 1.5 Mean Reversion Models

**Ornstein-Uhlenbeck Process:**
```
dS = theta * (mu - S) * dt + sigma * dW
```
- `theta` = mean reversion speed
- `mu` = long-term mean
- `sigma` = volatility
- Estimate parameters via MLE or method of moments
- Half-life of mean reversion: `t_half = ln(2) / theta`

**Application to Market Making:**
- Estimate fair value as OU mean
- Set spread width inversely proportional to mean reversion strength
- Stronger mean reversion -> tighter spreads (less directional risk)
- Weaker mean reversion -> wider spreads

### 1.6 Inventory Management Beyond Basic Skew

**Dynamic Order Sizing:**
```
order_size(q) = base_size * exp(-alpha * |q| / max_inventory)
```
- Exponentially decrease order sizes as inventory grows in one direction
- Prevents accumulating more in the direction of existing exposure

**Asymmetric Quoting:**
- When long: tighten ask spread, widen bid spread
- When short: tighten bid spread, widen ask spread
- Magnitude proportional to inventory / max_inventory

**Inventory Penalty Function:**
```
penalty(q) = gamma * q^2 * sigma^2 / 2
```
- Quadratic penalty discourages large positions
- Added to the objective function in optimization

**Hard Inventory Limits:**
- Stop quoting on one side when inventory hits max
- GLFT approach: hard boundary conditions on position
- Soft approach: exponentially increase spread on one side

**Directional Bets with Inventory Control:**
- Gueant et al. (2012) extended framework allows directional bets while controlling inventory risk
- Non-martingale framework: shift reservation price by alpha based on signal
```
r_adjusted = r + alpha * signal_strength
```

---

## 2. Techniques and Tricks

### 2.1 Order Flow Toxicity Detection (VPIN)

**VPIN (Volume-Synchronized Probability of Informed Trading):**

VPIN measures the probability that incoming order flow is from informed traders.

**Calculation:**
1. Divide trading activity into equal-volume buckets (not time buckets)
2. For each bucket, classify trades as buy-initiated or sell-initiated (using tick rule or Lee-Ready)
3. Calculate volume imbalance per bucket:
   ```
   V_imbalance(n) = |V_buy(n) - V_sell(n)|
   ```
4. VPIN over rolling window of N buckets:
   ```
   VPIN = (1/N) * sum(V_imbalance(n)) / V_bucket_size
   ```

**Interpretation:**
- VPIN range: [0, 1]
- VPIN > 0.7: Very high toxicity -- widen spreads significantly or stop quoting
- VPIN > 0.5: Elevated toxicity -- widen spreads
- VPIN < 0.3: Low toxicity -- normal quoting
- VPIN significantly predicts future price jumps in crypto

**Implementation Details:**
- Bucket size: typically 1/50th of average daily volume
- Window: 50 buckets
- Update frequency: every new bucket completion
- Use volume clock (not wall clock) for bucket boundaries

**Trade Classification (Tick Rule):**
```
if price(t) > price(t-1): buy
if price(t) < price(t-1): sell
if price(t) == price(t-1): same as previous classification
```

### 2.2 Kyle's Lambda (Price Impact Measure)

**Estimation:**
```
Delta_P(t) = lambda * SignedVolume(t) + epsilon(t)
```
- Regress price changes on signed order flow
- Higher lambda = less liquid, more information asymmetry
- Lower lambda = more liquid, safer to quote tightly

**Rolling Estimation:**
- Use 5-minute or 15-minute windows
- Recalculate continuously
- Spike in lambda -> widen spreads immediately

**Practical Use:**
- If `lambda` doubles from its 1-hour moving average -> increase spread by 50%
- Track `lambda` per venue to identify which venues have more toxic flow

### 2.3 Adverse Selection Protection

**Techniques:**
1. **Fill analysis**: Track P&L per fill at 1s, 5s, 30s, 60s horizons
   ```
   adverse_selection_cost = avg(mid_price(t+tau) - fill_price) * direction
   ```
   If consistently negative, your quotes are being picked off by informed traders

2. **Fade detection**: Monitor how often price moves against you immediately after fill
   ```
   fade_ratio = count(adverse_moves_within_1s) / total_fills
   ```
   If fade_ratio > 0.6, increase spreads

3. **Toxic flow identification**: Track adverse selection by counterparty (if available), by time-of-day, by order size buckets

4. **Speed bumps**: Implement minimum quote life (don't update quotes for 50-200ms after placement)

5. **Last-look protection**: Cancel orders if price moved against you during latency window

### 2.4 Quote Stuffing Detection

**Indicators:**
- Message-to-trade ratio > 100:1 (abnormal)
- Rapid order submission + cancellation within < 10ms
- Burst of orders concentrated at prices away from BBO
- Sudden increase in order book message rate without corresponding volume

**Protection Measures:**
- Rate-limit quote updates (e.g., max 10 updates/second per price level)
- Implement minimum resting time for own quotes
- Don't react to order book changes that revert within < 100ms
- Use time-weighted order book snapshots rather than raw L2 updates

### 2.5 Latency Arbitrage Protection

**Techniques:**
1. **Stale quote detection**: Monitor price on correlated venues; if reference price moves, cancel/update own quotes within latency budget
2. **Reference price monitoring**: Maintain fast feed from most liquid venue (e.g., Binance BTC/USDT) as reference
3. **Preemptive cancellation**: If BTC moves >X bps on reference venue, cancel all quotes on slower venues immediately
4. **Quote validity windows**: Automatically expire quotes after N milliseconds
5. **Co-location**: Place servers as close to exchange matching engines as possible
   - Binance: AWS Tokyo
   - Bybit: AWS Singapore
   - OKX: Alibaba Cloud Hong Kong

### 2.6 Spread Decomposition

The bid-ask spread can be decomposed into three components:

```
Spread = Adverse_Selection_Cost + Inventory_Holding_Cost + Order_Processing_Cost
```

**Madhavan-Richardson-Roomans (MRR) Model:**
```
Delta_P(t) = theta * x(t) + (phi + theta) * (x(t) - rho * x(t-1)) + epsilon(t)
```
- `theta` = adverse selection component (information asymmetry)
- `phi` = order processing cost
- `x(t)` = trade direction (+1 buy, -1 sell)
- `rho` = serial correlation of order flow

**Application:**
- High `theta` relative to `phi` -> reduce quoting, adverse selection dominant
- Low `theta` -> safe to quote tighter
- Track decomposition over time to detect regime changes

### 2.7 Information Asymmetry Detection

**PIN Model (Probability of Informed Trading):**
```
PIN = (alpha * mu) / (alpha * mu + 2 * epsilon)
```
- `alpha` = probability of information event
- `mu` = informed trader arrival rate
- `epsilon` = uninformed trader arrival rate

**Real-time Signals:**
1. Order flow imbalance sustained for > 5 minutes
2. Large orders appearing on one side only
3. Cross-venue price divergence (informed traders hit cheapest venue first)
4. Unusual options activity (if trading derivatives)
5. Funding rate divergence from historical norm

---

## 3. Exchange Contractual Obligations

### 3.1 Binance

**Spot Market Maker Program:**
- Application requirement: 30-day trading volume > 1,000 BTC equivalent
- Scoring factors: maker volume, spread, depth, trading pair weight, two-sided quoting, order duration (>3 seconds emphasized)
- 2-week grace period with zero maker fees before ranking begins
- Daily performance reviews, weekly ranking reviews
- Contact: mmprogram@binance.com

**Binance Futures Liquidity Provider Program:**
- Maker fee rebates: 0.005% - 0.008%
- Weekly evaluation: maker volume % must be >= 0.20%
- Server location: AWS Tokyo (co-locate here for lowest latency)
- Benefits: higher API rate limits, low-latency connection, tailored configuration, onboarding trial period

**Altcoin LiquidityBoost Program (2025):**
- Tier 1: 0.5% maker volume percentage -> 0.5 bps rebate
- Tier 2: 1.0% maker volume percentage -> 1.0 bps rebate
- Minimum 30-day volume: 20,000,000 USDT equivalent

**VIP Fee Tiers (Spot):**

| Tier | 30-day Volume | Maker Fee | Taker Fee |
|------|-------------|-----------|-----------|
| Regular | < $1M | 0.100% | 0.100% |
| VIP 1 | >= $1M | 0.090% | 0.100% |
| VIP 5 | >= $150M | 0.025% | 0.031% |
| VIP 9 | >= $5B | 0.000% | ~0.017% |

### 3.2 Bybit

**Market Maker Incentive Program:**
- Rebates: 0.0025% - 0.0125%
- Requirements: institutional trading firm, sustain large volumes, API usage >20%
- Multiple pair quoting requirement, minimum order sizes
- Server location: AWS Singapore

**Pro Tiers:**

| Tier | Requirement | Notes |
|------|------------|-------|
| Pro 1 | Application + API >20% | Entry level |
| Pro 6 | >$5B 30-day volume | Highest tier |
| Supreme VIP | >$100M spot or $500M derivatives | 0% maker, 0.03% taker |

**Fee Structure:**
- Standard: 0.10% maker/taker (spot), 0.01% maker / 0.055% taker (futures)
- Best available: -0.015% rebate (market maker program)
- Benefits: higher rate limits, low-latency connection, tailored configuration

### 3.3 OKX

**Market Maker Program:**
- Obligation: provide consistent liquidity to orderbook >50% of time
- Liquidity Index formula: `Liquidity_Index = Liquidity_Multiplier / Best_Bid_Ask_Spread`
- Liquidity multiplier: weighted by order proximity to BBO -- closer orders get higher weight
- Only orders within 30% of best bid/offer are considered
- Rebates: up to 0.005%
- Daily performance reports: traded pairs, liquidity index, rebate amounts
- Server location: Alibaba Cloud Hong Kong

**Fee Structure (Spot):**

| Tier | 30-day Volume | Maker Fee | Taker Fee |
|------|-------------|-----------|-----------|
| Regular | < $5M | 0.080% | 0.100% |
| VIP 1 | >= $5M | 0.045% | 0.050% |
| Higher | Varies | Down to 0.020% | Varies |

### 3.4 Kraken

**Market Participation Program:**
- Invitation-based for largest clients (by volume)
- Participants contributing significantly to liquidity earn Kraken-equity-linked incentives (warrants)
- Specific spread/depth obligations negotiated individually
- Certain spot pairs eligible for maker fee rebates

**Fee Structure:**
- Standard maker fees competitive with other exchanges
- Volume-based tier discounts

### 3.5 Typical DMM Obligations (Industry Standard)

Based on traditional exchange DMM programs and crypto adaptations:

| Obligation | Typical Requirement |
|-----------|-------------------|
| Two-sided quoting | Must maintain both bid and ask simultaneously |
| Maximum spread | 1-3% for majors (BTC, ETH), 3-10% for altcoins |
| Minimum depth | $2,000-$50,000 per side depending on pair |
| Minimum uptime | 50-90% of trading hours |
| Order duration | Orders should rest >3 seconds (Binance scoring) |
| Number of pairs | Typically 5-20 pairs minimum |
| Evaluation period | Weekly or monthly |
| Grace period | 2-4 weeks for new participants |

### 3.6 Penalty Structures

- **Soft penalties**: Loss of rebate tier, reduced to base fees
- **Hard penalties**: Suspension from program, clawback of rebates
- **Performance drift**: Weekly ranking adjustments (Binance model)
- **Typical consequence of missing SLA**: dropped one tier for the next evaluation period

### 3.7 Exchange API Features for Market Makers

| Feature | Binance | Bybit | OKX |
|---------|---------|-------|-----|
| Batch order placement | Yes (up to 5) | Yes | Yes (up to 20) |
| Batch order cancel | Yes | Yes | Yes |
| WebSocket order management | Yes | Yes | Yes |
| Rate limits (MM tier) | 1,200/min+ | Enhanced | Enhanced |
| Co-location available | Via AWS Tokyo | Via AWS Singapore | Via Alibaba HK |
| Dedicated endpoints | VIP only | VIP only | VIP only |
| Self-trade prevention | Yes | Yes | Yes |

---

## 4. Risk Management

### 4.1 Greeks Management for Derivatives

**Delta (directional risk):**
```
portfolio_delta = sum(position_i * delta_i)
```
- Hedge immediately after each fill
- Target: portfolio delta = 0
- Hedge instruments: spot, perpetual futures, quarterly futures
- Hedging frequency: every fill or every N seconds

**Gamma (convexity risk):**
```
portfolio_gamma = sum(position_i * gamma_i)
```
- Long gamma: profit from volatility, lose from theta
- Short gamma: profit from theta, lose from volatility (dangerous in crypto)
- Gamma scalping: dynamically hedge delta to capture gamma P&L
- Rule: never be short gamma more than X% of portfolio value

**Vega (volatility risk):**
```
portfolio_vega = sum(position_i * vega_i)
```
- Monitor implied vs realized volatility spread
- Long vega when IV < RV, short when IV > RV
- Use calendar spreads to manage term structure exposure
- Crypto vega risk is extreme: IV can move 20+ points in minutes

**Theta (time decay):**
```
daily_theta = sum(position_i * theta_i)
```
- Represents "rent" paid/received for option positions
- Long gamma/vega positions have negative theta (cost)
- Short gamma/vega positions have positive theta (income)

**Practical Delta Hedging Workflow:**
1. Fill received on options book
2. Calculate new portfolio delta
3. If |delta| > threshold: execute hedge
4. Prefer hedging with underlying (spot/perps) to avoid adding more Greek exposure
5. Don't hedge with other options unless specifically managing gamma/vega

### 4.2 Cross-Asset Hedging

**Perpetual Futures Hedging:**
```
hedge_size = -inventory * hedge_ratio
funding_cost = position_size * funding_rate * (time / 8_hours)
```
- Long spot + short perp = market neutral (but pay/receive funding)
- Monitor funding rates: if extreme, may need to adjust

**Cross-Asset Correlation Hedging:**
- Hedge altcoin inventory with BTC or ETH futures (beta hedging)
```
hedge_ratio = beta * position_value_altcoin / btc_price
beta = cov(altcoin_returns, btc_returns) / var(btc_returns)
```
- Warning: correlations spike to ~1.0 during crypto crashes, rendering diversification useless exactly when needed most
- Continuously recalculate betas using rolling windows (e.g., 24-hour, 7-day)

**Options as Hedge:**
- Buy OTM puts on BTC/ETH as tail risk protection
- Cost: 1-5% annually for 20% OTM puts
- Particularly valuable around known events (FOMC, halving, major unlocks)

### 4.3 Value at Risk (VaR) Models

**Parametric VaR:**
```
VaR_alpha = portfolio_value * z_alpha * sigma * sqrt(t)
```
- 99% confidence: z = 2.326
- Crypto caveat: returns are fat-tailed, not normal
- Underestimates tail risk significantly

**Historical VaR:**
- Use actual return distribution from historical data
- More accurate for fat-tailed crypto returns
- Require sufficient history (>1 year)

**Conditional VaR (CVaR / Expected Shortfall):**
```
CVaR_alpha = E[Loss | Loss > VaR_alpha]
```
- More conservative: measures average loss in worst cases
- Better for crypto's fat tails
- Use this, not plain VaR

**Monte Carlo VaR:**
- Simulate 10,000+ price paths using fitted distribution
- Use Student-t or GJR-GARCH for realistic tail behavior
- Include correlation structure via copulas

### 4.4 Tail Risk Management

**Circuit Breakers (beyond basic):**
1. **Per-asset**: halt quoting if price moves >X% in Y seconds
2. **Portfolio-wide**: halt all quoting if portfolio VaR exceeds limit
3. **Correlation-based**: halt if cross-asset correlation spikes (systemic risk)
4. **Drawdown-based**: reduce size by 50% at -2% daily drawdown, halt at -5%
5. **Volatility regime**: if realized vol > 3x normal, reduce all positions by 75%

**Tail Risk Hedging:**
- Maintain permanent OTM put positions (1-2% of AUM cost)
- Variance swaps (where available)
- Dynamic hedging: increase hedge ratio as volatility increases
- Pre-position for known events (token unlocks, regulatory announcements)

**Stress Testing:**
- March 2020 crash (-50% BTC in 24h)
- May 2021 crash (-53% from peak)
- FTX collapse (-25% BTC, exchange counterparty risk)
- Luna/UST depeg (correlated collapse, stablecoin risk)
- Simulate these scenarios daily against current portfolio

### 4.5 Correlation-Based Risk

**Dynamic Correlation Monitoring:**
```
rho(t) = rolling_correlation(asset_a, asset_b, window=24h)
```
- During normal times: BTC/ETH correlation ~0.7-0.85
- During crashes: correlation -> 0.95+
- Track correlation of every traded pair vs BTC

**Concentration Risk:**
```
concentration_index = sum(position_i^2) / (sum(position_i))^2
```
- Maximum allocation per asset: 20-30% of total inventory
- Maximum correlated cluster exposure: 50%

---

## 5. Market Microstructure

### 5.1 Queue Position Management

**Why It Matters:**
- In large-tick-size assets (e.g., CRVUSDT with 0.001 tick), most fills happen at BBO
- Queue priority (FIFO) determines who gets filled
- Speed to join queue at a new price level is critical

**Strategies:**
1. **Early queue joining**: Place orders at anticipated price levels before they become BBO
2. **Queue position estimation**: Track your position in the queue
   ```
   estimated_position = orders_ahead_of_you / total_queue_depth
   fill_probability ~= 1 - estimated_position (simplified)
   ```
3. **Avoid unnecessary cancel/replace**: Each cancel-replace puts you at back of queue
4. **Queue fade detection**: If BBO depth drops below threshold (e.g., 250,000 units), back off one tick to avoid being last in queue when price moves through

**Book Pressure Signal:**
```
book_pressure = (best_bid * best_ask_qty + best_ask * best_bid_qty) / (best_bid_qty + best_ask_qty)
```
- Use as microprice / fair value estimate
- More accurate than simple mid-price for large-tick assets

**Grid Strategy for Queue Management:**
- Maintain 10 orders on each side (buy/sell), spaced at tick intervals
- Cancel orders outside current optimal grid each interval
- Replace with new quotes aligned to computed fair value
- Keeps you in queue at multiple levels

### 5.2 Tick-Size Regime Adaptation

**Large Tick Assets (spread = 1 tick most of the time):**
- Queue position is everything
- Speed matters more than price optimization
- Use queue-based models over price-based models
- Reduce cancel/replace frequency
- Performance metric: queue position, fill rate

**Small Tick Assets (spread = many ticks):**
- Price optimization dominates
- AS/GLFT models work well
- Can undercut competitors by 1 tick
- Cancel/replace cost is lower (queue position less important)
- Performance metric: spread capture, adverse selection

**Detection:**
```
spread_in_ticks = average_spread / tick_size
if spread_in_ticks < 2.0: large_tick_regime
if spread_in_ticks > 5.0: small_tick_regime
```

**Tick Size Impact:**
- Too-large tick size -> oversupply of liquidity, long queues, speed arms race
- Too-small tick size -> undercutting, unstable quotes, penny-jumping
- Optimal: 1-3 tick spread in normal conditions

### 5.3 Fee Tier Optimization

**Strategy: Maximize Rebate Tier:**
```
net_spread_capture = gross_spread - taker_fee_if_hedging + maker_rebate
```
- Key insight: maker rebate is effectively part of your edge
- At highest tiers, maker rebate can be 0.5-1.5 bps
- This can be 20-50% of total P&L for tight-spread assets

**Volume Generation for Tier Qualification:**
- Self-trade (wash trade) is prohibited and detected
- Instead: run aggressive strategies on high-volume pairs to build volume
- Accept slightly negative edge on volume-building pairs if rebate tier increase is worth it
- Calculate: `rebate_on_all_volume_at_new_tier - losses_from_volume_building > 0`

**Fee-Aware Spread Calculation:**
```
min_profitable_spread = taker_fee_maker_venue + taker_fee_hedge_venue - maker_rebate
```
- Never quote tighter than min_profitable_spread
- Include all costs: exchange fees, gas (if DeFi), withdrawal fees

### 5.4 Maker Rebate Strategies

**Rebate Capture:**
- At -0.015% rebate (best Bybit), you earn 1.5 bps per fill on the passive side
- Strategy: quote aggressively to maximize fill rate, hedge immediately as taker
- Net P&L = maker_rebate - taker_fee_on_hedge - adverse_selection

**Cross-Venue Rebate Optimization:**
- Different exchanges offer different rebate tiers
- Route passive flow to highest-rebate exchange
- Route aggressive hedges to lowest-fee exchange
- Net: capture spread + rebate differential

---

## 6. Modern Features

### 6.1 Multi-Venue Quoting

**Architecture:**
```
[Market Data Aggregator] -> [Fair Price Engine] -> [Quoting Engine per Venue]
                                                 -> [Risk Manager]
                                                 -> [Inventory Aggregator]
```

**Key Considerations:**
- Unified order book view across all venues
- Cross-venue inventory netting
- Venue-specific spread adjustments (based on fee, latency, toxicity)
- Consolidated position tracking
- Smart allocation of total desired depth across venues

**Latency Budget:**
- Market data: venue -> your server: 1-50ms (co-located: <1ms)
- Internal processing: <100 microseconds (Rust advantage)
- Order placement: your server -> venue: 1-50ms
- Total round-trip: 2-100ms depending on co-location

### 6.2 Smart Order Routing (SOR)

**Decision Logic:**
```
for each venue:
  effective_price = fill_price + fees - rebates
  fill_probability = estimate_from(depth, queue_position, historical_fill_rate)
  latency_adjusted_price = effective_price + lambda * expected_adverse_movement(latency)
  score = fill_probability * (target_price - latency_adjusted_price)
route to venue with highest score
```

**Venue Selection Factors:**
1. Best available price (adjusted for fees)
2. Available depth at that price
3. Historical fill rates
4. Latency to venue
5. Toxicity level of venue (via VPIN per venue)
6. Current fee tier / rebate eligibility
7. Settlement risk

**Implementation:**
- 400+ configurable parameters in production SOR systems
- Automatic parameter tuning using ML (venue weights rebalanced when performance drifts)
- Timeout and retry logic per venue

### 6.3 Real-Time PnL Attribution

**Components:**
```
Total_PnL = Spread_PnL + Inventory_PnL + Rebate_PnL + Hedge_PnL + Funding_PnL
```

| Component | Calculation |
|-----------|------------|
| Spread PnL | `sum(fill_spread / 2)` for each passive fill |
| Inventory PnL | `inventory * (current_price - avg_entry_price)` |
| Rebate PnL | `sum(fill_volume * rebate_rate)` |
| Hedge PnL | P&L from hedge trades (should be ~zero or slightly negative) |
| Funding PnL | Funding payments on perpetual futures positions |

**Time-Based Attribution:**
- Track PnL per second, per minute, per hour
- Identify which hours/sessions are profitable
- Detect when strategies degrade

**Per-Fill Attribution:**
```
for each fill:
  mark_to_market_at = [1s, 5s, 30s, 60s, 300s]
  for tau in mark_to_market_at:
    pnl(tau) = direction * (mid_price(t+tau) - fill_price)
```
- Positive at short horizons = good spread capture
- Negative at short horizons = adverse selection (getting picked off)

### 6.4 Automated Parameter Tuning

**Approaches:**
1. **Genetic Algorithms**: Optimize (gamma, kappa, spread_limits, inventory_limits) using historical backtest
2. **Bayesian Optimization**: Efficient search of parameter space with Gaussian Process surrogate
3. **Reinforcement Learning**: Train agent to adjust parameters based on market state
4. **Grid Search with Walk-Forward**: Exhaustive search on training period, validate on out-of-sample

**RL State Features (from literature):**
- Current inventory level
- Recent volatility
- Order book imbalance
- VPIN / toxicity measure
- Spread relative to historical
- Time of day / day of week
- Recent P&L

**RL Action Space:**
- Spread adjustment: [-5, -3, -1, 0, +1, +3, +5] ticks
- Inventory skew adjustment: [-3, -1, 0, +1, +3] ticks
- Order size scaling: [0.5, 0.75, 1.0, 1.25, 1.5]

**Multi-Objective RL (M3ORL):**
- Objective 1: Maximize P&L
- Objective 2: Minimize inventory risk
- Objective 3: Minimize drawdown
- Use Pareto front optimization instead of weighted sum

**Practical Tips:**
- Start with manual parameters, use ML to fine-tune
- Never let ML change parameters by more than X% from baseline
- Always have human-set hard limits that ML cannot override
- Retrain on rolling window (e.g., weekly)
- Feature importance via Random Forest helps select which LOB features matter

### 6.5 Machine Learning Integration

**Use Cases in Production:**
1. **Fair price estimation**: CNN/Transformer on LOB data (order book imbalance, trade flow) to predict microprice
2. **Volatility prediction**: GARCH + LSTM hybrid for short-term volatility forecasting
3. **Regime detection**: Hidden Markov Model or clustering to identify market regimes (trending, mean-reverting, volatile)
4. **Fill probability**: Logistic regression or gradient boosting to predict probability of order fill at given price/depth
5. **Adverse selection prediction**: Classify incoming orders as informed/uninformed using features (size, timing, venue, speed)
6. **Optimal spread**: Deep RL to learn dynamic spread policy

**Feature Engineering for LOB Models:**
- Order book imbalance at levels 1-5
- Weighted mid-price (microprice)
- Trade imbalance (rolling 1s, 5s, 30s)
- Volume profile (bid/ask depth at each level)
- Price momentum (1s, 5s, 30s returns)
- Volatility (realized vol at multiple horizons)
- Spread (current vs rolling average)
- Queue position at BBO

**Architecture:**
```
Raw LOB Data -> Feature Extraction (handcrafted + CNN)
             -> Ensemble Model (XGBoost + LSTM)
             -> Signal: fair_price_adjustment, spread_adjustment, regime
             -> Strategy Engine (GLFT with ML adjustments)
```

**Caution:**
- Most academic papers use handcrafted features, not end-to-end raw LOB
- End-to-end models (Attn-LOB, DeepLOB) show promise but are harder to deploy
- ML should augment, not replace, the core mathematical models
- Overfitting is the #1 risk: crypto regimes change fast

---

## Key Implementation Priorities for Rust

Based on this research, priority implementation order for a production system:

### Phase 1: Core Market Making
1. GLFT model with configurable parameters (preferred over AS for crypto)
2. Multi-venue connectivity (Binance, Bybit, OKX WebSocket)
3. Basic inventory management (skew + hard limits)
4. Fee-aware spread calculation
5. Real-time PnL tracking

### Phase 2: Risk & Protection
1. VPIN calculation (volume-bucketed)
2. Adverse selection monitoring (per-fill P&L at multiple horizons)
3. Circuit breakers (per-asset, portfolio-wide, volatility-based)
4. Cross-asset hedging (beta hedge vs BTC/ETH)
5. Position limits and drawdown controls

### Phase 3: Advanced Strategies
1. Queue position management for large-tick assets
2. Smart order routing across venues
3. Optimal execution (TWAP/VWAP) for inventory unwinding
4. Statistical arbitrage signals (cross-exchange, funding rate)
5. Mean reversion models (OU process)

### Phase 4: ML & Optimization
1. Fair price model (order book imbalance features)
2. Volatility regime detection
3. Automated parameter tuning (Bayesian optimization)
4. Reinforcement learning for spread/skew optimization
5. Deep learning microprice prediction

---

## Sources

### Algorithms & Models
- [GLFT Market Making Model and Grid Trading (hftbacktest)](https://hftbacktest.readthedocs.io/en/py-v2.1.0/tutorials/GLFT%20Market%20Making%20Model%20and%20Grid%20Trading.html)
- [Queue-Based Market Making in Large Tick Size Assets (hftbacktest)](https://hftbacktest.readthedocs.io/en/latest/tutorials/Queue-Based%20Market%20Making%20in%20Large%20Tick%20Size%20Assets.html)
- [Technical Deep Dive into Avellaneda-Stoikov (Hummingbot)](https://hummingbot.org/blog/technical-deep-dive-into-the-avellaneda--stoikov-strategy/)
- [Comprehensive Guide to Avellaneda-Stoikov (Hummingbot)](https://medium.com/hummingbot/a-comprehensive-guide-to-avellaneda-stoikovs-market-making-strategy-102d64bf5df6)
- [RL approach to improve AS algorithm (PLOS ONE)](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0277042)
- [High-frequency market-making with inventory constraints (arXiv)](https://arxiv.org/abs/1206.4810)
- [Dealing with Inventory Risk (arXiv)](https://arxiv.org/abs/1105.3115)
- [Optimal High-Frequency Market Making (Stanford)](https://stanford.edu/class/msande448/2018/Final/Reports/gr5.pdf)
- [Deep Learning for VWAP Execution in Crypto (arXiv)](https://arxiv.org/html/2502.13722v2)
- [TWAP vs VWAP in Crypto Trading (TradingView)](https://www.tradingview.com/news/cointelegraph:4e659b29e094b:0-twap-vs-vwap-in-crypto-trading-what-s-the-difference/)

### Order Flow & Microstructure
- [From PIN to VPIN: Introduction to Order Flow Toxicity](https://www.quantresearch.org/From%20PIN%20to%20VPIN.pdf)
- [VPIN: The Coolest Market Metric (Krypton Labs)](https://medium.com/@kryptonlabs/vpin-the-coolest-market-metric-youve-never-heard-of-e7b3d6cbacf1)
- [Order Flow Toxicity in Bitcoin (Lucas Astorian)](https://medium.com/@lucasastorian/empirical-market-microstructure-f67eff3517e0)
- [Flow Toxicity and Liquidity in a High Frequency World (NYU Stern)](https://www.stern.nyu.edu/sites/default/files/assets/documents/con_035928.pdf)
- [Kyle's Lambda and Information Asymmetry (NBER)](https://www.nber.org/system/files/working_papers/w24297/w24297.pdf)
- [Identifying Information Asymmetry in Securities Markets](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2565216)
- [Bitcoin Order Flow Toxicity and Price Jumps (ScienceDirect)](https://www.sciencedirect.com/science/article/pii/S0275531925004192)

### Exchange Programs
- [Binance Futures Market Maker Program](https://www.binance.com/en/support/faq/binance-futures-market-maker-program-b65fefd0fee84893ad946dc6f707dedc)
- [Binance Market Maker Program Introduction](https://www.binance.com/en/support/announcement/introducing-the-binance-market-maker-program-360034573691)
- [Bybit Market Maker Incentive Program](https://www.bybit.com/en/help-center/article/Introduction-to-the-Market-Maker-Incentive-Program)
- [OKX Market Maker Program](https://support.okexcn.com/hc/en-us/articles/360001189471-%E2%85%A2-At-the-Heart-of-the-Market-Maker-Program-of-OKX)
- [Kraken Market Participation Program](https://blog.kraken.com/product/kraken-institutional/market-participation-program)
- [Kraken Institutional Market Makers](https://www.kraken.com/institutions/market-makers)
- [Backpack Exchange Market Maker Program](https://support.backpack.exchange/exchange/programs/market-maker-program)
- [Market Maker Programs Overview (hftbacktest)](https://hftbacktest.readthedocs.io/en/latest/market_maker_program.html)
- [Exchange Fee Comparison (WhalePortal)](https://whaleportal.com/blog/which-crypto-exchange-has-the-lowest-fees/)

### Risk Management
- [Options Market Making (Paradigm)](https://www.paradigm.co/blog/options-market-making)
- [Options Greeks for Risk Management (Pi42)](https://pi42.com/blog/options-greeks-for-risk-management/)
- [Hedging Strategies of Crypto Market Makers (DWF Labs)](https://www.dwf-labs.com/news/understanding-market-maker-hedging)
- [Crypto Portfolio Risk Simulation Framework (arXiv)](https://arxiv.org/html/2507.08915v1)

### Machine Learning
- [Multi-Objective RL for Market Making (ScienceDirect)](https://www.sciencedirect.com/science/article/pii/S0957417425024844)
- [Market Making with Deep RL from LOB (arXiv)](https://arxiv.org/abs/2305.15821)
- [Optimal Market Making by RL (arXiv)](https://arxiv.org/abs/2104.04036)
- [Market Making via RL (Spooner, arXiv)](https://arxiv.org/pdf/1804.04216)
- [Predictive Market Making via ML (Springer)](https://link.springer.com/article/10.1007/s43069-022-00124-0)

### Statistical Arbitrage
- [Statistical Arbitrage Strategies Using Cointegration](https://ijsra.net/sites/default/files/fulltext_pdf/IJSRA-2026-0283.pdf)
- [Crypto Arbitrage: 3 Statistical Approaches (CoinAPI)](https://www.coinapi.io/blog/3-statistical-arbitrage-strategies-in-crypto)
- [Advanced Statistical Arbitrage with RL (arXiv)](https://arxiv.org/html/2403.12180v1)
- [Pairs Trading Definitive Guide (Hudson & Thames)](https://hudsonthames.org/definitive-guide-to-pairs-trading/)

### Industry
- [Wintermute Trading Infrastructure (Bitget)](https://www.bitget.com/academy/wintermute-crypto)
- [Top 20 Crypto Market Makers 2026 (DWF Labs)](https://www.dwf-labs.com/news/20-top-crypto-market-makers)
- [Inside Jump Crypto (Insights4VC)](https://insights4vc.substack.com/p/inside-jump-crypto-13b-terra-trade)
- [4 Core Market Making Strategies (DWF Labs)](https://www.dwf-labs.com/news/4-common-strategies-that-crypto-market-makers-use)
- [Quote Stuffing Detection (AlgoTradingLib)](https://algotradinglib.com/en/pedia/q/quote_stuffing.html)
- [Latency Arbitrage (QuantVPS)](https://www.quantvps.com/blog/what-is-latency-arbitrage)

---

## 7. Compliance & Audit Trail

### 7.1 MiCA Requirements (EU Markets in Crypto-Assets Regulation)

**Record-Keeping Obligations:**
- CASPs must retain all transaction records for **minimum 5 years** from end of client agreement
- On request by competent authority, retention extends to **7 years**
- Records must include: all orders placed, all trades executed, all cancellations, timestamps, counterparty info
- ESMA published data standards (November 28, 2025) requiring **JSON format** adhering to **ISO 20022 methodology**
- NCAs began requesting data in the new JSON file format starting mid-2026

**Order Book Record Requirements (Commission Delegated Regulation EU 2025/1140):**
- Every order must be recorded with: order ID, instrument identifier, side (buy/sell), order type, price, quantity, timestamp (microsecond precision), order status (new/modified/cancelled/filled), client identifier
- All modifications and cancellations must be logged with original order reference
- Records must be in **chronological order** and **easily searchable**
- Must be machine-readable (JSON schema published by ESMA)

**What We Need to Implement:**
```
TradeLog {
    log_id: UUID,
    timestamp: DateTime<Utc>,        // microsecond precision
    event_type: OrderNew | OrderModify | OrderCancel | Fill | PartialFill,
    order_id: String,
    exchange_order_id: String,
    instrument: String,              // e.g., "BTC-USDT"
    side: Buy | Sell,
    order_type: Limit | Market | PostOnly,
    price: Decimal,
    quantity: Decimal,
    filled_quantity: Decimal,
    remaining_quantity: Decimal,
    client_id: Option<String>,       // for multi-client setups
    venue: String,                   // exchange name
    strategy_id: String,             // which strategy generated this
    fees: Decimal,
    fee_currency: String,
    status: New | PartiallyFilled | Filled | Cancelled | Rejected,
    metadata: HashMap<String, String>, // extensible for venue-specific data
}
```

**Storage Requirements:**
- Primary: append-only database (PostgreSQL with partitioning, or ClickHouse for time-series)
- Export format: JSON lines (one JSON object per line, ESMA-compatible)
- Backup: immutable storage (S3 with object lock, or similar)
- Index by: timestamp, order_id, instrument, client_id, strategy_id
- Must support efficient range queries for regulatory export

### 7.2 MiFID II Applicability

MiFID II applies to crypto assets that qualify as financial instruments. Key requirements:
- **Transaction reporting**: report to NCAs within T+1
- **Order record keeping**: same as MiCA but stricter timestamp requirements (microsecond sync)
- **Clock synchronization**: all timestamps must be synchronized to UTC reference within 1ms for HFT, 1s for others
- **Best execution**: must demonstrate best execution policy and monitor execution quality
- **Algorithmic trading**: additional requirements for algo firms including kill switches, pre-trade risk controls, and annual self-assessment

### 7.3 Market Surveillance (Solidus Labs / Chainalysis Requirements)

**Manipulative Behaviors to Detect and Log:**

1. **Spoofing & Layering**: Placing large orders with no intention of executing
   - Solidus HALO uses 200+ parameters to detect layering
   - Must log: order placement time, cancellation time, fill ratio, order-to-trade ratio
   - Alert if: order cancellation rate >95% within 1 second of placement

2. **Wash Trading**: Trading with yourself to inflate volume
   - Must track: self-trade prevention (STP) events, same-account-pair trades
   - Log: all matched trades with counterparty IDs when available

3. **Pump-and-Dump**: Solidus flagged >6,000 incidents on DEXs since January 2024
   - Monitor: unusual volume spikes correlated with social media activity
   - Track: inventory accumulation patterns (wallets holding >60-70% circulating supply)

4. **Cornering & Ramping**: Accumulating dominant position to manipulate price
   - Monitor: position concentration across venues
   - Alert: when position exceeds threshold % of average daily volume

**Data Required by Surveillance Platforms:**
```
SurveillanceRecord {
    // Order lifecycle
    all_order_events: Vec<OrderEvent>,  // place, modify, cancel, fill
    order_to_trade_ratio: f64,
    cancel_rate_1s: f64,                // % cancelled within 1 second
    cancel_rate_5s: f64,

    // Position
    position_by_venue: HashMap<String, Decimal>,
    total_position: Decimal,
    position_as_pct_adv: f64,           // % of average daily volume

    // Pattern detection
    two_sided_order_correlation: f64,    // do buy/sell orders move together?
    price_impact_vs_position_change: f64,
    volume_anomaly_score: f64,
}
```

**Integration Requirements:**
- Real-time event streaming (Kafka/NATS) to surveillance system
- REST API for historical query by regulators
- Export in FIX/FIXML or JSON format
- Retention: minimum 5 years (7 on request)

### 7.4 Gap Analysis vs. Our Current System

| Requirement | Current Status | Priority |
|-------------|---------------|----------|
| Append-only trade log | persistence/checkpoint.rs saves state, but no structured audit trail | CRITICAL |
| JSON export format (ESMA) | Not implemented | CRITICAL |
| Microsecond timestamps | Need to verify precision | HIGH |
| 5-7 year retention | No retention policy | HIGH |
| Clock synchronization | Not implemented | HIGH |
| Surveillance data export | Not implemented | MEDIUM |
| Order-to-trade ratio tracking | Not implemented | MEDIUM |
| Self-trade detection logging | Not implemented | MEDIUM |
| Regulatory query API | Not implemented | MEDIUM |

---

## 8. Client-Facing API & Client Management

### 8.1 Industry Standard: What MM Firms Expose to Clients

**GSR One Platform (reference architecture):**
- Real-time trading activity dashboard
- Order book depth visualization
- Customized performance metrics
- Market making analytics
- Programmatic execution tracking
- Treasury services integration
- Wallet control and security management
- API + UI access to 200+ assets

**Wintermute:**
- Zero-fee OTC platform
- RFQ (Request for Quote) API for institutional clients
- Real-time trade execution via API
- Supports 150+ token liquidity relationships across 65+ venues

**Flowdesk (Retainer Model):**
- Real-time dashboards or regular reports tracking trading activity, spreads, PnL
- KPI monitoring: bid-ask spread, uptime (>95%), latency, responsiveness
- Client-defined trading strategy collaboration
- Monthly service fee billing

### 8.2 Client Management API Design

**Authentication & Authorization:**
```
POST   /api/v1/auth/token          # OAuth2 client credentials flow
POST   /api/v1/auth/refresh         # Token refresh

# Role-based access:
# - admin: full control, key rotation, strategy params
# - trader: view positions, request quotes, execute OTC
# - viewer: read-only dashboards and reports
# - auditor: read-only trade logs and compliance data
```

**Position & Portfolio Endpoints:**
```
GET    /api/v1/positions                    # Real-time positions across venues
GET    /api/v1/positions/{venue}            # Position on specific venue
GET    /api/v1/portfolio/summary            # Aggregated P&L, exposure, inventory
GET    /api/v1/portfolio/history            # Historical portfolio snapshots
WebSocket /ws/v1/positions                  # Real-time position stream
```

**Trading & Execution:**
```
POST   /api/v1/orders/rfq                   # Request for quote
POST   /api/v1/orders/execute               # Execute at quoted price
GET    /api/v1/orders                       # List orders (with filters)
GET    /api/v1/orders/{id}                  # Order details + fill history
GET    /api/v1/fills                        # Fill history
POST   /api/v1/execution/twap              # Submit TWAP execution request
POST   /api/v1/execution/vwap              # Submit VWAP execution request
```

**Reporting:**
```
GET    /api/v1/reports/daily                # Daily summary report
GET    /api/v1/reports/monthly              # Monthly performance report
GET    /api/v1/reports/fill-quality         # Fill quality analysis
GET    /api/v1/reports/spread-quality       # Spread maintenance metrics
GET    /api/v1/reports/sla                  # SLA compliance dashboard data
GET    /api/v1/reports/pnl-attribution      # P&L breakdown by component
GET    /api/v1/reports/audit-trail          # Compliance audit trail export
```

**Market Data:**
```
GET    /api/v1/market/orderbook/{pair}      # Aggregated order book
GET    /api/v1/market/trades/{pair}         # Recent trades
WebSocket /ws/v1/market/{pair}              # Real-time market data stream
GET    /api/v1/market/metrics/{pair}        # Spread, depth, volume metrics
```

### 8.3 Token Lending / Borrowing Interface

**Standard Agreement Terms to Track:**
```
TokenLoan {
    loan_id: UUID,
    lender: ClientId,                       // token project
    borrower: String,                       // our firm
    token: String,                          // e.g., "PROJECT_TOKEN"
    quantity: Decimal,
    quantity_as_pct_supply: Decimal,         // % of total/circulating supply
    model: LoanCall | Retainer,

    // Loan/Call model
    call_option_strike: Option<Decimal>,
    call_option_expiry: Option<DateTime>,

    // Retainer model
    monthly_fee: Option<Decimal>,
    quote_currency_loan: Option<Decimal>,   // USDC provided by client

    // Common
    start_date: DateTime,
    end_date: Option<DateTime>,             // None = open-ended
    recall_notice_days: u32,                // e.g., 7 days notice for recall
    collateral_type: Option<String>,
    collateral_ratio: Option<Decimal>,      // e.g., 1.2 = 120% collateralization
    margin_call_threshold: Option<Decimal>, // trigger for additional collateral
    interest_rate: Option<Decimal>,         // annual rate
    rate_type: Fixed | Variable,

    // Status
    status: Active | Recalled | Returned | Defaulted,
    tokens_outstanding: Decimal,
    tokens_returned: Decimal,
}
```

**Loan Management Endpoints:**
```
GET    /api/v1/loans                        # List all active loans
GET    /api/v1/loans/{id}                   # Loan details
GET    /api/v1/loans/{id}/inventory         # Current inventory status of loaned tokens
POST   /api/v1/loans/{id}/recall            # Client initiates token recall
GET    /api/v1/loans/{id}/collateral        # Collateral status and ratio
GET    /api/v1/loans/{id}/interest          # Accrued interest calculations
GET    /api/v1/loans/{id}/performance       # MM performance with this loan
```

### 8.4 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Client authentication/authorization | No client API exists | HIGH |
| REST API for positions/reports | dashboard/server.rs has metrics endpoint only | HIGH |
| WebSocket streaming for clients | Not implemented | MEDIUM |
| Token lending management | Not implemented | MEDIUM |
| RFQ/OTC interface | Not implemented | LOW |
| Client-specific strategy params | Not implemented | LOW |

---

## 9. Alerting & Monitoring

### 9.1 Industry Practices

**Chicago Trading Company (CTC) uses PagerDuty for:**
- Escalating incidents across time zones for 24/7 operations (20 hours/day, 6 days/week)
- Reducing alert fatigue with Event Intelligence
- Communicating with senior traders on the floor
- Measuring SLAs and team health with Operational Reviews and Analytics Dashboards

**Typical Trading Firm Monitoring Stack:**
```
[Application Metrics] --> [Prometheus/Datadog/InfluxDB]
                     --> [Grafana Dashboards]
                     --> [Alert Rules Engine]
                     --> [PagerDuty/OpsGenie] --> [On-Call Engineer]
                     --> [Telegram Bot]       --> [Trading Desk]
                     --> [Slack]              --> [Operations Team]
```

### 9.2 Critical Alerts for MM Operations

**Tier 1 - Immediate Action (page on-call + Telegram to trading desk):**

| Alert | Condition | Action |
|-------|-----------|--------|
| Exchange disconnection | WebSocket closed or >5s no heartbeat | Auto-reconnect; if fails 3x, cancel all orders, page |
| Kill switch triggered | Any kill switch level activated | Page immediately, log reason |
| Position limit breach | Inventory > max_position | Stop quoting on that side, page |
| Drawdown threshold | Daily P&L < -X% | Reduce size or halt, page |
| Order rejection spike | >10 rejections in 1 minute | Likely rate limit or API issue, page |
| Stale order book | No book update in >10 seconds | Cancel all quotes, page |
| Price dislocation | Our quote >5% from reference price | Cancel all quotes, page |
| Self-trade prevention triggered | STP event detected | Log and alert for compliance |

**Tier 2 - Urgent Warning (Telegram + Slack):**

| Alert | Condition | Action |
|-------|-----------|--------|
| Spread too wide | Spread >2x target for >1 minute | Investigate connectivity/book |
| Fill rate anomaly | Fill rate <50% of expected or >200% | Check for adverse selection |
| VPIN spike | VPIN >0.7 | Strategy should auto-widen; alert for awareness |
| Latency degradation | Round-trip >100ms (normally <50ms) | Check network, consider pausing |
| Fee tier at risk | Volume trending below tier threshold | Increase volume on eligible pairs |
| Funding rate extreme | Funding rate >0.1% per 8h | Rebalance perp/spot positions |
| Balance mismatch | Internal tracking != exchange balance | Reconcile immediately |

**Tier 3 - Informational (Slack/dashboard only):**

| Alert | Condition | Action |
|-------|-----------|--------|
| New token listing | Exchange announces new pair | Evaluate for MM opportunity |
| Strategy parameter drift | Autotune changed params >20% | Review for reasonableness |
| Daily P&L summary | End of UTC day | Report to stakeholders |
| SLA compliance report | Hourly | Track uptime, spread, depth obligations |
| API key expiry approaching | Key expires within 7 days | Rotate proactively |

### 9.3 Telegram Bot Implementation Pattern

```
TelegramAlertBot:
    channels:
        - critical_alerts: -100xxxxx (trading desk + management)
        - operations: -100xxxxx (engineering + ops)
        - daily_reports: -100xxxxx (all stakeholders)

    message_format:
        severity: P1 | P2 | P3
        title: "Exchange Disconnection: Binance"
        details: "WebSocket closed, 3 reconnect attempts failed"
        action_required: "Manual investigation required"
        timestamp: "2026-03-19T14:23:45Z"
        affected: "BTC-USDT, ETH-USDT on Binance"
        current_state: "All quotes cancelled, kill switch level 3"

    rate_limiting:
        - Deduplicate same alert within 60 seconds
        - Escalate if same alert fires >5 times in 10 minutes
        - Quiet hours: suppress P3 between 00:00-06:00 UTC

    interactive_commands:
        /status          - Current system status (all venues, positions)
        /positions       - Current inventory across venues
        /pnl             - Today's P&L summary
        /spread {pair}   - Current spread vs target
        /kill {level}    - Trigger kill switch (requires 2FA confirmation)
        /restart {venue} - Restart venue connector
```

### 9.4 Metrics to Export (Prometheus/OpenTelemetry)

```
# Business metrics
mm_pnl_total{strategy, venue, pair}                   gauge
mm_pnl_spread{strategy, venue, pair}                   gauge
mm_pnl_inventory{strategy, venue, pair}                gauge
mm_inventory_position{venue, pair}                     gauge
mm_inventory_value_usd{venue, pair}                    gauge
mm_spread_current{venue, pair}                         gauge
mm_spread_target{venue, pair}                          gauge
mm_fill_rate{venue, pair, side}                        gauge
mm_uptime_seconds{venue, pair}                         counter
mm_sla_spread_compliance_ratio{venue, pair}            gauge
mm_sla_depth_compliance_ratio{venue, pair}             gauge

# Operational metrics
mm_ws_latency_ms{venue}                                histogram
mm_order_round_trip_ms{venue}                          histogram
mm_book_age_ms{venue, pair}                            gauge
mm_orders_placed_total{venue, pair, side}               counter
mm_orders_cancelled_total{venue, pair, reason}          counter
mm_orders_rejected_total{venue, pair, reason}           counter
mm_reconnections_total{venue}                          counter
mm_kill_switch_activations_total{level}                counter
mm_vpin{venue, pair}                                   gauge
mm_kyle_lambda{venue, pair}                            gauge
mm_adverse_selection_cost{venue, pair, horizon}         gauge

# System metrics
mm_event_loop_duration_us                              histogram
mm_memory_bytes                                        gauge
mm_cpu_usage_percent                                   gauge
mm_api_rate_limit_remaining{venue}                     gauge
```

### 9.5 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Prometheus metrics export | server/metrics.rs exists with basic metrics | MEDIUM (extend) |
| Grafana dashboards | dashboard crate exists | MEDIUM (extend) |
| PagerDuty/OpsGenie integration | Not implemented | HIGH |
| Telegram bot alerts | Not implemented | HIGH |
| Alert deduplication | Not implemented | HIGH |
| Tiered alert severity | Not implemented | HIGH |
| Interactive Telegram commands | Not implemented | MEDIUM |
| On-call rotation support | Not implemented | MEDIUM |

---

## 10. Multi-Venue Reconciliation

### 10.1 Reconciliation Architecture

**The Problem:**
- Each exchange has different data formats, timestamps, and settlement times
- Positions must be tracked both internally (our ledger) and externally (exchange balances)
- Discrepancies can arise from: partial fills, network latency, exchange bugs, untracked fees, funding payments, dust rounding

**Three-Way Reconciliation:**
```
1. Internal Ledger (our tracked positions from fills)
2. Exchange Reported Balances (via REST API)
3. Blockchain State (for on-chain assets)

Reconciliation = Internal == Exchange == On-chain (where applicable)
```

**Reconciliation Process:**
```
ReconciliationEngine:
    frequency: every 60 seconds (hot), full daily (cold)

    hot_reconciliation():
        for each venue:
            internal_balance = sum(all fills since last sync) + last_known_balance
            exchange_balance = fetch_balance_via_api(venue)
            diff = internal_balance - exchange_balance
            if abs(diff) > dust_threshold:   // e.g., > $1 or > 0.001 BTC
                alert(P2, "Balance mismatch on {venue}: internal={internal}, exchange={exchange}")
                log_discrepancy(venue, diff, timestamp)

    cold_reconciliation():
        for each venue:
            // Full audit: replay all fills, deposits, withdrawals, fees, funding
            expected = replay_all_events(venue, start_of_day)
            actual = fetch_full_balance_snapshot(venue)
            generate_reconciliation_report(expected, actual)
```

**Data Model:**
```
ReconciliationRecord {
    recon_id: UUID,
    timestamp: DateTime<Utc>,
    venue: String,
    asset: String,
    internal_balance: Decimal,
    exchange_balance: Decimal,
    blockchain_balance: Option<Decimal>,
    discrepancy: Decimal,
    discrepancy_pct: Decimal,
    status: Matched | Pending | Discrepancy | Resolved,
    resolution_note: Option<String>,
    // Breakdown
    fills_since_last: Vec<FillId>,
    deposits_since_last: Vec<TransferId>,
    withdrawals_since_last: Vec<TransferId>,
    fees_since_last: Decimal,
    funding_payments_since_last: Decimal,
}
```

### 10.2 Exchange Downtime Handling

**Protocol:**
```
ExchangeDowntimeHandler:
    detection:
        - WebSocket heartbeat timeout (>5s)
        - REST API 5xx errors (3 consecutive)
        - Exchange status page monitoring (RSS/API)

    immediate_actions:
        1. Cancel all pending orders (best-effort, may fail if exchange unreachable)
        2. Mark venue as DISCONNECTED in internal state
        3. Freeze internal position for that venue (no updates until reconnected)
        4. Alert P1 with details

    cross_venue_impact:
        - Recalculate total inventory without disconnected venue
        - Adjust hedges on other venues to account for frozen position
        - Widen spreads on remaining venues (increased risk)
        - If >50% of venues down: trigger portfolio-level kill switch

    reconnection:
        1. Exponential backoff reconnection attempts (1s, 2s, 4s, ... max 60s)
        2. On reconnect: immediately fetch full balance snapshot
        3. Reconcile internal state vs exchange state
        4. Resolve any discrepancies (fills that happened during downtime)
        5. Replay any missed WebSocket events if exchange supports sequence numbers
        6. Resume quoting only after reconciliation is clean

    post_incident:
        - Generate downtime report (duration, impact on P&L, SLA impact)
        - Adjust venue risk score for future allocation decisions
```

### 10.3 Position Transfer / Rebalancing

**Cross-Venue Rebalancing:**
```
RebalancingEngine:
    triggers:
        - Venue inventory imbalance > threshold (e.g., >70% of total on one venue)
        - Risk-weighted exposure exceeds per-venue limit
        - Fee tier optimization (move volume to venue where close to next tier)
        - Exchange maintenance announcement

    execution:
        1. Calculate target allocation per venue (based on volume, fees, latency)
        2. Determine transfer amounts
        3. Initiate withdrawal from source venue
        4. Monitor blockchain confirmation
        5. Confirm deposit on target venue
        6. Update internal ledger
        7. Adjust strategy parameters for new allocation

    constraints:
        - Withdrawal limits per venue per 24h
        - Minimum balance requirements per venue
        - Blockchain confirmation times (BTC: ~60min, ETH: ~15min, USDT-TRC20: ~3min)
        - Transfer fees
        - Never transfer >X% of total assets in a single transaction

    safety:
        - Pre-verify deposit address via whitelist
        - Small test transfer before large amounts
        - Track transfer status with timeout alerts
        - Reconcile on both sides after completion
```

### 10.4 Settlement and Netting

**Internal Netting for Multi-Client Operations:**
```
NettingEngine:
    // Instead of settling each trade individually, net across the day
    daily_netting():
        for each client:
            for each asset:
                net_flow = sum(buys) - sum(sells)
                net_fees = sum(all fees for the day)
                settlement_amount = net_flow - net_fees

        // Cross-client netting (if acting as intermediary)
        for each asset:
            total_buy_side = sum(all client buys)
            total_sell_side = sum(all client sells)
            net_exchange_settlement = total_buy_side - total_sell_side
            // Only settle the net amount on exchange, reducing transfer costs
```

**Crypto Settlement Considerations:**
- Settlement is not instant despite blockchain -- must wait for finality
- Bitcoin: 6 confirmations (~60 minutes)
- Ethereum: 2 epochs (~12.8 minutes post-Merge)
- Stablecoins on fast chains (TRON, Solana): seconds to minutes
- Exchange internal transfers (between accounts): typically instant
- Cross-exchange: withdrawal processing time + blockchain time + deposit crediting time

### 10.5 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Balance reconciliation | balance_cache.rs tracks internal; no exchange comparison | CRITICAL |
| Exchange downtime detection | exchange-core has reconnection; no formal protocol | HIGH |
| Cross-venue rebalancing | Not implemented | HIGH |
| Transfer tracking | Not implemented | HIGH |
| Settlement netting | Not implemented | LOW |
| Reconciliation reports | Not implemented | MEDIUM |
| Blockchain balance verification | Not implemented | LOW |

---

## 11. Security

### 11.1 API Key Management

**Production Architecture:**
```
[Market Maker Engine]
        |
        v
[HashiCorp Vault / AWS Secrets Manager]
        |
        v
[Exchange API Keys (encrypted at rest)]

Flow:
1. Engine starts -> authenticates to Vault via AppRole/K8s auth
2. Vault returns short-lived API key lease (TTL: 1 hour)
3. Engine uses key for exchange operations
4. Key is renewed before TTL expires
5. Key is never written to disk, config files, or environment variables in plaintext
```

**Key Rotation Protocol:**
```
KeyRotationPolicy:
    rotation_interval: 30-90 days (or immediately if compromised)

    rotation_steps:
        1. Generate new API key on exchange (via exchange API or manually)
        2. Store new key in Vault with new version
        3. Update engine to use new key (hot-swap without restart)
        4. Verify new key works (place and cancel a small test order)
        5. Revoke old key on exchange
        6. Audit log: who rotated, when, reason

    emergency_rotation:
        - If key suspected compromised: immediately revoke on exchange
        - Generate new key, update Vault, restart affected services
        - Audit all activity with compromised key
        - Report incident
```

**Key Permission Scoping:**
- Trade-only keys: can place/cancel orders, read positions -- NO withdrawal permission
- Read-only keys: for monitoring and reporting systems
- Withdrawal keys: separate key, never used by trading engine, requires IP whitelist + 2FA
- Fund transfer keys: for internal transfers between sub-accounts only

### 11.2 IP Whitelisting

**Requirements:**
- All exchange API keys must have IP restrictions enabled
- Whitelist only: trading server IPs, monitoring server IPs, Vault server IPs
- Maintain separate IP whitelist per key purpose (trading vs monitoring vs withdrawal)
- 70% of major exchanges support IP whitelisting natively
- For exchanges without IP whitelisting: use network-level restrictions (VPN, firewall)
- Document all whitelisted IPs in configuration management

### 11.3 Withdrawal Address Whitelisting

**Protocol:**
```
WithdrawalSecurity:
    address_whitelist:
        - Pre-register all valid withdrawal addresses on each exchange
        - Enable "whitelist-only" mode on all exchange accounts
        - New address addition requires: 24-48h cooling period + email/2FA confirmation
        - Review whitelist monthly, remove unused addresses

    withdrawal_policy:
        - Trading engine keys must NOT have withdrawal permission
        - Withdrawals only via separate admin process with:
            - Multi-signature approval (2 of 3 admin keys)
            - 2FA confirmation
            - IP restriction to office/VPN
            - Amount limits per transaction and per 24h
            - Mandatory test transfer for new addresses
```

### 11.4 Admin Operations Security

**2FA for Admin Operations:**
```
AdminSecurityPolicy:
    operations_requiring_2fa:
        - Strategy parameter changes beyond threshold
        - Kill switch manual override
        - API key rotation
        - Withdrawal initiation
        - New client onboarding
        - System configuration changes
        - User permission changes

    implementation:
        - TOTP (Google Authenticator / Authy) for web dashboard
        - Hardware security key (YubiKey) for production deployments
        - Telegram bot 2FA: confirm critical operations via separate channel
        - Audit log: every admin action logged with user, timestamp, 2FA method
```

**Rate Limiting on Admin API:**
```
AdminRateLimits:
    login_attempts: 5 per 15 minutes (then lockout for 30 min)
    strategy_changes: 10 per hour
    kill_switch_changes: 5 per hour
    config_changes: 20 per hour
    api_key_operations: 3 per hour
    withdrawal_requests: 2 per hour

    brute_force_protection:
        - Progressive lockout (5 min, 15 min, 1 hour, 24 hours)
        - Alert on 3+ failed login attempts
        - IP-based rate limiting in addition to user-based
```

### 11.5 Network Security

**Production Network Architecture:**
```
[Public Internet]
       |
[WAF / DDoS Protection (Cloudflare)]
       |
[Load Balancer (TLS termination)]
       |
[Admin API] ---- [VPN Required]
       |
[Trading Engine] ---- [Direct to Exchange (whitelisted IPs)]
       |
[Internal Services] ---- [Private VPC/subnet]
       |
[Vault / Secrets] ---- [Most restricted subnet]
```

### 11.6 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Secrets management (Vault) | API keys likely in config/env | CRITICAL |
| IP whitelisting documentation | Not tracked | HIGH |
| Key rotation automation | Not implemented | HIGH |
| Withdrawal address whitelist | Not applicable (no withdrawal in engine) | MEDIUM |
| Admin 2FA | No admin API exists | MEDIUM |
| Rate limiting on admin | No admin API exists | MEDIUM |
| Network architecture docs | Not documented | MEDIUM |
| Audit logging for admin actions | Not implemented | HIGH |

---

## 12. Advanced Order Types

### 12.1 Iceberg Orders

**Purpose:** Execute large orders without revealing full size to the market, preventing front-running and reducing market impact.

**Implementation:**
```
IcebergOrder {
    total_quantity: Decimal,
    visible_quantity: Decimal,       // "tip of the iceberg" shown on book
    filled_quantity: Decimal,
    price: Decimal,
    side: Buy | Sell,
    variance: Option<Decimal>,       // randomize visible qty +/- X% to avoid detection
    price_limit: Decimal,            // worst price we're willing to accept

    execution_logic:
        1. Place visible_quantity at price as limit order
        2. When filled, immediately place next visible_quantity
        3. Randomize visible_quantity by +/- variance to avoid pattern detection
        4. Continue until total_quantity filled or price exceeds limit
        5. Optionally: add random delay (100-500ms) between refills to mimic human behavior
}
```

**Crypto-Specific Considerations:**
- Bybit natively supports iceberg orders with split settings and price variance
- Binance does not natively support -- must implement client-side
- Some exchanges detect iceberg patterns and may flag as potential manipulation

### 12.2 TWAP (Time-Weighted Average Price)

**Already documented in Section 1.3, but adding implementation details for our system:**

```
TwapExecutor {
    total_quantity: Decimal,
    duration: Duration,
    num_slices: u32,
    min_slice_size: Decimal,         // don't go below exchange minimum
    max_participation_rate: f64,     // e.g., 5% -- never be more than 5% of volume
    price_limit: Option<Decimal>,    // abort if price exceeds limit

    execution:
        interval = duration / num_slices
        slice_size = total_quantity / num_slices

        every interval:
            if remaining_quantity <= 0: done
            actual_slice = min(slice_size, remaining_quantity)
            // Limit participation
            recent_volume = get_volume_last_interval()
            max_by_participation = recent_volume * max_participation_rate
            actual_slice = min(actual_slice, max_by_participation)
            // Place order
            if actual_slice >= min_slice_size:
                place_limit_order(actual_slice, mid_price +/- offset)
            // Carry forward unfilled to next slice
            remaining += unfilled_from_last_slice

    monitoring:
        twap_benchmark = sum(mid_prices at each interval) / num_intervals
        execution_price = sum(fill_price * fill_qty) / sum(fill_qty)
        slippage = execution_price - twap_benchmark
}
```

### 12.3 VWAP (Volume-Weighted Average Price)

```
VwapExecutor {
    total_quantity: Decimal,
    duration: Duration,
    volume_profile: Vec<f64>,        // historical volume distribution by interval
    max_participation_rate: f64,
    price_limit: Option<Decimal>,

    pre_computation:
        // Build volume profile from historical data
        // e.g., for each hour: what % of daily volume typically trades
        volume_profile = compute_from_historical(lookback_days=30, interval=5min)

    execution:
        for each interval:
            target_pct = volume_profile[interval_index]
            target_qty = total_quantity * target_pct
            // Adjust for actual observed volume
            actual_volume = get_real_time_volume()
            adjusted_qty = target_qty * (actual_volume / expected_volume)
            place_order(adjusted_qty)

    monitoring:
        vwap_benchmark = sum(price * volume) / sum(volume) over duration
        execution_price = sum(fill_price * fill_qty) / sum(fill_qty)
        slippage = execution_price - vwap_benchmark
}
```

### 12.4 Peg Orders (Adaptive)

```
PegOrder {
    peg_type: MidPrice | BestBid | BestAsk | MicroPrice,
    offset: Decimal,                 // e.g., -0.01% from mid = slightly aggressive
    quantity: Decimal,
    side: Buy | Sell,
    limit_price: Decimal,            // hard limit -- peg won't go beyond this
    repeg_interval: Duration,        // how often to check and re-peg (e.g., 100ms)
    min_price_change: Decimal,       // only re-peg if reference moved by this much

    execution:
        loop every repeg_interval:
            reference = match peg_type:
                MidPrice => (best_bid + best_ask) / 2
                BestBid => best_bid
                BestAsk => best_ask
                MicroPrice => weighted_mid(bbo)

            new_price = reference + offset
            new_price = clamp(new_price, limit_price, ...)

            if abs(new_price - current_order_price) > min_price_change:
                modify_order(new_price)
                // Note: modifying loses queue priority on most exchanges
                // Alternative: cancel + new only if price change is significant
}
```

### 12.5 Conditional Orders (Inventory-Based)

```
ConditionalOrderEngine {
    rules: Vec<ConditionalRule>,

    ConditionalRule {
        name: String,
        condition: ConditionExpr,
        action: ActionExpr,
        cooldown: Duration,           // minimum time between triggers
    }

    examples:
        // Stop-loss on inventory
        ConditionalRule {
            name: "inventory_stop_loss",
            condition: inventory_value_usd > 100_000 AND unrealized_pnl < -5_000,
            action: execute_twap(sell, excess_inventory, duration=5min),
            cooldown: 15min,
        }

        // Aggressive unwind when toxicity spikes
        ConditionalRule {
            name: "toxic_flow_unwind",
            condition: vpin > 0.8 AND abs(inventory) > max_inventory * 0.5,
            action: widen_spread(200%) AND execute_twap(reduce_inventory, duration=10min),
            cooldown: 30min,
        }

        // Rebalance when venue concentration too high
        ConditionalRule {
            name: "venue_rebalance",
            condition: venue_concentration("binance") > 0.7,
            action: initiate_transfer(from="binance", to="bybit", amount=30%),
            cooldown: 4h,
        }
}
```

### 12.6 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Iceberg orders | Not implemented | MEDIUM |
| TWAP execution | Not implemented | HIGH (needed for inventory unwinding) |
| VWAP execution | Not implemented | MEDIUM |
| Peg orders | Not implemented | MEDIUM |
| Conditional orders | kill_switch.rs has some conditional logic | LOW |
| Stop-loss on inventory | kill_switch.rs handles extreme cases only | MEDIUM |

---

## 13. Operational: Deployment & Testing

### 13.1 Blue/Green Deployment for Zero-Downtime

**Architecture:**
```
[Load Balancer / Router]
     |           |
  [Blue]      [Green]
  (v1.2)      (v1.3)
  ACTIVE      STANDBY

Deployment Steps:
1. Green environment runs new version (v1.3)
2. Green connects to exchanges in read-only mode (market data only, no trading)
3. Verify Green is receiving correct data, no errors
4. Switch router to Green: Green starts trading, Blue goes passive
5. Monitor for 5-10 minutes: compare P&L, fill rate, spread quality
6. If issues: instant rollback by switching router back to Blue
7. If stable: tear down Blue (or keep as next standby)

Critical Considerations for MM:
- Both Blue and Green must NOT quote simultaneously (would double exposure)
- Transition must be atomic: one system stops quoting, other starts
- Maintain shared state in external database (not in-process)
- Exchange connections need graceful handoff (cancel orders on old, place on new)
```

**Transition Protocol:**
```
BlueGreenTransition:
    1. Green instance starts, connects to market data feeds
    2. Green loads current positions from shared state DB
    3. Operator triggers switchover
    4. Blue receives STOP signal:
       a. Stops placing new orders
       b. Cancels all outstanding orders
       c. Confirms all orders cancelled via exchange API
       d. Writes final state to DB
       e. Signals "DRAINED"
    5. Green receives START signal:
       a. Reads latest state from DB
       b. Begins quoting
    6. Total downtime: 2-5 seconds (order cancel + new order placement)
```

### 13.2 Canary Deployment

**Purpose:** Test new strategy logic or parameters on a small allocation before full rollout.

```
CanaryDeployment:
    canary_allocation: 5-10% of total capital
    canary_pairs: select 2-3 pairs (1 major + 1-2 altcoins)
    duration: 24-72 hours minimum

    monitoring:
        compare_metrics:
            - Canary P&L per $ deployed vs. control
            - Canary spread quality vs. control
            - Canary fill rate vs. control
            - Canary adverse selection vs. control
            - Canary drawdown vs. control

    promotion_criteria:
        - Canary P&L within 20% of control (or better)
        - No increase in adverse selection
        - No anomalous behavior (flash spikes in orders, cancellations)
        - Running for minimum 24 hours with no P1/P2 alerts

    rollback_criteria:
        - Canary P&L < -50% of control
        - Any P1 alert triggered
        - Drawdown > 2x control drawdown
        - Automatic rollback: no human required
```

### 13.3 A/B Testing Strategies

```
ABTestFramework:
    setup:
        - Split capital allocation: A=50%, B=50% (or A=90%, B=10% for riskier tests)
        - Each runs on non-overlapping pairs or venues to avoid interference
        - Share the same risk management (kill switches apply to both)

    metrics_to_compare:
        primary:
            - Sharpe ratio (annualized)
            - Total P&L per $ deployed
            - Maximum drawdown
        secondary:
            - Average spread captured
            - Fill rate
            - Adverse selection cost
            - Inventory duration (how long until flat)
        operational:
            - Order-to-trade ratio
            - Number of cancellations
            - Latency profile

    statistical_significance:
        - Minimum 1000 fills per variant
        - Use Welch's t-test for P&L comparison
        - Report p-value and confidence interval
        - Don't make decisions on <48 hours of data
```

### 13.4 Performance Regression Testing

```
PerformanceRegressionSuite:
    benchmarks:
        - Event loop latency: p50 < 10us, p99 < 100us, p999 < 1ms
        - Order placement latency: p99 < 5ms (excluding network)
        - Book update processing: p99 < 50us
        - Strategy computation: p99 < 100us
        - Memory usage: < 500MB RSS for single-pair
        - Throughput: > 10,000 events/second

    automation:
        - Run on every PR before merge
        - Compare against baseline from main branch
        - Fail if any metric degrades >10% from baseline
        - Use criterion.rs for Rust microbenchmarks
        - Use historical replay data for end-to-end benchmarks

    historical_replay_test:
        - Replay 1 week of recorded market data
        - Compare: P&L, fill rate, spread quality, inventory profile
        - Ensure deterministic replay (same inputs -> same outputs)
        - Detect regressions in strategy behavior, not just performance
```

### 13.5 Incident Runbook / Playbook

**Runbook: Exchange Disconnection**
```
Severity: P1
Trigger: WebSocket disconnection for >10 seconds after 3 retry attempts

Steps:
1. AUTOMATED: Cancel all pending orders (best-effort)
2. AUTOMATED: Freeze positions for that venue
3. AUTOMATED: Alert via PagerDuty + Telegram
4. ON-CALL: Verify exchange status page (is it their outage?)
5. ON-CALL: Check our server health (CPU, memory, network)
6. ON-CALL: If exchange outage: wait and monitor. Set 15-min check-in timer.
7. ON-CALL: If our issue: check logs, restart connector if needed
8. ON-RECONNECT: Verify balance reconciliation before resuming
9. RESUME: Enable quoting, monitor for 5 minutes
10. POST-INCIDENT: File incident report, update runbook if needed
```

**Runbook: Unexpected Large Loss**
```
Severity: P1
Trigger: Drawdown > daily_loss_limit or single trade loss > $X

Steps:
1. AUTOMATED: Kill switch activates (level depends on severity)
2. AUTOMATED: Alert via PagerDuty + Telegram + Slack
3. ON-CALL: Verify the loss is real (not a data issue)
4. ON-CALL: Check for: adverse selection event, exchange issue, strategy bug
5. ON-CALL: If strategy bug: halt strategy, deploy previous version
6. ON-CALL: If market event: assess if temporary (widen spreads) or structural (halt)
7. ON-CALL: Review all fills in the loss period
8. MANAGEMENT: Decide on resume conditions and reduced allocation
9. POST-INCIDENT: Root cause analysis, strategy review
```

**Runbook: Kill Switch Triggered**
```
Severity: P1
Trigger: Any kill switch level >= 3

Steps:
1. AUTOMATED: Actions per kill switch level (widen/stop/cancel/flatten/disconnect)
2. AUTOMATED: Alert all channels
3. ON-CALL (within 5 min): Acknowledge alert
4. ON-CALL: Identify trigger cause from logs
5. ON-CALL: If false positive: document why, reset kill switch with 2FA
6. ON-CALL: If real: follow appropriate sub-runbook (loss, exchange issue, etc.)
7. ON-CALL: Do NOT reset kill switch without understanding root cause
8. POST-INCIDENT: Review kill switch thresholds
```

**Runbook: API Key Compromise Suspected**
```
Severity: P0
Trigger: Unauthorized activity detected, or key exposed in logs/repos

Steps:
1. IMMEDIATE: Revoke compromised key on exchange (within 5 minutes)
2. IMMEDIATE: Enable withdrawal lock on exchange account
3. IMMEDIATE: Generate new API key with same permissions
4. IMMEDIATE: Update Vault with new key
5. IMMEDIATE: Restart affected services with new key
6. INVESTIGATE: Audit all activity with compromised key
7. INVESTIGATE: Check for unauthorized trades, withdrawals, transfers
8. INVESTIGATE: Determine how key was compromised
9. REMEDIATE: Fix the vulnerability that led to exposure
10. COMMUNICATE: Notify relevant stakeholders, file incident report
```

### 13.6 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Blue/green deployment | Not implemented (single binary) | HIGH |
| Canary deployment | Not implemented | MEDIUM |
| A/B testing framework | Not implemented | LOW |
| Performance benchmarks | 32 tests but no perf benchmarks | HIGH |
| Criterion.rs benchmarks | Not implemented | HIGH |
| Historical replay regression | backtester crate exists | MEDIUM (extend) |
| Incident runbooks | Not documented | HIGH |
| Shared state for deployment | In-process state only | HIGH |

---

## 14. What Clients Expect: Reporting & SLAs

### 14.1 Real-Time Position Reporting

**Dashboard Requirements (per GSR One / industry standard):**
```
RealTimeDashboard:
    views:
        position_overview:
            - Per-venue: asset, quantity, avg_entry_price, unrealized_pnl, value_usd
            - Aggregated: total inventory, total value, net exposure
            - Historical chart: position over time (24h, 7d, 30d)

        order_book_visualization:
            - Our quotes highlighted on live order book
            - Depth chart showing our contribution to liquidity
            - Historical spread chart

        pnl_real_time:
            - Running P&L (today, this week, this month)
            - P&L attribution: spread, inventory, rebates, fees, funding
            - P&L chart with annotations (strategy changes, market events)

        risk_metrics:
            - Current inventory level vs limits
            - VaR/CVaR current value
            - VPIN and toxicity indicators
            - Exposure by venue

    update_frequency:
        - Positions: every 1 second
        - P&L: every 5 seconds
        - Order book visualization: every 500ms
        - Risk metrics: every 10 seconds
```

### 14.2 Fill Quality Reports

```
FillQualityReport:
    frequency: daily, with monthly aggregation

    metrics:
        execution_quality:
            - Average fill price vs mid-price at time of fill
            - Average fill price vs VWAP benchmark
            - Slippage distribution (histogram)
            - Percentage of fills at BBO vs away from BBO

        speed_metrics:
            - Average time from order to fill
            - Fill rate (% of orders that execute)
            - Partial fill rate

        adverse_selection:
            - Post-fill price movement at 1s, 5s, 30s, 60s
            - Percentage of fills followed by adverse movement
            - Average adverse selection cost per fill

        venue_comparison:
            - Fill quality per venue
            - Best execution analysis (did we route to best venue?)
```

### 14.3 Spread Quality Metrics

```
SpreadQualityReport:
    metrics:
        spread_maintenance:
            - Time-weighted average spread (actual vs target)
            - Percentage of time spread was within target
            - Spread distribution (histogram: how often at 1bps, 2bps, 5bps, etc.)
            - Maximum spread observed

        depth_maintenance:
            - Average order book depth provided (per side, in USD)
            - Percentage of time depth was above minimum requirement
            - Depth distribution by price level

        uptime:
            - Quoting uptime percentage (target: >95%)
            - Number and duration of quoting interruptions
            - Reasons for interruptions (exchange downtime, kill switch, etc.)

        comparison:
            - Our spread vs market average spread
            - Our depth vs total market depth
            - Market share of order book
```

### 14.4 Monthly Performance Reports

```
MonthlyReport:
    executive_summary:
        - Total trading volume
        - Net P&L and P&L attribution
        - Sharpe ratio (annualized)
        - Maximum drawdown
        - Return on deployed capital

    sla_compliance:
        - Uptime achieved vs SLA target
        - Spread compliance % vs target
        - Depth compliance % vs target
        - Number of SLA breaches and causes

    market_analysis:
        - Market conditions summary (volatility, volume trends)
        - Impact of market events on performance
        - Comparison vs benchmark (e.g., passive holding)

    operational_summary:
        - Number of incidents and severity
        - System uptime and availability
        - Strategy parameter changes
        - Infrastructure changes

    recommendations:
        - Suggested parameter adjustments
        - New pair opportunities
        - Risk management improvements
```

### 14.5 SLA Dashboard

**Typical SLA Commitments:**
```
SLA:
    uptime: >= 95% (quoting active during market hours)
    max_spread: <= 2% for majors, <= 5% for altcoins
    min_depth: >= $10,000 per side for majors
    response_time: quotes updated within 1 second of market move
    reporting: daily summary by 09:00 UTC, monthly report by 5th of month

Dashboard Components:
    current_sla_status:
        - Green/Yellow/Red indicator per metric
        - Current value vs target
        - Trend (improving/stable/degrading)

    historical_compliance:
        - Daily compliance % for past 30 days
        - Monthly compliance % for past 12 months
        - Heatmap: compliance by hour-of-day

    breach_log:
        - Timestamp, duration, metric breached, reason, impact
        - Root cause for each breach
        - Remediation taken

    financial_impact:
        - Rebate earned vs potential rebate at 100% compliance
        - Cost of breaches (if penalty clause exists)
```

### 14.6 Third-Party Monitoring (Coinwatch Track / Kaiko)

Projects increasingly use third-party monitoring to verify their market maker:
- **Coinwatch Track**: real-time monitoring of order placement, spread maintenance, and inventory levels via exchange API data
- **Kaiko**: institutional-grade market data and analytics for benchmarking MM performance
- **CoinGecko/CMC**: public metrics that token projects monitor (volume, spread, depth)

Our system should be prepared for clients cross-referencing our reports with these independent data sources.

### 14.7 Gap Analysis vs. Our Current System

| Feature | Current Status | Priority |
|---------|---------------|----------|
| Real-time position dashboard | dashboard crate has basic state | HIGH (extend significantly) |
| Fill quality reports | Not implemented | HIGH |
| Spread quality metrics | sla.rs tracks some metrics | MEDIUM (extend) |
| Monthly report generation | Not implemented | MEDIUM |
| SLA dashboard | sla.rs has SlaTracker | MEDIUM (extend to dashboard) |
| P&L attribution reports | pnl.rs has PnlTracker | MEDIUM (extend to client reports) |
| Export in PDF/CSV | Not implemented | LOW |
| Client-specific views | Not implemented | MEDIUM |

---

## Summary: Critical Gaps (Priority Order)

### P0 - Must Have Before Production

1. **Structured audit trail** (append-only trade log with all order lifecycle events)
2. **Secrets management** (Vault or equivalent -- no plaintext keys in config)
3. **Balance reconciliation** (internal ledger vs exchange balance comparison)
4. **Incident runbooks** (documented procedures for all P1 scenarios)

### P1 - Must Have for Professional Operation

5. **ESMA-compatible JSON trade log export** (MiCA compliance)
6. **PagerDuty/Telegram alerting** (tiered alerts, on-call rotation)
7. **Exchange downtime protocol** (formal disconnection handling with reconciliation)
8. **API key rotation automation** (30-90 day rotation, no manual key handling)
9. **Blue/green deployment** (zero-downtime upgrades)
10. **Performance benchmarks** (criterion.rs, regression testing)
11. **Client-facing position API** (REST + WebSocket for real-time reporting)
12. **TWAP execution engine** (for inventory unwinding and client orders)

### P2 - Expected by Institutional Clients

13. **Fill quality reporting**
14. **Spread quality / SLA dashboard**
15. **Monthly performance reports**
16. **Cross-venue rebalancing**
17. **Iceberg orders**
18. **VWAP execution**
19. **Peg orders**
20. **Canary deployment**
21. **Surveillance data export** (Solidus Labs / Chainalysis compatible)
22. **Token lending management**

### P3 - Competitive Advantage

23. **A/B testing framework for strategies**
24. **Conditional order engine**
25. **Client-specific strategy configuration**
26. **RFQ/OTC interface**
27. **Third-party monitoring compatibility**
28. **Settlement netting engine**

---

## Sources (Sections 7-14)

### Compliance & Regulation
- [ESMA MiCA Activities](https://www.esma.europa.eu/esmas-activities/digital-finance-and-innovation/markets-crypto-assets-regulation-mica)
- [MiCA Regulation Updated Guide 2026 (InnReg)](https://www.innreg.com/blog/mica-regulation-guide)
- [MiCA Regulation 2026 Guide (Adam Smith)](https://adamsmith.lt/en/mica-license-2025/)
- [MiCA for Crypto Exchanges Tactical Playbook (Flagright)](https://www.flagright.com/post/mica-for-crypto-exchanges-a-tactical-aml-monitoring-playbook)
- [ESMA Statement on MiCA Data Standards](https://www.esma.europa.eu/sites/default/files/2025-11/ESMA75-1303207761-6284_Statement_to_support_the_smooth_implementation_of_MiCA_standards_and_format.pdf)
- [ESMA MiCA Technical Standards (Ashurst)](https://www.ashurst.com/en/insights/esma-consults-on-micar-technical-standards/)
- [Article 7 Record Keeping (Better Regulation)](https://service.betterregulation.com/document/688642)
- [MiFID II Guidelines on Transaction Reporting (ESMA)](https://www.esma.europa.eu/document/guidelines-transaction-reporting-order-record-keeping-and-clock-synchronisation-under-mifid)
- [Navigating MiCA Compliance (Global Relay)](https://www.globalrelay.com/resources/the-compliance-hub/rules-and-regulations/navigating-mica-compliance-for-crypto-asset-service-providers/)

### Market Surveillance
- [Solidus Labs Trade Surveillance](https://www.soliduslabs.com/solutions/trade-surveillance)
- [Solidus Labs: Crypto Market Makers Under the Spotlight](https://www.soliduslabs.com/post/enforcement-around-the-corner-crypto-market-makers-under-the-spotlight)
- [Solidus Labs HALO Platform](https://www.soliduslabs.com/solutions/platform)
- [Chainalysis: Crypto Insider and Wash Trading](https://www.chainalysis.com/blog/crypto-insider-and-wash-trading-ep-101/)

### Client APIs & Market Maker Platforms
- [GSR One Platform](https://www.gsrone.gsr.io/)
- [GSR Unveils GSR One](https://www.gsr.io/insights/gsr-unveils-gsr-one-a-unified-platform-redefining-transparency-across-trading-treasury-and-market-making)
- [GSR Expands Institutional Platform (CoinDesk)](https://www.coindesk.com/business/2025/11/21/gsr-expands-institutional-platform-to-raise-transparency-control-in-crypto-trading)
- [Flowdesk: Retainer vs Loan/Call Model](https://flowdesk.co/updates/blogs/67215e228e4bf46d9bd3f247/)
- [Wintermute Zero-Fee OTC Platform (CoinDesk)](https://www.coindesk.com/business/2022/04/06/crypto-trading-firm-wintermute-launches-zero-fee-otc-platform)
- [Token Lending Agreements Guide (RealDealDocs)](https://realdealdocs.com/the-essential-guide-to-token-lending-agreements-what-every-business-needs-to-know/)

### Alerting & Monitoring
- [PagerDuty: CTC Case Study](https://www.pagerduty.com/customer/ctc/)
- [PagerDuty: IG Case Study](https://www.pagerduty.com/customer/ig/)

### Reconciliation & Post-Trade
- [Reconciliation Tools for Multi-Layer Crypto Operations (FinchTrade)](https://finchtrade.com/blog/reconciliation-tools-for-multi-layer-crypto-operations)
- [Crypto Post-Trade Workflows Explained (FinchTrade)](https://finchtrade.com/blog/crypto-post-trade-workflows-explained-clearing-settlement-and-reconciliation-for-institutions)
- [Post-Trade Reconciliation Automation (FinchTrade)](https://finchtrade.com/blog/post-trade-reconciliation-automation-for-crypto-transactions)
- [Crypto Reconciliation Guide (SolveXia)](https://www.solvexia.com/blog/crypto-reconciliation)
- [Crypto Middle Office Reconciliation (Cryptoworth)](https://www.cryptoworth.com/crypto-middle-office-reconciliation-system)
- [Multi-Exchange Integration 2026 (Darkbot)](https://darkbot.io/blog/multi-exchange-integration-in-crypto-trading-2026)

### Security
- [API Keys in Crypto: 70% Support IP Security (Darkbot)](https://darkbot.io/blog/api-keys-in-crypto-70percent-of-exchanges-support-ip-security)
- [API Key Security Guide (TradLink)](https://tradelink.pro/blog/how-to-secure-api-key/)
- [HashiCorp Vault Secrets Management](https://developer.hashicorp.com/vault/docs/secrets/key-management)
- [Vault Key Rotation](https://developer.hashicorp.com/vault/docs/internals/rotation)
- [8 API Key Management Best Practices (MultitaskAI)](https://multitaskai.com/blog/api-key-management-best-practices/)

### Advanced Order Types
- [Bybit Iceberg Orders](https://www.bybit.com/en/help-center/article/Iceberg-Order)
- [Crypto Trading Order Types (Axon Trade)](https://axon.trade/a-practical-overview-of-crypto-trading-order-types)
- [VWAP Trading Strategy (Empirica)](https://empirica.io/blog/vwap-algorithm/)
- [Interactive Brokers Order Types](https://ndcdyn.interactivebrokers.com/en/trading/ordertypes.php)
- [Smart Order Routing (Wikipedia)](https://en.wikipedia.org/wiki/Smart_order_routing)

### Deployment & Operations
- [Blue-Green and Canary Deployments (Harness)](https://www.harness.io/blog/blue-green-canary-deployment-strategies)
- [Blue-Green vs Canary: How to Choose (Codefresh)](https://codefresh.io/learn/software-deployment/blue-green-deployment-vs-canary-5-key-differences-and-how-to-choose/)
- [Incident Response Runbooks Guide (Rootly)](https://rootly.com/incident-response/runbooks)
- [Systemic Failures in Algorithmic Trading (PMC)](https://pmc.ncbi.nlm.nih.gov/articles/PMC8978471/)

### Client Expectations & SLAs
- [Crypto Market Makers: What to Expect (Efficient Frontier)](https://medium.com/efficient-frontier/cryptocurrency-exchange-market-makers-what-to-expect-b3e9cf0c86a6)
- [Crypto Market Making for Exchanges (B2Broker)](https://b2broker.com/news/crypto-market-making/)
- [How Market Making Impacts Token Performance (Empirica)](https://empirica.io/crypto-market-making/)
- [Top Crypto Exchange Liquidity Providers (ChainUp)](https://www.chainup.com/blog/top-crypto-exchange-liquidity-providers-how-to-choose-one/)
- [Crypto Market Making: Leverage Data (CoinAPI)](https://www.coinapi.io/blog/market-making-in-crypto)
