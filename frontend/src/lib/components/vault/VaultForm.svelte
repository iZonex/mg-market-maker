<script>
  /*
   * Vault entry form — kind-specific values + metadata + optional
   * expiry + allowed-agents list for exchange credentials.
   *
   * Mode is either 'create' (new entry) or 'rotate' (existing
   * entry: name is fixed, all secret values blank). The parent
   * owns state transitions and the api client; this component
   * owns in-form validation + secret visibility toggle.
   */
  import Card from '../Card.svelte'
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'
  import { KINDS, kindSpec, emptyForm } from '../../vault-kinds.js'

  let {
    mode = 'create',        // 'create' | 'rotate'
    editingName = null,
    initialForm = null,     // prefilled form for rotate
    busy = false,
    onSubmit,               // (body) => Promise
    onCancel,
  } = $props()

  let form = $state(initialForm ?? emptyForm('exchange'))
  let showSecrets = $state(false)
  let msg = $state(null)

  const currentSpec = $derived(kindSpec(form.kind))

  function onKindChange() {
    const keep = { name: form.name, description: form.description, kind: form.kind }
    form = { ...emptyForm(form.kind), ...keep }
  }

  async function submit(e) {
    e.preventDefault()
    msg = null
    const spec = kindSpec(form.kind)
    if (!form.name.trim()) { msg = { tone: 'err', text: 'Name is required' }; return }
    for (const v of spec.values) {
      if (!form.values[v.key]) { msg = { tone: 'err', text: `${v.label} is required` }; return }
    }
    for (const m of spec.metadata.filter((m) => m.required)) {
      if (!form.metadata[m.key]) { msg = { tone: 'err', text: `${m.label} is required` }; return }
    }
    const values = {}
    for (const [k, v] of Object.entries(form.values)) if (v) values[k] = v
    const metadata = {}
    for (const [k, v] of Object.entries(form.metadata)) if (v !== undefined && v !== '') metadata[k] = v
    const allowed_agents = form.allowed_agents
      .split(',').map((s) => s.trim()).filter(Boolean)
    const body = {
      name: form.name.trim(),
      kind: form.kind,
      description: form.description.trim() || null,
      values,
      metadata,
      allowed_agents,
      expires_at_ms: form.expires_at
        ? new Date(form.expires_at + 'T23:59:59').getTime()
        : null,
    }
    try {
      await onSubmit(body)
    } catch (err) {
      msg = { tone: 'err', text: err?.message || 'Save failed' }
    }
  }
</script>

<Card
  title={mode === 'create' ? `New ${currentSpec.label.toLowerCase()} entry` : `Rotate · ${editingName}`}
  subtitle={mode === 'create' ? currentSpec.hint : 'enter a new value · name is fixed'}
  span={1}
