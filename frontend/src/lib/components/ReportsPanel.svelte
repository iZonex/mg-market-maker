<script>
  /*
   * UX-6 — compact reports hub for the compliance page.
   *
   * Surfaces the three report surfaces the backend already
   * exposes:
   *   /api/v1/report/daily        — JSON (in-page preview)
   *   /api/v1/report/daily/csv    — CSV download
   *   /api/v1/report/history      — list of archived daily reports
   *   /api/v1/report/history/{d}  — fetch one by date
   *
   * Downloads go via Bearer-token fetch (not plain <a>) because
   * every non-public endpoint requires auth. We stream the body
   * into a Blob, then trigger a synthetic anchor click so the
   * browser still uses its native save-as flow.
   */

  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = createApiClient(auth)

  let history = $state([])
  let error = $state('')
  let loading = $state(false)
  let busy = $state(false)

  // A1 — monthly-export period pickers. Default to "last full
  // calendar month" so the common use case (regulator hand-off
  // after month close) is one click.
  function lastMonthRange() {
    const now = new Date()
    const firstThis = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1))
    const lastPrev = new Date(firstThis.getTime() - 86400000)
    const firstPrev = new Date(Date.UTC(lastPrev.getUTCFullYear(), lastPrev.getUTCMonth(), 1))
    const iso = d => d.toISOString().slice(0, 10)
    return { from: iso(firstPrev), to: iso(lastPrev) }
  }
  const initial = lastMonthRange()
  let mFrom = $state(initial.from)
  let mTo = $state(initial.to)
  let mClientId = $state('')

  async function loadHistory() {
    loading = true
    error = ''
    try {
      // Backend returns a bare `Vec<String>` (client_api.rs:
      // get_report_history). The old `data.dates || data` fallback
      // only worked by JS truthy-coercion on arrays and would
      // silently break under any response-shape normaliser.
      const data = await api.getJson('/api/v1/report/history')
      history = Array.isArray(data) ? data.slice(0, 30) : []
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  async function download(path, suggestedName) {
    if (busy) return
    busy = true
    try {
      const resp = await api.authedFetch(path)
      if (!resp.ok) throw new Error(`${path} → ${resp.status}`)
      const blob = await resp.blob()
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = suggestedName
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      busy = false
    }
  }

  function today() {
    return new Date().toISOString().slice(0, 10)
  }

  function monthlyUrl(ext) {
    const p = new URLSearchParams({ from: mFrom, to: mTo })
    if (mClientId.trim()) p.set('client_id', mClientId.trim())
    return `/api/v1/report/monthly.${ext}?${p.toString()}`
  }
  function monthlyName(ext) {
    const cid = mClientId.trim() || 'all'
    return `monthly-${cid}-${mFrom}-to-${mTo}.${ext}`
  }

  $effect(() => { loadHistory() })
</script>

<div class="panel">
  <div class="top">
    <div class="header">
      <span class="label">Reports</span>
      <span class="hint">daily JSON / CSV, historical archive</span>
    </div>
    <button type="button" class="btn ghost" onclick={loadHistory} disabled={loading}>
      <Icon name="refresh" size={14} />
      <span>{loading ? 'Loading…' : 'Reload'}</span>
    </button>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <div class="section">
    <div class="section-title">Today</div>
    <div class="actions">
      <button type="button" class="btn" onclick={() => download('/api/v1/report/daily/csv', `daily-${today()}.csv`)} disabled={busy}>
        <Icon name="external" size={14} />
        <span>Download CSV</span>
      </button>
      <button type="button" class="btn" onclick={() => download('/api/v1/report/daily', `daily-${today()}.json`)} disabled={busy}>
        <Icon name="external" size={14} />
        <span>Download JSON</span>
      </button>
    </div>
  </div>

  <div class="section">
    <div class="section-title">Monthly MiCA export</div>
    <div class="form">
      <label>
        <span class="form-label">From</span>
        <input type="date" bind:value={mFrom} />
      </label>
      <label>
        <span class="form-label">To</span>
        <input type="date" bind:value={mTo} />
      </label>
      <label>
        <span class="form-label">Client ID <span class="muted">(optional)</span></span>
        <input type="text" bind:value={mClientId} placeholder="all" />
      </label>
    </div>
    <div class="actions">
      <button type="button" class="btn" onclick={() => download(monthlyUrl('csv'), monthlyName('csv'))} disabled={busy}>
        <Icon name="external" size={14} /> <span>CSV</span>
      </button>
      <button type="button" class="btn" onclick={() => download(monthlyUrl('xlsx'), monthlyName('xlsx'))} disabled={busy}>
        <Icon name="external" size={14} /> <span>XLSX</span>
      </button>
      <button type="button" class="btn" onclick={() => download(monthlyUrl('pdf'), monthlyName('pdf'))} disabled={busy}>
        <Icon name="external" size={14} /> <span>PDF</span>
      </button>
      <button type="button" class="btn ghost" onclick={() => download(monthlyUrl('json'), monthlyName('json'))} disabled={busy}>
        <span>JSON</span>
      </button>
      <button type="button" class="btn ghost" onclick={() => download(monthlyUrl('manifest'), monthlyName('manifest.json'))} disabled={busy}>
        <span>Manifest (HMAC)</span>
      </button>
    </div>
  </div>

  <div class="section">
    <div class="section-title">Compliance bundle</div>
    <div class="muted">
      ZIP with summary (JSON+CSV+XLSX+PDF), fills.jsonl, audit.jsonl, HMAC-signed manifest + README. Same period + client selector as above.
    </div>
    <div class="actions">
      <button type="button" class="btn" onclick={() => download(`/api/v1/export/bundle?${new URLSearchParams({ from: mFrom, to: mTo, ...(mClientId.trim() ? { client_id: mClientId.trim() } : {}) }).toString()}`, `bundle-${mClientId.trim() || 'all'}-${mFrom}-to-${mTo}.zip`)} disabled={busy}>
        <Icon name="external" size={14} /> <span>Download bundle</span>
      </button>
    </div>
  </div>

  <div class="section">
    <div class="section-title">History ({history.length})</div>
    {#if history.length === 0 && !loading}
      <div class="muted">No archived reports yet — the daily snapshot job writes one entry per UTC day.</div>
    {:else}
      <div class="dates">
        {#each history as d (d)}
          <button
            type="button"
            class="date-chip"
            title={`Download report for ${d}`}
            onclick={() => download(`/api/v1/report/history/${d}`, `daily-${d}.json`)}
            disabled={busy}
          >
            <Icon name="doc" size={12} />
            <span>{d}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: var(--s-4); }
  .top { display: flex; justify-content: space-between; align-items: center; gap: var(--s-3); }
  .header { display: flex; flex-direction: column; gap: 2px; }
  .label {
    font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .hint { font-size: var(--fs-xs); color: var(--fg-muted); }

  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title {
    font-size: var(--fs-xs); font-weight: 600; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .actions { display: flex; gap: var(--s-2); flex-wrap: wrap; }
  .btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-size: var(--fs-xs); cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .btn:hover:not(:disabled) { background: var(--bg-raised); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.ghost { background: transparent; }

  .form { display: flex; gap: var(--s-3); flex-wrap: wrap; }
  .form label { display: flex; flex-direction: column; gap: 2px; font-size: var(--fs-xs); color: var(--fg-secondary); }
  .form-label { font-size: var(--fs-xs); color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .form input {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: var(--fs-xs);
    min-width: 140px;
  }
  .form input:focus { outline: 2px solid var(--accent); outline-offset: 1px; }

  .dates { display: flex; flex-wrap: wrap; gap: var(--s-2); }
  .date-chip {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised); border: 1px solid var(--border-subtle);
    border-radius: var(--r-pill); color: var(--fg-secondary);
    font-family: var(--font-mono); font-size: var(--fs-xs); cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out), color var(--dur-fast) var(--ease-out);
  }
  .date-chip:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-primary); }
  .date-chip:disabled { opacity: 0.5; cursor: not-allowed; }

  .error {
    padding: var(--s-3); background: var(--danger-bg);
    border-radius: var(--r-md); color: var(--danger); font-size: var(--fs-sm);
  }
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); padding: var(--s-2) 0; }
</style>
