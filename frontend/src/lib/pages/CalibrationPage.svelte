<script>
  /*
   * Calibration page — hyperopt review loop.
   *
   * AdaptivePanel + ParamTuner moved to Settings (where the
   * "what's configured / numeric knobs" story lives). What
   * remains here is the hyperopt-specific surface operators
   * actually use day-to-day: pending calibration approvals +
   * live microstructure signals that trigger re-runs.
   */
  import Card from '../components/Card.svelte'
  import PendingCalibrationCard from '../components/PendingCalibrationCard.svelte'
  import SignalsPanel from '../components/SignalsPanel.svelte'
  let { ws, auth } = $props()
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Pending calibrations" subtitle="hyperopt review" span={1}>
      {#snippet children()}<PendingCalibrationCard data={ws} {auth} />{/snippet}
    </Card>
    <Card title="Signals" subtitle="microstructure" span={2}>
      {#snippet children()}<SignalsPanel data={ws} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
</style>
