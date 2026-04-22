<script>
  /*
   * Wave B4 — per-client drilldown page.
   *
   * Joins data from three sources:
   *   - /api/admin/clients                  — registered client directory
   *   - /api/v1/client/{id}/pnl             — fleet-aggregated PnL
   *   - /api/v1/client/{id}/sla             — fleet-aggregated SLA
   *   - /api/v1/client/{id}/sla/certificate — signed SLA certificate
   *   - /api/v1/fleet + /api/v1/approvals   — which agents carry
   *                                           this tenant (profile.client_id)
   *
   * Layout:
   *   Left column  — list of registered clients (click to select).
   *   Right column — selected client's rollup cards (positions,
   *                  PnL attribution per symbol, SLA card, agents
   *                  carrying this tenant).
   */
  import Card from '../components/Card.svelte'
  import EmptyStateGuide from '../components/EmptyStateGuide.svelte'
  import ClientOnboardingPanel from '../components/ClientOnboardingPanel.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 5000

  let clients = $state([])
  let selected = $state(null)
  let pnl = $state(null)
  let sla = $state(null)
  let cert = $state(null)
  // Wave D3 — webhook test + delivery log state.
  let webhookDeliveries = $state([])
  let webhookTestBusy = $state(false)
  let webhookTestStatus = $state(null)
  // Wave E4 — invite URL state. Admin clicks "Generate invite"
  // → signed token comes back → we render a copy-to-clipboard
  // URL that the admin hands to the tenant.
  let invite = $state(null)
  let inviteBusy = $state(false)
  let fleet = $state([])
  let approvals = $state([])
  let error = $state(null)
  let loading = $state(true)

  async function refreshClients() {
    try {
      const [list, f, a] = await Promise.all([
        api.getJson('/api/admin/clients'),
        api.getJson('/api/v1/fleet'),
        api.getJson('/api/v1/approvals').catch(() => []),
      ])
      clients = Array.isArray(list) ? list : []
      fleet = Array.isArray(f) ? f : []
      approvals = Array.isArray(a) ? a : []
      if (!selected && clients.length > 0) {
        selected = clients[0].client_id || clients[0].id
      }
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  async function refreshSelected() {
    if (!selected) {
      pnl = null; sla = null; cert = null; webhookDeliveries = []
      return
    }
    try {
      const [p, s, c, wh] = await Promise.all([
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/pnl`).catch(() => null),
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/sla`).catch(() => null),
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/sla/certificate`).catch(() => null),
        api.getJson(`/api/admin/clients/${encodeURIComponent(selected)}/webhooks/deliveries`).catch(() => ({ deliveries: [] })),
      ])
      pnl = p; sla = s; cert = c
      webhookDeliveries = wh?.deliveries || []
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  async function generateInvite() {
    if (!selected || inviteBusy) return
    inviteBusy = true
    try {
      const r = await api.authedFetch(`/api/admin/clients/${encodeURIComponent(selected)}/invite`, {
        method: 'POST',
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json()
      invite = {
        url: `${window.location.origin}${body.invite_url}`,
        token: body.invite_token,
        expires_at: body.expires_at,
      }
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      inviteBusy = false
    }
  }

  async function copyInvite() {
    if (!invite) return
    await navigator.clipboard.writeText(invite.url).catch(() => {})
  }

  async function testWebhooks() {
    if (!selected || webhookTestBusy) return
    webhookTestBusy = true
    webhookTestStatus = { phase: 'pending', text: 'firing test payloads…' }
    try {
      const r = await api.authedFetch(`/api/admin/clients/${encodeURIComponent(selected)}/webhooks/test`, {
        method: 'POST',
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json()
      webhookTestStatus = {
        phase: body.attempted === 0 ? 'info' : body.succeeded === body.attempted ? 'ok' : 'warn',
        text: body.attempted === 0
          ? 'No webhook URLs configured for this client.'
          : `${body.succeeded}/${body.attempted} URLs acknowledged`,
      }
      await refreshSelected()
    } catch (e) {
      webhookTestStatus = { phase: 'err', text: e?.message || String(e) }
    } finally {
      webhookTestBusy = false
    }
  }

  $effect(() => {
    refreshClients()
    const iv = setInterval(refreshClients, REFRESH_MS)
    return () => clearInterval(iv)
  })

  $effect(() => {
    // Re-fetch selected whenever selection changes.
    selected
    refreshSelected()
    const iv = setInterval(refreshSelected, REFRESH_MS)
    return () => clearInterval(iv)
  })

  // Approvals join: find every accepted agent whose profile.client_id
  // matches the selected tenant. Fleet row lookup by agent_id gives
  // us deployment counts + live state.
  const tenantAgents = $derived.by(() => {
    if (!selected) return []
    const fpByClient = new Map()
    for (const a of approvals || []) {
      if (a.profile?.client_id === selected) {
        fpByClient.set(a.fingerprint, a)
      }
    }
    return (fleet || [])
      .filter(a => {
        // match by fingerprint OR by agent_id tag in profile.labels
        if (fpByClient.has(a.pubkey_fingerprint)) return true
        return false
      })
      .map(a => ({
        ...a,
        approval: fpByClient.get(a.pubkey_fingerprint) || null,
      }))
  })

  function fmtDec(v, digits = 2) {
    if (v === null || v === undefined || v === '') return '—'
    const n = Number(v)
    if (!Number.isFinite(n)) return String(v)
    return n.toLocaleString(undefined, { maximumFractionDigits: digits })
  }

  function slaTone(pct) {
    const n = Number(pct || 0)
    if (n >= 99) return 'ok'
    if (n >= 95) return 'warn'
    return 'bad'
  }
  const SLA_LEGEND = '≥99% compliant · 95–99% warning · <95% breach'
</script>

<div class="page scroll">
  {#if !loading && clients.length === 0}
    <EmptyStateGuide
      title="No clients registered yet"
      message="Tenants are created once per business relationship. Each client gets their own PnL bucket, SLA tracking, webhook delivery log, and portal login."
      steps={[
        {
          title: 'Register the client',
          description: 'Use the "Register client" card below. Enter client_id, name, symbols they trade, jurisdiction, and webhook URLs.',
        },
        {
          title: 'Tag credentials with client_id',
          description: 'Vault credentials have an optional client_id — only agents tagged for this client can receive them. Cross-tenant leak is gated at deploy time.',
          action: { label: 'Open Vault', route: 'vault' },
        },
        {
          title: 'Generate a portal invite',
          description: 'Select the client on this page and click "Generate invite URL". The single-use 24h link lets them sign up for a ClientReader account and see their own data only.',
        },
      ]}
      {onNavigate}
    />
    <div class="grid onboard-only">
      <Card title="Register client" subtitle="jurisdiction-gated onboarding" span={3}>
        {#snippet children()}<ClientOnboardingPanel {auth} />{/snippet}
      </Card>
    </div>
  {:else}
  <div class="grid">
    <Card title="Clients" subtitle="registered tenants · click to drill down" span={1}>
      {#snippet children()}
        {#if error}
          <div class="error">{error}</div>
        {:else if loading}
          <div class="muted">Loading…</div>
        {:else if clients.length === 0}
          <div class="empty">
            No clients registered.
          </div>
        {:else}
          <div class="client-list">
            {#each clients as c (c.client_id || c.id)}
              {@const id = c.client_id || c.id}
              <button
                type="button"
                class="client-row"
                class:selected={selected === id}
                onclick={() => (selected = id)}
              >
                <span class="c-name">{c.name || id}</span>
                <span class="c-id mono">{id}</span>
                {#if c.jurisdiction}<span class="c-tag">{c.jurisdiction}</span>{/if}
              </button>
            {/each}
          </div>
        {/if}
      {/snippet}
    </Card>

    {#if selected}
      <Card title="PnL attribution" subtitle={`tenant ${selected}`} span={1}>
        {#snippet children()}
          {#if !pnl}
            <div class="muted">No data yet — deploy a strategy on an agent tagged with this client_id.</div>
          {:else}
            <div class="kv-row">
              <div class="kv-cell">
                <span class="k">total PnL</span>
                <span class="v mono" class:pos={Number(pnl.total_pnl) > 0} class:neg={Number(pnl.total_pnl) < 0}>
                  {fmtDec(pnl.total_pnl, 4)}
                </span>
              </div>
              <div class="kv-cell">
                <span class="k">volume</span>
                <span class="v mono">{fmtDec(pnl.total_volume)}</span>
              </div>
              <div class="kv-cell">
                <span class="k">round trips</span>
                <span class="v mono">{pnl.total_fills ?? 0}</span>
              </div>
            </div>
            {#if pnl.symbols?.length > 0}
              <table class="sym-table">
                <thead>
                  <tr><th>symbol</th><th class="num">PnL</th><th class="num">volume</th><th class="num">fills</th></tr>
                </thead>
                <tbody>
                  {#each pnl.symbols as r (r.symbol)}
                    <tr>
                      <td class="mono">{r.symbol}</td>
                      <td class="num mono" class:pos={Number(r.pnl) > 0} class:neg={Number(r.pnl) < 0}>
                        {fmtDec(r.pnl, 4)}
                      </td>
                      <td class="num mono">{fmtDec(r.volume)}</td>
                      <td class="num mono">{r.fills}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          {/if}
        {/snippet}
      </Card>

      <Card title="SLA" subtitle="fleet presence · two-sided · spread compliance" span={1}>
        {#snippet children()}
          {#if !sla}
            <div class="muted">No SLA data yet.</div>
          {:else}
            <div class="kv-row">
              <div class="kv-cell" title={SLA_LEGEND}>
                <span class="k">avg presence</span>
                <span class="v mono tone-{slaTone(sla.avg_presence_pct)}">
                  {fmtDec(sla.avg_presence_pct, 2)}%
                </span>
              </div>
              <div class="kv-cell" title={SLA_LEGEND}>
                <span class="k">avg two-sided</span>
                <span class="v mono tone-{slaTone(sla.avg_two_sided_pct)}">
                  {fmtDec(sla.avg_two_sided_pct, 2)}%
                </span>
              </div>
              <div class="kv-cell" title={SLA_LEGEND}>
                <span class="k">min presence</span>
                <span class="v mono tone-{slaTone(sla.min_presence_pct)}">
                  {fmtDec(sla.min_presence_pct, 2)}%
                </span>
              </div>
              <div class="kv-cell">
                <span class="k">compliant</span>
                <span class="v chip tone-{sla.is_compliant ? 'ok' : 'bad'}">
                  {sla.is_compliant ? 'YES' : 'NO'}
                </span>
              </div>
            </div>
            <div class="legend muted" title={SLA_LEGEND}>
              Compliance bands: ≥99% ok · 95–99% warn · &lt;95% breach
            </div>
            {#if sla.symbols?.length > 0}
              <table class="sym-table">
                <thead>
                  <tr>
                    <th>symbol</th>
                    <th class="num">presence</th>
                    <th class="num">two-sided</th>
                    <th class="num">spread cmp</th>
                    <th class="num">minutes</th>
                  </tr>
                </thead>
                <tbody>
                  {#each sla.symbols as r (r.symbol)}
                    <tr>
                      <td class="mono">{r.symbol}</td>
                      <td class="num mono tone-{slaTone(r.presence_pct)}">{fmtDec(r.presence_pct, 2)}%</td>
                      <td class="num mono tone-{slaTone(r.two_sided_pct)}">{fmtDec(r.two_sided_pct, 2)}%</td>
                      <td class="num mono tone-{slaTone(r.spread_compliance_pct)}">{fmtDec(r.spread_compliance_pct, 2)}%</td>
                      <td class="num mono">{r.minutes_with_data ?? 0}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
            {#if cert?.signature}
              <div class="cert-line">
                Signed certificate: <code class="mono sig">{cert.signature.slice(0, 24)}…</code>
                <span class="muted">· generated {cert.generated_at}</span>
              </div>
            {/if}
          {/if}
        {/snippet}
      </Card>

      <Card title="Portal invite" subtitle="generate a one-time signup URL for this client" span={3}>
        {#snippet children()}
          <div class="invite-row">
            <button
              type="button"
              class="btn ok small"
              disabled={inviteBusy}
              onclick={generateInvite}
            >
              {inviteBusy ? 'Generating…' : invite ? 'Regenerate invite URL' : 'Generate invite URL'}
            </button>
            {#if invite}
              <div class="invite-detail">
                <code class="invite-url mono">{invite.url}</code>
                <button type="button" class="btn ghost small" onclick={copyInvite}>Copy</button>
                <span class="invite-exp muted">expires {new Date(invite.expires_at).toLocaleString()}</span>
              </div>
            {/if}
          </div>
          <div class="muted small">
            Send this URL to the tenant. They pick a name + password, get a
            ClientReader account scoped to <code>{selected}</code>. Single-use
            inside a 24-hour window.
          </div>
        {/snippet}
      </Card>

      <Card title="Webhook deliveries" subtitle="last 50 · fires on SLA breaches, fills, kill events" span={3}>
        {#snippet children()}
          <div class="wh-actions">
            <button
              type="button"
              class="btn ok small"
              disabled={webhookTestBusy}
              onclick={testWebhooks}
            >
              {webhookTestBusy ? 'Testing…' : 'Send test payload'}
            </button>
            {#if webhookTestStatus}
              <span class="wh-status {webhookTestStatus.phase}">{webhookTestStatus.text}</span>
            {/if}
          </div>
          {#if webhookDeliveries.length === 0}
            <div class="empty">No deliveries logged yet. Fire a test, or wait for the next event.</div>
          {:else}
            <table class="sym-table">
              <thead>
                <tr>
                  <th>when</th>
                  <th>url</th>
                  <th>event</th>
                  <th class="num">status</th>
                  <th class="num">latency</th>
                </tr>
              </thead>
              <tbody>
                {#each webhookDeliveries as d (d.timestamp + d.url)}
                  <tr>
                    <td class="mono">{new Date(d.timestamp).toLocaleTimeString()}</td>
                    <td class="mono" style="max-width: 340px; overflow: hidden; text-overflow: ellipsis;" title={d.url}>{d.url}</td>
                    <td class="mono">{d.event_type}</td>
                    <td class="num mono">
                      {#if d.ok}
                        <span class="chip tone-ok">{d.http_status ?? 'ok'}</span>
                      {:else}
                        <span class="chip tone-bad" title={d.error || ''}>{d.http_status ?? 'err'}</span>
                      {/if}
                    </td>
                    <td class="num mono">{d.latency_ms ?? '—'}ms</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/snippet}
      </Card>

      <Card title="Register new client" subtitle="jurisdiction-gated onboarding" span={3}>
        {#snippet children()}<ClientOnboardingPanel {auth} />{/snippet}
      </Card>

      <Card title="Agents carrying this tenant" subtitle={`${tenantAgents.length} approved · matched by profile.client_id`} span={3}>
        {#snippet children()}
          {#if tenantAgents.length === 0}
            <div class="empty">
              No agent has <code>profile.client_id = "{selected}"</code> — set it in Fleet → Edit on the agent card.
            </div>
          {:else}
            <table class="agent-table">
              <thead>
                <tr>
                  <th>agent</th>
                  <th>region</th>
                  <th>environment</th>
                  <th class="num">deployments</th>
                  <th class="num">live orders</th>
                  <th>state</th>
                </tr>
              </thead>
              <tbody>
                {#each tenantAgents as a (a.agent_id)}
                  {@const live = (a.deployments || []).filter(d => d.running).length}
                  {@const orders = (a.deployments || []).reduce((n, d) => n + (d.live_orders || 0), 0)}
                  <tr>
                    <td class="mono">{a.agent_id}</td>
                    <td>{a.approval?.profile?.region || '—'}</td>
                    <td>{a.approval?.profile?.environment || '—'}</td>
                    <td class="num mono">{live}/{(a.deployments || []).length}</td>
                    <td class="num mono">{orders}</td>
                    <td>
                      <span class="chip tone-{a.approval_state === 'accepted' ? 'ok' : 'muted'}">
                        {a.approval_state || 'unknown'}
                      </span>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/snippet}
      </Card>
    {/if}
  </div>
  {/if}
</div>

<style>
  .page { padding: var(--s-4); }
  .scroll { overflow-y: auto; }
  .grid {
    display: grid;
    grid-template-columns: minmax(240px, 1fr) repeat(2, minmax(0, 1fr));
    gap: var(--s-3);
  }
  .grid.onboard-only { margin-top: var(--s-3); }
  .error { color: var(--neg); font-size: var(--fs-sm); }
  .muted { color: var(--fg-muted); font-size: var(--fs-xs); }
  .empty {
    padding: var(--s-3); color: var(--fg-muted);
    font-size: var(--fs-sm); text-align: center;
  }

  .client-list { display: flex; flex-direction: column; gap: 4px; }
  .client-row {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: inherit;
    cursor: pointer; text-align: left;
  }
  .client-row:hover { border-color: var(--accent); }
  .client-row.selected { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .c-name { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .c-id { font-size: 10px; color: var(--fg-muted); }
  .c-tag { font-size: 10px; color: var(--fg-muted); }

  .kv-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: var(--s-2);
    margin-bottom: var(--s-2);
  }
  .kv-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2); background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .v.pos { color: var(--pos); }
  .v.neg { color: var(--neg); }

  .sym-table, .agent-table { width: 100%; border-collapse: collapse; margin-top: var(--s-2); }
  .sym-table th, .sym-table td, .agent-table th, .agent-table td {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .sym-table th, .agent-table th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .num { text-align: right; }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .tone-ok   { color: var(--pos); }
  .tone-warn { color: var(--warn); }
  .tone-bad  { color: var(--neg); }

  .chip {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    border-radius: var(--r-sm); background: var(--bg-raised);
  }
  .chip.tone-ok    { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .chip.tone-bad   { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .chip.tone-muted { background: var(--bg-raised); color: var(--fg-muted); }

  .cert-line {
    margin-top: var(--s-2); font-size: var(--fs-xs);
    color: var(--fg-secondary);
  }
  .sig { color: var(--accent); }

  .wh-actions {
    display: flex; align-items: center; gap: var(--s-2);
    margin-bottom: var(--s-2);
  }
  .wh-status {
    font-size: 11px; font-family: var(--font-mono);
    padding: 2px 8px; border-radius: var(--r-sm);
  }
  .wh-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .wh-status.ok      { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .wh-status.warn    { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .wh-status.info    { background: var(--bg-raised); color: var(--fg-secondary); }
  .wh-status.err     { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .legend { font-size: 10px; margin-top: var(--s-2); }

  .invite-row { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; margin-bottom: var(--s-2); }
  .invite-detail { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .invite-url {
    padding: 2px 8px; font-size: 11px;
    background: var(--bg-chip); border-radius: var(--r-sm);
    max-width: 520px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .invite-exp { font-size: 10px; }
  .muted.small { font-size: 10px; color: var(--fg-muted); margin-top: 4px; }
</style>
