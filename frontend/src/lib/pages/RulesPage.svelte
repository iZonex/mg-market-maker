<script>
  /*
   * Rules — operator-authored rule surfaces.
   *
   * Webhook URLs, alert rules, loan agreements, sentiment
   * headline overrides. Lives under Configure because it's
   * authoring runtime policy, not system-level admin.
   */
  import Card from '../components/Card.svelte'
  import AdminConfigPanels from '../components/AdminConfigPanels.svelte'

  let { auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card
      title="Runtime rules"
      subtitle="webhooks · alerts · loans · sentiment headline"
      span={3}
    >
      {#snippet children()}<AdminConfigPanels {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
