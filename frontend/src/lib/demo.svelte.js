/**
 * Demo mode — generates realistic fake data for screenshots.
 * Activated by ?demo in URL or when backend is unreachable.
 */

export function isDemoMode() {
  return window.location.search.includes('demo')
}

export function createDemoStore() {
  const basePrice = 67432.50
  let tick = 0

  // Pre-generate 120 points of history (1 minute at 500ms intervals).
  const now = Date.now()
  const initPnl = []
  const initSpread = []
  const initInv = []
  for (let i = 120; i > 0; i--) {
    const t = now - i * 500
    const k = 120 - i
    initPnl.push({ time: t, value: 35 + Math.sin(k * 0.05) * 8 + k * 0.06 })
    initSpread.push({ time: t, value: 3.0 + Math.sin(k * 0.15) * 1.2 + Math.random() * 0.3 })
    initInv.push({ time: t, value: 0.003 + Math.sin(k * 0.03) * 0.002 })
  }

  const state = $state({
    connected: true,
    symbols: ['BTCUSDT'],
    data: {
      BTCUSDT: generateSymbolData(basePrice),
    },
    pnlHistory: initPnl,
    spreadHistory: initSpread,
    inventoryHistory: initInv,
    fills: generateFills(basePrice),
    alerts: generateAlerts(),
  })

  // Simulate live updates every 500ms.
  setInterval(() => {
    tick++
    const mid = basePrice + Math.sin(tick * 0.1) * 15 + (Math.random() - 0.5) * 10
    const pnl = 42.37 + Math.sin(tick * 0.05) * 8 + tick * 0.06
    const spread = 3.2 + Math.sin(tick * 0.15) * 1.5 + Math.random() * 0.5
    const inv = 0.0032 + Math.sin(tick * 0.03) * 0.002

    state.data.BTCUSDT = {
      ...state.data.BTCUSDT,
      mid_price: mid.toFixed(2),
      spread_bps: spread.toFixed(1),
      inventory: inv.toFixed(6),
      inventory_value: (inv * mid).toFixed(2),
      live_orders: 6,
      vpin: (0.35 + Math.sin(tick * 0.08) * 0.15).toFixed(3),
      kyle_lambda: (0.000042 + Math.random() * 0.00001).toFixed(6),
      adverse_bps: (1.8 + Math.random() * 1.2).toFixed(2),
      // Synthesised Market Resilience — dips every 300 ticks
      // to show the panel's recovery animation.
      market_resilience: (tick % 300 < 30
        ? 0.2 + (tick % 300) / 150
        : 0.9 + Math.random() * 0.1).toFixed(3),
      // Random walk around 2.0 — elevated OTR territory.
      order_to_trade_ratio: (2.0 + Math.sin(tick * 0.05) * 1.2).toFixed(2),
      // HMA tracks mid with a small lag.
      hma_value: (mid + Math.sin(tick * 0.1 - 0.3) * 12).toFixed(2),
      volatility: (0.0234 + Math.sin(tick * 0.02) * 0.005).toFixed(4),
      sla_uptime_pct: '97.3',
      regime: tick % 200 < 120 ? 'Quiet' : tick % 200 < 160 ? 'Trending' : 'Volatile',
      pnl: {
        total: pnl.toFixed(4),
        spread: (pnl * 0.7).toFixed(4),
        inventory: (pnl * 0.15).toFixed(4),
        rebates: (pnl * 0.2).toFixed(4),
        fees: (pnl * 0.05).toFixed(4),
        round_trips: 47 + Math.floor(tick / 20),
        volume: (12840 + tick * 5).toFixed(2),
      },
      bids: generateBookSide(mid, 'bid'),
      asks: generateBookSide(mid, 'ask'),
      open_orders: generateOpenOrders(mid),
    }

    const t = Date.now()
    state.pnlHistory = [...state.pnlHistory.slice(-500), { time: t, value: pnl }]
    state.spreadHistory = [...state.spreadHistory.slice(-500), { time: t, value: spread }]
    state.inventoryHistory = [...state.inventoryHistory.slice(-500), { time: t, value: inv }]

    // Occasional fill.
    if (tick % 7 === 0) {
      const side = Math.random() > 0.5 ? 'buy' : 'sell'
      state.fills = [{
        timestamp: new Date().toISOString(),
        side,
        price: (mid + (side === 'buy' ? -2.5 : 2.5)).toFixed(2),
        qty: (0.001 + Math.random() * 0.002).toFixed(5),
        is_maker: true,
      }, ...state.fills.slice(0, 49)]
    }
  }, 500)

  function send() { /* noop in demo */ }

  return {
    get state() { return state },
    send,
  }
}

