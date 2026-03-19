# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do NOT** open a public GitHub issue
2. Email: [security contact — add your email here]
3. Include: description, reproduction steps, potential impact
4. We will acknowledge within 48 hours and provide a fix timeline

## Security Best Practices

### API Keys
- **NEVER** commit API keys to the repository
- Use environment variables: `MM_API_KEY`, `MM_API_SECRET`
- Use read-only / trade-only keys (no withdrawal permission)
- Enable IP whitelisting on all exchange API keys
- Rotate keys every 30-90 days

### Deployment
- Run as non-root user (Docker image does this by default)
- Use TLS for all exchange connections (all connectors use `https://` / `wss://`)
- Restrict dashboard port access (firewall, VPN, or bind to localhost)
- Monitor the audit trail (`data/audit/`) for anomalies

### Configuration
- The `config/default.toml` file should NOT contain secrets
- Use `.env` file (excluded from git) or environment variables for secrets
- Validate config at startup (the server does this automatically)

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Known Limitations

- The paper trading mode simulates fills locally but does not model slippage or partial fills accurately
- Backtester fill models are approximations — real fill rates depend on queue position
- The reconciliation system queries open orders periodically but may miss rapid state changes between cycles
