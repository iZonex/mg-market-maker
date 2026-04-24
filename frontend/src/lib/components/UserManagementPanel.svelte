<script>
  /*
   * User management panel (UX-1).
   *
   * Lists API users registered on the auth state + exposes a
   * small create-user form. Create returns the freshly-minted
   * API key ONCE — the backend does not persist the plaintext
   * anywhere else, so the panel surfaces a one-time copy box.
   *
   * Admin-only at the server side — the panel still gates the
   * form on `auth.canControl()` so operators don't see fields
   * they can't submit.
   */

  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let users = $state([])
  let loadError = $state('')
  let refreshing = $state(false)

  // Create-user form state.
  let showForm = $state(false)
  let formName = $state('')
  let formRole = $state('viewer')
  let formSymbolsCsv = $state('')
  let formBusy = $state(false)
  let formError = $state('')
  // Last-created user's plaintext API key — rendered once
  // then cleared on the next form open.
  let justIssuedKey = $state('')
  let justIssuedName = $state('')

  // Wave H1 — password-reset URL surface. Admin clicks
  // "Reset password" on a row; we POST to mint a one-shot
  // signed token and render the URL once. Admin delivers
  // the link out-of-band.
  let resetUrl = $state('')
  let resetForName = $state('')
  let resetExpires = $state('')
  let resetBusyFor = $state('')
  let resetError = $state('')

  async function refresh() {
    refreshing = true
    try {
      users = await api.getJson('/api/admin/users')
      loadError = ''
    } catch (e) {
      loadError = e.message
    } finally {
      refreshing = false
    }
  }

  function openForm() {
    showForm = true
    formName = ''
    formRole = 'viewer'
    formSymbolsCsv = ''
    formError = ''
    justIssuedKey = ''
    justIssuedName = ''
  }

  async function submitForm() {
    if (!formName.trim()) {
      formError = 'Name is required.'
      return
    }
    formBusy = true
    formError = ''
    try {
      const body = {
        name: formName.trim(),
        role: formRole,
        allowed_symbols: formSymbolsCsv
          .split(',')
          .map((s) => s.trim())
          .filter((s) => s.length > 0),
      }
      const resp = await api.postJson('/api/admin/users', body)
      justIssuedKey = resp.api_key
      justIssuedName = resp.name
      // Reset form fields; keep panel open so the operator
      // can copy the key.
      formName = ''
      formSymbolsCsv = ''
      formRole = 'viewer'
      await refresh()
    } catch (e) {
      formError = e.message
    } finally {
      formBusy = false
    }
  }

  async function copyKey() {
    try {
      await navigator.clipboard.writeText(justIssuedKey)
    } catch (_) {
      // Clipboard API is gated on a user gesture + secure
      // context; if it fails we just leave the box visible
      // so the operator can select + ⌘C manually.
    }
  }

  async function copyResetLink() {
    try {
      await navigator.clipboard.writeText(absoluteUrl(resetUrl))
    } catch (_) {
      // Same graceful-fallback — operator can still select
      // the text in the code block and copy manually.
    }
  }

  function absoluteUrl(path) {
    if (!path) return ''
    try {
      return new URL(path, window.location.origin).toString()
    } catch (_) {
      return path
    }
  }

  async function startReset(u) {
    resetError = ''
    resetUrl = ''
    resetForName = ''
    resetExpires = ''
    resetBusyFor = u.id
    try {
      const resp = await api.postJson(
        `/api/admin/users/${encodeURIComponent(u.id)}/reset-password`,
        {},
      )
      resetUrl = resp.reset_url
      resetForName = u.name
      resetExpires = resp.expires_at
    } catch (e) {
      resetError = e.message
    } finally {
      resetBusyFor = ''
    }
  }

  function dismissReset() {
    resetUrl = ''
    resetForName = ''
    resetExpires = ''
    resetError = ''
  }

  function dismissKey() {
    justIssuedKey = ''
    justIssuedName = ''
    showForm = false
  }

  const canControl = $derived(auth?.canControl?.() ?? false)

  $effect(() => {
    refresh()
    // Slow refresh — user list changes rarely.
    const id = setInterval(refresh, 30_000)
    return () => clearInterval(id)
  })

  function roleChipClass(role) {
    switch (role) {
      case 'admin': return 'chip-neg'
      case 'operator': return 'chip-warn'
      default: return 'chip-muted'
    }
  }
</script>

