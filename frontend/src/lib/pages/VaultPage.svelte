<script>
  /*
   * Vault — admin-only encrypted secret store.
   *
   * Unified store for everything with a password or a token:
   * exchange credentials, telegram tokens, sentry DSNs, webhook
   * URLs, SMTP credentials, on-chain RPC keys, or arbitrary named
   * values. Exchange entries are the only kind pushed to agents;
   * the rest live server-side.
   *
   * The dashboard never reads values back after save. Rotate =
   * new value. Delete = gone. Matches the "no reveal" rule of
   * any serious secrets manager.
   *
   * This page is the coordinator — list + form + picker live in
   * components/vault/*, kind catalogue + validation helpers in
   * vault-kinds.js.
   */
  import Icon from '../components/Icon.svelte'
  import VaultKindPicker from '../components/vault/VaultKindPicker.svelte'
  import VaultForm from '../components/vault/VaultForm.svelte'
  import VaultEntryList from '../components/vault/VaultEntryList.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'
  import { emptyForm, kindSpec } from '../vault-kinds.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let rows = $state([])
  let loading = $state(true)
  let loadError = $state(null)

  // 'idle' | 'pick-kind' | 'create' | 'rotate'
  let mode = $state('idle')
  let editingName = $state(null)
  let formInitial = $state(null)
  let formBusy = $state(false)
  let busyName = $state({})

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/vault')
      loadError = null
    } catch (e) {
      loadError = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, 5_000)
    return () => clearInterval(t)
  })

  function startPickKind() {
    mode = 'pick-kind'
    editingName = null
    formInitial = null
  }

  function pickKindAndProceed(kind) {
    mode = 'create'
    editingName = null
    formInitial = emptyForm(kind)
  }

  function startRotate(row) {
    const spec = kindSpec(row.kind)
    formInitial = {
      name: row.name,
      kind: row.kind,
      description: row.description || '',
      values: Object.fromEntries(spec.values.map((v) => [v.key, ''])),
      metadata: Object.fromEntries(
        spec.metadata.map((m) => [m.key, (row.metadata && row.metadata[m.key]) || (m.enum ? m.enum[0].value : '')])
      ),
      allowed_agents: (row.allowed_agents || []).join(', '),
      expires_at: row.expires_at_ms
        ? new Date(row.expires_at_ms).toISOString().slice(0, 10)
        : '',
    }
    editingName = row.name
    mode = 'rotate'
  }

  async function submitForm(body) {
    formBusy = true
    try {
      const url = mode === 'create'
        ? '/api/v1/vault'
        : `/api/v1/vault/${encodeURIComponent(editingName)}`
      const method = mode === 'create' ? 'POST' : 'PUT'
      const r = await api.authedFetch(url, { method, body: JSON.stringify(body) })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      cancelForm()
      await refresh()
    } finally {
      formBusy = false
    }
  }

  async function deleteRow(name) {
    busyName[name] = true
    try {
      const r = await api.authedFetch(`/api/v1/vault/${encodeURIComponent(name)}`, { method: 'DELETE' })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      await refresh()
    } catch (_) {
      /* swallow — list row stays, operator retries */
    } finally {
      delete busyName[name]
      busyName = { ...busyName }
    }
  }

  function cancelForm() {
    mode = 'idle'
    editingName = null
    formInitial = null
  }
</script>

<div class="page scroll">
  <div class="container">
    <header class="page-header">
      <div>
        <h1>Vault</h1>
        <p class="page-sub">
          Encrypted store for every secret the controller holds —
          exchange API keys, Telegram tokens, Sentry DSNs, webhooks, RPC keys,
          anything else. Values are encrypted at rest (AES-256-GCM) under the
          controller's master key and never returned from the dashboard after save.
        </p>
      </div>
      {#if mode === 'idle'}
        <Button variant="primary" onclick={startPickKind}>
          {#snippet children()}<Icon name="check" size={12} />
          <span>Add entry</span>{/snippet}
        </Button>
      {/if}
    </header>

    {#if loadError}
      <div class="inline-msg err">
        <Icon name="alert" size={12} />
        <span>{loadError}</span>
      </div>
    {/if}

    {#if mode === 'pick-kind'}
      <VaultKindPicker onPick={pickKindAndProceed} onCancel={cancelForm} />
    {/if}

    {#if (mode === 'create' || mode === 'rotate') && formInitial}
      <VaultForm
        {mode}
        {editingName}
        initialForm={formInitial}
        busy={formBusy}
        onSubmit={submitForm}
        onCancel={cancelForm}
      />
    {/if}

    <VaultEntryList
      {rows}
      {loading}
      {busyName}
      onRotate={startRotate}
      onDelete={deleteRow}
    />
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .container { max-width: 880px; margin: 0 auto; display: flex; flex-direction: column; gap: var(--s-4); }
  .page-header { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--s-4); margin-bottom: var(--s-2); }
  .page-header h1 { margin: 0 0 6px; font-size: var(--fs-xl); font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .page-sub { margin: 0; color: var(--fg-muted); font-size: var(--fs-sm); line-height: 1.5; max-width: 620px; }

  .inline-msg {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .inline-msg.err { color: var(--danger); background: color-mix(in srgb, var(--danger) 8%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent); }
</style>
