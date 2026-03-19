/**
 * WebSocket reactive store for real-time market maker data.
 * Connects to Rust backend at /ws and keeps state updated.
 */

export function createWsStore() {
  // Reactive state using Svelte 5 runes approach — we use plain objects
  // and let components poll / use $effect.
  const state = $state({
    connected: false,
    symbols: [],
    // Per-symbol data (keyed by symbol string).
    data: {},
    // Time series for charts.
    pnlHistory: [],
    spreadHistory: [],
    inventoryHistory: [],
    // Latest fills.
    fills: [],
    // Alerts.
    alerts: [],
  })

  let ws = null
  let reconnectTimer = null

  function connect() {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const url = `${protocol}//${window.location.host}/ws`

    ws = new WebSocket(url)

    ws.onopen = () => {
      state.connected = true
      console.log('WS connected')
    }

    ws.onclose = () => {
      state.connected = false
      console.log('WS disconnected, reconnecting in 2s...')
      reconnectTimer = setTimeout(connect, 2000)
    }

    ws.onerror = (e) => {
      console.error('WS error:', e)
    }

    ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data)
        handleMessage(msg)
      } catch (e) {
        console.warn('Failed to parse WS message:', e)
      }
    }
  }

  function handleMessage(msg) {
    switch (msg.type) {
      case 'snapshot':
        // Full state snapshot from server.
        if (msg.symbols) {
          state.symbols = msg.symbols.map(s => s.symbol)
          for (const sym of msg.symbols) {
            state.data[sym.symbol] = sym
          }
        }
        break

      case 'update':
        // Incremental symbol update.
        if (msg.symbol && msg.data) {
          state.data[msg.symbol] = msg.data

          // Append to time series.
          const now = Date.now()
          state.pnlHistory = [...state.pnlHistory.slice(-500), {
            time: now, value: parseFloat(msg.data.pnl?.total || '0')
          }]
          state.spreadHistory = [...state.spreadHistory.slice(-500), {
            time: now, value: parseFloat(msg.data.spread_bps || '0')
          }]
          state.inventoryHistory = [...state.inventoryHistory.slice(-500), {
            time: now, value: parseFloat(msg.data.inventory || '0')
          }]
        }
        break

      case 'fill':
        state.fills = [msg.data, ...state.fills.slice(0, 99)]
        break

      case 'alert':
        state.alerts = [msg.data, ...state.alerts.slice(0, 49)]
        break

      case 'book':
        if (msg.symbol) {
          if (!state.data[msg.symbol]) state.data[msg.symbol] = {}
          state.data[msg.symbol].bids = msg.bids || []
          state.data[msg.symbol].asks = msg.asks || []
        }
        break
    }
  }

  function send(msg) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg))
    }
  }

  // Auto-connect.
  connect()

  // Fallback: poll REST API every 5s if WS not available.
  setInterval(async () => {
    if (!state.connected) {
      try {
        const resp = await fetch('/api/status')
        if (resp.ok) {
          const symbols = await resp.json()
          state.symbols = symbols.map(s => s.symbol)
          for (const sym of symbols) {
            state.data[sym.symbol] = sym
          }
        }
      } catch (_) { /* ignore */ }
    }
  }, 5000)

  return {
    get state() { return state },
    send,
  }
}