<div class="panel">
  {#if loadError}
    <div class="empty-state">
      <span class="empty-state-icon" style="color: var(--neg)"><Icon name="alert" size={18} /></span>
      <span class="empty-state-title">Failed to load users</span>
      <span class="empty-state-hint">{loadError}</span>
    </div>
  {:else}
    <header class="head">
      <div class="head-left">
        <span class="label">Users</span>
        <span class="chip">{users.length}</span>
      </div>
      {#if canControl}
        <Button variant="primary" onclick={openForm}
 disabled={showForm || formBusy}>
          {#snippet children()}<Icon name="check" size={12} />
          <span>New user</span>{/snippet}
        </Button>
      {/if}
    </header>

    {#if users.length === 0}
      <div class="empty-state" style="padding: var(--s-3)">
        <span class="empty-state-title">No users registered</span>
        <span class="empty-state-hint">
          Create one to issue an API key for dashboard access.
        </span>
      </div>
    {:else}
      <table class="tbl">
        <thead>
          <tr>
            <th>Name</th>
            <th>Role</th>
            <th>Symbols</th>
            <th>API key</th>
            {#if canControl}
              <th class="actions-col">Actions</th>
            {/if}
          </tr>
        </thead>
        <tbody>
          {#each users as u (u.id)}
            <tr>
              <td class="name">{u.name}</td>
              <td><span class="chip {roleChipClass(u.role)}">{u.role}</span></td>
              <td class="meta">
                {#if u.allowed_symbols && u.allowed_symbols.length > 0}
                  {u.allowed_symbols.join(', ')}
                {:else}
                  <span class="muted">all</span>
                {/if}
              </td>
              <td class="mono">{u.api_key_hint}</td>
              {#if canControl}
                <td class="actions-cell">
                  <Button variant="primary" onclick={() => startReset(u)}
 disabled={resetBusyFor === u.id}
 title="Generate a one-shot password-reset URL for this user">
          {#snippet children()}{#if resetBusyFor === u.id}
                      <span class="spinner"></span>
                      <span>Issuing…</span>
                    {:else}
                      <Icon name="shield" size={12} />
                      <span>Reset password</span>
                    {/if}{/snippet}
        </Button>
                </td>
              {/if}
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}

  {#if showForm && canControl}
    <form class="form" onsubmit={(e) => { e.preventDefault(); submitForm() }}>
      <div class="form-head">
        <span class="label">Create user</span>
        <Button variant="primary" onclick={() => { showForm = false }}
 aria-label="Close">
          {#snippet children()}<Icon name="close" size={12} />{/snippet}
        </Button>
      </div>
      <div class="form-row">
        <label class="f-label" for="nu-name">Name</label>
        <input
          id="nu-name"
          type="text"
          class="text-input"
          bind:value={formName}
          placeholder="Alice"
          disabled={formBusy}
        />
      </div>
      <div class="form-row">
        <label class="f-label" for="nu-role">Role</label>
        <select id="nu-role" class="select-input" bind:value={formRole} disabled={formBusy}>
          <option value="viewer">viewer</option>
          <option value="operator">operator</option>
          <option value="admin">admin</option>
        </select>
      </div>
      <div class="form-row">
        <label class="f-label" for="nu-symbols">Allowed symbols</label>
        <input
          id="nu-symbols"
          type="text"
          class="text-input"
          bind:value={formSymbolsCsv}
          placeholder="BTCUSDT,ETHUSDT (empty = all)"
          disabled={formBusy}
        />
      </div>
      <div class="actions">
        <Button variant="primary" type="submit" disabled={formBusy}>
          {#snippet children()}{#if formBusy}
            <span class="spinner"></span>
            <span>Creating…</span>
          {:else}
            <Icon name="check" size={14} />
            <span>Create</span>
          {/if}{/snippet}
        </Button>
        <Button variant="primary" onclick={() => (showForm = false)} disabled={formBusy}>
          {#snippet children()}Cancel{/snippet}
        </Button>
      </div>
      {#if formError}
        <div class="error-line">
          <Icon name="alert" size={12} />
          <span>{formError}</span>
        </div>
      {/if}
    </form>
  {/if}

  {#if resetError}
    <div class="error-line">
      <Icon name="alert" size={12} />
      <span>{resetError}</span>
    </div>
  {/if}

  {#if resetUrl}
    <div class="issued-box" role="alert">
      <div class="issued-head">
        <Icon name="shield" size={14} />
        <span class="issued-title">Password reset for “{resetForName}”</span>
      </div>
      <p class="issued-hint">
        Deliver this link to the user via a secure channel
        (Signal, in-person). It is one-shot and expires at
        {new Date(resetExpires).toLocaleString()}.
      </p>
      <div class="issued-key">
        <code>{absoluteUrl(resetUrl)}</code>
        <Button variant="primary" onclick={copyResetLink}>
          {#snippet children()}<Icon name="check" size={12} />
          <span>Copy</span>{/snippet}
        </Button>
      </div>
      <div class="actions">
        <Button variant="primary" onclick={dismissReset}>
          {#snippet children()}Done{/snippet}
        </Button>
      </div>
    </div>
  {/if}

  {#if justIssuedKey}
    <div class="issued-box" role="alert">
      <div class="issued-head">
        <Icon name="shield" size={14} />
        <span class="issued-title">API key for “{justIssuedName}”</span>
      </div>
      <p class="issued-hint">
        Copy now — the key will not be shown again. The user
        logs into the dashboard with this value in the login
        field.
      </p>
      <div class="issued-key">
        <code>{justIssuedKey}</code>
        <Button variant="primary" onclick={copyKey}>
          {#snippet children()}<Icon name="check" size={12} />
          <span>Copy</span>{/snippet}
        </Button>
      </div>
      <div class="actions">
        <Button variant="primary" onclick={dismissKey}>
          {#snippet children()}Done{/snippet}
        </Button>
      </div>
    </div>
  {/if}
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: var(--s-4); }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .head-left { display: flex; align-items: center; gap: var(--s-2); }

  .name { color: var(--fg-primary); font-weight: 500; }
  .muted { color: var(--fg-muted); }
  .mono {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  .actions-col { width: 1%; white-space: nowrap; }
  .actions-cell { text-align: right; }

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

  .actions { display: flex; gap: var(--s-2); flex-wrap: wrap; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0, 0, 0, 0.25);
    border-top-color: #001510;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .error-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
    color: var(--neg);
  }

  .issued-box {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-4);
    background: rgba(0, 208, 156, 0.06);
    border: 1px solid rgba(0, 208, 156, 0.35);
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
</style>
