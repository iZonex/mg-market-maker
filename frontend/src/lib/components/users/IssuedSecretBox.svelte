<script>
  /*
   * One-shot secret reveal box — the shared visual for both a
   * freshly-minted API key and a password-reset URL. Admin copies,
   * then dismisses. The backend does not persist the plaintext
   * anywhere else, so this is the only chance.
   */
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    title,
    hint,
    secret,
    onCopy,
    onDismiss,
  } = $props()
</script>

<div class="issued-box" role="alert">
  <div class="issued-head">
    <Icon name="shield" size={14} />
    <span class="issued-title">{title}</span>
  </div>
  <p class="issued-hint">{hint}</p>
  <div class="issued-key">
    <code>{secret}</code>
    <Button variant="primary" onclick={onCopy}>
      {#snippet children()}<Icon name="check" size={12} />
      <span>Copy</span>{/snippet}
    </Button>
  </div>
  <div class="actions">
    <Button variant="primary" onclick={onDismiss}>
      {#snippet children()}Done{/snippet}
    </Button>
  </div>
</div>

<style>
  .issued-box {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-4);
    background: color-mix(in srgb, var(--pos) 6%, transparent);
    border: 1px solid color-mix(in srgb, var(--pos) 35%, transparent);
    border-radius: var(--r-md);
  }
  .issued-head {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    color: var(--pos);
  }
  .issued-title { font-weight: 600; }
  .issued-hint {
    margin: 0;
    font-size: var(--fs-xs);
    line-height: var(--lh-snug);
    color: var(--fg-secondary);
  }
  .issued-key {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    overflow-x: auto;
  }
  .issued-key code {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--fg-primary);
    user-select: all;
    white-space: nowrap;
  }
  .actions { display: flex; gap: var(--s-2); flex-wrap: wrap; }
</style>
