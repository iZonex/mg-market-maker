<script>
  /*
   * User management panel (UX-1).
   *
   * Lists API users + exposes a create-user form. Create returns
   * the freshly-minted API key ONCE — the backend does not
   * persist the plaintext anywhere else. Admin-only at the server
   * side; the panel also gates the form on `auth.canControl()`.
   *
   * Form + one-shot-secret surface live in components/users/*.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'
  import NewUserForm from './users/NewUserForm.svelte'
  import IssuedSecretBox from './users/IssuedSecretBox.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let users = $state([])
  let loadError = $state('')

  let showForm = $state(false)
  let formBusy = $state(false)
  let formError = $state('')
  // Last-created user's plaintext API key — rendered once then
  // cleared on the next form open.
  let justIssuedKey = $state('')
  let justIssuedName = $state('')

  // Wave H1 — password-reset URL surface. Admin clicks
  // "Reset password" on a row; we POST to mint a one-shot
  // signed token and render the URL once.
  let resetUrl = $state('')
  let resetForName = $state('')
  let resetExpires = $state('')
  let resetBusyFor = $state('')
  let resetError = $state('')

  async function refresh() {
    try {
      users = await api.getJson('/api/admin/users')
      loadError = ''
    } catch (e) {
      loadError = e.message
    }
  }

  function openForm() {
    showForm = true
    formError = ''
    justIssuedKey = ''
    justIssuedName = ''
  }

  async function submitForm(body) {
    formBusy = true
    formError = ''
    try {
      const resp = await api.postJson('/api/admin/users', body)
      justIssuedKey = resp.api_key
      justIssuedName = resp.name
      await refresh()
    } catch (e) {
      formError = e.message
    } finally {
      formBusy = false
    }
  }

  async function copyKey() {
    // Clipboard API is gated on a user gesture + secure context;
    // if it fails we just leave the box visible so the operator
    // can select + ⌘C manually.
    try { await navigator.clipboard.writeText(justIssuedKey) } catch {}
  }

  async function copyResetLink() {
    try { await navigator.clipboard.writeText(absoluteUrl(resetUrl)) } catch {}
  }

  function absoluteUrl(path) {
    if (!path) return ''
    try { return new URL(path, window.location.origin).toString() }
    catch { return path }
  }

  async function startReset(u) {
    resetError = ''
    resetUrl = ''
    resetForName = ''
    resetExpires = ''
    resetBusyFor = u.id
    try {
      const resp = await api.postJson(
        `/api/admin/users/${encodeURIComponent(u.id)}/reset-password`, {},
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
    resetUrl = ''; resetForName = ''; resetExpires = ''; resetError = ''
  }

  function dismissKey() {
    justIssuedKey = ''; justIssuedName = ''; showForm = false
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
        <Button variant="primary" onclick={openForm} disabled={showForm || formBusy}>
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
                  <Button
                    variant="ghost"
                    size="sm"
                    onclick={() => startReset(u)}
                    disabled={resetBusyFor === u.id}
                    title="Generate a one-shot password-reset URL for this user"
                  >
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
    <NewUserForm
      busy={formBusy}
      error={formError}
      onSubmit={submitForm}
      onCancel={() => { showForm = false }}
    />
  {/if}

  {#if resetError}
    <div class="error-line">
      <Icon name="alert" size={12} />
      <span>{resetError}</span>
    </div>
  {/if}

  {#if resetUrl}
    <IssuedSecretBox
      title={`Password reset for “${resetForName}”`}
      hint={`Deliver this link to the user via a secure channel (Signal, in-person). It is one-shot and expires at ${new Date(resetExpires).toLocaleString()}.`}
      secret={absoluteUrl(resetUrl)}
      onCopy={copyResetLink}
      onDismiss={dismissReset}
    />
  {/if}

  {#if justIssuedKey}
    <IssuedSecretBox
      title={`API key for “${justIssuedName}”`}
      hint="Copy now — the key will not be shown again. The user logs into the dashboard with this value in the login field."
      secret={justIssuedKey}
      onCopy={copyKey}
      onDismiss={dismissKey}
    />
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
  .actions-col { width: 1%; white-space: nowrap; }
  .actions-cell { text-align: right; }

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
