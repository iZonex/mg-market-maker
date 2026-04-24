<script>
  import { Button } from '../primitives/index.js'

  /*
   * M4-GOBS — timeline / time-travel scrubber for live graph.
   *
   * Renders the most recent window of `TickTrace` entries as a
   * horizontal strip. Per-tick column height encodes the tick's
   * total_elapsed_ns (relative), colour encodes sinks_fired
   * count. Clicking a column "pins" that tick — StrategyPage
   * treats it as the visible snapshot until the operator hits
   * "Back to live".
   *
   * Contract:
   *   props:
   *     traces        — newest-first TickTrace[] (length ≤ 256)
   *     pinnedTickNum — tick_num currently pinned, or null for live
   *     onPin(n)      — operator clicked a column; pass its tick_num
   *     onUnpin()     — operator clicked "Back to live"
   */

  let {
    traces = [],
    pinnedTickNum = null,
    onPin = () => {},
    onUnpin = () => {},
  } = $props()

  // Oldest-first for left-to-right rendering.
  const ordered = $derived.by(() => [...traces].reverse())
  const maxElapsed = $derived.by(() => {
    let m = 1
    for (const t of ordered) {
      if ((t.total_elapsed_ns ?? 0) > m) m = t.total_elapsed_ns
    }
    return m
  })

  const currentTick = $derived(
    pinnedTickNum != null
      ? ordered.find((t) => t.tick_num === pinnedTickNum) ?? null
      : ordered[ordered.length - 1] ?? null,
  )

  function sinkTone(n) {
    if (n === 0) return 'idle'
    if (n === 1) return 'low'
    if (n <= 3) return 'mid'
    return 'hot'
  }

  function fmtTime(ms) {
    if (!ms) return '—'
    const d = new Date(Number(ms))
    return `${d.toLocaleTimeString()}.${String(d.getMilliseconds()).padStart(3, '0')}`
  }
</script>

<div class="timeline" class:pinned={pinnedTickNum != null}>
  <header>
    <span class="label">Timeline</span>
    {#if currentTick}
      <span class="meta mono">
        tick #{currentTick.tick_num} · {fmtTime(currentTick.tick_ms)}
        · {currentTick.nodes?.length ?? 0}n
        · {currentTick.sinks_fired?.length ?? 0}s
      </span>
    {:else}
      <span class="meta muted">waiting for first tick…</span>
    {/if}
    {#if pinnedTickNum != null}
      <Button variant="primary" onclick={onUnpin} title="Release pin, resume live">
          {#snippet children()}<span class="live-dot"></span>
        <span>Back to live</span>{/snippet}
        </Button>
    {:else}
      <span class="tl-live-pill">
        <span class="live-dot pulsing"></span>
        <span>live</span>
      </span>
    {/if}
  </header>
  {#if ordered.length === 0}
    <div class="empty">no traces yet</div>
  {:else}
    <div class="strip" role="listbox" aria-label="Tick history">
      {#each ordered as t (t.tick_num)}
        {@const heightPct = Math.max(
          12,
          Math.round(((t.total_elapsed_ns ?? 0) / maxElapsed) * 100),
        )}
        {@const tone = sinkTone(t.sinks_fired?.length ?? 0)}
        {@const isPinned = pinnedTickNum === t.tick_num}
        {@const isCurrent = !isPinned && pinnedTickNum == null && t.tick_num === currentTick?.tick_num}
        <button
          type="button"
          class="col tone-{tone}"
          class:pinned={isPinned}
          class:current={isCurrent}
          style:height={heightPct + '%'}
          title={`tick #${t.tick_num} · ${fmtTime(t.tick_ms)} · ${t.sinks_fired?.length ?? 0} sinks · ${t.total_elapsed_ns}ns`}
          aria-label={`tick ${t.tick_num}`}
          aria-selected={isPinned}
          onclick={() => onPin(t.tick_num)}
        ></button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .timeline {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px var(--s-3);
    background: var(--bg-raised);
    border-top: 1px solid var(--border-subtle);
    font-family: var(--font-sans);
  }
  .timeline.pinned {
    border-top-color: var(--warn);
    box-shadow: inset 0 2px 0 0 color-mix(in srgb, var(--warn) 40%, transparent);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
  }
  .label {
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    font-size: 10px;
  }
  .meta { color: var(--fg-muted); font-size: 10px; }
  .meta.muted { color: var(--fg-muted); }
  .tl-live-pill {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    font-size: 10px;
    font-weight: 600;
    color: var(--pos);
    border: 1px solid color-mix(in srgb, var(--pos) 40%, transparent);
    border-radius: var(--r-pill);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .btn-live {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    border-radius: var(--r-pill);
    cursor: pointer;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .btn-live:hover { background: color-mix(in srgb, var(--warn) 18%, transparent); }
  .live-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--warn);
  }
  .tl-live-pill .live-dot { background: var(--pos); }
  .live-dot.pulsing {
    animation: dot-pulse 1.5s infinite;
  }
  @keyframes dot-pulse {
    0%   { box-shadow: 0 0 0 0 color-mix(in srgb, var(--pos) 60%, transparent); }
    70%  { box-shadow: 0 0 0 5px transparent; }
    100% { box-shadow: 0 0 0 0 transparent; }
  }

  .empty {
    height: 48px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--fg-muted);
    font-size: var(--fs-xs);
  }

  .strip {
    display: flex;
    align-items: flex-end;
    gap: 1px;
    height: 48px;
    padding: 4px 0;
  }
  .col {
    flex: 1 1 auto;
    min-width: 2px;
    max-width: 8px;
    background: var(--bg-chip);
    border: none;
    border-radius: 1px;
    cursor: pointer;
    padding: 0;
    transition: transform 0.08s, background 0.08s;
  }
  .col:hover { transform: scaleY(1.1); background: var(--fg-muted); }
  .col.tone-idle { background: var(--bg-chip); }
  .col.tone-low  { background: color-mix(in srgb, var(--accent) 35%, var(--bg-chip)); }
  .col.tone-mid  { background: color-mix(in srgb, var(--accent) 70%, var(--bg-chip)); }
  .col.tone-hot  { background: var(--accent); }
  .col.current {
    background: var(--pos);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--pos) 60%, transparent);
  }
  .col.pinned {
    background: var(--warn);
    box-shadow: 0 0 0 2px var(--warn);
    transform: scaleY(1.15);
  }
</style>
