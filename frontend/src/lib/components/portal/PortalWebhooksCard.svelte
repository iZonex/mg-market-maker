<script>
  /*
   * Self-service webhook registration for the client portal.
   *
   * Add URL → posts to /webhooks, Remove → DELETE, Test-fire →
   * POST /webhooks/test which returns a per-URL dispatch report
   * that stays visible until the next action.
   */
  import Card from '../Card.svelte'
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    urls = [],
    busy = false,
    error = '',
    testReport = null,
    onAdd,
    onRemove,
    onTest,
  } = $props()

  let newUrl = $state('')

  async function addUrl() {
    const trimmed = newUrl.trim()
    if (!trimmed) return
    await onAdd(trimmed)
    newUrl = ''
  }
</script>

<Card title="My webhooks" subtitle="self-service registration" span={2}>
  {#snippet children()}
    <div class="wh-form">
      <input
        type="text"
        class="wh-input"
        placeholder="https://your-service.example/webhook"
        bind:value={newUrl}
        disabled={busy}
        onkeydown={(e) => { if (e.key === 'Enter') addUrl() }}
      />
      <Button variant="primary" size="sm" onclick={addUrl} disabled={busy || !newUrl.trim()}>
        {#snippet children()}<Icon name="check" size={12} />
        <span>Add</span>{/snippet}
      </Button>
      <Button variant="ghost" size="sm" onclick={onTest}
        disabled={busy || urls.length === 0}
        title={urls.length === 0 ? 'Add a URL first' : 'Fire a synthetic test event to every URL'}>
        {#snippet children()}<Icon name="shield" size={12} />
        <span>Test-fire</span>{/snippet}
      </Button>
    </div>
    {#if error}
      <div class="wh-err">{error}</div>
    {/if}
    {#if urls.length === 0}
      <div class="muted">
        No webhooks registered. Add a URL to receive fill, PnL, and
        SLA events. We POST JSON payloads with a short retry window;
        delivery history is below.
      </div>
    {:else}
      <ul class="wh-list">
        {#each urls as u (u)}
          <li class="wh-row">
            <code class="mono" title={u}>{u}</code>
            <Button variant="ghost" size="xs" onclick={() => onRemove(u)} disabled={busy} aria-label="Remove webhook">
              {#snippet children()}<Icon name="close" size={10} />
              <span>Remove</span>{/snippet}
            </Button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if testReport}
      <div class="wh-report">
        <span class="k">Test dispatch</span>
        <span class="v mono">{testReport.succeeded}/{testReport.attempted}</span>
        <div class="wh-results">
          {#each (testReport.results || []) as r (r.url + ':' + r.timestamp)}
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

<style>
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
  .k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .wh-results { display: flex; flex-direction: column; gap: 4px; }
  .wh-result {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s-2); font-size: var(--fs-xs);
  }
  .wh-result code { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
