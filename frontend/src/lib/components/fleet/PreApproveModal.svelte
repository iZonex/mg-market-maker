<script>
  /*
   * Wave F2 — pre-approve flow. Admin pastes the fingerprint the
   * agent logged on boot (before it ever connects); controller
   * creates an Accepted record with empty pubkey. When the agent
   * does connect, its pubkey binds silently and the handshake
   * clears without a Pending step.
   */
  import { Button, Modal } from '../../primitives/index.js'

  let {
    open = false,
    busy = false,
    error = null,
    onSubmit,
    onClose,
  } = $props()

  let fingerprint = $state('')
  let notes = $state('')

  $effect(() => {
    if (open) {
      fingerprint = ''
      notes = ''
    }
  })

  async function submit() {
    const fp = fingerprint.trim()
    if (!fp) return
    await onSubmit({ fingerprint: fp, notes: notes.trim() || null })
  }
</script>

<Modal {open} ariaLabel="Pre-approve fingerprint" maxWidth="520px" {onClose}>
  {#snippet children()}
    <div class="preapprove-title">Pre-approve fingerprint</div>
    <div class="preapprove-body">
      <p class="preapprove-lead">
        Paste the fingerprint the agent logged on its first boot —
        you'll see it in the agent's stdout / systemd journal:
        <code class="mono">mm-agent starting … fingerprint=d5d0bf4df0ad14f5</code>.
        When the agent connects, it'll be auto-accepted without a
        Pending step.
      </p>
      <label class="field">
        <span>Fingerprint</span>
        <input type="text" bind:value={fingerprint} placeholder="d5d0bf4df0ad14f5" disabled={busy} />
      </label>
      <label class="field">
        <span>Notes (optional)</span>
        <input type="text" bind:value={notes} placeholder="e.g. eu-01 trading box, ACME tenant" disabled={busy} />
      </label>
      {#if error}
        <div class="preapprove-err">{error}</div>
      {/if}
    </div>
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={onClose} disabled={busy}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button variant="ok" onclick={submit} disabled={busy || !fingerprint.trim()}>
      {#snippet children()}{busy ? 'Creating…' : 'Pre-approve'}{/snippet}
    </Button>
  {/snippet}
</Modal>

<style>
  .preapprove-title { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 600; }
  .preapprove-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .preapprove-lead { margin: 0; font-size: var(--fs-xs); color: var(--fg-secondary); line-height: 1.5; }
  .preapprove-lead code { background: var(--bg-chip); padding: 1px 4px; border-radius: var(--r-sm); font-size: 10px; }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field span {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .field input {
    padding: var(--s-2); background: var(--bg-chip);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    color: var(--fg-primary); font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .preapprove-err {
    padding: var(--s-2); background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger); border-radius: var(--r-sm); font-size: var(--fs-xs);
  }
</style>