function generateSymbolData(mid) {
  return {
    symbol: 'BTCUSDT',
    mid_price: mid.toFixed(2),
    spread_bps: '3.2',
    inventory: '0.003200',
    inventory_value: (0.0032 * mid).toFixed(2),
    live_orders: 6,
    total_fills: 47,
    vpin: '0.342',
    kyle_lambda: '0.000042',
    market_resilience: '0.95',
    order_to_trade_ratio: '2.10',
    hma_value: mid.toFixed(2),
    adverse_bps: '1.83',
    volatility: '0.0234',
    kill_level: 0,
    sla_uptime_pct: '97.3',
    regime: 'Quiet',
    pnl: {
      total: '42.3700',
      spread: '29.6590',
      inventory: '6.3555',
      rebates: '8.4740',
      fees: '2.1185',
      round_trips: 47,
      volume: '12840.00',
    },
    bids: generateBookSide(mid, 'bid'),
    asks: generateBookSide(mid, 'ask'),
    open_orders: generateOpenOrders(mid),
  }
}

function generateBookSide(mid, side) {
  const levels = []
  for (let i = 0; i < 10; i++) {
    const offset = (i + 1) * 0.5 + Math.random() * 0.3
    const price = side === 'bid' ? mid - offset : mid + offset
    const qty = (0.5 + Math.random() * 3).toFixed(4)
    levels.push([price.toFixed(2), qty])
  }
  return levels
}

function generateOpenOrders(mid) {
  return [
    { side: 'buy', price: (mid - 1.2).toFixed(2), qty: '0.00100', status: 'open' },
    { side: 'buy', price: (mid - 3.5).toFixed(2), qty: '0.00100', status: 'open' },
    { side: 'buy', price: (mid - 6.1).toFixed(2), qty: '0.00100', status: 'open' },
    { side: 'sell', price: (mid + 1.3).toFixed(2), qty: '0.00100', status: 'open' },
    { side: 'sell', price: (mid + 3.8).toFixed(2), qty: '0.00100', status: 'open' },
    { side: 'sell', price: (mid + 6.4).toFixed(2), qty: '0.00100', status: 'open' },
  ]
}

function generateFills(mid) {
  const fills = []
  for (let i = 0; i < 15; i++) {
    const side = i % 3 === 0 ? 'sell' : 'buy'
    const ago = i * 45000
    fills.push({
      timestamp: new Date(Date.now() - ago).toISOString(),
      side,
      price: (mid + (side === 'buy' ? -(1 + Math.random() * 3) : (1 + Math.random() * 3))).toFixed(2),
      qty: (0.001 + Math.random() * 0.003).toFixed(5),
      is_maker: true,
    })
  }
  return fills
}

function generateAlerts() {
  return [
    { severity: 'Warning', title: 'VPIN elevated', message: 'VPIN reached 0.72 on BTCUSDT — spreads widened' },
    { severity: 'Info', title: 'Regime change', message: 'BTCUSDT: Quiet → Trending' },
    { severity: 'Info', title: 'Reconciliation OK', message: 'Orders and balances match exchange state' },
  ]
}
