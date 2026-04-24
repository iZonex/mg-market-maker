<script>
  /*
   * Per-agent card — head + profile line + runtime grid + cred
   * chips + deployments table + optional edit form + optional
   * denial banner.
   *
   * Parent owns every side effect (accept/revoke/edit save,
   * deploy launch, deployment drilldown/retire, batch selection).
   * This component owns only local edit-form state.
   */
  import { Button } from '../../primitives/index.js'
  import { fmtDuration as formatAge } from '../../format.js'

  let {
    row,
    credentials = [],
    nowMs = Date.now(),
    busy = false,
    batchSelected = false,
    onToggleBatch,
    onDeploy,
    onAccept,
    onRevoke,
    onEditSave,
    onRetireDeployment,
    onOpenDrilldown,
  } = $props()

  let editing = $state(false)
  let form = $state({
    description: '',
    client_id: '',
    region: '',
    environment: '',
    purpose: '',
    owner_contact: '',
    notes: '',
    labels: '',
  })

  function startEdit() {
    const p = row.profile || {}
    form = {
      description: p.description || '',
      client_id: p.client_id || '',
      region: p.region || '',
      environment: p.environment || '',
      purpose: p.purpose || '',
      owner_contact: p.owner_contact || '',
      notes: p.notes || '',
      labels: Object.entries(p.labels || {}).map(([k, v]) => `${k}=${v}`).join(', '),
    }
    editing = true
  }

  async function saveEdit() {
    const labels = {}
    for (const pair of (form.labels || '').split(',').map((s) => s.trim()).filter(Boolean)) {
      const eq = pair.indexOf('=')
      if (eq > 0) labels[pair.slice(0, eq).trim()] = pair.slice(eq + 1).trim()
    }
    await onEditSave({
      description: form.description,
      client_id: form.client_id,
      region: form.region,
      environment: form.environment,
      purpose: form.purpose,
      owner_contact: form.owner_contact,
      notes: form.notes,
      labels,
    })
    editing = false
  }

  function cancelEdit() { editing = false }

  function stateTone(s) {
    switch (s) {
      case 'accepted': return 'ok'
      case 'pending': return 'warn'
      case 'rejected':
      case 'revoked':
      case 'unsigned': return 'danger'
      default: return 'muted'
    }
  }

  function fmtDecimal(s, digits = 4) {
    if (s === null || s === undefined || s === '') return '—'
    const n = Number(s)
    if (!Number.isFinite(n)) return s
    return n.toFixed(digits)
  }

  function leaseTtlMs(lease) {
    if (!lease?.expires_at) return null
    return new Date(lease.expires_at).getTime() - nowMs
  }

  const live = $derived(row.live)
  const ttl = $derived(live ? leaseTtlMs(live.current_lease) : null)
  const lastSeen = $derived(row.last_seen_ms ? nowMs - row.last_seen_ms : null)
  const liveOrders = $derived(
    (live?.deployments || []).reduce((n, d) => n + (d.live_orders || 0), 0),
  )
  const STALE_MS = 30_000
  const freshDeps = $derived(
    (live?.deployments || []).filter(
      (d) => d.sampled_at_ms && nowMs - d.sampled_at_ms < STALE_MS,
    ),
  )
  const staleCount = $derived((live?.deployments || []).length - freshDeps.length)
  const totalPnl = $derived(
    freshDeps.reduce((s, d) => s + Number(d.unrealized_pnl_quote || 0), 0),
  )
  const killedCount = $derived(
    (live?.deployments || []).filter((d) => (d.kill_level || 0) > 0).length,
  )
</script>

