<script>
  /*
   * Rx-freshness pill — tells the operator whether the websocket
   * is alive and how fresh the last frame was. Four states:
   *   - disconnected: socket down
   *   - waiting: socket up but no frame yet
   *   - fresh (<= 2s): pulsing green
   *   - stale (2-5s): amber
   *   - frozen (> 5s): red, pulsing fast
   */
  let { rxMs = null, connected = false } = $props()

  let now = $state(Date.now())
  $effect(() => {
    const id = setInterval(() => { now = Date.now() }, 500)
    return () => clearInterval(id)
  })

  const ageSecs = $derived(rxMs ? Math.max(0, Math.floor((now - rxMs) / 1000)) : null)
  const fresh = $derived(ageSecs !== null && ageSecs <= 2)
  const stale = $derived(ageSecs !== null && ageSecs > 2 && ageSecs <= 5)
  const frozen = $derived(ageSecs !== null && ageSecs > 5)
</script>

<div class="freshness" class:fresh class:stale class:frozen class:offline={!connected}>
  <span class="freshness-dot"></span>
  <span class="freshness-text">
    {#if !connected}
      DISCONNECTED
    {:else if ageSecs === null}
      WAITING
    {:else if fresh}
      LIVE · {ageSecs}s
    {:else if stale}
      STALE · {ageSecs}s
    {:else}
      FROZEN · {ageSecs}s
    {/if}
  </span>
</div>

<style>
  .freshness {
    display: inline-flex;
    align-items: center;
    gap: var(--s-2);
    height: 24px;
    padding: 0 var(--s-3);
    border-radius: var(--r-pill);
    font-size: var(--fs-2xs);
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    border: 1px solid var(--border-subtle);
    background: var(--bg-chip);
    color: var(--fg-muted);
    font-family: var(--font-mono);
  }
  .freshness.fresh  { color: var(--pos); background: var(--pos-bg); border-color: color-mix(in srgb, var(--pos) 30%, transparent); }
  .freshness.stale  { color: var(--warn); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn) 30%, transparent); }
  .freshness.frozen, .freshness.offline { color: var(--neg); background: var(--neg-bg); border-color: color-mix(in srgb, var(--neg) 30%, transparent); }
  .freshness-dot {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .freshness.fresh .freshness-dot {
    animation: pulseDot 1.8s ease-in-out infinite;
  }
  .freshness.frozen .freshness-dot,
  .freshness.offline .freshness-dot {
    animation: pulseDot 0.7s ease-in-out infinite;
  }
  @keyframes pulseDot {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.4; transform: scale(0.7); }
  }
</style>
