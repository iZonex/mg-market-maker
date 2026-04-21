<script>
  /*
   * Wave F6 — reusable empty-state guide with actionable next-step
   * cards. Drop into any page that has "nothing to show" state so
   * fresh-install operators get a clear path forward instead of
   * silent "—" columns.
   *
   * Props:
   *   title    — headline (e.g. "No agents connected yet")
   *   message  — one-paragraph explanation
   *   steps    — ordered list of { title, description, action?: {label, route} }
   */
  import Icon from './Icon.svelte'

  let { title, message, steps = [], onNavigate = null } = $props()
</script>

<div class="empty-guide">
  <div class="hero">
    <div class="hero-icon"><Icon name="info" size={24} /></div>
    <div class="hero-text">
      <h2>{title}</h2>
      {#if message}<p class="lead">{message}</p>{/if}
    </div>
  </div>

  {#if steps.length > 0}
    <ol class="steps">
      {#each steps as step, i (step.title)}
        <li class="step">
          <span class="step-n">{i + 1}</span>
          <div class="step-body">
            <div class="step-title">{step.title}</div>
            {#if step.description}<div class="step-desc">{step.description}</div>{/if}
            {#if step.action && onNavigate}
              <button
                type="button"
                class="step-action"
                onclick={() => onNavigate(step.action.route)}
              >
                {step.action.label} <Icon name="chevronR" size={12} />
              </button>
            {:else if step.action?.external}
              <a class="step-action" href={step.action.external} target="_blank" rel="noopener">
                {step.action.label} <Icon name="external" size={12} />
              </a>
            {/if}
          </div>
        </li>
      {/each}
    </ol>
  {/if}
</div>

<style>
  .empty-guide {
    padding: var(--s-5);
    max-width: 720px;
    margin: var(--s-5) auto;
    display: flex; flex-direction: column; gap: var(--s-4);
  }
  .hero { display: flex; gap: var(--s-3); align-items: flex-start; }
  .hero-icon {
    width: 44px; height: 44px;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--accent);
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .hero-text h2 { margin: 0 0 4px 0; font-size: var(--fs-lg); color: var(--fg-primary); }
  .lead { margin: 0; font-size: var(--fs-sm); color: var(--fg-secondary); line-height: 1.5; }

  .steps { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: var(--s-2); }
  .step {
    display: flex; gap: var(--s-3); align-items: flex-start;
    padding: var(--s-3);
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .step-n {
    flex-shrink: 0;
    width: 28px; height: 28px; border-radius: 50%;
    background: var(--bg-chip); color: var(--fg-secondary);
    display: flex; align-items: center; justify-content: center;
    font-family: var(--font-mono); font-size: var(--fs-xs); font-weight: 600;
  }
  .step-body { display: flex; flex-direction: column; gap: 4px; flex: 1; }
  .step-title { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .step-desc { font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.5; }
  .step-action {
    align-self: flex-start;
    display: inline-flex; align-items: center; gap: 4px;
    margin-top: 4px;
    padding: 4px 10px;
    background: var(--accent); color: var(--bg-base);
    border: 0; border-radius: var(--r-sm);
    font-size: var(--fs-xs); font-weight: 500;
    cursor: pointer; text-decoration: none;
    font-family: inherit;
  }
  .step-action:hover { filter: brightness(1.1); }
</style>