<div class="agent-card">
  <div class="head">
    <div class="head-left">
      {#if row.state === 'accepted' && row.connected}
        <label class="batch-check" title="Include in batch deploy">
          <input type="checkbox" checked={batchSelected} onchange={() => onToggleBatch(row.fingerprint)} />
        </label>
      {/if}
      <span class="fp mono">{row.fingerprint}</span>
      <span class="chip tone-{stateTone(row.state)}">{(row.state || 'unknown').toUpperCase()}</span>
      {#if row.connected}<span class="chip tone-ok">CONNECTED</span>
      {:else}<span class="chip tone-muted">OFFLINE</span>{/if}
    </div>
    <div class="head-right">
      {#if row.state === 'accepted' && row.connected}
        <Button variant="ok" size="sm" onclick={() => onDeploy(row)}>
          {#snippet children()}Deploy strategy{/snippet}
        </Button>
      {/if}
      {#if row.state === 'accepted'}
        <Button variant="danger" size="sm" disabled={busy} onclick={() => onRevoke(row.fingerprint)}>
          {#snippet children()}Revoke{/snippet}
        </Button>
      {:else if row.state === 'rejected' || row.state === 'revoked' || row.state === 'pending'}
        <Button variant="ok" size="sm" disabled={busy} onclick={() => onAccept(row.fingerprint)}>
          {#snippet children()}Accept{/snippet}
        </Button>
      {/if}
      {#if !editing}
        <Button variant="ghost" size="sm" onclick={startEdit}>
          {#snippet children()}Edit{/snippet}
        </Button>
      {/if}
    </div>
  </div>

  <div class="profile-line">
    {#if row.profile?.description}<span class="desc">{row.profile.description}</span>{/if}
    <span class="kv"><span class="k">id</span><span class="v mono">{row.agent_id}</span></span>
    {#if row.profile?.environment}<span class="env-chip env-{row.profile.environment}">{row.profile.environment}</span>{/if}
    {#if row.profile?.client_id}<span class="kv"><span class="k">client</span><span class="v mono">{row.profile.client_id}</span></span>{/if}
    {#if row.profile?.region}<span class="kv"><span class="k">region</span><span class="v mono">{row.profile.region}</span></span>{/if}
    {#if row.profile?.purpose}<span class="kv"><span class="k">purpose</span><span class="v">{row.profile.purpose}</span></span>{/if}
    {#if row.profile?.owner_contact}<span class="kv"><span class="k">on-call</span><span class="v mono">{row.profile.owner_contact}</span></span>{/if}
    {#each Object.entries(row.profile?.labels || {}) as [k, v] (k)}
      <span class="label-chip">{k}={v}</span>
    {/each}
  </div>

  {#if row.profile?.notes}
    <div class="notes-line">{row.profile.notes}</div>
  {/if}

  {#if editing}
    <div class="edit-form">
      <label>Description<input type="text" bind:value={form.description} placeholder="Frankfurt HFT box #2" /></label>
      <label>Client<input type="text" bind:value={form.client_id} placeholder="alice" /></label>
      <label>Region<input type="text" bind:value={form.region} placeholder="eu-fra" /></label>
      <label>Environment
        <select bind:value={form.environment}>
          <option value="">—</option>
          <option value="production">production</option>
          <option value="staging">staging</option>
          <option value="dev">dev</option>
          <option value="smoke">smoke</option>
        </select>
      </label>
      <label>Purpose<input type="text" bind:value={form.purpose} placeholder="primary BTC/ETH market-maker" /></label>
      <label>On-call contact<input type="text" bind:value={form.owner_contact} placeholder="oncall@team or @slack-handle" /></label>
      <label>Labels<input type="text" bind:value={form.labels} placeholder="env=prod, role=hft" /></label>
      <label class="notes-field">Notes<textarea rows="3" bind:value={form.notes} placeholder="Deployment gotchas, runbook links, known quirks"></textarea></label>
      <div class="edit-actions">
        <Button variant="ok" size="sm" disabled={busy} onclick={saveEdit}>
          {#snippet children()}Save{/snippet}
        </Button>
        <Button variant="ghost" size="sm" onclick={cancelEdit}>
          {#snippet children()}Cancel{/snippet}
        </Button>
      </div>
    </div>
  {/if}

  {#if credentials.length}
    <div class="cred-list">
      <span class="cred-list-k">Available credentials</span>
      <div class="cred-chips">
        {#each credentials as c (c.id)}
          <span class="cred-chip" title={`${c.exchange}/${c.product}`}>
            <span class="cred-id">{c.id}</span>
            <span class="cred-venue">{c.exchange}/{c.product}</span>
          </span>
        {/each}
      </div>
    </div>
  {:else if row.state === 'accepted'}
    <div class="cred-empty">
      No credentials authorised for this agent — add one in Credentials, or
      widen an existing credential's <code>allowed_agents</code>.
    </div>
  {/if}

  <div class="runtime">
    <div class="kv-col">
      <span class="k">lease TTL</span>
      <span class="v mono">{ttl !== null ? formatAge(ttl) : '—'}</span>
    </div>
    <div class="kv-col">
      <span class="k">last seen</span>
      <span class="v mono" class:stale-txt={lastSeen !== null && lastSeen > 10_000}>
        {lastSeen !== null ? `${formatAge(lastSeen)} ago` : '—'}
      </span>
    </div>
    <div class="kv-col">
      <span class="k">protocol</span>
      <span class="v mono">{live ? `v${live.protocol_version}` : '—'}</span>
    </div>
    <div class="kv-col">
      <span class="k">agent ver</span>
      <span class="v mono">{live?.agent_version || '—'}</span>
    </div>
    <div class="kv-col">
      <span class="k">applied seq</span>
      <span class="v mono">{live ? `#${live.last_applied_seq}` : '—'}</span>
    </div>
    <div class="kv-col">
      <span class="k">deployments</span>
      <span class="v mono">
        {(live?.deployments || []).filter((d) => d.running).length}
        <span class="faint">/ {live?.deployments?.length ?? 0}</span>
      </span>
    </div>
    <div class="kv-col">
      <span class="k">live orders</span>
      <span class="v mono">{liveOrders}</span>
    </div>
    <div class="kv-col">
      <span class="k">
        unrealised PnL
        {#if staleCount > 0}
          <span class="stale-chip" title={`${staleCount} deployment(s) last sampled > 30s ago — excluded from this sum`}>
            {staleCount} stale
          </span>
        {/if}
      </span>
      <span class="v mono" class:pos={totalPnl > 0} class:neg={totalPnl < 0}>
        {totalPnl !== 0 ? totalPnl.toFixed(2) : '—'}
      </span>
    </div>
    {#if killedCount > 0}
      <div class="kv-col">
        <span class="k">kill escalated</span>
        <span class="v mono neg">{killedCount}</span>
      </div>
    {/if}
  </div>

  {#if live?.deployments?.length}
    <table class="dep-table">
      <thead>
        <tr>
          <th>deployment</th>
          <th>symbol</th>
          <th>template</th>
          <th>graph</th>
          <th>running</th>
          <th class="num">inventory</th>
          <th class="num">unrealised PnL</th>
          <th></th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each live.deployments as d (d.deployment_id)}
          <tr
            class="dep-row"
            onclick={() => onOpenDrilldown(live, d)}
            title="Open drilldown"
          >
            <td class="mono">{d.deployment_id}</td>
            <td class="mono">{d.symbol}</td>
            <td class="mono">{d.template || '—'}</td>
            <td class="mono">
              {#if d.active_graph?.hash}
                <span title={`${d.active_graph.name || ''} · ${d.active_graph.hash} · deployed ${new Date(d.active_graph.deployed_at_ms).toLocaleString()}`}>
                  {d.active_graph.hash.slice(0, 8)}
                </span>
              {:else}—{/if}
            </td>
            <td>
              <span class="chip tone-{d.running ? 'ok' : 'muted'}">
                {d.running ? 'RUN' : 'STOP'}
              </span>
            </td>
            <td class="num mono">{fmtDecimal(d.inventory, 6)}</td>
            <td class="num mono">{fmtDecimal(d.unrealized_pnl_quote, 4)}</td>
            <td class="dep-actions">
              <Button variant="ghost" size="xs" title="Retire deployment (remove from desired set)" onclick={(e) => { e.stopPropagation(); onRetireDeployment(live.agent_id, d.deployment_id, d.symbol) }}>
                {#snippet children()}Retire{/snippet}
              </Button>
            </td>
            <td class="chev">›</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if row.state === 'rejected' || row.state === 'revoked'}
    <div class="denial">
      <span class="k">{row.state === 'rejected' ? 'rejected' : 'revoked'}</span>
      {#if row.revoked_by} by <span class="mono">{row.revoked_by}</span>{/if}
      {#if row.revoke_reason} — "{row.revoke_reason}"{/if}
    </div>
  {/if}
</div>

<style>
  .agent-card {
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    padding: var(--s-3);
    display: flex; flex-direction: column; gap: var(--s-2);
  }
  .head { display: flex; justify-content: space-between; align-items: center; gap: var(--s-2); }
  .head-left { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .head-right { display: flex; gap: var(--s-1); }
  .fp { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-sm); }
  .batch-check { display: inline-flex; align-items: center; margin-right: 6px; }
  .batch-check input { cursor: pointer; }

  .profile-line {
    display: flex; align-items: baseline; gap: var(--s-3);
    flex-wrap: wrap; font-size: var(--fs-xs);
  }
  .desc { color: var(--fg-primary); font-weight: 500; }
  .label-chip {
    font-family: var(--font-mono); font-size: 10px;
    background: var(--bg-raised); color: var(--fg-secondary);
    padding: 2px 6px; border-radius: var(--r-sm);
  }
  .env-chip {
    font-family: var(--font-mono); font-size: 10px; font-weight: 600;
    padding: 2px 6px; border-radius: var(--r-sm);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    background: var(--bg-chip); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle);
  }
  .env-chip.env-production { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); border-color: color-mix(in srgb, var(--danger) 30%, transparent); }
  .env-chip.env-staging { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); border-color: color-mix(in srgb, var(--warn) 30%, transparent); }
  .env-chip.env-dev { background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); border-color: color-mix(in srgb, var(--accent) 30%, transparent); }
  .env-chip.env-smoke { background: var(--bg-raised); color: var(--fg-muted); }

  .notes-line {
    font-size: var(--fs-xs); color: var(--fg-secondary);
    padding: var(--s-2); background: var(--bg-raised); border-radius: var(--r-sm);
    white-space: pre-wrap;
  }

  .edit-form {
    display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2);
    padding: var(--s-2); background: var(--bg-raised); border-radius: var(--r-sm);
  }
  .edit-form label { display: flex; flex-direction: column; gap: 2px; font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .edit-form label.notes-field { grid-column: 1 / -1; }
  .edit-form input,
  .edit-form select,
  .edit-form textarea {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .edit-form textarea { font-family: inherit; resize: vertical; }
  .edit-actions { grid-column: 1 / -1; display: flex; gap: var(--s-2); justify-content: flex-end; }

  .runtime {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: var(--s-2);
    padding: var(--s-2);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .kv-col { display: flex; flex-direction: column; gap: 2px; }
  .k { color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .v { color: var(--fg-primary); }
  .v.stale-txt { color: var(--warn); }
  .v.pos { color: var(--pos); }
  .v.neg { color: var(--neg); }
  .v .faint { color: var(--fg-faint); }
  .v.mono.neg { color: var(--neg); }
  .stale-chip {
    display: inline-block; margin-left: 4px;
    padding: 1px 4px; font-size: 9px;
    background: color-mix(in srgb, var(--warn) 18%, transparent);
    color: var(--warn); border-radius: var(--r-sm);
  }

  .cred-list {
    display: flex; flex-direction: column; gap: 6px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .cred-list-k {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
  }
  .cred-chips { display: flex; gap: var(--s-2); flex-wrap: wrap; }
  .cred-chip {
    display: inline-flex; flex-direction: column; gap: 1px;
    padding: 4px 8px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-size: 10px;
  }
  .cred-id {
    font-family: var(--font-mono);
    color: var(--fg-primary);
    font-weight: 500;
  }
  .cred-venue {
    font-family: var(--font-mono);
    color: var(--fg-muted);
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .cred-empty {
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--warn) 6%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 20%, transparent);
    border-radius: var(--r-sm);
    color: var(--fg-muted);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .cred-empty code {
    font-family: var(--font-mono);
    background: var(--bg-chip);
    padding: 0 4px;
    border-radius: 3px;
    color: var(--fg-primary);
  }

  .dep-table {
    width: 100%; border-collapse: collapse; font-size: var(--fs-xs);
  }
  .dep-table th, .dep-table td {
    padding: var(--s-2); text-align: left; border-bottom: 1px solid var(--border-subtle);
  }
  .dep-table th {
    color: var(--fg-muted); text-transform: uppercase;
    letter-spacing: var(--tracking-label); font-size: 10px; font-weight: 600;
  }
  .num { text-align: right; }
  .dep-row { cursor: pointer; transition: background var(--dur-fast) var(--ease-out); }
  .dep-row:hover { background: var(--bg-raised); }
  .chev { color: var(--fg-muted); text-align: right; font-size: var(--fs-md); width: 16px; }
  .dep-row:hover .chev { color: var(--accent); }
  .dep-actions { text-align: right; width: 1%; white-space: nowrap; }
  .denial {
    font-size: var(--fs-xs); color: var(--fg-muted);
    border-top: 1px solid var(--border-subtle); padding-top: var(--s-2);
  }
</style>
