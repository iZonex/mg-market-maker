<script>
  /*
   * Credential checkbox grid with tenant-isolation tone.
   *
   * A credential is "tenant-bad" when its client_id is set but
   * doesn't match the agent's client_id, OR when the agent is
   * untagged and the credential is tagged. We paint those rows
   * red but still allow ticking them — the parent reads the
   * full selection and blocks submission on tenant mismatch.
   */
  import Icon from '../Icon.svelte'

  let {
    creds = [],
    selected = new Set(),
    agentTenant = '',
    disabled = false,
    onToggle,
  } = $props()
</script>

{#if creds.length === 0}
  <div class="warn-banner">
    <Icon name="alert" size={12} />
    <span>No credentials authorised for this agent. Add one in <strong>Admin → Credentials</strong> (or widen <code>allowed_agents</code> on an existing one) before deploying.</span>
  </div>
{:else}
  <div class="tenant-line">
    {#if agentTenant}
      Agent tenant: <code class="mono">{agentTenant}</code> · credentials must match (or be shared).
    {:else}
      Agent is untagged — only shared credentials (no <code>client_id</code>) will pass the tenant gate.
    {/if}
  </div>
  <div class="cred-picker">
    {#each creds as c (c.id)}
      {@const tenantBad = (agentTenant && c.client_id && c.client_id !== agentTenant)
        || (!agentTenant && c.client_id)}
      <label
        class="cred-option"
        class:selected={selected.has(c.id)}
        class:tenant-bad={tenantBad}
        title={tenantBad ? `tenant '${c.client_id}' does not match agent tenant '${agentTenant || '(none)'}'` : ''}
      >
        <input
          type="checkbox"
          checked={selected.has(c.id)}
          {disabled}
          onchange={() => onToggle(c.id)}
        />
        <span class="c-id mono">{c.id}</span>
        <span class="c-meta">
          <span class="c-venue">{c.exchange} · {c.product}</span>
          {#if c.client_id}
            <span class="c-tenant" class:c-tenant-bad={tenantBad}>{c.client_id}</span>
          {:else}
            <span class="c-tenant c-tenant-shared">shared</span>
          {/if}
        </span>
      </label>
    {/each}
  </div>
{/if}

<style>
  .cred-picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: var(--s-2);
  }
  .cred-option {
    display: grid;
    grid-template-columns: 18px 1fr auto;
    gap: var(--s-2);
    align-items: center;
    padding: 8px 12px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    font-size: var(--fs-sm);
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .cred-option.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .cred-option.tenant-bad {
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
  }
  .cred-option.tenant-bad.selected {
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }
  .cred-option input { margin: 0; }
  .c-id { font-weight: 500; color: var(--fg-primary); }
  .c-meta { display: inline-flex; gap: 6px; align-items: center; }
  .c-venue { font-family: var(--font-mono); font-size: 10px; color: var(--fg-muted); text-transform: uppercase; }
  .c-tenant {
    font-family: var(--font-mono); font-size: 9px;
    padding: 1px 6px; border-radius: var(--r-sm);
    background: var(--bg-base); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .c-tenant.c-tenant-shared { color: var(--fg-muted); }
  .c-tenant.c-tenant-bad {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }
  .tenant-line {
    padding: 4px 8px;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
  }
  .tenant-line code {
    font-family: var(--font-mono); background: var(--bg-chip);
    padding: 0 4px; border-radius: 3px; color: var(--fg-primary);
  }

  .warn-banner {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 25%, transparent);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .warn-banner code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }
</style>
