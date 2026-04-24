<script>
  /*
   * GOBS-M5 + M5.2 — "Replay vs deployed" modal, extracted out of
   * `StrategyPage.svelte` as part of the Wave 6 modal-decomposition
   * refactor. Takes a pre-computed `result` payload (the
   * `graph_replay` details-topic response) and two graph snapshots
   * (deployed + candidate) that feed the side-by-side SVG mini-
   * canvas. The divergence scrubber is local state — there is no
   * reason the parent should care which tick the operator is
   * currently staring at.
   */

  let {
    /** @type {any | null} — replay payload from the agent. `null` hides the modal. */
    result = null,
    /** @type {any | null} — full deployed graph JSON for the left mini-canvas. */
    deployedGraph = null,
    /** @type {string | null} — deployed graph name for the left column label. */
    deployedGraphName = null,
    /** @type {any | null} — candidate graph JSON for the right mini-canvas. */
    candidateGraph = null,
    /** @type {() => void} — invoked when the operator closes the modal. */
    onClose = () => {},
  } = $props()

  // Divergence cursor — index into `result.divergences`. Parent
  // owns the result list, we own the pointer because changing the
  // pointer should never invalidate anything above us.
  let divergenceIdx = $state(0)

  // Snap the cursor back to 0 whenever a fresh result lands, so
  // the modal doesn't open on an index that belonged to the
  // previous replay run.
  $effect(() => {
    if (result) divergenceIdx = 0
  })

  const activeDivergence = $derived.by(() => {
    const list = result?.divergences ?? []
    if (list.length === 0) return null
    const idx = Math.max(0, Math.min(divergenceIdx, list.length - 1))
    return list[idx]
  })
  const activeDivergingKinds = $derived(
    new Set(activeDivergence?.diverging_kinds ?? []),
  )

  // Project a backend-shape graph (nodes[].pos, edges[].from/to)
  // into a viewbox-fitted list of SVG rects + straight edges.
  // Returns null when the graph is empty so the caller can
  // short-circuit to an "unavailable" placeholder.
  function projectGraphForMiniCanvas(graph) {
    if (!graph?.nodes?.length) return null
    const NODE_W = 92
    const NODE_H = 28
    const xs = graph.nodes.map((n) => (Array.isArray(n.pos) ? n.pos[0] : 0))
    const ys = graph.nodes.map((n) => (Array.isArray(n.pos) ? n.pos[1] : 0))
    const minX = Math.min(...xs)
    const minY = Math.min(...ys)
    const maxX = Math.max(...xs) + NODE_W
    const maxY = Math.max(...ys) + NODE_H
    const padding = 12
    const vbX = minX - padding
    const vbY = minY - padding
    const vbW = maxX - minX + padding * 2
    const vbH = maxY - minY + padding * 2
    const posById = new Map()
    const projectedNodes = graph.nodes.map((n) => {
      const x = (Array.isArray(n.pos) ? n.pos[0] : 0)
      const y = (Array.isArray(n.pos) ? n.pos[1] : 0)
      posById.set(n.id, { cx: x + NODE_W / 2, cy: y + NODE_H / 2 })
      return {
        id: n.id,
        kind: n.kind,
        x,
        y,
        w: NODE_W,
        h: NODE_H,
      }
    })
    const projectedEdges = (graph.edges ?? [])
      .map((e, i) => {
        const from = posById.get(e.from?.node)
        const to = posById.get(e.to?.node)
        if (!from || !to) return null
        return { id: `e-${i}`, x1: from.cx, y1: from.cy, x2: to.cx, y2: to.cy }
      })
      .filter(Boolean)
    return {
      viewBox: `${vbX} ${vbY} ${vbW} ${vbH}`,
      nodes: projectedNodes,
      edges: projectedEdges,
    }
  }

  const miniDeployed = $derived(projectGraphForMiniCanvas(deployedGraph))
  const miniCandidate = $derived(projectGraphForMiniCanvas(candidateGraph))
</script>

