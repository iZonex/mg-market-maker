# Market Maker Dashboard (Frontend)

Real-time web dashboard for the Market Maker engine.

## Tech Stack

- **Svelte 5** — reactive UI framework (compiled, no virtual DOM)
- **TradingView Lightweight Charts** — professional financial charts
- **Vite** — fast build tool
- **WebSocket** — real-time updates from Rust backend

## Development

```bash
# Install dependencies
npm install

# Start dev server (proxies API/WS to localhost:9090)
npm run dev

# Open http://localhost:3000
```

The Rust backend must be running on port 9090 (`cargo run -p mm-server`).

## Build for Production

```bash
npm run build
# Output in dist/
```

## Architecture

```
Frontend (Svelte)          Rust Backend (Axum)
:3000                      :9090
┌────────────┐             ┌────────────┐
│ WebSocket  │────ws://───→│ /ws        │ real-time updates
│ REST polls │───http://──→│ /api/*     │ fallback polling
│            │             │ /health    │
│            │             │ /metrics   │
└────────────┘             └────────────┘
```

## Panels

| Panel | Data Source | Description |
|-------|-----------|-------------|
| **Header** | WS snapshot | Mid price, spread, PnL, inventory, kill level, SLA, regime |
| **PnL Chart** | WS time series | Real-time total PnL (lightweight-charts) |
| **Spread Chart** | WS time series | Bid-ask spread in bps over time |
| **Order Book** | WS book events | Top 10 bid/ask levels with depth bars |
| **Inventory & Signals** | WS snapshot | Position, VPIN, Kyle's Lambda, adverse selection, volatility |
| **Controls & PnL** | WS + REST | PnL attribution breakdown + kill switch buttons |
| **Open Orders** | WS snapshot | Live orders table |
| **Fill History** | WS fill events | Recent fills with side/price/qty/role |
| **Alert Log** | WS alert events | Severity-colored alert stream |
