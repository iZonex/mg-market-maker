<script>
  /*
   * Settings — landing + fleet-wide live summary.
   *
   * Post distributed-control-plane refactor, configuration is
   * split across several dedicated pages by concern:
   *
   *   Platform  — controller-level runtime tunables (lease TTL,
   *               version pinning, deploy defaults) · Admin
   *   Vault     — encrypted secret store (API keys, tokens,
   *               DSNs, webhook URLs, …) · Admin
   *   Fleet     — per-agent profile: description, client,
   *               region, labels, authorised credentials
   *   Deploy    — strategy-level template + variables + credential
   *               pick (launched from a Fleet agent row)
   *
   * The per-strategy tuning panels (FeatureStatusPanel,
   * ParamTuner, ConfigViewer, AdaptivePanel) reshaped onto the
   * new architecture in Wave 2: they now live inside
   * `DeploymentDrilldown`, opened from the Fleet deployment
   * table. SettingsPage shows a fleet-wide roll-up of those
   * deployments so the operator has one place to see the state
   * of the whole estate and jump into the relevant drilldown.
   */
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {} } = $props()
  const api = createApiClient(auth)
  const role = $derived(auth?.state?.role || 'viewer')
  const isAdmin = $derived(role === 'admin')

  const REFRESH_MS = 3_000

  let fleet = $state([])
  let fleetErr = $state(null)
  let fleetLoading = $state(true)

  async function refreshFleet() {
    try {
      const data = await api.getJson('/api/v1/fleet')
      fleet = Array.isArray(data) ? data : []
      fleetErr = null
    } catch (e) {
      fleetErr = e?.message || String(e)
    } finally {
      fleetLoading = false
    }
  }

  $effect(() => {
    refreshFleet()
    const t = setInterval(refreshFleet, REFRESH_MS)
    return () => clearInterval(t)
  })

  // Flatten per-agent deployments into one fleet-wide list.
  // Each row carries the agent context so Fleet → drilldown can
  // re-hydrate without a separate lookup.
  const deployments = $derived.by(() => {
    const out = []
    for (const a of fleet) {
      if (!Array.isArray(a?.deployments)) continue
      for (const d of a.deployments) {
        out.push({ agent: a, deployment: d })
      }
    }
    out.sort((x, y) => {
      const ax = (x.agent.agent_id || '') + (x.deployment.deployment_id || '')
      const ay = (y.agent.agent_id || '') + (y.deployment.deployment_id || '')
      return ax.localeCompare(ay)
    })
    return out
  })

  // Fleet-wide rollups — counters the operator cares about at a
  // glance: how many are running, what regimes are live, how
  // many kill-switch escalations are out there, total live
  // orders in flight.
  const rollup = $derived.by(() => {
    const r = {
      total: deployments.length,
      running: 0,
      stopped: 0,
      live_orders: 0,
      kill_escalated: 0,
      regimes: new Map(),
      modes: new Map(),
      templates: new Map(),
    }
    for (const { deployment: d } of deployments) {
      if (d.running) r.running += 1; else r.stopped += 1
      r.live_orders += (d.live_orders || 0)
      if ((d.kill_level || 0) > 0) r.kill_escalated += 1
      if (d.regime) r.regimes.set(d.regime, (r.regimes.get(d.regime) || 0) + 1)
      if (d.mode) r.modes.set(d.mode, (r.modes.get(d.mode) || 0) + 1)
      if (d.template) r.templates.set(d.template, (r.templates.get(d.template) || 0) + 1)
    }
    return r
  })

  const links = [
    {
      id: 'platform',
      icon: 'settings',
      label: 'Platform',
      hint: 'Controller-level runtime tunables (lease TTL, version pinning, defaults).',
      admin: true,
    },
    {
      id: 'vault',
      icon: 'shield',
      label: 'Vault',
      hint: 'Encrypted secret store — exchange keys, Telegram / Sentry / webhook / SMTP / RPC credentials.',
      admin: true,
    },
    {
      id: 'fleet',
      icon: 'pulse',
      label: 'Fleet',
      hint: 'Connected agents, per-agent profile (description, client, region, labels), authorised credentials, and the Deploy strategy launcher.',
      admin: false,
    },
    {
      id: 'profile',
      icon: 'users',
      label: 'My profile',
      hint: 'Your account — change password, enable 2FA.',
      admin: false,
    },
  ]
  const visible = $derived(links.filter(l => !l.admin || isAdmin))

  function mapEntries(m) {
    return Array.from(m.entries()).sort((a, b) => b[1] - a[1])
  }
