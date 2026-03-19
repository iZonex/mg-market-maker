# Deployment Guide

## Quick Start (Paper Trading)

```bash
git clone https://github.com/your-org/market-maker.git
cd market-maker
cargo run -p mm-server
```

This starts in paper mode by default with localhost exchange.

## Docker (Recommended for Production)

### Prerequisites
- Docker and Docker Compose

### Steps

1. **Configure:**
   ```bash
   cp config/default.toml config/production.toml
   # Edit config/production.toml with your exchange URLs
   ```

2. **Set secrets:**
   ```bash
   cat > .env << 'EOF'
   MM_API_KEY=your-exchange-api-key
   MM_API_SECRET=your-exchange-api-secret
   MM_MODE=live
   MM_TELEGRAM_TOKEN=your-telegram-bot-token
   MM_TELEGRAM_CHAT=your-telegram-chat-id
   EOF
   ```

3. **Start:**
   ```bash
   docker compose up -d
   ```

4. **Monitor:**
   - Dashboard: http://localhost:9090/api/status
   - Prometheus: http://localhost:9091
   - Grafana: http://localhost:3000 (admin/admin)
   - Audit trail: `docker compose exec market-maker cat data/audit/btcusdt.jsonl`

### Endpoints

| URL | Description |
|-----|-------------|
| `GET /health` | Health check |
| `GET /api/status` | All symbols state |
| `GET /api/v1/positions` | Current positions |
| `GET /api/v1/pnl` | PnL breakdown |
| `GET /api/v1/sla` | SLA compliance |
| `GET /api/v1/report/daily` | Daily report |
| `GET /metrics` | Prometheus metrics |

## Native Binary

```bash
cargo build --release
./target/release/mm-server
```

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `MM_CONFIG` | No | Config file path (default: `config/default.toml`) |
| `MM_API_KEY` | For live | Exchange API key |
| `MM_API_SECRET` | For live | Exchange API secret |
| `MM_MODE` | No | `live` or `paper` (default: `live`) |
| `MM_TELEGRAM_TOKEN` | No | Telegram bot token |
| `MM_TELEGRAM_CHAT` | No | Telegram chat ID |
| `RUST_LOG` | No | Log level (default: `info`) |

## Security Checklist

- [ ] API keys have **no withdrawal permission**
- [ ] IP whitelisting enabled on exchange
- [ ] Dashboard port restricted (firewall/VPN)
- [ ] `.env` file has `chmod 600`
- [ ] Audit trail directory is backed up
- [ ] Telegram bot token is kept secret
- [ ] Running as non-root user (Docker does this)
