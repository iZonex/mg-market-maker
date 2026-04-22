<script>
  /*
   * R10.1 — client onboarding UX.
   *
   * Before this panel, creating a client was curl-only. The
   * ClientCircuitPanel empty-state even read "add clients via
   * POST /api/admin/clients" — a pointer to a missing UI.
   *
   * This panel lists existing clients (GET) and runs a create
   * form (POST) with:
   *   - jurisdiction dropdown (drives Epic 40.10 US/perp gate;
   *     403 → user-readable message)
   *   - 409 duplicate id → user-readable message
   *   - symbols CSV + webhook URLs one-per-line
   *
   * Registration does NOT spawn engines (see admin_clients.rs
   * module doc) — surface that restart caveat in the success
   * banner so operators know.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let clients = $state([])
  let listError = $state('')

  let id = $state('')
  let name = $state('')
  let symbolsCsv = $state('')
  let webhookLines = $state('')
  let jurisdiction = $state('global')

  let busy = $state(false)
  let formError = $state('')
  let success = $state('')

  async function refresh() {
    try {
      clients = await api.getJson('/api/admin/clients')
      listError = ''
    } catch (e) {
      listError = String(e)
    }
  }

  function parseList(raw, sep) {
    return raw
      .split(sep)
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
  }

  async function createClient() {
    formError = ''
    success = ''
    if (!id.trim()) {
      formError = 'client id required'
      return
    }
    const symbols = parseList(symbolsCsv, ',')
    if (symbols.length === 0) {
      formError = 'at least one symbol required'
      return
    }
    const webhook_urls = parseList(webhookLines, '\n')

    busy = true
    try {
      const body = {
        id: id.trim(),
        name: name.trim() || id.trim(),
        symbols,
        webhook_urls,
        jurisdiction,
      }
      // Raw fetch so we can read the status + body and surface
      // 403/409 with something operator-readable instead of the
      // generic `api.postJson` throw.
      const r = await api.authedFetch('/api/admin/clients', {
        method: 'POST',
        body: JSON.stringify(body),
      })
      if (r.status === 403) {
        const j = await r.json().catch(() => ({}))
        formError = `Jurisdiction blocked: ${j.message || 'US clients cannot register on a perp engine. Use a spot engine or change jurisdiction.'}`
        return
      }
      if (r.status === 409) {
        formError = `Client id "${body.id}" already exists — choose a different id.`
        return
      }
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        formError = `POST failed: ${r.status} ${text}`
        return
      }
      success = `Client "${body.id}" registered with ${symbols.length} symbol(s). Engines for new symbols require a server restart to spawn.`
      id = ''
      name = ''
      symbolsCsv = ''
      webhookLines = ''
      jurisdiction = 'global'
      await refresh()
    } catch (e) {
      formError = String(e)
    } finally {
      busy = false
    }
  }

  $effect(() => {
    refresh()
  })
</script>

<div class="panel">
  <div class="panel-head">
    <span class="name">Register client</span>
    <span class="stats">{clients.length} registered</span>
  </div>
  <div class="panel-body">
    <div class="row wrap">
      <label class="field">
        <span class="k">id *</span>
        <input type="text" placeholder="acme" bind:value={id} disabled={busy} />
      </label>
      <label class="field">
        <span class="k">name</span>
        <input type="text" placeholder="Acme Corp" bind:value={name} disabled={busy} />
      </label>
      <label class="field">
        <span class="k">jurisdiction</span>
        <select bind:value={jurisdiction} disabled={busy}>
          <option value="global">global</option>
          <option value="US">US (blocks perp)</option>
          <option value="EU">EU</option>
          <option value="UK">UK</option>
          <option value="JP">JP</option>
          <option value="SG">SG</option>
        </select>
      </label>
    </div>
    <div class="row">
      <label class="field wide">
        <span class="k">symbols (csv) *</span>
        <input type="text" placeholder="BTCUSDT,ETHUSDT,SOLUSDT" bind:value={symbolsCsv} disabled={busy} />
      </label>
    </div>
    <div class="row">
      <label class="field wide">
        <span class="k">webhook urls (one per line)</span>
        <textarea rows="2" placeholder="https://example.com/webhook" bind:value={webhookLines} disabled={busy}></textarea>
      </label>
    </div>
    <div class="row">
      <button type="button" class="btn" onclick={createClient} disabled={busy}>
        {busy ? 'Registering…' : 'Register client'}
      </button>
    </div>
    {#if formError}<div class="error">{formError}</div>{/if}
    {#if success}<div class="success">{success}</div>{/if}
  </div>

  <div class="panel-head sub">
    <span class="name">Existing</span>
    {#if listError}<span class="error small">{listError}</span>{/if}
  </div>
  <div class="panel-body">
    {#if clients.length === 0}
      <div class="muted small">No clients registered yet.</div>
    {:else}
      <table class="rules">
        <thead>
          <tr><th>ID</th><th>Symbols</th></tr>
        </thead>
        <tbody>
          {#each clients as c (c.id)}
            <tr>
              <td class="mono">{c.id}</td>
              <td class="mono">{c.symbols?.join(', ') || '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

<style>
  .panel {
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .panel-head {
    display: flex; justify-content: space-between; align-items: center;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-xs);
  }
  .panel-head.sub { border-top: 1px solid var(--border-subtle); }
  .name { font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .stats { color: var(--fg-muted); font-size: var(--fs-2xs); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .panel-body { padding: var(--s-3); display: flex; flex-direction: column; gap: var(--s-2); }
  .row { display: flex; gap: var(--s-2); align-items: flex-end; }
  .row.wrap { flex-wrap: wrap; }
  .field { display: flex; flex-direction: column; gap: 2px; min-width: 140px; }
  .field.wide { flex: 1 1 auto; }
  .field .k { font-size: var(--fs-2xs); text-transform: uppercase; color: var(--fg-muted); letter-spacing: var(--tracking-label); }
  input, select, textarea {
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    padding: 4px var(--s-2);
    font-size: var(--fs-xs);
    font-family: inherit;
  }
  textarea { resize: vertical; min-height: 40px; }
  .btn {
    height: 28px; padding: 0 var(--s-3);
    background: var(--accent-dim); color: var(--accent);
    border: 1px solid var(--accent); border-radius: var(--r-sm);
    cursor: pointer; font-size: var(--fs-xs);
  }
  .btn:hover:not(:disabled) { background: var(--accent); color: var(--bg-base); }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .error { color: var(--danger); font-size: var(--fs-xs); }
  .success { color: var(--pos, #10b981); font-size: var(--fs-xs); }
  .muted { color: var(--fg-muted); }
  .small { font-size: var(--fs-xs); }
  .rules { width: 100%; border-collapse: collapse; }
  .rules th, .rules td { padding: 4px var(--s-2); font-size: var(--fs-2xs); text-align: left; border-bottom: 1px solid var(--border-subtle); }
  .rules th { color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
