/**
 * WebSocket reactive store for real-time market maker data.
 * Connects to Rust backend at /ws and keeps state updated.
 *
 * As of Epic 1 security hardening the backend rejects WS
 * upgrades without a valid session token passed as `?token=`.
 * The store now takes an `auth` argument and reconnects
 * whenever the token changes (logout → login) instead of
 * permanently failing with 401.
 */

export function createWsStore(auth) {
  // Reactive state using Svelte 5 runes approach — we use plain objects
  // and let components poll / use $effect.
  const state = $state({
    connected: false,
    symbols: [],
    /// Currently-selected symbol (Epic 37.3). Panels should
    /// prefer `state.activeSymbol ?? state.symbols[0]` over
    /// `symbols[0]` so the operator's switcher pick flows.
    activeSymbol: '',
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
    // Per-venue balance snapshots keyed by symbol.
    venueBalances: {},
    // Per-symbol last-rx timestamps for stale-badge rendering.
    venueBalancesRxMs: {},
  })

  let ws = null
  let reconnectTimer = null
  // When WS upgrade keeps failing immediately, the most likely
  // cause is an expired / stale token (we re-sign tokens on every
  // backend restart because MM_AUTH_SECRET rotates). Count the
  // rapid closes — after 3 closes inside 6 s force a logout so
  // the operator sees the Login screen instead of a permanently
  // stale dashboard.
  let rapidCloseCount = 0
  let rapidCloseWindowStart = 0

  function connect() {
    // Backend rejects WS upgrades without a valid token. Skip
    // the connect attempt entirely if there is no session yet —
    // App.svelte only mounts us after login, but a logout-in-
    // another-tab can race this.
    const token = auth?.state?.token || ''
    if (!token) {
      reconnectTimer = setTimeout(connect, 1000)
      return
    }
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const url = `${protocol}//${window.location.host}/ws?token=${encodeURIComponent(token)}`

    const openedAt = Date.now()
    ws = new WebSocket(url)

    ws.onopen = () => {
      state.connected = true
      rapidCloseCount = 0
      console.log('WS connected')
    }

    ws.onclose = () => {
      state.connected = false
      // If the socket closed within 500 ms of opening, treat it as
      // an auth-layer rejection. Browser WebSocket API does not
      // expose the 401 status, but this heuristic catches the most
      // common case: stale token after a server restart.
      const now = Date.now()
      if (now - openedAt < 500) {
        if (now - rapidCloseWindowStart > 6000) {
          rapidCloseWindowStart = now
          rapidCloseCount = 1
        } else {
          rapidCloseCount += 1
        }
        if (rapidCloseCount >= 3) {
          console.warn('WS rejected repeatedly — logging out')
          auth?.logout?.()
          return
        }
      }
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
          const now = Date.now()
          for (const sym of msg.symbols) {
            state.data[sym.symbol] = { ...sym, _rx_ms: now }
          }
        }
        break

      case 'update':
        // Incremental symbol update.
        if (msg.symbol && msg.data) {
          // Epic 37.1 — stamp the receive time so panels can
          // grey out when WS stalls. Without this the screen
          // shows stale numbers as if they were fresh, which is
          // the most common way MM dashboards eat P&L.
          const now = Date.now()
          state.data[msg.symbol] = { ...msg.data, _rx_ms: now }
          if (!state.symbols.includes(msg.symbol)) {
            state.symbols = [...state.symbols, msg.symbol]
          }

          // Compute QUOTED spread from our own open orders
          // (best sell ask − best buy bid) / mid × 10_000. Venue
          // spread on tick-size-limited pairs is useless as a
          // trading signal — the quoted spread is what the MM
          // actually posts. Falls back to venue spread when no
          // live orders.
          const mid = parseFloat(msg.data.mid_price || 0)
          let quoted = null
          const orders = msg.data.open_orders || []
          if (orders.length && mid > 0) {
            let bb = -Infinity, ba = Infinity
            for (const o of orders) {
              const p = parseFloat(o.price || 0)
              const side = (o.side || '').toLowerCase()
              if (side === 'buy'  && p > bb) bb = p
              if (side === 'sell' && p < ba) ba = p
            }
            if (Number.isFinite(bb) && Number.isFinite(ba) && ba > bb) {
              quoted = ((ba - bb) / mid) * 10_000
            }
          }
          const spreadValue = quoted ?? parseFloat(msg.data.spread_bps || '0')

          state.pnlHistory = [...state.pnlHistory.slice(-500), {
            time: now, value: parseFloat(msg.data.pnl?.total || '0')
          }]
          state.spreadHistory = [...state.spreadHistory.slice(-500), {
            time: now, value: spreadValue,
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
          state.data[msg.symbol]._rx_ms = Date.now()
        }
        break

      case 'venue_balances':
        // Per-symbol per-venue balance snapshots pushed by the
        // engine on every refresh_balances tick. Panel binds to
        // `state.venueBalances[symbol]` for live updates.
        if (msg.symbol) {
          state.venueBalances[msg.symbol] = msg.rows || []
          state.venueBalancesRxMs = { ...state.venueBalancesRxMs, [msg.symbol]: Date.now() }
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

  // 23-P1-4 — the backend now emits per-symbol `type: "update"` WS
  // pushes from `DashboardState::update()` on every refresh tick, so
  // live data flows through the `onmessage` handler above. The
  // `/api/status` poll remains as a fallback for WS-disconnected
  // sessions only: it refreshes the symbols list and `_rx_ms` so the
  // stale badge reflects reality, but it no longer appends to the
  // history arrays (the WS path owns that to avoid double-points).
  // On 401 we log out so the operator sees the Login screen instead
  // of silently-stale panels.
  setInterval(async () => {
    if (state.connected) return
    try {
      const token = auth?.state?.token || ''
      if (!token) return
      const resp = await fetch('/api/status', {
        headers: { Authorization: `Bearer ${token}` },
      })
      if (resp.status === 401) {
        auth?.logout?.()
        return
      }
      if (resp.ok) {
        const symbols = await resp.json()
        if (!Array.isArray(symbols)) return
        state.symbols = symbols.map(s => s.symbol)
        const now = Date.now()
        for (const sym of symbols) {
          state.data[sym.symbol] = { ...sym, _rx_ms: now }
        }
      }
    } catch (_) { /* ignore transient errors */ }
  }, 2000)

  // Preload historical buffers so the dashboard opens with the
  // MM's actual past — not blank panels that fill up over 30 s of
  // polling. Backend exposes rolling per-symbol buffers on these
  // endpoints:
  //   /api/v1/pnl/timeseries?symbol=…  — 24 h PnL, 1-min cadence
  //   /api/v1/fills/recent?symbol=…    — last N fills with NBBO
  async function preloadHistory(sym) {
    if (!sym) return
    const token = auth?.state?.token || ''
    if (!token) return
    const headers = { Authorization: `Bearer ${token}` }

    // PnL timeseries
    try {
      const resp = await fetch(`/api/v1/pnl/timeseries?symbol=${encodeURIComponent(sym)}`, { headers })
      if (resp.ok) {
        const rows = await resp.json()
        if (Array.isArray(rows) && rows.length > 0) {
          state.pnlHistory = rows.map(r => ({
            time: r.timestamp_ms,
            value: parseFloat(r.total_pnl || '0'),
          }))
        }
      }
    } catch (_) {}

    // UX-2 — spread-bps rolling history backfill. Charts
    // previously warmed up from live ticks (empty on page
    // load, ~4 min to fill). Backfilling from the engine's
    // 1440-point buffer gives operators up to 24 hours of
    // context immediately.
    try {
      const resp = await fetch(`/api/v1/spread/timeseries?symbol=${encodeURIComponent(sym)}`, { headers })
      if (resp.ok) {
        const rows = await resp.json()
        if (Array.isArray(rows) && rows.length > 0) {
          state.spreadHistory = rows.map(r => ({
            time: r.timestamp_ms,
            value: parseFloat(r.value || '0'),
          }))
        }
      }
    } catch (_) {}

    // UX-2 — inventory rolling history backfill.
    try {
      const resp = await fetch(`/api/v1/inventory/timeseries?symbol=${encodeURIComponent(sym)}`, { headers })
      if (resp.ok) {
        const rows = await resp.json()
        if (Array.isArray(rows) && rows.length > 0) {
          state.inventoryHistory = rows.map(r => ({
            time: r.timestamp_ms,
            value: parseFloat(r.value || '0'),
          }))
        }
      }
    } catch (_) {}

    // Recent fills
    try {
      const resp = await fetch(`/api/v1/fills/recent?symbol=${encodeURIComponent(sym)}&limit=50`, { headers })
      if (resp.ok) {
        const rows = await resp.json()
        if (Array.isArray(rows) && rows.length > 0) {
          // Normalise to the same shape the live `fill` WS push uses.
          state.fills = rows.map(r => ({
            timestamp: r.timestamp,
            symbol:    r.symbol,
            side:      (r.side || '').toLowerCase(),
            price:     r.price,
            qty:       r.qty,
            is_maker:  !!r.is_maker,
            fee:       r.fee,
            slippage_bps: r.slippage_bps,
          }))
        }
      }
    } catch (_) {}
  }

  function setActiveSymbol(sym) {
    state.activeSymbol = sym
    preloadHistory(sym)
  }

  // Kick a preload once symbols land from the first poll.
  let preloaded = false
  $effect.root(() => {
    $effect(() => {
      if (!preloaded && state.symbols.length > 0) {
        preloaded = true
        preloadHistory(state.activeSymbol || state.symbols[0])
      }
    })
  })

  return {
    get state() { return state },
    send,
    setActiveSymbol,
    preloadHistory,
  }
}
