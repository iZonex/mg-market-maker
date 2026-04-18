<script>
  import Card from '../components/Card.svelte'
  import OpenOrders from '../components/OpenOrders.svelte'
  import FillHistory from '../components/FillHistory.svelte'
  import AuditStream from '../components/AuditStream.svelte'

  let { ws, auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Open orders" subtitle="live" span={1}>
      {#snippet children()}<OpenOrders data={ws} />{/snippet}
    </Card>
    <Card title="Fill history" subtitle="latest 20" span={2}>
      {#snippet children()}<FillHistory data={ws} />{/snippet}
    </Card>
    <Card title="Audit stream" subtitle="MiCA trail" span={3}>
      {#snippet children()}<AuditStream {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--s-4);
  }
</style>
