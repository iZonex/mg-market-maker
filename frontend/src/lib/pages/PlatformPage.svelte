<script>
  /*
   * Platform — controller-level runtime tunables.
   *
   * Schema-driven form: pulls `/api/v1/tunables/schema` on mount
   * and renders one input per field (bool toggle, number with
   * min/max, string, semver-optional). Values come from
   * `/api/v1/tunables`. PUT replaces the whole blob.
   *
   * These are controller-scoped knobs — lease policy, agent
   * version pinning, deploy dialog defaults, etc. Per-strategy
   * engine behaviour lives in the Deploy dialog's variables
   * map; per-agent rails live in each agent's settings.toml.
   * This page owns only the runtime controller-wide surface.
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  let schema = $state([])
  let values = $state({})
  let original = $state({})        // last known server state, for diff + dirty indicator
  let loading = $state(true)
  let loadError = $state(null)
  let busy = $state(false)
  let saveMsg = $state(null)

  async function load() {
    try {
      const [s, v] = await Promise.all([
        api.getJson('/api/v1/tunables/schema'),
        api.getJson('/api/v1/tunables'),
      ])
      schema = Array.isArray(s) ? s : []
      values = { ...v }
      original = { ...v }
      loadError = null
    } catch (e) {
      loadError = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  $effect(() => { load() })

  const dirty = $derived.by(() => {
    for (const k of Object.keys(values)) {
      if (values[k] !== original[k]) return true
    }
    return false
  })

  const grouped = $derived.by(() => {
    const by = new Map()
    for (const f of schema) {
      const k = f.category || 'General'
      if (!by.has(k)) by.set(k, [])
      by.get(k).push(f)
    }
    return Array.from(by.entries()).map(([category, fields]) => ({ category, fields }))
  })

  async function save() {
    saveMsg = null
    busy = true
    try {
      // Coerce number-typed fields (HTML inputs give strings)
      const body = { ...values }
      for (const f of schema) {
        if (f.kind === 'u32') body[f.key] = Number(body[f.key] ?? 0)
      }
      const r = await api.authedFetch('/api/v1/tunables', {
        method: 'PUT',
        body: JSON.stringify(body),
      })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      const updated = await r.json()
      values = { ...updated }
      original = { ...updated }
      saveMsg = { tone: 'ok', text: 'Tunables saved. Most values apply to new sessions / new deployments; a few require a server restart (see field help).' }
    } catch (err) {
      saveMsg = { tone: 'err', text: err.message || 'Save failed' }
    } finally {
      busy = false
    }
  }

  function discard() {
    values = { ...original }
    saveMsg = null
  }
</script>

<div class="page scroll">
  <div class="container">
    <header class="page-header">
      <div>
        <h1>Platform settings</h1>
        <p class="page-sub">
          Controller-level runtime tunables — lease policy, version pinning, deploy defaults.
          Edits persist to <code>{'MM_TUNABLES'}</code> (default: <code>./tunables.json</code>)
          and apply to new sessions / new deployments. Per-strategy engine behaviour lives in
          the Deploy dialog's variables; per-agent rails live in each agent's settings file.
        </p>
      </div>
    </header>

    {#if loadError}
      <div class="inline-msg err">
        <Icon name="alert" size={12} />
        <span>{loadError}</span>
      </div>
    {/if}

    {#if loading}
      <div class="muted">Loading tunables…</div>
    {:else}
      <form class="form" onsubmit={(e) => { e.preventDefault(); save() }}>
        {#each grouped as g (g.category)}
          <Card title={g.category} subtitle="" span={1}>
            {#snippet children()}
              <div class="fields">
                {#each g.fields as f (f.key)}
                  <div class="field-row">
                    <div class="field-meta">
                      <label for={`tn-${f.key}`} class="field-label">{f.label}</label>
                      <div class="field-key mono">{f.key}</div>
                      <div class="field-desc">{f.description}</div>
                    </div>
                    <div class="field-input">
                      {#if f.kind === 'bool'}
                        <label class="toggle">
                          <input
                            id={`tn-${f.key}`}
                            type="checkbox"
                            bind:checked={values[f.key]}
                            disabled={busy}
                          />
                          <span>{values[f.key] ? 'on' : 'off'}</span>
                        </label>
                      {:else if f.kind === 'u32'}
                        <input
                          id={`tn-${f.key}`}
                          type="number"
                          bind:value={values[f.key]}
                          min={f.min}
                          max={f.max}
                          disabled={busy}
                          class="num"
                        />
                        {#if f.min !== undefined || f.max !== undefined}
                          <span class="range-hint">
                            {f.min ?? '—'}…{f.max ?? '—'}
                          </span>
                        {/if}
                      {:else if f.kind === 'semver_opt'}
                        <input
                          id={`tn-${f.key}`}
                          type="text"
                          bind:value={values[f.key]}
                          disabled={busy}
                          placeholder="empty = no bound"
                          class="mono"
                        />
                        <span class="range-hint">semver · e.g. 0.4.0</span>
                      {:else}
                        <input
                          id={`tn-${f.key}`}
                          type="text"
                          bind:value={values[f.key]}
                          disabled={busy}
                          class="mono"
                        />
                      {/if}
                    </div>
                  </div>
                {/each}
              </div>
            {/snippet}
          </Card>
        {/each}

        <div class="sticky-foot">
          {#if saveMsg}
            <div class="inline-msg {saveMsg.tone === 'ok' ? 'ok' : 'err'}">
              <Icon name={saveMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
              <span>{saveMsg.text}</span>
            </div>
          {/if}
          <div class="actions">
            <button type="button" class="btn ghost" disabled={busy || !dirty} onclick={discard}>
              Discard
            </button>
            <button type="submit" class="btn primary" disabled={busy || !dirty}>
              {#if busy}<span class="spinner"></span>{/if}
              <span>{busy ? 'Saving…' : dirty ? 'Save changes' : 'Nothing to save'}</span>
            </button>
          </div>
        </div>
      </form>
    {/if}
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .container { max-width: 860px; margin: 0 auto; display: flex; flex-direction: column; gap: var(--s-4); }
  .page-header h1 { margin: 0 0 6px; font-size: var(--fs-xl); font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .page-sub { margin: 0; color: var(--fg-muted); font-size: var(--fs-sm); line-height: 1.5; max-width: 620px; }
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); }
  code { font-family: var(--font-mono); background: var(--bg-chip); padding: 1px 6px; border-radius: 3px; color: var(--fg-primary); font-size: 11px; }

  .form { display: flex; flex-direction: column; gap: var(--s-4); }
  .fields { display: flex; flex-direction: column; gap: var(--s-4); }
  .field-row {
    display: grid;
    grid-template-columns: 1fr 240px;
    gap: var(--s-4);
    align-items: flex-start;
    padding-bottom: var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
  }
  .field-row:last-child { padding-bottom: 0; border-bottom: 0; }
  @media (max-width: 640px) {
    .field-row { grid-template-columns: 1fr; }
  }
  .field-meta { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .field-label { font-size: var(--fs-sm); font-weight: 500; color: var(--fg-primary); }
  .field-key {
    font-family: var(--font-mono); font-size: 10px; color: var(--fg-faint);
    letter-spacing: 0.02em;
  }
  .field-desc { font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.5; }

  .field-input { display: flex; align-items: center; gap: var(--s-2); }
  .field-input input[type="number"], .field-input input[type="text"] {
    padding: 8px 12px;
    background: rgba(10, 14, 20, 0.5);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    outline: none;
    min-width: 140px;
  }
  .field-input input.num { max-width: 120px; }
  .field-input input:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-ring); }
  .range-hint { font-size: 10px; color: var(--fg-faint); font-family: var(--font-mono); }

  .toggle {
    display: inline-flex; align-items: center; gap: 6px;
    font-size: var(--fs-xs); color: var(--fg-secondary);
    cursor: pointer; user-select: none;
  }
  .toggle input { margin: 0; cursor: pointer; width: 18px; height: 18px; }

  .sticky-foot {
    position: sticky;
    bottom: 0;
    background: linear-gradient(to bottom, rgba(17,19,23,0), var(--bg-base) 40%);
    padding: var(--s-3) 0;
    display: flex; flex-direction: column; gap: var(--s-2);
  }
  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; }

  .btn {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 9px 18px;
    border: 1px solid;
    border-radius: var(--r-md);
    font-size: var(--fs-sm); font-weight: 600;
    background: transparent;
    cursor: pointer;
    font-family: var(--font-sans);
  }
  .btn.primary { background: var(--accent); color: #001510; border-color: var(--accent); }
  .btn.primary:hover:not(:disabled) { filter: brightness(1.1); }
  .btn.ghost { color: var(--fg-secondary); border-color: var(--border-default); }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-primary); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0,0,0,0.25);
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
</style>