{#if result}
  <div
    class="modal-backdrop"
    role="button"
    tabindex="-1"
    aria-label="Close replay"
    onclick={onClose}
    onkeydown={(e) => { if (e.key === 'Escape') onClose() }}
  >
    <div
      class="modal replay-card"
      role="dialog"
      aria-modal="true"
      aria-label="Replay vs deployed"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <h3>Replay vs deployed</h3>
      <div class="replay-summary" class:bad={result.divergence_count > 0}>
        <div class="replay-summary-line">{result.summary}</div>
        {#if result.candidate_issues?.length}
          <div class="replay-issues">
            {#each result.candidate_issues as iss}
              <code class="v-issue">{iss}</code>
            {/each}
          </div>
        {/if}
      </div>

      {#if activeDivergence}
        {@const divList = result.divergences}
        <div class="replay-scrubber">
          <button
            type="button"
            class="btn ghost sm"
            disabled={divergenceIdx <= 0}
            onclick={() => (divergenceIdx = Math.max(0, divergenceIdx - 1))}
            aria-label="Previous divergent tick"
          >‹</button>
          <span class="replay-scrubber-meta">
            <code>tick #{activeDivergence.tick_num}</code>
            <span class="muted">
              ({divergenceIdx + 1}/{divList.length}) ·
              {new Date(activeDivergence.tick_ms).toLocaleTimeString()}
            </span>
            {#if activeDivergence.diverging_kinds?.length}
              <span class="replay-kinds">
                kinds:
                {#each activeDivergence.diverging_kinds as k}
                  <code class="replay-kind-chip">{k}</code>
                {/each}
              </span>
            {/if}
          </span>
          <input
            type="range"
            min="0"
            max={Math.max(0, divList.length - 1)}
            bind:value={divergenceIdx}
            aria-label="Divergence cursor"
          />
          <button
            type="button"
            class="btn ghost sm"
            disabled={divergenceIdx >= divList.length - 1}
            onclick={() => (divergenceIdx = Math.min(divList.length - 1, divergenceIdx + 1))}
            aria-label="Next divergent tick"
          >›</button>
        </div>

        <div class="replay-canvas-pair">
          <div class="replay-mini-col">
            <span class="col-label">
              deployed{deployedGraphName ? ` · ${deployedGraphName}` : ''}
            </span>
            {#if miniDeployed}
              <svg
                class="mini-canvas"
                viewBox={miniDeployed.viewBox}
                preserveAspectRatio="xMidYMid meet"
              >
                {#each miniDeployed.edges as e (e.id)}
                  <line class="mini-edge"
                        x1={e.x1} y1={e.y1} x2={e.x2} y2={e.y2} />
                {/each}
                {#each miniDeployed.nodes as n (n.id)}
                  {@const diverging = activeDivergingKinds.has(n.kind)}
                  <g class="mini-node" class:diverging>
                    <rect x={n.x} y={n.y} width={n.w} height={n.h} rx="4" />
                    <text x={n.x + n.w / 2} y={n.y + n.h / 2 + 3}>
                      {n.kind}
                    </text>
                  </g>
                {/each}
              </svg>
            {:else}
              <div class="mini-empty muted">deployed graph unavailable</div>
            {/if}
          </div>
          <div class="replay-mini-col">
            <span class="col-label">candidate</span>
            {#if miniCandidate}
              <svg
                class="mini-canvas"
                viewBox={miniCandidate.viewBox}
                preserveAspectRatio="xMidYMid meet"
              >
                {#each miniCandidate.edges as e (e.id)}
                  <line class="mini-edge"
                        x1={e.x1} y1={e.y1} x2={e.x2} y2={e.y2} />
                {/each}
                {#each miniCandidate.nodes as n (n.id)}
                  {@const diverging = activeDivergingKinds.has(n.kind)}
                  <g class="mini-node" class:diverging>
                    <rect x={n.x} y={n.y} width={n.w} height={n.h} rx="4" />
                    <text x={n.x + n.w / 2} y={n.y + n.h / 2 + 3}>
                      {n.kind}
                    </text>
                  </g>
                {/each}
              </svg>
            {:else}
              <div class="mini-empty muted">candidate graph unavailable</div>
            {/if}
          </div>
        </div>

        <details class="replay-sink-diff">
          <summary>Sink JSON for tick #{activeDivergence.tick_num}</summary>
          <div class="replay-diff-cols">
            <div class="col-old">
              <span class="col-label">deployed</span>
              <pre class="mono">{JSON.stringify(activeDivergence.original_sinks, null, 1)}</pre>
            </div>
            <div class="col-new">
              <span class="col-label">candidate</span>
              <pre class="mono">{JSON.stringify(activeDivergence.replay_sinks, null, 1)}</pre>
            </div>
          </div>
        </details>
      {/if}

      <div class="modal-actions">
        <button type="button" class="btn ghost" onclick={onClose}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 40;
  }
  .modal {
    background: var(--bg-elev-1, var(--bg-base));
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    padding: var(--s-3);
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .replay-card { min-width: 640px; max-width: 900px; max-height: 80vh; display: flex; flex-direction: column; }
  .replay-summary {
    padding: var(--s-3);
    background: color-mix(in srgb, var(--ok) 8%, transparent);
    border-radius: var(--r-sm);
    font-size: var(--fs-sm);
    color: var(--fg-primary);
  }
  .replay-summary.bad { background: color-mix(in srgb, var(--warn) 14%, transparent); color: var(--warn); }
  .replay-summary-line { font-weight: 500; }
  .replay-issues { margin-top: var(--s-2); display: flex; flex-wrap: wrap; gap: 4px; }
  .v-issue {
    font-family: var(--font-mono); font-size: 10px;
    padding: 2px 6px; border-radius: var(--r-sm);
    background: var(--bg-chip); color: var(--warn);
  }
  .replay-scrubber {
    margin-top: var(--s-3);
    padding: var(--s-2);
    display: grid;
    grid-template-columns: auto 1fr auto auto;
    gap: var(--s-2);
    align-items: center;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    border: 1px solid var(--border-subtle);
    font-size: var(--fs-xs);
  }
  .replay-scrubber .sm { padding: 2px 8px; min-width: 24px; }
  .replay-scrubber-meta { display: flex; flex-wrap: wrap; gap: var(--s-2); align-items: center; }
  .replay-scrubber input[type="range"] { grid-column: 1 / -1; width: 100%; accent-color: var(--warn); }
  .replay-kinds { display: inline-flex; gap: 4px; flex-wrap: wrap; }
  .replay-kind-chip {
    background: color-mix(in srgb, var(--warn) 18%, transparent);
    color: var(--warn);
    padding: 1px 6px;
    border-radius: var(--r-pill);
    font-size: 10px;
    font-family: var(--font-mono);
  }
  .replay-canvas-pair {
    margin-top: var(--s-3);
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-2);
  }
  .replay-mini-col { display: flex; flex-direction: column; gap: 4px; }
  .replay-mini-col .col-label {
    font-size: 10px; letter-spacing: var(--tracking-label);
    text-transform: uppercase; color: var(--fg-muted);
  }
  .mini-canvas {
    width: 100%;
    height: 220px;
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .mini-empty {
    height: 220px;
    display: flex; align-items: center; justify-content: center;
    background: var(--bg-base);
    border: 1px dashed var(--border-subtle);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .mini-edge { stroke: var(--fg-muted); stroke-width: 1.2; opacity: 0.5; }
  .mini-node rect {
    fill: var(--bg-raised);
    stroke: var(--border-subtle);
    stroke-width: 1;
  }
  .mini-node text {
    fill: var(--fg-secondary);
    font-family: var(--font-mono);
    font-size: 10px;
    text-anchor: middle;
    pointer-events: none;
  }
  .mini-node.diverging rect {
    stroke: var(--warn);
    stroke-width: 2;
    fill: color-mix(in srgb, var(--warn) 15%, var(--bg-raised));
    filter: drop-shadow(0 0 6px color-mix(in srgb, var(--warn) 70%, transparent));
  }
  .mini-node.diverging text {
    fill: var(--warn);
    font-weight: 700;
  }
  .replay-sink-diff {
    margin-top: var(--s-3);
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    background: var(--bg-raised);
  }
  .replay-sink-diff summary {
    cursor: pointer;
    font-size: 10px;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .replay-sink-diff[open] summary { margin-bottom: var(--s-2); }
  .replay-diff-cols { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2); }
  .replay-diff-cols .col-label {
    font-size: 10px; letter-spacing: var(--tracking-label);
    text-transform: uppercase; color: var(--fg-muted);
  }
  .replay-diff-cols pre {
    margin: 2px 0 0; padding: var(--s-2);
    background: var(--bg-base); border-radius: var(--r-sm);
    font-size: 10px; line-height: 1.3; overflow: auto; max-height: 160px;
  }
  .replay-diff-cols .col-old pre { border-left: 2px solid var(--fg-muted); }
  .replay-diff-cols .col-new pre { border-left: 2px solid var(--accent); }
  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--s-2);
    margin-top: var(--s-2);
  }
  .btn {
    padding: 6px 12px;
    border-radius: var(--r-sm);
    font-size: var(--fs-sm);
    cursor: pointer;
    background: var(--accent);
    color: var(--fg-on-accent, #fff);
    border: none;
  }
  .btn.ghost {
    background: transparent;
    color: var(--fg-primary);
    border: 1px solid var(--border-subtle);
  }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .muted { color: var(--fg-muted); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
