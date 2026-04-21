<script>
  /*
   * Wave F1 — first-install wizard. Shown on Overview when the
   * controller detects "fresh state": no vault credentials, no
   * approved agents, no clients. The admin dismisses it when
   * they're done; the "dismissed" flag is stored in localStorage
   * so it doesn't nag on every page load.
   *
   * Each step polls a relevant endpoint and marks itself done
   * automatically — so the admin doesn't have to manually tick
   * boxes. Progress survives refreshes.
   */
  import Icon from './Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {} } = $props()
  const api = createApiClient(auth)

  const STORAGE_KEY = 'mm_install_wizard_dismissed'

  let dismissed = $state(localStorage.getItem(STORAGE_KEY) === 'true')
  let checks = $state({
    vault: { done: false, loading: true, count: 0 },
    agents: { done: false, loading: true, count: 0 },
    deploys: { done: false, loading: true, count: 0 },
  })
  let lastFetch = $state(null)

  async function pollState() {
    try {
      const [vaultRes, fleetRes] = await Promise.all([
        api.getJson('/api/v1/vault').catch(() => []),
        api.getJson('/api/v1/fleet').catch(() => []),
      ])
      const vault = Array.isArray(vaultRes) ? vaultRes : []
      const fleet = Array.isArray(fleetRes) ? fleetRes : []
      const acceptedAgents = fleet.filter(a => a.approval_state === 'accepted')
      const totalDeploys = fleet.reduce((n, a) => n + (a.deployments?.length || 0), 0)

      checks = {
        vault: { done: vault.length > 0, loading: false, count: vault.length },
        agents: { done: acceptedAgents.length > 0, loading: false, count: acceptedAgents.length },
        deploys: { done: totalDeploys > 0, loading: false, count: totalDeploys },
      }
      lastFetch = new Date()
    } catch {
      // Silent — will retry.
    }
  }

  $effect(() => {
    if (dismissed) return
    pollState()
    const iv = setInterval(pollState, 5000)
    return () => clearInterval(iv)
  })

  const allDone = $derived(checks.vault.done && checks.agents.done && checks.deploys.done)

  // Auto-dismiss when every step is green for > 10s — once the
  // system is populated, the wizard has nothing to offer.
  let autoHideTimer = null
  $effect(() => {
    if (allDone && !dismissed && !autoHideTimer) {
      autoHideTimer = setTimeout(() => {
        dismissed = true
        localStorage.setItem(STORAGE_KEY, 'true')
      }, 10_000)
    }
  })

  function dismiss() {
    dismissed = true
    localStorage.setItem(STORAGE_KEY, 'true')
  }

  function reset() {
    localStorage.removeItem(STORAGE_KEY)
    dismissed = false
  }
</script>

