<script>
  /*
   * Wave E3 — client portal. Sole page visible to role=ClientReader.
   *
   * Everything is tenant-scoped on the backend via the
   * `/api/v1/client/self/*` endpoints: the auth middleware
   * rewrites `self` → token.client_id, so this page never needs
   * to know the caller's tenant. The server-side scope gate
   * (tenant_scope_middleware) prevents a ClientReader from ever
   * reaching operator surfaces.
   *
   * Cards (pnl, sla, fills, webhook deliveries) are inline;
   * the non-trivial self-service webhook surface lives in
   * components/portal/PortalWebhooksCard.svelte.
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'
  import PortalWebhooksCard from '../components/portal/PortalWebhooksCard.svelte'
  import { fmtDec, fmtTime, slaTone } from '../components/client/client-helpers.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 5000

  let pnl = $state(null)
  let sla = $state(null)
  let cert = $state(null)
  let fills = $state([])
  let webhooks = $state([])
  let webhookUrls = $state([])
  let error = $state(null)
  let lastFetch = $state(null)

  // Wave I1 — self-service webhook CRUD state.
  let webhookBusy = $state(false)
  let webhookError = $state('')
  let webhookTestReport = $state(null)

  async function refresh() {
    try {
      const [p, s, c, f, w, list] = await Promise.all([
        api.getJson('/api/v1/client/self/pnl').catch(() => null),
        api.getJson('/api/v1/client/self/sla').catch(() => null),
        api.getJson('/api/v1/client/self/sla/certificate').catch(() => null),
        api.getJson('/api/v1/client/self/fills?limit=50').catch(() => []),
        api.getJson('/api/v1/client/self/webhook-deliveries').catch(() => ({ deliveries: [] })),
        api.getJson('/api/v1/client/self/webhooks').catch(() => ({ urls: [] })),
      ])
      pnl = p; sla = s; cert = c
      fills = Array.isArray(f) ? f : []
      webhooks = w?.deliveries || []
      webhookUrls = list?.urls || []
      error = null
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  async function addWebhook(url) {
    webhookBusy = true
    webhookError = ''
    try {
      const resp = await api.postJson('/api/v1/client/self/webhooks', { url })
      webhookUrls = resp.urls || []
    } catch (e) {
      webhookError = e?.message || String(e)
    } finally {
      webhookBusy = false
    }
  }

  async function removeWebhook(url) {
    webhookBusy = true
    webhookError = ''
    try {
      const resp = await api.authedFetch('/api/v1/client/self/webhooks', {
        method: 'DELETE',
        body: JSON.stringify({ url }),
      })
      if (!resp.ok) throw new Error(await resp.text().catch(() => `${resp.status}`))
      const body = await resp.json()
      webhookUrls = body?.urls || []
    } catch (e) {
      webhookError = e?.message || String(e)
    } finally {
      webhookBusy = false
    }
  }

  async function testWebhooks() {
    webhookBusy = true
    webhookError = ''
    webhookTestReport = null
    try {
      webhookTestReport = await api.postJson('/api/v1/client/self/webhooks/test', {})
    } catch (e) {
      webhookError = e?.message || String(e)
    } finally {
      webhookBusy = false
    }
  }

  $effect(() => {
    refresh()
    const iv = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(iv)
  })

  async function downloadCertificate() {
    const resp = await api.authedFetch('/api/v1/client/self/sla/certificate')
    if (!resp.ok) { error = `certificate download: ${resp.status}`; return }
    const body = await resp.json()
    const blob = new Blob([JSON.stringify(body, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `sla-certificate-${new Date().toISOString().slice(0, 10)}.json`
    document.body.appendChild(a); a.click(); a.remove()
    URL.revokeObjectURL(url)
  }
</script>

<div class="page scroll">
  <header class="portal-head">
    <div>
      <span class="title">Client portal</span>
      <span class="sub">
        {#if pnl?.client_id}<code class="mono">{pnl.client_id}</code>{/if}
        {#if lastFetch}· updated {lastFetch.toLocaleTimeString()}{/if}
      </span>
    </div>
    {#if error}<span class="error">{error}</span>{/if}
  </header>

  <div class="grid">
    <Card title="PnL" subtitle="live attribution across your symbols" span={2}>
      {#snippet children()}
        {#if !pnl}
          <div class="muted">Loading PnL…</div>
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
              <span class="v mono">{fmtDec(pnl.total_volume, 2)}</span>
            </div>
            <div class="kv-cell">
              <span class="k">round trips</span>
              <span class="v mono">{pnl.total_fills ?? 0}</span>
            </div>
          </div>
          {#if pnl.symbols?.length > 0}
            <table class="compact">
              <thead><tr><th>symbol</th><th class="num">PnL</th><th class="num">volume</th><th class="num">fills</th></tr></thead>
              <tbody>
                {#each pnl.symbols as r (r.symbol)}
                  <tr>
                    <td class="mono">{r.symbol}</td>
                    <td class="num mono" class:pos={Number(r.pnl) > 0} class:neg={Number(r.pnl) < 0}>{fmtDec(r.pnl, 4)}</td>
                    <td class="num mono">{fmtDec(r.volume, 2)}</td>
                    <td class="num mono">{r.fills}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/if}
      {/snippet}
    </Card>

    <Card title="SLA" subtitle="presence · two-sided · spread compliance" span={1}>
      {#snippet children()}
        {#if !sla}
          <div class="muted">Loading SLA…</div>
        {:else}
          <div class="kv-row">
            <div class="kv-cell">
              <span class="k">presence</span>
              <span class="v mono tone-{slaTone(sla.avg_presence_pct)}">{fmtDec(sla.avg_presence_pct, 2)}%</span>
            </div>
            <div class="kv-cell">
              <span class="k">two-sided</span>
              <span class="v mono tone-{slaTone(sla.avg_two_sided_pct)}">{fmtDec(sla.avg_two_sided_pct, 2)}%</span>
            </div>
            <div class="kv-cell">
              <span class="k">status</span>
              <span class="v chip tone-{sla.is_compliant ? 'ok' : 'bad'}">
                {sla.is_compliant ? 'COMPLIANT' : 'BREACH'}
              </span>
            </div>
          </div>
          {#if cert?.signature}
            <Button variant="ghost" size="sm" onclick={downloadCertificate}>
              {#snippet children()}<Icon name="shield" size={12} />
              Download signed certificate{/snippet}
            </Button>
            <div class="cert-hint">
              HMAC-SHA256 signed · recompute with your shared secret to verify authenticity.
            </div>
          {/if}
        {/if}
      {/snippet}
    </Card>

    <Card title="Recent fills" subtitle={`last ${fills.length}`} span={2}>
      {#snippet children()}
        {#if fills.length === 0}
          <div class="muted">No fills yet — check back once the engine has traded on your symbols.</div>
        {:else}
          <table class="compact">
            <thead>
              <tr>
                <th>when</th>
                <th>symbol</th>
                <th>side</th>
                <th class="num">price</th>
                <th class="num">qty</th>
                <th>role</th>
              </tr>
            </thead>
            <tbody>
              {#each fills as f (f.timestamp + f.price + f.qty)}
                <tr>
                  <td class="mono">{fmtTime(f.timestamp)}</td>
                  <td class="mono">{f.symbol}</td>
                  <td class="mono" class:pos={f.side === 'Buy'} class:neg={f.side === 'Sell'}>{f.side}</td>
                  <td class="num mono">{fmtDec(f.price, 4)}</td>
                  <td class="num mono">{fmtDec(f.qty, 6)}</td>
                  <td>{f.is_maker ? 'maker' : 'taker'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>

    <PortalWebhooksCard
      urls={webhookUrls}
      busy={webhookBusy}
      error={webhookError}
      testReport={webhookTestReport}
      onAdd={addWebhook}
      onRemove={removeWebhook}
      onTest={testWebhooks}
    />

    <Card title="Webhook deliveries" subtitle={`last ${webhooks.length}`} span={1}>
      {#snippet children()}
        {#if webhooks.length === 0}
          <div class="muted">
            No deliveries logged. If you expected webhook calls and don't
            see them, check with your MM provider that the URL is correct.
          </div>
        {:else}
          <table class="compact">
            <thead>
              <tr><th>when</th><th>event</th><th class="num">status</th></tr>
            </thead>
            <tbody>
              {#each webhooks as d (d.timestamp + d.url)}
                <tr>
                  <td class="mono">{fmtTime(d.timestamp)}</td>
                  <td class="mono">{d.event_type}</td>
                  <td class="num mono">
                    {#if d.ok}
                      <span class="chip tone-ok">{d.http_status ?? 'ok'}</span>
                    {:else}
                      <span class="chip tone-bad" title={d.error || ''}>{d.http_status ?? 'err'}</span>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-4); }
  .scroll { overflow-y: auto; }
  .portal-head {
    display: flex; align-items: center; justify-content: space-between;
    margin-bottom: var(--s-3);
  }
  .title { font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); }
  .sub { font-size: var(--fs-xs); color: var(--fg-muted); margin-left: var(--s-3); }
  .error { color: var(--neg); font-size: var(--fs-sm); }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-3); }

  .kv-row {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: var(--s-2); margin-bottom: var(--s-2);
  }
  .kv-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2); background: var(--bg-raised); border-radius: var(--r-sm);
  }
  .k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .v.pos { color: var(--pos); }
  .v.neg { color: var(--neg); }

  .compact { width: 100%; border-collapse: collapse; }
  .compact th, .compact td {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .compact th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .num { text-align: right; }
  .cert-hint { font-size: 10px; color: var(--fg-muted); margin-top: 4px; }
</style>
