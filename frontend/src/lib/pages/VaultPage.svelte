<script>
  /*
   * Vault — admin-only encrypted secret store.
   *
   * Unified store for everything with a password or a token:
   *   - exchange credentials (api_key + api_secret + exchange/product)
   *   - telegram bot tokens
   *   - sentry DSNs
   *   - webhook URLs
   *   - SMTP passwords
   *   - on-chain RPC api keys
   *   - …arbitrary named secrets
   *
   * Kind tag on each entry determines the form shape + server-side
   * required fields. Exchange entries are the only kind pushed to
   * agents; the rest live server-side.
   *
   * The dashboard never reads values back after save. Rotate = new
   * value. Delete = gone. Matches the "no reveal" rule of any
   * serious secrets manager.
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  // ── Catalogue of kinds. Drives the dropdown + dynamic form.
  const KINDS = [
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

  function kindSpec(kindValue) {
    return KINDS.find(k => k.value === kindValue) || KINDS[KINDS.length - 1]
  }

  let rows = $state([])
  let loading = $state(true)
  let loadError = $state(null)

  // 'idle' | 'pick-kind' | 'create' | 'rotate'
  // 'pick-kind' → operator picks what kind of secret they want
  // to add first, THEN the form adapts. Without this step the
  // form defaulted to exchange and made the vault feel
  // exchange-only even though it stores anything.
  let mode = $state('idle')
  let editingName = $state(null)
  let form = $state(emptyForm('generic'))
  let showSecrets = $state(false)
  let formBusy = $state(false)
  let formMsg = $state(null)
  let busyName = $state({})
  let confirmDeleteName = $state(null)

  function emptyForm(kind) {
    return {
      name: '',
      kind,
      description: '',
      values: Object.fromEntries(kindSpec(kind).values.map(v => [v.key, ''])),
      metadata: Object.fromEntries(
        kindSpec(kind).metadata.filter(m => m.enum).map(m => [m.key, m.enum[0].value])
          .concat(kindSpec(kind).metadata.filter(m => !m.enum).map(m => [m.key, '']))
      ),
      allowed_agents: '',
      // Wave C6 — expiry is optional. UI input is a native
      // date (YYYY-MM-DD); submit converts to epoch millis.
      // Empty string = no expiry.
      expires_at: '',
    }
  }

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/vault')
      loadError = null
    } catch (e) {
      loadError = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, 5_000)
    return () => clearInterval(t)
  })

  function startPickKind() {
    mode = 'pick-kind'
    editingName = null
    showSecrets = false
    formMsg = null
  }

  function pickKindAndProceed(kind) {
    mode = 'create'
    form = emptyForm(kind)
    formMsg = null
  }

  function startCreate() {
    mode = 'create'
    editingName = null
    form = emptyForm('exchange')
    showSecrets = false
    formMsg = null
  }

  function startRotate(row) {
    mode = 'rotate'
    editingName = row.name
    const spec = kindSpec(row.kind)
    form = {
      name: row.name,
      kind: row.kind,
      description: row.description || '',
      // values always blank on rotate — operator enters new plaintext
      values: Object.fromEntries(spec.values.map(v => [v.key, ''])),
      metadata: Object.fromEntries(
        spec.metadata.map(m => [m.key, (row.metadata && row.metadata[m.key]) || (m.enum ? m.enum[0].value : '')])
      ),
      allowed_agents: (row.allowed_agents || []).join(', '),
      expires_at: row.expires_at_ms
        ? new Date(row.expires_at_ms).toISOString().slice(0, 10)
        : '',
    }
    showSecrets = false
    formMsg = null
  }

  function onKindChange() {
    // Re-seed form for the new kind while preserving name/description.
    const keep = { name: form.name, description: form.description, kind: form.kind }
    form = { ...emptyForm(form.kind), ...keep }
  }

  async function submit(e) {
    e.preventDefault()
    formMsg = null
    const spec = kindSpec(form.kind)
    if (!form.name.trim()) {
      formMsg = { tone: 'err', text: 'Name is required' }; return
    }
    for (const v of spec.values) {
      if (!form.values[v.key]) {
        formMsg = { tone: 'err', text: `${v.label} is required` }; return
      }
    }
    for (const m of spec.metadata.filter(m => m.required)) {
      if (!form.metadata[m.key]) {
        formMsg = { tone: 'err', text: `${m.label} is required` }; return
      }
    }
    const values = {}
    for (const [k, v] of Object.entries(form.values)) if (v) values[k] = v
    const metadata = {}
    for (const [k, v] of Object.entries(form.metadata)) if (v !== undefined && v !== '') metadata[k] = v
    const allowed_agents = form.allowed_agents
      .split(',').map(s => s.trim()).filter(Boolean)
    const body = {
      name: form.name.trim(),
      kind: form.kind,
      description: form.description.trim() || null,
      values,
      metadata,
      allowed_agents,
      expires_at_ms: form.expires_at
        ? new Date(form.expires_at + 'T23:59:59').getTime()
        : null,
    }
    formBusy = true
    try {
      const url = mode === 'create'
        ? '/api/v1/vault'
        : `/api/v1/vault/${encodeURIComponent(editingName)}`
      const method = mode === 'create' ? 'POST' : 'PUT'
      const r = await api.authedFetch(url, { method, body: JSON.stringify(body) })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      mode = 'idle'
      editingName = null
      form = emptyForm('exchange')
      showSecrets = false
      await refresh()
    } catch (err) {
      formMsg = { tone: 'err', text: err.message || 'Save failed' }
    } finally {
      formBusy = false
    }
  }

  async function deleteRow(name) {
    busyName[name] = true
    try {
      const r = await api.authedFetch(`/api/v1/vault/${encodeURIComponent(name)}`, { method: 'DELETE' })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      await refresh()
    } catch (_) {
    } finally {
      delete busyName[name]
      busyName = { ...busyName }
      confirmDeleteName = null
    }
  }

  function cancelForm() {
    mode = 'idle'
    editingName = null
    showSecrets = false
    formMsg = null
  }

  function formatDate(ms) {
    if (!ms) return '—'
    return new Date(ms).toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit',
    })
  }

  // Wave C6 — tone the expiry chip based on time-to-expiry.
  // Returns 'ok' (>30d), 'warn' (7-30d), 'bad' (<7d), 'expired' (<0).
  function expiryTone(ms) {
    if (!ms) return null
    const dt = ms - Date.now()
    if (dt < 0) return 'expired'
    const days = dt / (1000 * 60 * 60 * 24)
    if (days < 7) return 'bad'
    if (days < 30) return 'warn'
    return 'ok'
  }

  function fmtExpiryRelative(ms) {
    if (!ms) return ''
    const dt = ms - Date.now()
    const days = Math.round(dt / (1000 * 60 * 60 * 24))
    if (days < 0) return `expired ${-days}d ago`
    if (days === 0) return 'expires today'
    if (days === 1) return 'expires in 1d'
    return `expires in ${days}d`
  }

  const currentSpec = $derived(kindSpec(form.kind))

  const grouped = $derived.by(() => {
    const by = new Map()
    for (const r of rows) {
      const k = r.kind || 'generic'
      if (!by.has(k)) by.set(k, [])
      by.get(k).push(r)
    }
    return Array.from(by.entries())
      .map(([kind, items]) => ({ kind, spec: kindSpec(kind), items }))
      .sort((a, b) => (a.spec.label || a.kind).localeCompare(b.spec.label || b.kind))
  })
</script>

<div class="page scroll">
  <div class="container">
    <header class="page-header">
      <div>
        <h1>Vault</h1>
        <p class="page-sub">
          Encrypted store for every secret the controller holds —
          exchange API keys, Telegram tokens, Sentry DSNs, webhooks, RPC keys,
          anything else. Values are encrypted at rest (AES-256-GCM) under the
          controller's master key and never returned from the dashboard after save.
        </p>
      </div>
      {#if mode === 'idle'}
        <button type="button" class="btn primary" onclick={startPickKind}>
          <Icon name="check" size={12} />
          <span>Add entry</span>
        </button>
      {/if}
    </header>

    {#if loadError}
      <div class="inline-msg err">
        <Icon name="alert" size={12} />
        <span>{loadError}</span>
      </div>
    {/if}

    {#if mode === 'pick-kind'}
      <Card title="Pick a kind" subtitle="vault stores more than just exchange keys — any service credential lives here" span={1}>
        {#snippet children()}
          <div class="kind-gallery">
            {#each KINDS as k (k.value)}
              <button type="button" class="kind-card" onclick={() => pickKindAndProceed(k.value)}>
                <div class="kind-name">{k.label}</div>
                <div class="kind-hint">{k.hint}</div>
                <div class="kind-fields">
                  {#each k.values as v (v.key)}
                    <span class="kind-chip">{v.label}</span>
                  {/each}
                  {#if k.value === 'exchange'}
                    <span class="kind-chip push">pushed to agents</span>
                  {:else}
                    <span class="kind-chip local">server-local</span>
                  {/if}
                </div>
              </button>
            {/each}
          </div>
          <div class="actions">
            <button type="button" class="btn ghost" onclick={cancelForm}>Cancel</button>
          </div>
        {/snippet}
      </Card>
    {/if}

    {#if mode === 'create' || mode === 'rotate'}
      <Card
        title={mode === 'create' ? `New ${currentSpec.label.toLowerCase()} entry` : `Rotate · ${editingName}`}
        subtitle={mode === 'create' ? currentSpec.hint : 'enter a new value · name is fixed'}
        span={1}
      >
        {#snippet children()}
          <form class="form" onsubmit={submit}>
            <div class="grid-2">
              <div class="field">
                <label for="v-name">Name</label>
                {#if mode === 'rotate'}
                  <div class="readonly">{form.name}</div>
                {:else}
                  <input id="v-name" type="text" bind:value={form.name} disabled={formBusy} placeholder="e.g. binance_spot_main" />
                {/if}
              </div>
              <div class="field">
                <label for="v-kind">Kind</label>
                <select id="v-kind" bind:value={form.kind} onchange={onKindChange} disabled={formBusy || mode === 'rotate'}>
                  {#each KINDS as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
                </select>
                <div class="hint">{currentSpec.hint}</div>
              </div>
            </div>

            <div class="field">
              <label for="v-description">Description <span class="opt">optional</span></label>
              <input id="v-description" type="text" bind:value={form.description} disabled={formBusy} placeholder="human-readable context" />
            </div>

            <!-- Secret values — kind-specific. -->
            <div class="section-head">
              Values
              <span class="hint">secret · encrypted before save</span>
              <label class="toggle">
                <input type="checkbox" bind:checked={showSecrets} />
                <span>Show values</span>
              </label>
            </div>
            <div class="grid-2">
              {#each currentSpec.values as v (v.key)}
                <div class="field">
                  <label for="v-val-{v.key}">{v.label}</label>
                  <input
                    id="v-val-{v.key}"
                    type={showSecrets ? 'text' : 'password'}
                    autocomplete="off"
                    spellcheck="false"
                    bind:value={form.values[v.key]}
                    disabled={formBusy}
                    placeholder={mode === 'rotate' ? 'enter new value' : 'paste secret'}
                  />
                </div>
              {/each}
            </div>

            <!-- Non-secret metadata — kind-specific. -->
            {#if currentSpec.metadata.length > 0}
              <div class="section-head">Metadata <span class="hint">labels — stored in plaintext</span></div>
              <div class="grid-2">
                {#each currentSpec.metadata as m (m.key)}
                  <div class="field">
                    <label for="v-meta-{m.key}">
                      {m.label}
                      {#if !m.required}<span class="opt">optional</span>{/if}
                    </label>
                    {#if m.enum}
                      <select id="v-meta-{m.key}" bind:value={form.metadata[m.key]} disabled={formBusy}>
                        {#each m.enum as opt (opt.value)}<option value={opt.value}>{opt.label}</option>{/each}
                      </select>
                    {:else}
                      <input id="v-meta-{m.key}" type="text" bind:value={form.metadata[m.key]} disabled={formBusy} placeholder={m.placeholder || ''} />
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}

            {#if currentSpec.showAllowedAgents}
              <div class="field">
                <label for="v-allowed">
                  Allowed agents <span class="opt">comma-separated — empty means every Accepted agent receives this</span>
                </label>
                <input id="v-allowed" type="text" bind:value={form.allowed_agents} disabled={formBusy} placeholder="eu-01, eu-02" />
              </div>
            {/if}

            <div class="field">
              <label for="v-expires">
                Expires on <span class="opt">optional — UI flags rows &lt; 7 days to operator</span>
              </label>
              <input id="v-expires" type="date" bind:value={form.expires_at} disabled={formBusy} />
            </div>

            {#if formMsg}
              <div class="inline-msg {formMsg.tone === 'ok' ? 'ok' : 'err'}">
                <Icon name={formMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
                <span>{formMsg.text}</span>
              </div>
            {/if}
            <div class="actions">
              <button type="button" class="btn ghost" onclick={cancelForm} disabled={formBusy}>Cancel</button>
              <button type="submit" class="btn primary" disabled={formBusy}>
                {#if formBusy}<span class="spinner"></span>{/if}
                <span>{formBusy ? 'Saving…' : (mode === 'rotate' ? 'Save rotation' : 'Save entry')}</span>
              </button>
            </div>
          </form>
        {/snippet}
      </Card>
    {/if}

    <Card
      title="Stored entries"
      subtitle={loading ? 'loading…' : `${rows.length} entry${rows.length === 1 ? '' : 'ies'}`}
      span={1}
    >
      {#snippet children()}
        {#if loading}
          <div class="muted">Loading…</div>
        {:else if rows.length === 0}
          <div class="empty">
            <div class="empty-icon"><Icon name="shield" size={22} /></div>
            <div class="empty-title">Vault is empty</div>
            <div class="empty-sub">
              Add an entry above. Values live in
              <code>{'MM_VAULT'}</code> (default: <code>./vault.json</code>) encrypted under
              the controller's master key.
            </div>
          </div>
        {:else}
          <div class="groups">
            {#each grouped as g (g.kind)}
              <div class="group">
                <div class="group-head">
                  <span class="group-label">{g.spec.label || g.kind}</span>
                  <span class="group-count">{g.items.length}</span>
                </div>
                <div class="rows">
                  {#each g.items as r (r.name)}
                    <div class="row">
                      <div class="row-main">
                        <div class="row-name mono">{r.name}</div>
                        {#if r.description}<div class="row-desc">{r.description}</div>{/if}
                        {#if r.metadata && Object.keys(r.metadata).length > 0}
                          <div class="row-meta-chips">
                            {#each Object.entries(r.metadata) as [k, v] (k)}
                              <span class="chip">{k}={v}</span>
                            {/each}
                          </div>
                        {/if}
                        {#if r.allowed_agents && r.allowed_agents.length > 0}
                          <div class="row-meta-chips">
                            <span class="chip-k">agents:</span>
                            {#each r.allowed_agents as a (a)}<span class="chip">{a}</span>{/each}
                          </div>
                        {/if}
                        <div class="row-dates">
                          created {formatDate(r.created_at_ms)}
                          {#if r.rotated_at_ms}
                            · rotated {formatDate(r.rotated_at_ms)}
                          {:else if r.updated_at_ms !== r.created_at_ms}
                            · edited {formatDate(r.updated_at_ms)}
                          {/if}
                          {#if r.expires_at_ms}
                            {@const tone = expiryTone(r.expires_at_ms)}
                            · <span class="expiry-chip expiry-{tone}" title={`expires ${formatDate(r.expires_at_ms)}`}>
                              {fmtExpiryRelative(r.expires_at_ms)}
                            </span>
                          {/if}
                        </div>
                      </div>
                      <div class="row-actions">
                        {#if confirmDeleteName === r.name}
                          <span class="confirm-text">Delete <code>{r.name}</code>?</span>
                          <button type="button" class="btn danger small" disabled={busyName[r.name]} onclick={() => deleteRow(r.name)}>
                            {busyName[r.name] ? 'Deleting…' : 'Yes, delete'}
                          </button>
                          <button type="button" class="btn ghost small" onclick={() => (confirmDeleteName = null)}>Cancel</button>
                        {:else}
                          <button type="button" class="btn ghost small" onclick={() => startRotate(r)}>
                            <Icon name="refresh" size={12} />
                            <span>Rotate</span>
                          </button>
                          <button type="button" class="btn ghost small" onclick={() => (confirmDeleteName = r.name)}>
                            <Icon name="close" size={12} />
                            <span>Delete</span>
                          </button>
                        {/if}
                      </div>
                    </div>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        {/if}
      {/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .container { max-width: 880px; margin: 0 auto; display: flex; flex-direction: column; gap: var(--s-4); }
  .page-header { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--s-4); margin-bottom: var(--s-2); }
  .page-header h1 { margin: 0 0 6px; font-size: var(--fs-xl); font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .page-sub { margin: 0; color: var(--fg-muted); font-size: var(--fs-sm); line-height: 1.5; max-width: 620px; }
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); }
  code {
    font-family: var(--font-mono); font-size: 11px;
    background: var(--bg-chip); color: var(--fg-primary);
    padding: 1px 6px; border-radius: 3px;
  }

  .kind-gallery {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: var(--s-3);
    margin-bottom: var(--s-3);
  }
  .kind-card {
    text-align: left;
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    display: flex; flex-direction: column; gap: 6px;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
    font-family: var(--font-sans);
    color: inherit;
  }
  .kind-card:hover {
    border-color: var(--accent);
    background: rgba(0, 209, 178, 0.05);
  }
  .kind-name {
    font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary);
  }
  .kind-hint {
    font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.4;
  }
  .kind-fields { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 2px; }
  .kind-chip {
    font-family: var(--font-mono); font-size: 10px;
    padding: 1px 6px;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }
  .kind-chip.push { color: var(--accent); background: rgba(0, 209, 178, 0.08); }
  .kind-chip.local { color: var(--fg-muted); }

  .form { display: flex; flex-direction: column; gap: var(--s-3); }
  .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-3); }
  @media (max-width: 560px) { .grid-2 { grid-template-columns: 1fr; } }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field label { font-size: 11px; color: var(--fg-muted); letter-spacing: 0.02em; }
  .field .opt { color: var(--fg-faint); margin-left: 4px; font-weight: 400; }
  .field input, .field select, .readonly {
    padding: 9px 12px;
    background: rgba(10, 14, 20, 0.5);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    outline: none;
  }
  .field input:focus, .field select:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-ring); }
  .readonly { color: var(--fg-muted); user-select: all; }
  .field .hint { font-size: 11px; color: var(--fg-faint); }

  .section-head {
    display: flex; align-items: center; gap: var(--s-2);
    font-size: 11px; font-weight: 600;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    margin-top: var(--s-2);
  }
  .section-head .hint { font-weight: 400; font-size: 10px; color: var(--fg-faint); text-transform: none; letter-spacing: normal; }
  .section-head .toggle {
    margin-left: auto;
    display: inline-flex; align-items: center; gap: 4px;
    font-size: 11px; color: var(--fg-muted);
    cursor: pointer; user-select: none;
    text-transform: none; letter-spacing: normal; font-weight: 400;
  }
  .section-head .toggle input { margin: 0; cursor: pointer; }

  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; }

  .btn {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 9px 16px;
    border: 1px solid;
    border-radius: var(--r-md);
    font-size: var(--fs-sm);
    font-weight: 600;
    background: transparent;
    cursor: pointer;
    font-family: var(--font-sans);
  }
  .btn.small { padding: 5px 10px; font-size: 11px; }
  .btn.primary { background: var(--accent); color: #001510; border-color: var(--accent); }
  .btn.primary:hover:not(:disabled) { filter: brightness(1.1); }
  .btn.ghost { color: var(--fg-secondary); border-color: var(--border-default); }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-primary); }
  .btn.danger { color: var(--danger); border-color: var(--danger); }
  .btn.danger:hover:not(:disabled) { background: rgba(239,68,68,0.1); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(255,255,255,0.2);
    border-top-color: currentColor;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .inline-msg {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .inline-msg.ok  { color: var(--accent); background: rgba(0, 209, 178, 0.10); border: 1px solid rgba(0, 209, 178, 0.25); }
  .inline-msg.err { color: var(--danger); background: rgba(239, 68, 68, 0.08); border: 1px solid rgba(239, 68, 68, 0.25); }

  .groups { display: flex; flex-direction: column; gap: var(--s-4); }
  .group { display: flex; flex-direction: column; gap: var(--s-2); }
  .group-head { display: flex; align-items: baseline; gap: var(--s-2); padding: 0 var(--s-2); }
  .group-label { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; font-weight: 600; }
  .group-count { font-size: 10px; color: var(--fg-faint); padding: 1px 6px; background: var(--bg-chip); border-radius: 10px; }

  .rows { display: flex; flex-direction: column; gap: 6px; }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    padding: 10px 12px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    gap: var(--s-3);
  }
  .row-main { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .row-name { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .row-desc { font-size: var(--fs-xs); color: var(--fg-secondary); }
  .row-meta-chips { display: flex; flex-wrap: wrap; gap: 4px; align-items: center; }
  .row-meta-chips .chip {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }
  .row-meta-chips .chip-k {
    font-size: 10px;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .row-dates { font-size: 10px; color: var(--fg-muted); font-family: var(--font-mono); margin-top: 2px; }
  .expiry-chip {
    display: inline-block; padding: 1px 6px;
    border-radius: var(--r-sm); font-family: var(--font-mono);
  }
  .expiry-ok      { background: color-mix(in srgb, var(--ok) 12%, transparent); color: var(--ok); }
  .expiry-warn    { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .expiry-bad     { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .expiry-expired { background: color-mix(in srgb, var(--danger) 30%, transparent); color: var(--danger); font-weight: 600; }
  .row-actions { display: flex; gap: 6px; align-items: center; flex-shrink: 0; }
  .confirm-text { font-size: var(--fs-xs); color: var(--fg-muted); margin-right: 6px; }

  .empty { display: flex; flex-direction: column; align-items: center; gap: var(--s-2); padding: var(--s-6) var(--s-4); text-align: center; }
  .empty-icon { width: 44px; height: 44px; display: flex; align-items: center; justify-content: center; border-radius: 50%; background: var(--bg-chip); color: var(--fg-muted); }
  .empty-title { color: var(--fg-primary); font-weight: 500; }
  .empty-sub { color: var(--fg-muted); font-size: var(--fs-xs); max-width: 480px; line-height: 1.5; }
</style>