</script>

<div class="page scroll">
  <div class="container">
    <header class="page-header">
      <h1>Settings</h1>
      <p class="page-sub">
        Configuration moved into dedicated pages by concern after the distributed-control-plane
        refactor. Pick the area you want to edit — each surface is focused on one thing.
      </p>
    </header>

    <div class="tiles">
      {#each visible as l (l.id)}
        <button type="button" class="tile" onclick={() => onNavigate(l.id)}>
          <span class="tile-icon"><Icon name={l.icon} size={22} /></span>
          <span class="tile-body">
            <span class="tile-label">{l.label}</span>
            <span class="tile-hint">{l.hint}</span>
          </span>
          <Icon name="chevronR" size={14} />
        </button>
      {/each}
    </div>

    <section class="summary">
      <header class="summary-head">
        <h2>Live deployments — fleet-wide</h2>
        <p class="summary-hint">
          Roll-up of every running deployment across every connected agent.
          Click a row to jump to Fleet — the drilldown panel there hosts the
          per-strategy γ / spread / momentum tuners.
        </p>
      </header>

      {#if fleetErr}
        <div class="err">error loading fleet: {fleetErr}</div>
      {:else if fleetLoading}
        <div class="muted">Loading fleet…</div>
      {:else if deployments.length === 0}
        <div class="muted">
          No running deployments right now. Head to <strong>Fleet</strong> and
          use <em>Deploy strategy</em> on an Accepted agent to start one.
        </div>
      {:else}
        <div class="rollup">
          <div class="stat">
            <span class="stat-k">Total</span>
            <span class="stat-v mono">{rollup.total}</span>
          </div>
          <div class="stat">
            <span class="stat-k">Running</span>
            <span class="stat-v mono pos">{rollup.running}</span>
          </div>
          {#if rollup.stopped > 0}
            <div class="stat">
              <span class="stat-k">Stopped</span>
              <span class="stat-v mono muted">{rollup.stopped}</span>
            </div>
          {/if}
          {#if rollup.kill_escalated > 0}
            <div class="stat">
              <span class="stat-k">Kill L1+</span>
              <span class="stat-v mono neg">{rollup.kill_escalated}</span>
            </div>
          {/if}
          <div class="stat">
            <span class="stat-k">Live orders</span>
            <span class="stat-v mono">{rollup.live_orders}</span>
          </div>
        </div>

        {#if rollup.regimes.size > 0 || rollup.modes.size > 0 || rollup.templates.size > 0}
          <div class="chips-row">
            {#each mapEntries(rollup.modes) as [k, v] (`mode-${k}`)}
              <span class="chip mono">{k} × {v}</span>
            {/each}
            {#each mapEntries(rollup.regimes) as [k, v] (`regime-${k}`)}
              <span class="chip mono regime">{k} × {v}</span>
            {/each}
            {#each mapEntries(rollup.templates) as [k, v] (`tpl-${k}`)}
              <span class="chip mono tpl">{k} × {v}</span>
            {/each}
          </div>
        {/if}

        <div class="rows">
          {#each deployments as { agent, deployment } (`${agent.agent_id}/${deployment.deployment_id}`)}
            <button type="button" class="row" onclick={() => onNavigate('fleet')}>
              <span class="row-main">
                <span class="row-title mono">{deployment.template || 'deployment'} · {deployment.symbol}</span>
                <span class="row-sub mono">
                  {agent.agent_id}
                  {#if deployment.venue}· {deployment.venue}{/if}
                  {#if deployment.product}· {deployment.product}{/if}
                  · <span class="faint">{deployment.deployment_id}</span>
                </span>
              </span>
              <span class="row-meta">
                <span class="chip tone-{deployment.running ? 'ok' : 'muted'}">
                  {deployment.running ? 'RUN' : 'STOP'}
                </span>
                {#if (deployment.kill_level || 0) > 0}
                  <span class="chip tone-danger">KILL L{deployment.kill_level}</span>
                {/if}
                {#if deployment.regime}
                  <span class="chip mono regime">{deployment.regime}</span>
                {/if}
                <span class="row-stat mono">{deployment.live_orders || 0} orders</span>
                <Icon name="chevronR" size={14} />
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </section>

    <div class="callout">
      <Icon name="info" size={14} />
      <div>
        Per-strategy tuning (γ, spread bps, momentum α, …) lives inside each
        <strong>deployment drilldown</strong> — Fleet → click a deployment row.
        Global flags that aren't strategy-specific live in <strong>Platform</strong>.
      </div>
    </div>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .container { max-width: 720px; margin: 0 auto; display: flex; flex-direction: column; gap: var(--s-5); }
  .page-header h1 { margin: 0 0 6px; font-size: var(--fs-xl); font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .page-sub { margin: 0; color: var(--fg-muted); font-size: var(--fs-sm); line-height: 1.5; max-width: 560px; }

  .tiles { display: flex; flex-direction: column; gap: var(--s-2); }
  .tile {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: var(--s-4);
    align-items: center;
    padding: var(--s-4);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    text-align: left;
    color: inherit;
    font-family: var(--font-sans);
    transition: border-color var(--dur-fast) var(--ease-out),
                background var(--dur-fast) var(--ease-out);
  }
  .tile:hover {
    border-color: var(--accent);
    background: rgba(0, 209, 178, 0.05);
  }
  .tile-icon {
    width: 44px; height: 44px;
    display: flex; align-items: center; justify-content: center;
    background: var(--bg-raised);
    border-radius: 50%;
    color: var(--fg-secondary);
  }
  .tile-body { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .tile-label { font-size: var(--fs-md); font-weight: 600; color: var(--fg-primary); }
  .tile-hint { font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.5; }

  .callout {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-3);
    background: rgba(0, 209, 178, 0.06);
    border: 1px solid rgba(0, 209, 178, 0.18);
    border-radius: var(--r-md);
    color: var(--fg-secondary);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .callout strong { color: var(--fg-primary); }
  .callout em { color: var(--accent); font-style: normal; }

  .summary { display: flex; flex-direction: column; gap: var(--s-3); }
  .summary-head h2 {
    margin: 0 0 4px;
    font-size: var(--fs-md);
    font-weight: 600; color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .summary-hint {
    margin: 0; color: var(--fg-muted); font-size: var(--fs-xs); line-height: 1.5;
  }
  .summary-hint strong { color: var(--fg-primary); }
  .summary-hint em { color: var(--accent); font-style: normal; }

  .err {
    padding: var(--s-3);
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.25);
    border-radius: var(--r-md);
    color: var(--danger);
    font-size: var(--fs-xs);
  }
  .muted {
    padding: var(--s-3);
    color: var(--fg-muted);
    font-size: var(--fs-sm);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .muted strong { color: var(--fg-primary); }
  .muted em { color: var(--accent); font-style: normal; }

  .rollup {
    display: flex; flex-wrap: wrap; gap: var(--s-3);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .stat { display: flex; flex-direction: column; gap: 2px; min-width: 80px; }
  .stat-k {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .stat-v {
    font-size: var(--fs-md); color: var(--fg-primary);
    font-weight: 600;
  }
  .stat-v.pos { color: var(--pos); }
  .stat-v.neg { color: var(--neg); }
  .stat-v.muted { color: var(--fg-muted); }

  .chips-row {
    display: flex; flex-wrap: wrap; gap: var(--s-1);
  }
  .chip {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: var(--r-sm);
    border: 1px solid currentColor;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .chip.regime { color: var(--accent); }
  .chip.tpl { color: var(--fg-secondary); }
  .chip.tone-ok { color: var(--pos); }
  .chip.tone-muted { color: var(--fg-muted); }
  .chip.tone-danger { color: var(--neg); }

  .rows { display: flex; flex-direction: column; gap: var(--s-1); }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    text-align: left;
    color: inherit;
    font-family: var(--font-sans);
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .row:hover { border-color: var(--accent); }
  .row-main { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
  .row-title { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .row-sub { font-size: 10px; color: var(--fg-muted); }
  .row-sub .faint { color: var(--fg-faint); }
  .row-meta { display: flex; align-items: center; gap: var(--s-2); }
  .row-stat { font-size: 10px; color: var(--fg-muted); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
