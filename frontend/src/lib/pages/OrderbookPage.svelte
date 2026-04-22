<script>
  import Card from '../components/Card.svelte'
  import OrderBook from '../components/OrderBook.svelte'
  import OpenOrders from '../components/OpenOrders.svelte'
  import InventoryPanel from '../components/InventoryPanel.svelte'
  import VenueInventoryPanel from '../components/VenueInventoryPanel.svelte'
  let { ws, auth } = $props()
</script>

<div class="page scroll">
  {#if !ws.state.connected}
    <div class="stale-banner">
      WebSocket disconnected — reconnecting. Orderbook, inventory and open-order panels will populate once the stream resumes.
    </div>
  {/if}
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
  .stale-banner {
    padding: var(--s-2) var(--s-3);
    margin-bottom: var(--s-3);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
</style>
