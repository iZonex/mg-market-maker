<script>
  /*
   * Epic H — deploy history footer. Collapsible. Shows every
   * recorded deploy with `{ name, hash, operator, deployed_at,
   * scope }`; clicking a row triggers the parent's `onReload`
   * handler so the operator can roll back or branch from an
   * earlier version.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth, onReload } = $props()
  const api = createApiClient(auth)

  let entries = $state([])
  let open = $state(false)
  let error = $state('')
  let listed = $state([])

  async function refresh() {
    try {
      entries = await api.getJson('/api/v1/strategy/deploys')
      listed = await api.getJson('/api/v1/strategy/graphs')
      error = ''
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => { if (open) refresh() })

  function fmtTs(t) {
    if (!t) return '—'
    const d = new Date(t)
    return d.toLocaleString()
  }
</script>

<div class="history">
  <button type="button" class="toggle" onclick={() => { open = !open; if (open) refresh() }}>
    <Icon name={open ? 'chevronDown' : 'chevronUp'} size={12} />
    <span>Deploy history</span>
    <span class="count">{entries.length}</span>
  </button>
  {#if open}
    <div class="panel">
      {#if error}
        <div class="error">{error}</div>
      {:else}
        <div class="section">
          <div class="section-title">Saved graphs</div>
          <div class="chips">
            {#each listed as name (name)}
              <button type="button" class="chip" onclick={() => onReload?.(name)}>{name}</button>
            {/each}
            {#if listed.length === 0}
              <span class="muted">none yet</span>
            {/if}
          </div>
        </div>
        <div class="section">
          <div class="section-title">Deploys</div>
          {#if entries.length === 0}
            <span class="muted">no deploys recorded</span>
          {:else}
            <table>
              <thead>
                <tr><th>When</th><th>Name</th><th>Hash</th><th>Operator</th><th>Scope</th></tr>
              </thead>
              <tbody>
                {#each entries.slice().reverse() as rec (rec.hash + rec.deployed_at)}
                  <tr>
                    <td class="num">{fmtTs(rec.deployed_at)}</td>
                    <td><code>{rec.name}</code></td>
                    <td class="num">{rec.hash.slice(0, 12)}…</td>
                    <td>{rec.operator}</td>
                    <td><code class="small">{rec.scope}</code></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .history { border-top: 1px solid var(--border-subtle); background: var(--bg-raised); }
  .toggle {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-4);
    width: 100%;
    background: transparent; border: none; cursor: pointer; color: var(--fg-primary);
    font-size: var(--fs-xs); text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .toggle:hover { background: var(--bg-chip); }
  .count {
    margin-left: auto;
    font-family: var(--font-mono); font-size: var(--fs-2xs); color: var(--fg-muted);
  }
  .panel { padding: var(--s-3) var(--s-4); display: flex; flex-direction: column; gap: var(--s-3); max-height: 280px; overflow-y: auto; }
  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title { font-size: var(--fs-2xs); color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .chips { display: flex; flex-wrap: wrap; gap: var(--s-2); }
  .chip {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-pill); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: var(--fs-2xs); cursor: pointer;
  }
  .chip:hover { border-color: var(--accent); color: var(--accent); }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: var(--s-2); font-size: var(--fs-xs); text-align: left; border-bottom: 1px solid var(--border-subtle); }
  th { color: var(--fg-muted); font-weight: 500; text-transform: uppercase; letter-spacing: var(--tracking-label); font-size: var(--fs-2xs); }
  .num, .small { font-family: var(--font-mono); font-size: var(--fs-2xs); }
  .muted { color: var(--fg-muted); font-size: var(--fs-xs); }
  .error { color: var(--neg); font-size: var(--fs-xs); }
</style>
