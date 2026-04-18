<script>
  // Typed-echo confirmation modal for destructive kill-switch
  // actions (Epic 37.2). Replaces native `confirm()` which an
  // operator's spacebar habit can dismiss without reading. The
  // user must TYPE the phrase to enable the action, and then a
  // 3-second cooldown still runs before the action dispatches.
  //
  // Props:
  //   open       — boolean; parent controls visibility
  //   level      — kill level label, e.g. "L3" / "L4"
  //   action     — human title, e.g. "CANCEL ALL" / "FLATTEN"
  //   symbol     — symbol being acted on
  //   preview    — string explaining effect (e.g. "Will cancel 7 orders")
  //   onConfirm  — fires once confirm phrase typed + cooldown done
  //   onCancel   — fires on X / Esc / backdrop click

  let {
    open = false,
    level = 'L4',
    action = 'FLATTEN',
    symbol = '',
    preview = '',
    onConfirm,
    onCancel,
  } = $props()

  let typed = $state('')
  let cooldownUntil = $state(0)
  let armed = $state(false)

  const expected = $derived(`${symbol} ${action}`)
  const typedOk = $derived(typed.trim().toUpperCase() === expected.toUpperCase())

  let now = $state(Date.now())
  $effect(() => {
    if (!open) return
    const id = setInterval(() => { now = Date.now() }, 100)
    return () => clearInterval(id)
  })

  const cooldownLeft = $derived(Math.max(0, Math.ceil((cooldownUntil - now) / 1000)))
  const canFire = $derived(typedOk && armed && cooldownLeft === 0)

  // Reset when modal opens/closes.
  $effect(() => {
    if (open) {
      typed = ''
      armed = false
      cooldownUntil = 0
    }
  })

  function arm() {
    if (!typedOk) return
    armed = true
    cooldownUntil = Date.now() + 3000
  }

  function fire() {
    if (!canFire) return
    onConfirm?.()
  }

  function handleKey(e) {
    if (!open) return
    if (e.key === 'Escape') { e.preventDefault(); onCancel?.() }
    if (e.key === 'Enter' && canFire) { e.preventDefault(); fire() }
  }
</script>

<svelte:window onkeydown={handleKey} />

{#if open}
  <div class="backdrop" role="presentation" onclick={() => onCancel?.()}>
    <div class="modal" role="dialog" aria-labelledby="kc-title" aria-modal="true"
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => e.stopPropagation()}>
      <header>
        <span class="level level-{level.toLowerCase()}">{level}</span>
        <h2 id="kc-title">{action} on {symbol}</h2>
      </header>

      <div class="body">
        {#if preview}
          <div class="preview">{preview}</div>
        {/if}
        <p class="instructions">
          Type <code>{expected}</code> to enable. A 3-second cooldown will
          run before the action dispatches — you can cancel during it.
        </p>
        <input
          class="typed"
          class:ok={typedOk}
          placeholder="type {expected}"
          bind:value={typed}
          autocomplete="off"
          autofocus
        />
      </div>

      <footer>
        <button class="btn cancel" onclick={() => onCancel?.()}>Cancel (Esc)</button>
        {#if !armed}
          <button class="btn arm" disabled={!typedOk} onclick={arm}>
            Arm {action}
          </button>
        {:else if cooldownLeft > 0}
          <button class="btn counting" disabled>
            Firing in {cooldownLeft}s…
          </button>
        {:else}
          <button class="btn fire" onclick={fire}>Fire {action} (Enter)</button>
        {/if}
      </footer>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.7);
    z-index: 100; display: flex; align-items: center; justify-content: center;
  }
  .modal {
    background: #161b22; border: 1px solid #30363d; border-radius: 8px;
    min-width: 420px; max-width: 520px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.6);
    overflow: hidden;
  }
  header {
    display: flex; align-items: center; gap: 10px;
    padding: 14px 18px; border-bottom: 1px solid #21262d;
    background: #1c2128;
  }
  header h2 { font-size: 14px; font-weight: 600; color: #f0f6fc; }
  .level {
    padding: 3px 8px; border-radius: 3px; font-size: 11px;
    font-weight: 800; letter-spacing: 0.5px;
  }
  .level-l3 { background: #da3633; color: #fff; }
  .level-l4 { background: #f85149; color: #fff; }
  .body { padding: 18px; }
  .preview {
    background: #0d1117; border: 1px solid #30363d; border-radius: 4px;
    padding: 10px 12px; margin-bottom: 12px; font-size: 12px; color: #c9d1d9;
    font-variant-numeric: tabular-nums;
  }
  .instructions { font-size: 12px; color: #8b949e; margin-bottom: 12px; }
  .instructions code {
    background: #0d1117; color: #f0f6fc; padding: 1px 6px;
    border-radius: 3px; font-family: inherit;
  }
  .typed {
    width: 100%; box-sizing: border-box;
    background: #0d1117; color: #f0f6fc; border: 1px solid #30363d;
    border-radius: 4px; padding: 10px 12px; font-family: inherit;
    font-size: 13px; font-weight: 600; letter-spacing: 1px;
    text-transform: uppercase;
  }
  .typed:focus { outline: none; border-color: #58a6ff; }
  .typed.ok { border-color: #3fb950; color: #3fb950; }
  footer {
    display: flex; justify-content: flex-end; gap: 8px;
    padding: 12px 18px; border-top: 1px solid #21262d; background: #1c2128;
  }
  .btn {
    padding: 8px 14px; border: none; border-radius: 4px; cursor: pointer;
    font-family: inherit; font-size: 12px; font-weight: 700;
    letter-spacing: 0.3px;
  }
  .btn.cancel { background: #30363d; color: #c9d1d9; }
  .btn.cancel:hover { background: #484f58; }
  .btn.arm { background: #d29922; color: #000; }
  .btn.arm:disabled { opacity: 0.4; cursor: not-allowed; }
  .btn.arm:not(:disabled):hover { background: #f0b93e; }
  .btn.counting { background: #8b949e; color: #0d1117; cursor: wait; }
  .btn.fire { background: #f85149; color: #fff; animation: pulse 0.7s ease infinite; }
  .btn.fire:hover { background: #ff6b6b; }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(248, 81, 73, 0.7); }
    50%      { box-shadow: 0 0 0 8px rgba(248, 81, 73, 0); }
  }
</style>
