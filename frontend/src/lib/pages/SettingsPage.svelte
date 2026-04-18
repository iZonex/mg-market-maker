<script>
  /*
   * Settings page (UX-3 restructure).
   *
   * Centralises every configurable knob the operator can
   * reach from the dashboard. Three sections:
   *
   * 1. `FeatureStatusPanel` — runtime feature toggles
   *    (momentum alpha, OTR snapshots, amend-in-place,
   *    market-resilience widening) + read-only status for
   *    config-only stage-2 features with TOML breadcrumbs.
   * 2. `ParamTuner` — numeric hot-reload knobs (γ, κ,
   *    spread floor, order size, levels, inventory cap,
   *    amend tick budget).
   * 3. `AdaptivePanel` — the adaptive controller's current
   *    γ-factor + pair-class tag + last-adjustment reason.
   */

  import Card from '../components/Card.svelte'
  import FeatureStatusPanel from '../components/FeatureStatusPanel.svelte'
  import ParamTuner from '../components/ParamTuner.svelte'
  import AdaptivePanel from '../components/AdaptivePanel.svelte'
  import ConfigViewer from '../components/ConfigViewer.svelte'

  let { ws, auth } = $props()
  const canControl = $derived(auth?.canControl?.() ?? false)
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Features" subtitle="runtime toggles + config-only status" span={2}>
      {#snippet children()}<FeatureStatusPanel data={ws} {auth} />{/snippet}
    </Card>
    <Card title="Adaptive state" subtitle="γ / PairClass" span={1}>
      {#snippet children()}<AdaptivePanel data={ws} />{/snippet}
    </Card>
    {#if canControl}
      <Card title="Live tuning" subtitle="numeric hot-reload" span={3}>
        {#snippet children()}<ParamTuner data={ws} {auth} />{/snippet}
      </Card>
    {/if}
    <Card title="Config snapshot" subtitle="effective AppConfig (read-only)" span={3}>
      {#snippet children()}<ConfigViewer {auth} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