{#if !dismissed}
  <div class="wiz">
    <div class="wiz-head">
      <div class="wiz-title">
        {allDone ? 'Setup complete ✓' : 'Welcome — first steps'}
      </div>
      <button type="button" class="wiz-close" onclick={dismiss} aria-label="Dismiss">
        <Icon name="close" size={14} />
      </button>
    </div>
    {#if !allDone}
      <p class="wiz-lead">
        Four clicks to a working paper deployment. Each step
        self-checks once the resource exists.
      </p>
    {:else}
      <p class="wiz-lead ok">
        The controller sees credentials, accepted agents, and live
        deployments. This panel will auto-hide in 10s.
      </p>
    {/if}

    <ol class="steps">
      <li class:done={checks.vault.done} class:loading={checks.vault.loading}>
        <span class="marker">
          {#if checks.vault.done}
            <Icon name="check" size={12} />
          {:else}
            1
          {/if}
        </span>
        <div class="step-body">
          <div class="step-title">Add an exchange credential to the vault</div>
          <div class="step-desc">
            Even paper-mode deploys need a credential (dummy key + testnet is fine).
            {#if checks.vault.count > 0}
              <span class="step-state">· {checks.vault.count} entries</span>
            {/if}
          </div>
          {#if !checks.vault.done}
            <button type="button" class="step-cta" onclick={() => onNavigate('vault')}>
              Open Vault <Icon name="chevronR" size={10} />
            </button>
          {/if}
        </div>
      </li>

      <li class:done={checks.agents.done} class:loading={checks.agents.loading} class:blocked={!checks.vault.done}>
        <span class="marker">
          {#if checks.agents.done}
            <Icon name="check" size={12} />
          {:else}
            2
          {/if}
        </span>
        <div class="step-body">
          <div class="step-title">Boot an mm-agent and accept it</div>
          <div class="step-desc">
            On your trading box:
            <code class="mono cmd">MM_BRAIN_WS_ADDR=ws://&lt;this-controller&gt;:9091 mm-agent</code>.
            The fingerprint lands on Fleet as Pending — or pre-approve it first from the same page.
            {#if checks.agents.count > 0}
              <span class="step-state">· {checks.agents.count} accepted</span>
            {/if}
          </div>
          {#if !checks.agents.done}
            <button type="button" class="step-cta" onclick={() => onNavigate('fleet')}>
              Open Fleet <Icon name="chevronR" size={10} />
            </button>
          {/if}
        </div>
      </li>

      <li class:done={checks.deploys.done} class:loading={checks.deploys.loading} class:blocked={!checks.agents.done}>
        <span class="marker">
          {#if checks.deploys.done}
            <Icon name="check" size={12} />
          {:else}
            3
          {/if}
        </span>
        <div class="step-body">
          <div class="step-title">Deploy a strategy to paper-test the pipe</div>
          <div class="step-desc">
            Fleet → click an accepted agent's "Deploy strategy" button.
            Pick <code>major-spot-basic</code> on BTCUSDT; it'll run paper by
            default and start populating PnL / SLA / fills within 60s.
            {#if checks.deploys.count > 0}
              <span class="step-state">· {checks.deploys.count} deployments</span>
            {/if}
          </div>
          {#if !checks.deploys.done}
            <button type="button" class="step-cta" onclick={() => onNavigate('fleet')}>
              Open Fleet <Icon name="chevronR" size={10} />
            </button>
          {/if}
        </div>
      </li>

      <li>
        <span class="marker">4</span>
        <div class="step-body">
          <div class="step-title">Invite your first client (optional)</div>
          <div class="step-desc">
            Register a client, generate an invite URL, send it over.
            They get a ClientReader account that sees their own PnL
            and nothing else.
          </div>
          <button type="button" class="step-cta" onclick={() => onNavigate('clients')}>
            Open Clients <Icon name="chevronR" size={10} />
          </button>
        </div>
      </li>
    </ol>

    {#if allDone}
      <div class="wiz-done-actions">
        <button type="button" class="btn ghost small" onclick={dismiss}>Hide now</button>
      </div>
    {/if}
  </div>
{:else}
  <button
    type="button"
    class="wiz-reopen"
    onclick={reset}
    title="Re-open the first-install wizard"
  >
    <Icon name="info" size={10} /> Install guide
  </button>
{/if}

<style>
  .wiz {
    background: var(--bg-raised);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border-subtle));
    border-radius: var(--r-lg);
    padding: var(--s-4);
    margin-bottom: var(--s-4);
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  .wiz-head { display: flex; align-items: center; justify-content: space-between; }
  .wiz-title { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 600; }
  .wiz-close {
    background: transparent; border: 0; padding: 4px;
    color: var(--fg-muted); cursor: pointer;
  }
  .wiz-close:hover { color: var(--fg-primary); }
  .wiz-lead { margin: 0; font-size: var(--fs-sm); color: var(--fg-secondary); line-height: 1.5; }
  .wiz-lead.ok { color: var(--ok); }

  .steps { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: var(--s-2); }
  .steps li {
    display: flex; gap: var(--s-3); align-items: flex-start;
    padding: var(--s-3);
    background: var(--bg-chip); border-radius: var(--r-md);
    border: 1px solid transparent;
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .steps li.done { border-color: color-mix(in srgb, var(--ok) 30%, transparent); }
  .steps li.blocked { opacity: 0.55; }
  .marker {
    flex-shrink: 0;
    width: 26px; height: 26px; border-radius: 50%;
    background: var(--bg-raised); color: var(--fg-secondary);
    display: flex; align-items: center; justify-content: center;
    font-family: var(--font-mono); font-size: 11px; font-weight: 600;
  }
  .steps li.done .marker {
    background: var(--ok); color: var(--bg-base);
  }
  .step-body { display: flex; flex-direction: column; gap: 4px; flex: 1; }
  .step-title { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .step-desc { font-size: 11px; color: var(--fg-muted); line-height: 1.5; }
  .step-state { color: var(--ok); font-weight: 500; }
  .cmd { background: var(--bg-base); padding: 1px 4px; border-radius: var(--r-sm); font-size: 10px; }
  .step-cta {
    align-self: flex-start; margin-top: 4px;
    display: inline-flex; align-items: center; gap: 3px;
    padding: 3px 10px;
    background: var(--accent); color: var(--bg-base);
    border: 0; border-radius: var(--r-sm);
    font-size: 11px; font-weight: 500;
    cursor: pointer;
  }
  .step-cta:hover { filter: brightness(1.1); }

  .wiz-done-actions { display: flex; justify-content: flex-end; }

  .wiz-reopen {
    position: fixed; bottom: var(--s-3); right: var(--s-3);
    display: inline-flex; align-items: center; gap: 4px;
    padding: 4px 10px;
    background: var(--bg-raised); color: var(--fg-muted);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    font-size: 10px; cursor: pointer; z-index: 50;
    opacity: 0.7;
  }
  .wiz-reopen:hover { opacity: 1; color: var(--fg-primary); }
</style>
