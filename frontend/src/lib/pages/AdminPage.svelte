<script>
  import Card from '../components/Card.svelte'
  import VenuesHealth from '../components/VenuesHealth.svelte'
  import AdminConfigPanels from '../components/AdminConfigPanels.svelte'
  import ClientOnboardingPanel from '../components/ClientOnboardingPanel.svelte'
  import SorDecisions from '../components/SorDecisions.svelte'
  import AtomicBundles from '../components/AtomicBundles.svelte'
  import RebalanceRecommendations from '../components/RebalanceRecommendations.svelte'
  import FundingArbPairs from '../components/FundingArbPairs.svelte'
  import AdverseSelection from '../components/AdverseSelection.svelte'
  import CalibrationStatus from '../components/CalibrationStatus.svelte'
  import ManipulationScores from '../components/ManipulationScores.svelte'
  import OnchainScores from '../components/OnchainScores.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { ws, auth, onNavigate = () => {} } = $props()
  const api = createApiClient(auth)

  // Wave F4 — kill-switch panel loads the live fleet and shows
  // every deployment whose kill_level > 0. One-click "Open
  // drilldown" takes the operator straight to Fleet with the
  // right row opened — no hunting through a wall of agents.
  let escalated = $state([])
  let lastKillFetch = $state(null)

  async function refreshEscalated() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const rows = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if ((d.kill_level || 0) > 0) {
            rows.push({ agent_id: a.agent_id, deployment: d })
          }
        }
      }
      escalated = rows
      lastKillFetch = new Date()
    } catch { /* ignore — re-try next tick */ }
  }

  $effect(() => {
    refreshEscalated()
    const iv = setInterval(refreshEscalated, 5000)
    return () => clearInterval(iv)
  })
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Kill switch" subtitle={escalated.length > 0 ? `${escalated.length} deployment(s) escalated` : 'no escalations'} span={1}>
      {#snippet children()}
        {#if escalated.length === 0}
          <div class="muted">
            No deployments are currently kill-escalated. Controls live
            on each deployment's drilldown:
            <button type="button" class="linklike" onclick={() => onNavigate('fleet')}>
              Open Fleet →
            </button>
          </div>
        {:else}
          <div class="esc-list">
            {#each escalated as r (r.agent_id + '/' + r.deployment.deployment_id)}
              <button
                type="button"
                class="esc-row"
                onclick={() => onNavigate('fleet')}
                title="Opens Fleet — click the deployment to drill down"
              >
                <span class="esc-kill">L{r.deployment.kill_level}</span>
                <span class="esc-sym mono">{r.deployment.symbol}</span>
                <span class="esc-agent mono">{r.agent_id}</span>
                <span class="esc-chev">›</span>
              </button>
            {/each}
          </div>
          <div class="muted">
            Click any row to open the Fleet page. The deployment
            drilldown's Ops section carries reset + widen /
            cancel-all / flatten controls.
          </div>
        {/if}
      {/snippet}
    </Card>
    <Card title="Quick links" span={2}>
      {#snippet children()}
        <div class="links">
          <a href="/metrics" target="_blank" rel="noopener">Prometheus /metrics</a>
          <a href="/api/v1/system/preflight" target="_blank" rel="noopener">Preflight JSON</a>
          <a href="/api/v1/system/diagnostics" target="_blank" rel="noopener">Diagnostics</a>
        </div>
      {/snippet}
    </Card>
    <Card title="Venues health" subtitle="per-venue aggregates" span={3}>
      {#snippet children()}<VenuesHealth {auth} />{/snippet}
    </Card>
    <Card title="SOR routing" subtitle="winner + runner-up per decision" span={3}>
      {#snippet children()}<SorDecisions {auth} />{/snippet}
    </Card>
    <Card title="Atomic bundles" subtitle="inflight maker / hedge pairs" span={3}>
      {#snippet children()}<AtomicBundles {auth} />{/snippet}
    </Card>
    <Card title="Rebalance advisories" subtitle="cross-venue transfer hints" span={3}>
      {#snippet children()}<RebalanceRecommendations {auth} />{/snippet}
    </Card>
    <Card title="Funding-arb pairs" subtitle="per-pair driver events" span={3}>
      {#snippet children()}<FundingArbPairs {auth} />{/snippet}
    </Card>
    <Card title="Adverse-selection" subtitle="Cartea ρ per side + toxicity bps" span={3}>
      {#snippet children()}<AdverseSelection {auth} />{/snippet}
    </Card>
    <Card title="Live calibration" subtitle="GLFT a / k retune status" span={3}>
      {#snippet children()}<CalibrationStatus {auth} />{/snippet}
    </Card>
    <Card title="Manipulation detector" subtitle="CEX-side pump / wash / thin-book" span={3}>
      {#snippet children()}<ManipulationScores {auth} />{/snippet}
    </Card>
    <Card title="On-chain surveillance" subtitle="holder concentration + CEX inflow" span={3}>
      {#snippet children()}<OnchainScores {auth} />{/snippet}
    </Card>
    <Card title="Client onboarding" subtitle="register + jurisdiction gate" span={3}>
      {#snippet children()}<ClientOnboardingPanel {auth} />{/snippet}
    </Card>
    <Card title="Config surfaces" subtitle="webhooks · alerts · loans · sentiment" span={3}>
      {#snippet children()}<AdminConfigPanels {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
  .muted { color: var(--fg-muted); font-size: var(--fs-xs); line-height: 1.55; }
  .links { display: flex; flex-direction: column; gap: var(--s-2); }
  .links a {
    padding: var(--s-2) var(--s-3);
    color: var(--fg-secondary);
    text-decoration: none;
    border-radius: var(--r-md);
    font-size: var(--fs-sm);
    transition: background var(--dur-fast) var(--ease-out), color var(--dur-fast) var(--ease-out);
  }
  .links a:hover { background: var(--bg-chip); color: var(--accent); }
  .esc-list { display: flex; flex-direction: column; gap: 4px; margin-bottom: var(--s-2); }
  .esc-row {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
    border-radius: var(--r-sm);
    cursor: pointer; color: inherit; text-align: left; font-size: var(--fs-xs);
  }
  .esc-row:hover { background: color-mix(in srgb, var(--danger) 18%, transparent); }
  .esc-kill {
    padding: 2px 6px; border-radius: var(--r-sm);
    background: var(--danger); color: var(--bg-base);
    font-family: var(--font-mono); font-weight: 600; font-size: 10px;
  }
  .esc-sym { flex: 1; }
  .esc-agent { color: var(--fg-muted); font-size: 10px; }
  .esc-chev { font-size: var(--fs-md); color: var(--fg-muted); }
  .linklike {
    background: none; border: 0; padding: 0;
    color: var(--accent); cursor: pointer; font-size: inherit;
    font-family: inherit;
  }
  .linklike:hover { text-decoration: underline; }
</style>
