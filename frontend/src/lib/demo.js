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

  const state = $state({
    connected: true,
    symbols: ['BTCUSDT'],
    data: {
      BTCUSDT: generateSymbolData(basePrice),
    },
    pnlHistory: [],
    spreadHistory: [],
    inventoryHistory: [],
    fills: generateFills(basePrice),
    alerts: generateAlerts(),
  })

  // Simulate live updates.
  setInterval(() => {
    tick++
    const mid = basePrice + Math.sin(tick * 0.1) * 15 + (Math.random() - 0.5) * 10
    const pnl = 42.37 + Math.sin(tick * 0.05) * 8 + tick * 0.02
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
    }

    const now = Date.now()
    state.pnlHistory = [...state.pnlHistory.slice(-300), { time: now, value: pnl }]
    state.spreadHistory = [...state.spreadHistory.slice(-300), { time: now, value: spread }]
    state.inventoryHistory = [...state.inventoryHistory.slice(-300), { time: now, value: inv }]

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
    {
      severity: 'Warning',
      title: 'VPIN elevated',
      message: 'VPIN reached 0.72 on BTCUSDT — spreads widened',
    },
    {
      severity: 'Info',
      title: 'Regime change',
      message: 'BTCUSDT: Quiet → Trending',
    },
    {
      severity: 'Info',
      title: 'Reconciliation OK',
      message: 'Orders and balances match exchange state',
    },
  ]
}
