<script>
  import Card from '../components/Card.svelte'
  import UserManagementPanel from '../components/UserManagementPanel.svelte'
  let { auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Users" subtitle="API keys + roles" span={3}>
      {#snippet children()}<UserManagementPanel {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
