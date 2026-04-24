<script>
  /*
   * Save-as-custom-template dialog with diff preview.
   *
   * Two phases inside the same modal:
   *   - Initial: operator types name + description
   *   - Diff preview: if the name already has saved versions, show
   *     the delta between the current canvas and the latest
   *     version; operator confirms to save as a new version.
   *
   * Parent owns `state` shape:
   *   { name, description, diffPreview: {existing, diff} | null,
   *     busy, checkBusy, error }
   */
  import { Button, Modal } from '../../primitives/index.js'

  let {
    open = false,
    state,
    onNameChange,
    onDescriptionChange,
    onSave,
    onClose,
  } = $props()
</script>

<Modal {open} ariaLabel="Save as template" maxWidth="640px" {onClose}>
  {#snippet children()}
    <h3>Save as template</h3>
    <label class="field stacked">
      <span class="field-label">Name</span>
      <input
        type="text"
        value={state.name}
        oninput={(e) => onNameChange(e.currentTarget.value)}
        placeholder="my-cool-setup"
        disabled={state.diffPreview !== null}
      />
    </label>
    <label class="field stacked">
      <span class="field-label">Description</span>
      <input
        type="text"
        value={state.description}
        oninput={(e) => onDescriptionChange(e.currentTarget.value)}
        placeholder="What does this do?"
      />
    </label>
    {#if state.diffPreview}
      {@const d = state.diffPreview.diff}
      {@const unchanged = d.totalChanges === 0}
      <div class="save-diff" class:clean={unchanged}>
        <div class="save-diff-head">
          {#if unchanged}
            No graph changes — will save a new version with just the
            updated description + timestamp.
          {:else}
            This name already has
            {state.diffPreview.existing.history?.length ?? 1} version(s).
            Saving will append version #{(state.diffPreview.existing.history?.length ?? 1) + 1}:
          {/if}
        </div>
        {#if !unchanged}
          <div class="save-diff-counts">
            <span class="diff-chip add">+{d.addedNodes.length} nodes</span>
            <span class="diff-chip rm">−{d.removedNodes.length} nodes</span>
            <span class="diff-chip mod">~{d.modifiedNodes.length} nodes</span>
            <span class="diff-chip add">+{d.addedEdges.length} edges</span>
            <span class="diff-chip rm">−{d.removedEdges.length} edges</span>
          </div>
          <details class="save-diff-detail">
            <summary>details</summary>
            {#if d.addedNodes.length > 0}
              <div class="diff-section">
                <div class="diff-label">added nodes</div>
                {#each d.addedNodes as n}
                  <code class="diff-line add">+ {n.kind} · {n.id.slice(0, 8)}</code>
                {/each}
              </div>
            {/if}
            {#if d.removedNodes.length > 0}
              <div class="diff-section">
                <div class="diff-label">removed nodes</div>
                {#each d.removedNodes as n}
                  <code class="diff-line rm">− {n.kind} · {n.id.slice(0, 8)}</code>
                {/each}
              </div>
            {/if}
            {#if d.modifiedNodes.length > 0}
              <div class="diff-section">
                <div class="diff-label">modified nodes</div>
                {#each d.modifiedNodes as n}
                  <code class="diff-line mod">
                    ~ {n.kind} · {n.id.slice(0, 8)}
                    {#if n.kindChanged}· kind {n.oldKind} → {n.kind}{/if}
                    {#if n.configChanged}· config updated{/if}
                  </code>
                {/each}
              </div>
            {/if}
            {#if d.addedEdges.length > 0}
              <div class="diff-section">
                <div class="diff-label">added edges</div>
                {#each d.addedEdges as e}
                  <code class="diff-line add">+ {e.from.node.slice(0, 6)}:{e.from.port} → {e.to.node.slice(0, 6)}:{e.to.port}</code>
                {/each}
              </div>
            {/if}
            {#if d.removedEdges.length > 0}
              <div class="diff-section">
                <div class="diff-label">removed edges</div>
                {#each d.removedEdges as e}
                  <code class="diff-line rm">− {e.from.node.slice(0, 6)}:{e.from.port} → {e.to.node.slice(0, 6)}:{e.to.port}</code>
                {/each}
              </div>
            {/if}
          </details>
        {/if}
      </div>
    {/if}
    {#if state.error}
      <div class="modal-err">{state.error}</div>
    {/if}
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={onClose}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button
      variant="primary"
      onclick={onSave}
      loading={state.busy || state.checkBusy}
      disabled={!state.name.trim()}
    >
      {#snippet children()}{#if state.busy}Saving…{:else if state.checkBusy}Checking…{:else if state.diffPreview}Save new version{:else}Save{/if}{/snippet}
    </Button>
  {/snippet}
</Modal>

<style>
  h3 { margin: 0 0 var(--s-3); font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); }
  .field { display: flex; flex-direction: column; gap: 4px; margin-bottom: var(--s-2); }
  .field.stacked { width: 100%; }
  .field-label { font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .field input {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .field input:focus { outline: none; border-color: var(--accent); }

  .save-diff {
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    display: flex; flex-direction: column; gap: var(--s-2);
    margin-top: var(--s-2);
  }
  .save-diff.clean { background: color-mix(in srgb, var(--ok) 6%, transparent); }
  .save-diff-head { font-size: var(--fs-xs); color: var(--fg-secondary); line-height: 1.5; }
  .save-diff-counts { display: flex; flex-wrap: wrap; gap: 6px; }
  .diff-chip {
    font-family: var(--font-mono); font-size: 10px;
    padding: 2px 8px; border-radius: var(--r-sm);
    background: var(--bg-raised); color: var(--fg-secondary);
  }
  .diff-chip.add { color: var(--ok); background: color-mix(in srgb, var(--ok) 10%, transparent); }
  .diff-chip.rm { color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); }
  .diff-chip.mod { color: var(--warn); background: color-mix(in srgb, var(--warn) 10%, transparent); }
  .save-diff-detail { font-size: 10px; color: var(--fg-muted); }
  .save-diff-detail summary { cursor: pointer; color: var(--fg-secondary); }
  .diff-section { margin-top: 6px; }
  .diff-label { font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); margin-bottom: 2px; }
  .diff-line {
    display: block;
    font-family: var(--font-mono); font-size: 10px;
    padding: 1px 4px;
  }
  .diff-line.add { color: var(--ok); }
  .diff-line.rm { color: var(--danger); }
  .diff-line.mod { color: var(--warn); }

  .modal-err {
    padding: var(--s-2);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
    font-size: var(--fs-xs);
    margin-top: var(--s-2);
  }
</style>
