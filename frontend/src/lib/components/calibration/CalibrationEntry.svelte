<script>
  /*
   * One pending-calibration entry — head + param diff table +
   * apply/discard actions. Parent owns busy state and dispatches
   * the API calls.
   */
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'
  import { paramRows, deltaPct, fmtNum, fmtLoss, relTimeIso } from './calibration-helpers.js'

  let {
    entry,
    busy = '',
    onApply,
    onDiscard,
  } = $props()

  const rows = $derived(paramRows(entry))
</script>

<article class="entry">
  <header class="entry-head">
    <div class="entry-id">
      <span class="sym num">{entry.symbol}</span>
      <span class="chip tone-info">{entry.loss_fn}</span>
      <span class="meta num">{entry.trials} trials</span>
      <span class="meta">{relTimeIso(entry.created_at)}</span>
    </div>
    <div class="entry-loss" title="Lowest loss across the trial run (lower = better)">
      <span class="meta">best loss</span>
      <span class="num loss">{fmtLoss(entry.best_loss)}</span>
    </div>
  </header>

  <table class="diff">
    <thead>
      <tr>
        <th>Param</th>
        <th class="right">Current</th>
        <th class="right">Suggested</th>
        <th class="right">Δ</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as r (r.key)}
        {@const d = deltaPct(r)}
        <tr>
          <td class="pk">{r.key}</td>
          <td class="num-cell right">{fmtNum(r.current)}</td>
          <td class="num-cell right hi">{fmtNum(r.suggested)}</td>
          <td class="num-cell right delta" class:pos={d !== null && d > 0.5} class:neg={d !== null && d < -0.5}>
            {#if d === null}
              —
            {:else}
              {d > 0 ? '+' : ''}{d.toFixed(1)}%
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>

  <div class="actions">
    <Button variant="primary" onclick={() => onApply(entry.symbol)} disabled={busy !== ''}>
      {#snippet children()}{#if busy === entry.symbol}
        <span class="spinner"></span>
        <span>Applying…</span>
      {:else}
        <Icon name="check" size={14} />
        <span>Apply</span>
      {/if}{/snippet}
    </Button>
    <Button variant="ghost" onclick={() => onDiscard(entry.symbol)} disabled={busy !== ''}>
      {#snippet children()}<Icon name="close" size={14} />
      <span>Discard</span>{/snippet}
    </Button>
  </div>
</article>

<style>
  .entry {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .entry-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    flex-wrap: wrap;
  }
  .entry-id {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    flex-wrap: wrap;
  }
  .sym {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .meta {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .entry-loss {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
  }
  .loss {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
    font-variant-numeric: tabular-nums;
  }

  .diff {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-xs);
  }
  .diff thead th {
    text-align: left;
    padding: var(--s-1) var(--s-2);
    color: var(--fg-muted);
    font-weight: 500;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    font-size: var(--fs-2xs);
    border-bottom: 1px solid var(--border-subtle);
  }
  .diff tbody td {
    padding: var(--s-1) var(--s-2);
    border-bottom: 1px solid var(--border-faint);
  }
  .diff tbody tr:last-child td { border-bottom: none; }
  .right { text-align: right; }
  .pk {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  .hi { color: var(--accent); font-weight: 600; }
  .delta.pos { color: var(--pos); }
  .delta.neg { color: var(--neg); }

  .actions {
    display: flex;
    gap: var(--s-2);
    flex-wrap: wrap;
  }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid color-mix(in srgb, var(--fg-on-accent) 25%, transparent);
    border-top-color: var(--fg-on-accent);
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
