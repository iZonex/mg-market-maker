<script>
  import Card from '../components/Card.svelte'
  import OrderBook from '../components/OrderBook.svelte'
  import OpenOrders from '../components/OpenOrders.svelte'
  import InventoryPanel from '../components/InventoryPanel.svelte'
  import VenueInventoryPanel from '../components/VenueInventoryPanel.svelte'
  let { ws, auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Orderbook" subtitle="live L2" span={2}>
      {#snippet children()}<OrderBook data={ws} />{/snippet}
    </Card>
    <Card title="Inventory" span={1}>
      {#snippet children()}<InventoryPanel data={ws} />{/snippet}
    </Card>
    <Card title="Open orders" subtitle="currently quoting" span={2}>
      {#snippet children()}<OpenOrders data={ws} />{/snippet}
    </Card>
    <Card title="Per-venue inventory" span={1}>
      {#snippet children()}<VenueInventoryPanel data={ws} {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
