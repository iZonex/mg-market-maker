<script>
  /*
   * Venues & Execution — operational observability surface.
   *
   * Aggregates the cross-venue / routing / calibration panels
   * that used to live on AdminPage. Watching-oriented, but
   * operator-facing: this is where you check that routing and
   * execution quality are healthy before and during live work.
   */
  import Card from '../components/Card.svelte'
  import VenuesHealth from '../components/VenuesHealth.svelte'
  import SorDecisions from '../components/SorDecisions.svelte'
  import AtomicBundles from '../components/AtomicBundles.svelte'
  import RebalanceRecommendations from '../components/RebalanceRecommendations.svelte'
  import FundingArbPairs from '../components/FundingArbPairs.svelte'
  import AdverseSelection from '../components/AdverseSelection.svelte'
  import OnchainScores from '../components/OnchainScores.svelte'

  let { auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Venues health" subtitle="per-venue aggregates · SLA · book latency · margin" span={3}>
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
    <Card title="Adverse selection" subtitle="Cartea ρ per side + toxicity bps" span={3}>
      {#snippet children()}<AdverseSelection {auth} />{/snippet}
    </Card>
    <Card title="On-chain surveillance" subtitle="holder concentration + CEX inflow" span={3}>
      {#snippet children()}<OnchainScores {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
