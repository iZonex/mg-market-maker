<script>
  /*
   * Portal invite — admin generates a one-time signed URL that a
   * tenant trades for a ClientReader account scoped to their
   * client_id. Single-use inside 24h.
   */
  import Card from '../Card.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    selected,
    invite = null,
    busy = false,
    onGenerate,
    onCopy,
  } = $props()
</script>

<Card title="Portal invite" subtitle="generate a one-time signup URL for this client" span={3}>
  {#snippet children()}
    <div class="invite-row">
      <Button variant="ok" size="sm" disabled={busy} onclick={onGenerate}>
        {#snippet children()}{busy ? 'Generating…' : invite ? 'Regenerate invite URL' : 'Generate invite URL'}{/snippet}
      </Button>
      {#if invite}
        <div class="invite-detail">
          <code class="invite-url mono">{invite.url}</code>
          <Button variant="ghost" size="sm" onclick={onCopy}>
            {#snippet children()}Copy{/snippet}
          </Button>
          <span class="invite-exp muted">expires {new Date(invite.expires_at).toLocaleString()}</span>
        </div>
      {/if}
    </div>
    <div class="muted small">
      Send this URL to the tenant. They pick a name + password, get a
      ClientReader account scoped to <code>{selected}</code>. Single-use
      inside a 24-hour window.
    </div>
  {/snippet}
</Card>

<style>
  .invite-row { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; margin-bottom: var(--s-2); }
  .invite-detail { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .invite-url {
    padding: 2px 8px; font-size: 11px;
    background: var(--bg-chip); border-radius: var(--r-sm);
    max-width: 520px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .invite-exp { font-size: 10px; }
  .small { font-size: 11px; }
  code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }
</style>
