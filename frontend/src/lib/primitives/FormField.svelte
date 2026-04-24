<script>
  /*
   * <FormField> — label + input wrapper with consistent spacing,
   * optional help text + validation error. Keeps every form in
   * the app aligned to the same vertical rhythm.
   *
   * Design-system contract:
   *   - Tokens only.
   *   - You provide the actual input via the `input` snippet
   *     (so you can choose `<input>`, `<select>`, `<textarea>`,
   *     custom combobox, etc. without FormField growing).
   *
   * Usage:
   *   <FormField label="Name" hint="Unique identifier" error={nameErr}>
   *     {#snippet input()}
   *       <input type="text" bind:value={name} />
   *     {/snippet}
   *   </FormField>
   */

  let {
    /** Label shown above the input. */
    label,
    /** Optional hint under the input (muted). */
    hint,
    /** Validation error text. Takes precedence over hint. */
    error = null,
    /** Horizontal (label left, input right) vs vertical (default). */
    horizontal = false,
    /** @type {(args?: any) => any} Snippet that renders the actual input. */
    input,
    /** Optional trailing content (unit, button, etc.) */
    trailing,
  } = $props()
</script>

<label class="field" class:horizontal class:has-error={error}>
  {#if label}<span class="label">{label}</span>{/if}
  <span class="row">
    <span class="input-wrap">
      {@render input?.()}
    </span>
    {#if trailing}<span class="trailing">{@render trailing()}</span>{/if}
  </span>
  {#if error}
    <span class="error" role="alert">{error}</span>
  {:else if hint}
    <span class="hint">{hint}</span>
  {/if}
</label>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .field.horizontal {
    flex-direction: row;
    align-items: center;
    gap: var(--s-3);
  }
  .field.horizontal .label { min-width: 120px; }

  .label {
    font-size: var(--fs-xs);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .input-wrap {
    flex: 1;
    display: flex;
  }
  /* Style the bare <input>/<select> inside — a consistent surface
     without asking every form to specify padding/border. */
  .input-wrap :global(input),
  .input-wrap :global(select),
  .input-wrap :global(textarea) {
    flex: 1;
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    padding: 6px 10px;
    color: var(--fg-primary);
    font: inherit;
    min-height: 32px;
  }
  .input-wrap :global(input:focus),
  .input-wrap :global(select:focus),
  .input-wrap :global(textarea:focus) {
    border-color: var(--accent);
    outline: 2px solid var(--accent-ring);
    outline-offset: -1px;
  }
  .has-error .input-wrap :global(input),
  .has-error .input-wrap :global(select),
  .has-error .input-wrap :global(textarea) {
    border-color: var(--danger);
  }
  .trailing { color: var(--fg-muted); font-size: var(--fs-xs); }
  .hint  { color: var(--fg-muted); font-size: 10px; }
  .error { color: var(--danger); font-size: 10px; }
</style>
