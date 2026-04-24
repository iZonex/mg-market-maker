<script>
  /*
   * Stored-entries list — groups entries by kind and renders
   * each row with rotate/delete actions plus metadata chips
   * and an expiry chip. Parent supplies rows, receives
   * onRotate(row) + onDelete(name) callbacks.
   */
  import Card from '../Card.svelte'
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'
  import { fmtDate } from '../../format.js'
  import { kindSpec, expiryTone, fmtExpiryRelative } from '../../vault-kinds.js'

  let {
    rows = [],
    loading = false,
    busyName = {},
    onRotate,
    onDelete,
  } = $props()

  const formatDate = (ms) => fmtDate(ms, 'full')
  let confirmDeleteName = $state(null)

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

  function requestDelete(name) {
    confirmDeleteName = name
  }

  async function confirmDelete(name) {
    try {
      await onDelete(name)
    } finally {
      if (confirmDeleteName === name) confirmDeleteName = null
    }
  }
</script>

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
                          <span class="vault-chip">{k}={v}</span>
                        {/each}
                      </div>
                    {/if}
                    {#if r.allowed_agents && r.allowed_agents.length > 0}
                      <div class="row-meta-chips">
                        <span class="chip-k">agents:</span>
                        {#each r.allowed_agents as a (a)}<span class="vault-chip">{a}</span>{/each}
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
                      <Button variant="danger" size="sm" disabled={busyName[r.name]} onclick={() => confirmDelete(r.name)}>
                        {#snippet children()}{busyName[r.name] ? 'Deleting…' : 'Yes, delete'}{/snippet}
                      </Button>
                      <Button variant="ghost" size="sm" onclick={() => (confirmDeleteName = null)}>
                        {#snippet children()}Cancel{/snippet}
                      </Button>
                    {:else}
                      <Button variant="ghost" size="sm" onclick={() => onRotate(r)}>
                        {#snippet children()}<Icon name="refresh" size={12} />
                        <span>Rotate</span>{/snippet}
                      </Button>
                      <Button variant="ghost" size="sm" onclick={() => requestDelete(r.name)}>
                        {#snippet children()}<Icon name="close" size={12} />
                        <span>Delete</span>{/snippet}
                      </Button>
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

<style>
  code {
    font-family: var(--font-mono); font-size: 11px;
    background: var(--bg-chip); color: var(--fg-primary);
    padding: 1px 6px; border-radius: 3px;
  }
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
  .vault-chip {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }
  .chip-k {
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
