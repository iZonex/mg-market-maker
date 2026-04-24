/*
 * Vault kind catalogue + helpers.
 *
 * The VaultPage stores a heterogeneous set of secret kinds; the
 * form shape, validation, and list grouping all key off this
 * catalogue. Keeping it in a plain module keeps the Svelte files
 * small and makes adding a new kind a one-file edit.
 */

export const KINDS = [
  {
    value: 'exchange',
    label: 'Exchange credential',
    hint: 'Venue API key + secret. Pushed to Accepted agents.',
    values: [
      { key: 'api_key', label: 'API key', secret: true },
      { key: 'api_secret', label: 'API secret', secret: true },
    ],
    metadata: [
      { key: 'exchange', label: 'Exchange', required: true, enum: [
        { value: 'binance', label: 'Binance' },
        { value: 'binance_testnet', label: 'Binance testnet' },
        { value: 'bybit', label: 'Bybit' },
        { value: 'bybit_testnet', label: 'Bybit testnet' },
        { value: 'hyperliquid', label: 'HyperLiquid' },
        { value: 'hyperliquid_testnet', label: 'HyperLiquid testnet' },
      ]},
      { key: 'product', label: 'Product', required: true, enum: [
        { value: 'spot', label: 'Spot' },
        { value: 'linear_perp', label: 'Linear perp' },
        { value: 'inverse_perp', label: 'Inverse perp' },
      ]},
      { key: 'default_symbol', label: 'Default symbol', required: false, placeholder: 'BTCUSDT' },
      { key: 'max_notional_quote', label: 'Max notional (quote)', required: false, placeholder: '50000' },
    ],
    showAllowedAgents: true,
  },
  {
    value: 'telegram',
    label: 'Telegram',
    hint: 'Bot token for alert delivery.',
    values: [{ key: 'token', label: 'Bot token', secret: true }],
    metadata: [
      { key: 'chat_id', label: 'Chat ID', required: false, placeholder: '-100123456789' },
    ],
    showAllowedAgents: false,
  },
  {
    value: 'sentry',
    label: 'Sentry',
    hint: 'Sentry DSN for error reporting.',
    values: [{ key: 'dsn', label: 'DSN', secret: true }],
    metadata: [],
    showAllowedAgents: false,
  },
  {
    value: 'webhook',
    label: 'Webhook URL',
    hint: 'Outbound webhook endpoint.',
    values: [{ key: 'url', label: 'URL', secret: true }],
    metadata: [
      { key: 'description', label: 'Purpose', required: false, placeholder: 'PnL daily summary' },
    ],
    showAllowedAgents: false,
  },
  {
    value: 'smtp',
    label: 'SMTP / email',
    hint: 'Outbound email credentials for reports.',
    values: [
      { key: 'username', label: 'Username', secret: true },
      { key: 'password', label: 'Password', secret: true },
    ],
    metadata: [
      { key: 'host', label: 'Host', required: false, placeholder: 'smtp.mailgun.org' },
      { key: 'port', label: 'Port', required: false, placeholder: '587' },
    ],
    showAllowedAgents: false,
  },
  {
    value: 'rpc',
    label: 'On-chain RPC',
    hint: 'RPC provider key (Alchemy, Infura, …).',
    values: [{ key: 'api_key', label: 'API key', secret: true }],
    metadata: [
      { key: 'url', label: 'RPC URL', required: false, placeholder: 'https://eth-mainnet.g.alchemy.com' },
      { key: 'chain', label: 'Chain', required: false, placeholder: 'eth / sol / base' },
    ],
    showAllowedAgents: false,
  },
  {
    value: 'generic',
    label: 'Generic',
    hint: 'Arbitrary named value — use when no other kind fits.',
    values: [{ key: 'value', label: 'Value', secret: true }],
    metadata: [],
    showAllowedAgents: false,
  },
]

export function kindSpec(kindValue) {
  return KINDS.find((k) => k.value === kindValue) || KINDS[KINDS.length - 1]
}

export function emptyForm(kind) {
  const spec = kindSpec(kind)
  return {
    name: '',
    kind,
    description: '',
    values: Object.fromEntries(spec.values.map((v) => [v.key, ''])),
    metadata: Object.fromEntries(
      spec.metadata.filter((m) => m.enum).map((m) => [m.key, m.enum[0].value])
        .concat(spec.metadata.filter((m) => !m.enum).map((m) => [m.key, '']))
    ),
    allowed_agents: '',
    expires_at: '',
  }
}

// Tone the expiry chip based on time-to-expiry.
// Returns 'ok' (>30d), 'warn' (7-30d), 'bad' (<7d), 'expired' (<0).
export function expiryTone(ms) {
  if (!ms) return null
  const dt = ms - Date.now()
  if (dt < 0) return 'expired'
  const days = dt / (1000 * 60 * 60 * 24)
  if (days < 7) return 'bad'
  if (days < 30) return 'warn'
  return 'ok'
}

export function fmtExpiryRelative(ms) {
  if (!ms) return ''
  const dt = ms - Date.now()
  const days = Math.round(dt / (1000 * 60 * 60 * 24))
  if (days < 0) return `expired ${-days}d ago`
  if (days === 0) return 'expires today'
  if (days === 1) return 'expires in 1d'
  return `expires in ${days}d`
}
