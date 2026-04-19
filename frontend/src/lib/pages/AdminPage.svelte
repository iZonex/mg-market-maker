<script>
  import Card from '../components/Card.svelte'
  import Controls from '../components/Controls.svelte'
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
  let { ws, auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Kill switch" subtitle="destructive" variant="glass" span={2}>
      {#snippet children()}<Controls data={ws} {auth} />{/snippet}
    </Card>
    <Card title="Quick links" span={1}>
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
</style>
