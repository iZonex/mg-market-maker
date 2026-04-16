# Operations & Troubleshooting Guide

## Modes

| Mode | Env | Behavior |
|------|-----|----------|
| `live` | `MM_MODE=live` | Real orders. Preflight must pass. |
| `paper` | `MM_MODE=paper` | Connects to real feed, preflight warnings don't block. |
| `smoke` | `MM_MODE=smoke` | Connector test only: subscribe, place/cancel test order, exit. |

## Pre-Flight Checks

Run automatically on startup. In `live` mode, any failure aborts.

| Check | What | Fail means |
|-------|------|-----------|
| `venue_connectivity` | `health_check()` | Exchange unreachable |
| `{symbol}_product_spec` | `get_product_spec()` | Symbol not found or invalid |
| `{symbol}_tick_size` | tick > 0 | Can't place orders |
| `{symbol}_fees` | Non-default fees | VIP tier not loaded |
| `balances` | Any balance > 0 | Account empty |
| `rate_limit` | Remaining > 100 | Rate budget low |
| `config_gamma` | gamma ≤ 5 | Spread will be too wide |
| `config_order_size` | > 0 | No orders will be placed |

## Stale Book Protection

If no book update for `stale_book_timeout_secs` (default 10s):
1. Engine cancels all orders
2. Pauses quoting
3. Logs `CircuitBreakerTripped` to audit
4. **Auto-resumes** when fresh data arrives

No manual intervention needed.

## Kill Switch

### Automatic Escalation

| Trigger | Level | Action |
|---------|-------|--------|
| Daily PnL < `daily_loss_warning` | 1 | Widen spreads ×2 |
| Position value > `max_position_value` | 2 | Stop new orders |
| Daily PnL < `daily_loss_limit` | 3 | Cancel all orders |
| Manual or paired-unwind | 4 | Flatten all positions (TWAP) |
| Manual only | 5 | Disconnect from exchange |

### Manual Reset

```bash
# Via API
curl -X POST http://localhost:9090/api/admin/symbols/BTCUSDT/resume

# Via Telegram
/resume BTCUSDT
```

## Hot Config Reload

Change parameters without restart:

```bash
# Single symbol
curl -X POST http://localhost:9090/api/admin/config/BTCUSDT \
  -H "Content-Type: application/json" \
  -d '{"field": "Gamma", "value": 0.2}'

# All symbols
curl -X POST http://localhost:9090/api/admin/config \
  -H "Content-Type: application/json" \
  -d '{"field": "MinSpreadBps", "value": 8}'
```

Available overrides: `Gamma`, `MinSpreadBps`, `OrderSize`, `MaxDistanceBps`, `NumLevels`, `MomentumEnabled`, `AmendEnabled`, `MaxInventory`, `PauseQuoting`, `ResumeQuoting`, `PortfolioRiskMult`.

## Recording Market Data

```toml
record_market_data = true
```

Writes `data/recorded/{symbol}.jsonl` with `BookSnapshot` + `Trade` events. Append mode — survives restarts. Use for backtesting and calibration.

## Checkpoint Recovery

```toml
checkpoint_restore = true
```

On startup with this flag:
1. Loads `data/checkpoint.json`
2. Validates (timestamp not future, inventory/price sane)
3. Restores `InventoryManager` state
4. Logs restore event to audit trail

Without this flag (default): engines start with zero inventory and rely on reconciliation.

## Common Issues

### "preflight failed: symbol not found"

The symbol doesn't exist on the exchange or the product spec endpoint is wrong.
- Check `exchange_type` matches your API key
- Verify the symbol format (Binance: `BTCUSDT`, Bybit: `BTCUSDT`, HyperLiquid: `BTC`)

### "rate_limit_remaining = 0"

Too many API calls. Possible causes:
- `refresh_interval_ms` too low (minimum recommended: 200ms)
- Multiple instances sharing same API key
- Fix: increase `refresh_interval_ms` or use separate API keys

### "circuit breaker tripped: stale book"

No market data for `stale_book_timeout_secs`.
- Check WS connection (exchange may have dropped it)
- Check network connectivity
- Engine auto-resumes when data returns

### "inventory drift detected"

`InventoryManager` tracker disagrees with exchange balance.
- Possible causes: missed fill, external transfer, API key shared with another bot
- Check audit log: `data/audit/{symbol}.jsonl`
- If `inventory_drift_auto_correct = true`: tracker auto-resets to match exchange
- If `false`: operator must investigate and restart

### Fills not arriving

- Check `user_stream_enabled = true` (Binance listen-key)
- Check exchange API key has trading permissions
- Check symbol is in `Trading` status (not halted)

### High adverse selection

PnL negative despite making markets:
- Increase `gamma` (wider spread)
- Increase `min_spread_bps`
- Check VPIN gauge — if consistently > 0.7, the pair has toxic flow
- Consider switching to a less liquid pair where spread capture is higher

## Log Files

```bash
# Engine logs (if log_file set in config)
tail -f data/mm.log

# Audit trail (per symbol)
tail -f data/audit/btcusdt.jsonl | jq .

# Fill history
tail -f data/fills.jsonl | jq .

# Recorded market data
wc -l data/recorded/btcusdt.jsonl  # count events
```

## Prometheus Metrics

Key gauges to monitor:

| Metric | Alert if |
|--------|----------|
| `mm_pnl_total` | Negative and declining |
| `mm_spread_bps` | > 2× configured min_spread |
| `mm_inventory` | Near max_inventory |
| `mm_vpin` | > 0.7 sustained |
| `mm_kill_switch_level` | > 0 |
| `mm_sla_presence_pct_24h` | < 95% |
| `mm_portfolio_var_95` | Breaching limit |

## Daily Operations Checklist

1. Check `GET /api/v1/system/preflight` — all green?
2. Check `GET /api/v1/pnl` — positive spread capture?
3. Check `GET /api/v1/sla` — uptime > 95%?
4. Check inventory — not pinned at max?
5. Review audit log for warnings/errors
6. Backup `data/checkpoint.json`
