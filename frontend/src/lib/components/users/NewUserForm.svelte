<script>
  /*
   * New-user form — name + role + allowed-symbols. Submit returns
   * the freshly-minted API key once; parent renders the
   * IssuedSecretBox from the onCreated callback's payload.
   */
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    busy = false,
    error = '',
    onSubmit,       // async ({ name, role, allowed_symbols }) => void
    onCancel,
  } = $props()

  let name = $state('')
  let role = $state('viewer')
  let symbolsCsv = $state('')

  async function submit() {
    if (!name.trim()) return
    await onSubmit({
      name: name.trim(),
      role,
      allowed_symbols: symbolsCsv
        .split(',').map((s) => s.trim()).filter(Boolean),
    })
    name = ''
    symbolsCsv = ''
    role = 'viewer'
  }
</script>

<form class="form" onsubmit={(e) => { e.preventDefault(); submit() }}>
  <div class="form-head">
    <span class="label">Create user</span>
    <Button variant="ghost" size="sm" iconOnly onclick={onCancel} aria-label="Close">
      {#snippet children()}<Icon name="close" size={12} />{/snippet}
    </Button>
  </div>
  <div class="form-row">
    <label class="f-label" for="nu-name">Name</label>
    <input id="nu-name" type="text" class="text-input" bind:value={name} placeholder="Alice" disabled={busy} />
  </div>
  <div class="form-row">
    <label class="f-label" for="nu-role">Role</label>
    <select id="nu-role" class="select-input" bind:value={role} disabled={busy}>
      <option value="viewer">viewer</option>
      <option value="operator">operator</option>
      <option value="admin">admin</option>
    </select>
  </div>
  <div class="form-row">
    <label class="f-label" for="nu-symbols">Allowed symbols</label>
    <input id="nu-symbols" type="text" class="text-input" bind:value={symbolsCsv} placeholder="BTCUSDT,ETHUSDT (empty = all)" disabled={busy} />
  </div>
  <div class="actions">
    <Button variant="ghost" onclick={onCancel} disabled={busy}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button variant="primary" type="submit" disabled={busy || !name.trim()}>
      {#snippet children()}{#if busy}
        <span class="spinner"></span>
        <span>Creating…</span>
      {:else}
        <Icon name="check" size={14} />
        <span>Create</span>
      {/if}{/snippet}
    </Button>
  </div>
  {#if error}
    <div class="error-line">
      <Icon name="alert" size={12} />
      <span>{error}</span>
    </div>
  {/if}
</form>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-4);
    background: var(--bg-base);
    border: 1px dashed var(--border-strong);
    border-radius: var(--r-md);
  }
  .form-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .form-row {
    display: grid;
    grid-template-columns: 140px 1fr;
    align-items: center;
    gap: var(--s-3);
  }
  .f-label {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    font-weight: 500;
  }
  .text-input,
  .select-input {
    padding: 6px 10px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .text-input:focus,
  .select-input:focus { outline: none; border-color: var(--accent); }

  .actions { display: flex; gap: var(--s-2); flex-wrap: wrap; justify-content: flex-end; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid color-mix(in srgb, var(--fg-on-accent) 25%, transparent);
    border-top-color: var(--fg-on-accent);
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .error-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
    color: var(--danger);
  }
</style>