>
  {#snippet children()}
    <form class="form" onsubmit={submit}>
      <div class="grid-2">
        <div class="field">
          <label for="v-name">Name</label>
          {#if mode === 'rotate'}
            <div class="readonly">{form.name}</div>
          {:else}
            <input id="v-name" type="text" bind:value={form.name} disabled={busy} placeholder="e.g. binance_spot_main" />
          {/if}
        </div>
        <div class="field">
          <label for="v-kind">Kind</label>
          <select id="v-kind" bind:value={form.kind} onchange={onKindChange} disabled={busy || mode === 'rotate'}>
            {#each KINDS as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
          </select>
          <div class="hint">{currentSpec.hint}</div>
        </div>
      </div>

      <div class="field">
        <label for="v-description">Description <span class="opt">optional</span></label>
        <input id="v-description" type="text" bind:value={form.description} disabled={busy} placeholder="human-readable context" />
      </div>

      <div class="section-head">
        Values
        <span class="hint">secret · encrypted before save</span>
        <label class="toggle">
          <input type="checkbox" bind:checked={showSecrets} />
          <span>Show values</span>
        </label>
      </div>
      <div class="grid-2">
        {#each currentSpec.values as v (v.key)}
          <div class="field">
            <label for="v-val-{v.key}">{v.label}</label>
            <input
              id="v-val-{v.key}"
              type={showSecrets ? 'text' : 'password'}
              autocomplete="off"
              spellcheck="false"
              bind:value={form.values[v.key]}
              disabled={busy}
              placeholder={mode === 'rotate' ? 'enter new value' : 'paste secret'}
            />
          </div>
        {/each}
      </div>

      {#if currentSpec.metadata.length > 0}
        <div class="section-head">Metadata <span class="hint">labels — stored in plaintext</span></div>
        <div class="grid-2">
          {#each currentSpec.metadata as m (m.key)}
            <div class="field">
              <label for="v-meta-{m.key}">
                {m.label}
                {#if !m.required}<span class="opt">optional</span>{/if}
              </label>
              {#if m.enum}
                <select id="v-meta-{m.key}" bind:value={form.metadata[m.key]} disabled={busy}>
                  {#each m.enum as opt (opt.value)}<option value={opt.value}>{opt.label}</option>{/each}
                </select>
              {:else}
                <input id="v-meta-{m.key}" type="text" bind:value={form.metadata[m.key]} disabled={busy} placeholder={m.placeholder || ''} />
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if currentSpec.showAllowedAgents}
        <div class="field">
          <label for="v-allowed">
            Allowed agents <span class="opt">comma-separated — empty means every Accepted agent receives this</span>
          </label>
          <input id="v-allowed" type="text" bind:value={form.allowed_agents} disabled={busy} placeholder="eu-01, eu-02" />
        </div>
      {/if}

      <div class="field">
        <label for="v-expires">
          Expires on <span class="opt">optional — UI flags rows &lt; 7 days to operator</span>
        </label>
        <input id="v-expires" type="date" bind:value={form.expires_at} disabled={busy} />
      </div>

      {#if msg}
        <div class="inline-msg {msg.tone === 'ok' ? 'ok' : 'err'}">
          <Icon name={msg.tone === 'ok' ? 'check' : 'alert'} size={12} />
          <span>{msg.text}</span>
        </div>
      {/if}
      <div class="actions">
        <Button variant="ghost" onclick={onCancel} disabled={busy}>
          {#snippet children()}Cancel{/snippet}
        </Button>
        <Button variant="primary" type="submit" disabled={busy}>
          {#snippet children()}{#if busy}<span class="spinner"></span>{/if}
          <span>{busy ? 'Saving…' : (mode === 'rotate' ? 'Save rotation' : 'Save entry')}</span>{/snippet}
        </Button>
      </div>
    </form>
  {/snippet}
</Card>

<style>
  .form { display: flex; flex-direction: column; gap: var(--s-3); }
  .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-3); }
  @media (max-width: 560px) { .grid-2 { grid-template-columns: 1fr; } }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field label { font-size: 11px; color: var(--fg-muted); letter-spacing: 0.02em; }
  .field .opt { color: var(--fg-faint); margin-left: 4px; font-weight: 400; }
  .field input, .field select, .readonly {
    padding: 9px 12px;
    background: color-mix(in srgb, var(--bg-raised) 50%, transparent);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    outline: none;
  }
  .field input:focus, .field select:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-ring); }
  .readonly { color: var(--fg-muted); user-select: all; }
  .field .hint { font-size: 11px; color: var(--fg-faint); }

  .section-head {
    display: flex; align-items: center; gap: var(--s-2);
    font-size: 11px; font-weight: 600;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    margin-top: var(--s-2);
  }
  .section-head .hint { font-weight: 400; font-size: 10px; color: var(--fg-faint); text-transform: none; letter-spacing: normal; }
  .section-head .toggle {
    margin-left: auto;
    display: inline-flex; align-items: center; gap: 4px;
    font-size: 11px; color: var(--fg-muted);
    cursor: pointer; user-select: none;
    text-transform: none; letter-spacing: normal; font-weight: 400;
  }
  .section-head .toggle input { margin: 0; cursor: pointer; }

  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid color-mix(in srgb, var(--fg-primary) 20%, transparent);
    border-top-color: currentColor;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .inline-msg {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .inline-msg.ok  { color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); border: 1px solid color-mix(in srgb, var(--accent) 25%, transparent); }
  .inline-msg.err { color: var(--danger); background: color-mix(in srgb, var(--danger) 8%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent); }
</style>
