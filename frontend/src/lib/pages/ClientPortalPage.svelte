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
   * Shows:
   *   - PnL summary + per-symbol attribution
   *   - SLA status + signed certificate download
   *   - Recent fills
   *   - Webhook delivery log (did our webhook fire?)
   *
   * No kill switches, no strategy tools, no fleet view.
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'

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

  // Wave I1 — self-service webhook CRUD state. These live on
  // the portal because the tenant is the sole user; admins see
  // them through the admin clients panel.
  let newUrl = $state('')
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

  async function addWebhook() {
    const trimmed = newUrl.trim()
    if (!trimmed) { webhookError = 'URL required'; return }
    webhookBusy = true
    webhookError = ''
    try {
      const resp = await api.postJson('/api/v1/client/self/webhooks', { url: trimmed })
      webhookUrls = resp.urls || []
      newUrl = ''
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

  function fmtDec(v, digits = 4) {
    if (v === null || v === undefined || v === '') return '—'
    const n = Number(v)
    if (!Number.isFinite(n)) return String(v)
    return n.toLocaleString(undefined, { maximumFractionDigits: digits })
  }

  function fmtTime(t) {
    if (!t) return '—'
    try { return new Date(t).toLocaleTimeString() } catch { return '—' }
  }

  function slaTone(pct) {
    const n = Number(pct || 0)
    if (n >= 99) return 'ok'
    if (n >= 95) return 'warn'
    return 'bad'
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
                {fmtDec(pnl.total_pnl)}
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
                    <td class="num mono" class:pos={Number(r.pnl) > 0} class:neg={Number(r.pnl) < 0}>{fmtDec(r.pnl)}</td>
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
                  <td class="num mono">{fmtDec(f.price)}</td>
                  <td class="num mono">{fmtDec(f.qty, 6)}</td>
                  <td>{f.is_maker ? 'maker' : 'taker'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>

    <Card title="My webhooks" subtitle="self-service registration" span={2}>
      {#snippet children()}
        <div class="wh-form">
          <input
            type="text"
            class="wh-input"
            placeholder="https://your-service.example/webhook"
            bind:value={newUrl}
            disabled={webhookBusy}
            onkeydown={(e) => { if (e.key === 'Enter') addWebhook() }}
          />
          <Button variant="primary" size="sm" onclick={addWebhook} disabled={webhookBusy}>
          {#snippet children()}<Icon name="check" size={12} />
            <span>Add</span>{/snippet}
        </Button>
          <Button variant="ghost" size="sm" onclick={testWebhooks}
 disabled={webhookBusy || webhookUrls.length === 0}
 title={webhookUrls.length === 0 ? 'Add a URL first' : 'Fire a synthetic test event to every URL'}>
          {#snippet children()}<Icon name="shield" size={12} />
            <span>Test-fire</span>{/snippet}
        </Button>
        </div>
        {#if webhookError}
          <div class="wh-err">{webhookError}</div>
        {/if}
        {#if webhookUrls.length === 0}
          <div class="muted">
            No webhooks registered. Add a URL to receive fill, PnL,
            and SLA events. We POST JSON payloads with a short
            retry window; delivery history is below.
          </div>
        {:else}
          <ul class="wh-list">
            {#each webhookUrls as u (u)}
              <li class="wh-row">
                <code class="mono" title={u}>{u}</code>
                <Button variant="ghost" size="xs" onclick={() => removeWebhook(u)}
 disabled={webhookBusy}
 aria-label="Remove webhook">
          {#snippet children()}<Icon name="close" size={10} />
                  <span>Remove</span>{/snippet}
        </Button>
              </li>
            {/each}
          </ul>
        {/if}
        {#if webhookTestReport}
          <div class="wh-report">
            <span class="k">Test dispatch</span>
            <span class="v mono">{webhookTestReport.succeeded}/{webhookTestReport.attempted}</span>
            <div class="wh-results">
              {#each (webhookTestReport.results || []) as r (r.url + ':' + r.timestamp)}
                <div class="wh-result">
                  <code class="mono" title={r.url}>{r.url}</code>
                  {#if r.ok}
                    <span class="chip tone-ok">{r.http_status ?? 'ok'}</span>
                  {:else}
                    <span class="chip tone-bad" title={r.error || ''}>{r.http_status ?? 'err'}</span>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/if}
      {/snippet}
    </Card>

    <Card title="Webhook deliveries" subtitle={`last ${webhooks.length}`} span={1}>
      {#snippet children()}
        {#if webhooks.length === 0}
          <div class="muted">
            No deliveries logged. If you expected webhook calls and don't see
            them, check with your MM provider that the URL is correct.
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
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); padding: var(--s-3); text-align: center; }

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
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .tone-ok { color: var(--pos); }
  .tone-warn { color: var(--warn); }
  .tone-bad { color: var(--neg); }

  .chip {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    border-radius: var(--r-sm); background: var(--bg-raised);
  }
  .chip.tone-ok { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .chip.tone-bad { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }

  .cert-hint { font-size: 10px; color: var(--fg-muted); margin-top: 4px; }

  .wh-preview {
    display: flex; align-items: flex-start; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 35%, transparent);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: 11px;
    line-height: 1.4;
    margin-bottom: var(--s-2);
  }
  .wh-form {
    display: flex; gap: var(--s-2); align-items: center;
    margin-bottom: var(--s-2);
  }
  .wh-input {
    flex: 1;
    padding: 6px 10px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    min-width: 0;
  }
  .wh-input:focus { outline: none; border-color: var(--accent); }
  .wh-list {
    list-style: none; margin: 0; padding: 0;
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .wh-row {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s-2);
    padding: 4px 8px;
    background: var(--bg-raised); border-radius: var(--r-sm);
  }
  .wh-row code {
    flex: 1; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; font-size: var(--fs-xs);
    color: var(--fg-primary);
  }
  .wh-err {
    padding: 4px 8px; border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--neg) 15%, transparent);
    color: var(--neg); font-size: var(--fs-xs);
    margin-bottom: var(--s-2);
  }
  .wh-report {
    margin-top: var(--s-2);
    padding: var(--s-2);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .wh-results { display: flex; flex-direction: column; gap: 4px; }
  .wh-result {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s-2); font-size: var(--fs-xs);
  }
  .wh-result code { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
