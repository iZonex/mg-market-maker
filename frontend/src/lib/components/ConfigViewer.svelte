<script>
  /*
   * Variables snapshot viewer — drilldown edition.
   *
   * Shape contract: { row: DeploymentStateRow }
   *
   * Renders the effective `variables` map the deployment is
   * running with right now. Top-level keys land in a scalar table;
   * nested objects + arrays collapse into a raw JSON drawer so
   * operators can still inspect them without shelling into the
   * host to `cat` the TOML.
   *
   * No editing from here — ParamTuner owns the mutation path. This
   * view is the authoritative "what is the strategy actually
   * running with right now" read-out.
   */

  import Icon from './Icon.svelte'

  import { Button } from '../primitives/index.js'

  let { row } = $props()

  const variables = $derived(row?.variables || {})

  let expanded = $state(false)

  const scalarEntries = $derived.by(() => {
    const out = []
    for (const [k, v] of Object.entries(variables)) {
      if (v === null) { out.push([k, '—', 'muted']); continue }
      const t = typeof v
      if (t === 'string') out.push([k, v, 'str'])
      else if (t === 'number') out.push([k, String(v), 'num'])
      else if (t === 'boolean') out.push([k, v ? 'true' : 'false', v ? 'on' : 'muted'])
      // Objects + arrays fall through to the raw drawer.
    }
    out.sort((a, b) => a[0].localeCompare(b[0]))
    return out
  })

  const nestedEntries = $derived.by(() => {
    const out = {}
    for (const [k, v] of Object.entries(variables)) {
      if (v !== null && (typeof v === 'object')) {
        out[k] = v
      }
    }
    return out
  })

  const nestedKeys = $derived(Object.keys(nestedEntries).sort())
</script>

<div class="viewer">
  <div class="top">
    <span class="meta">
      {scalarEntries.length} scalar · {nestedKeys.length} nested
    </span>
    <Button variant="ghost" onclick={() => (expanded = !expanded)}>
          {#snippet children()}<span>{expanded ? 'Hide raw JSON' : 'Show raw JSON'}</span>{/snippet}
        </Button>
  </div>

  {#if scalarEntries.length === 0 && nestedKeys.length === 0}
    <div class="empty">
      <Icon name="info" size={12} />
      <span>No variables — the deployment is running on template defaults.</span>
    </div>
  {/if}

  {#if scalarEntries.length > 0}
    <div class="rows">
      {#each scalarEntries as [k, v, cls] (k)}
        <div class="row">
          <span class="row-label mono">{k}</span>
          <span class="row-value mono {cls}">{v}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if nestedKeys.length > 0}
    <div class="section">
      <div class="section-title">Nested</div>
      <ul class="nested">
        {#each nestedKeys as k (k)}
          <li>
            <span class="mono">{k}</span>
            <span class="faint">
              {Array.isArray(nestedEntries[k])
                ? `[${nestedEntries[k].length}]`
                : `{${Object.keys(nestedEntries[k]).length}}`}
            </span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if expanded}
    <pre class="raw">{JSON.stringify(variables, null, 2)}</pre>
  {/if}
</div>

<style>
  .viewer { display: flex; flex-direction: column; gap: var(--s-2); }

  .top {
    display: flex; justify-content: space-between; align-items: center;
    gap: var(--s-3);
  }
  .meta { font-size: var(--fs-2xs); color: var(--fg-muted); }


  .empty {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-muted);
    font-size: var(--fs-xs);
  }

  .rows {
    display: flex; flex-direction: column; gap: 1px;
    background: var(--border-subtle);
    border-radius: var(--r-sm);
    overflow: hidden;
  }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    padding: 4px var(--s-3);
    background: var(--bg-base);
    font-size: var(--fs-xs);
    gap: var(--s-3);
  }
  .row-label { color: var(--fg-secondary); }
  .row-value {
    font-size: var(--fs-2xs);
    text-align: right;
    word-break: break-all;
  }
  .row-value.on { color: var(--pos); }
  .row-value.muted { color: var(--fg-muted); }
  .row-value.str { color: var(--fg-primary); }
  .row-value.num { color: var(--accent); }
  .section { display: flex; flex-direction: column; gap: 4px; }
  .section-title {
    font-size: 10px; font-weight: 600; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .nested {
    list-style: none; margin: 0; padding: 0;
    display: flex; flex-wrap: wrap; gap: var(--s-2);
  }
  .nested li {
    display: inline-flex; gap: 4px; align-items: baseline;
    padding: 2px 8px;
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
  }
  .raw {
    max-height: 320px; overflow: auto;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-family: var(--font-mono); font-size: var(--fs-2xs);
    color: var(--fg-secondary); white-space: pre;
  }
</style>
