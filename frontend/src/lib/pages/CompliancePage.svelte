<script>
  import Card from '../components/Card.svelte'
  import AuditStream from '../components/AuditStream.svelte'
  import ConnectivityPanel from '../components/ConnectivityPanel.svelte'
  import AlertLog from '../components/AlertLog.svelte'
  import ClientCircuitPanel from '../components/ClientCircuitPanel.svelte'
  import ReportsPanel from '../components/ReportsPanel.svelte'
  import SentimentPanel from '../components/SentimentPanel.svelte'
  import ViolationsPanel from '../components/ViolationsPanel.svelte'
  let { ws, auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Open violations" subtitle="fleet rollup · SLA · kill · recon · manipulation" span={3}>
      {#snippet children()}<ViolationsPanel {auth} />{/snippet}
    </Card>
    <Card title="Alerts" subtitle="live" span={2}>
      {#snippet children()}<AlertLog data={ws} {auth} />{/snippet}
    </Card>
    <Card title="Connectivity" subtitle="venues" span={1}>
      {#snippet children()}<ConnectivityPanel {auth} />{/snippet}
    </Card>
    <Card title="Reports" subtitle="daily + archive" span={2}>
      {#snippet children()}<ReportsPanel {auth} />{/snippet}
    </Card>
    <Card title="Per-client circuit" subtitle="loss guard" span={1}>
      {#snippet children()}<ClientCircuitPanel {auth} />{/snippet}
    </Card>
    <Card title="Social risk" subtitle="mentions · sentiment · kill" span={3}>
      {#snippet children()}<SentimentPanel {auth} />{/snippet}
    </Card>
    <Card title="Audit trail" subtitle="hash-chained events" span={3}>
      {#snippet children()}<AuditStream {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
