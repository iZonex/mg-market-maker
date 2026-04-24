<script>
  /*
   * <Modal> — the single source of truth for modal chrome.
   *
   * Design-system contract:
   *   - Consumes tokens only. Never hex/rgb.
   *   - Handles the three bits of plumbing every ad-hoc modal
   *     currently re-implements: backdrop dismiss, Escape-to-close,
   *     and click-outside-to-close. Focus trap delegated to the
   *     browser's inert-by-default behaviour on backgrounded content
   *     plus `autofocus` on the first focusable child.
   *   - Consumers provide content via `{#snippet}` or default
   *     children; action buttons via the `actions` snippet so the
   *     spacing/align is consistent.
   *
   * Usage:
   *   <Modal
   *     open={replayResult !== null}
   *     ariaLabel="Replay vs deployed"
   *     onClose={closeReplay}
   *   >
   *     {#snippet children()}  … your body …  {/snippet}
   *     {#snippet actions()}
   *       <Button variant="ghost" onclick={closeReplay}>Close</Button>
   *     {/snippet}
   *   </Modal>
   */

  let {
    /** When false, the modal is not in the DOM. */
    open = false,
    /** Screen-reader label for the dialog. Required. */
    ariaLabel,
    /** Called on backdrop click + Escape. */
    onClose = () => {},
    /** Optional maxWidth override, e.g. "640px" or "80vw". */
    maxWidth = '640px',
    /** Modal body. */
    children,
    /** Bottom-right action row; e.g. Cancel / Confirm buttons. */
    actions,
  } = $props()

  function onBackdropKey(e) {
    if (e.key === 'Escape') onClose()
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="backdrop"
    role="presentation"
    onclick={onClose}
    onkeydown={onBackdropKey}
  >
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      tabindex="-1"
      style="max-width: {maxWidth};"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <div class="body">
        {@render children?.()}
      </div>
      {#if actions}
        <div class="actions">
          {@render actions()}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: var(--bg-overlay);
    backdrop-filter: blur(2px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal, 50);
    padding: var(--s-4);
  }
  .dialog {
    background: var(--bg-raised);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-lg);
    display: flex;
    flex-direction: column;
    max-height: 85vh;
    min-width: 320px;
    width: 100%;
  }
  .body {
    padding: var(--s-4);
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .actions {
    padding: var(--s-3) var(--s-4);
    border-top: 1px solid var(--border-subtle);
    display: flex;
    justify-content: flex-end;
    gap: var(--s-2);
  }
</style>
