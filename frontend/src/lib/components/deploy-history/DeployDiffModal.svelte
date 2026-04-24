<script>
  /*
   * Side-by-side diff of two strategy-deploy graphs. Parent
   * loads both JSON bodies and feeds a `{ current, prior,
   * currentJson, priorJson }` state object; this component
   * renders the layout.
   */
  import { diffMarkers } from './deploy-diff-utils.js'

  let {
    state = null,      // null | { current, prior, currentJson, priorJson }
    loading = false,
    error = '',
    onClose,
  } = $props()

  function fmtTs(t) {
    if (!t) return '—'
    return new Date(t).toLocaleString()
  }
</script>

{#if loading}
  <div class="diff-backdrop">
    <div class="diff-card"><div class="diff-title">loading diff…</div></div>
  </div>
{:else if state}
  <div
    class="diff-backdrop"
    role="button"
    tabindex="-1"
    aria-label="Close diff"
    onclick={onClose}
    onkeydown={(e) => { if (e.key === 'Escape') onClose() }}
  >
    <div
      class="diff-card"
      role="dialog"
      aria-modal="true"
      aria-label="Graph diff"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <div class="diff-head">
        <div class="diff-title">
          <code>{state.current.name}</code>
          <span class="muted">·</span>
          <span class="small">diff</span>
        </div>
        <button type="button" class="rb-btn" onclick={onClose}>Close</button>
      </div>
      {#if error}
        <div class="error">{error}</div>
      {:else if !state.prior}
        <div class="muted small">First deploy of this graph — nothing to diff against.</div>
        <pre class="diff-one">{state.currentJson}</pre>
      {:else}
        <div class="diff-meta">
          <div class="diff-col muted small">
            prev · {state.prior.hash.slice(0, 12)}…
            <span class="muted">· {fmtTs(state.prior.deployed_at)}</span>
          </div>
          <div class="diff-col muted small">
            this · {state.current.hash.slice(0, 12)}…
            <span class="muted">· {fmtTs(state.current.deployed_at)}</span>
          </div>
        </div>
        <div class="diff-rows">
          {#each diffMarkers(state.priorJson, state.currentJson) as row}
            <div class="diff-row diff-{row.tag}">
              <div class="diff-cell"><code>{row.left}</code></div>
              <div class="diff-cell"><code>{row.right}</code></div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .diff-backdrop {
    position: fixed; inset: 0;
    background: var(--bg-overlay);
    display: flex; align-items: center; justify-content: center;
    z-index: 10;
  }
  .diff-card {
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    width: min(1100px, 92vw);
    max-height: 82vh;
    display: flex; flex-direction: column;
    padding: var(--s-3);
    gap: var(--s-2);
  }
  .diff-head {
    display: flex; justify-content: space-between; align-items: center;
  }
  .diff-title { display: flex; align-items: center; gap: var(--s-2); font-size: var(--fs-xs); }
  .rb-btn {
    padding: 2px var(--s-2);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-secondary);
    font-size: var(--fs-2xs); cursor: pointer;
  }
  .rb-btn:hover { border-color: var(--warn); color: var(--warn); }
  .diff-meta { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2); }
  .diff-col { font-family: var(--font-mono); font-size: var(--fs-2xs); }
  .diff-rows {
    display: flex; flex-direction: column;
    max-height: 60vh; overflow: auto;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    background: var(--bg-chip);
  }
  .diff-row {
    display: grid; grid-template-columns: 1fr 1fr;
    gap: 1px; background: var(--border-subtle);
    font-family: var(--font-mono); font-size: var(--fs-2xs);
  }
  .diff-cell {
    padding: 2px var(--s-2);
    background: var(--bg-raised);
    white-space: pre;
  }
  .diff-row.diff-eq .diff-cell { opacity: 0.6; }
  .diff-row.diff-add .diff-cell:last-child { background: color-mix(in srgb, var(--ok) 18%, transparent); }
  .diff-row.diff-del .diff-cell:first-child { background: color-mix(in srgb, var(--danger) 18%, transparent); }
  .diff-row.diff-chg .diff-cell { background: color-mix(in srgb, var(--warn) 15%, transparent); }
  .diff-one {
    max-height: 60vh; overflow: auto;
    padding: var(--s-2);
    background: var(--bg-chip);
    border-radius: var(--r-sm);
    font-family: var(--font-mono); font-size: var(--fs-2xs);
  }
  .small { font-size: var(--fs-2xs); color: var(--fg-muted); }
  .error { color: var(--neg); font-size: var(--fs-xs); }
</style>
