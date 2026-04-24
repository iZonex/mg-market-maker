<script>
  /*
   * Operator avatar + dropdown menu. Shows name, role, and
   * offers "My profile" + "Log out" entries. Click-outside
   * handling lives here so the TopBar doesn't have to thread
   * it through.
   */
  import Icon from '../Icon.svelte'

  let { auth, onNavigate = () => {} } = $props()

  let open = $state(false)

  const role = $derived(auth?.state?.role || 'viewer')
  const name = $derived(auth?.state?.name || 'Operator')
  const initials = $derived(
    name.split(/\s+/).map((w) => w[0]).join('').slice(0, 2).toUpperCase()
  )

  function handleLogout() {
    open = false
    auth?.logout?.()
  }

  function onGlobalClick(e) {
    if (!e.target.closest('.user-menu-wrap')) open = false
  }
  $effect(() => {
    window.addEventListener('mousedown', onGlobalClick)
    return () => window.removeEventListener('mousedown', onGlobalClick)
  })
</script>

<div class="user-menu-wrap">
  <button
    type="button"
    class="user-btn"
    class:open
    onclick={() => (open = !open)}
    aria-haspopup="menu"
    aria-expanded={open}
  >
    <span class="avatar" data-role={role}>{initials}</span>
    <span class="user-meta">
      <span class="user-name">{name}</span>
      <span class="user-role">{role}</span>
    </span>
    <Icon name="chevronDown" size={14} />
  </button>

  {#if open}
    <div class="user-menu card-glass" role="menu">
      <div class="menu-header">
        <span class="avatar avatar-lg" data-role={role}>{initials}</span>
        <div class="menu-header-text">
          <div class="menu-name">{name}</div>
          <div class="menu-sub">
            <span class="chip chip-role" data-role={role}>{role}</span>
          </div>
        </div>
      </div>
      <div class="menu-items">
        <button
          type="button"
          class="menu-item"
          onclick={() => { open = false; onNavigate('profile') }}
        >
          <Icon name="settings" size={14} />
          <span>My profile</span>
        </button>
        <button type="button" class="menu-item menu-item-danger" onclick={handleLogout}>
          <Icon name="logout" size={14} />
          <span>Log out</span>
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .user-menu-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }
  .user-btn {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: 4px var(--s-3) 4px 4px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-pill);
    color: var(--fg-primary);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out);
  }
  .user-btn:hover,
  .user-btn.open { background: var(--bg-chip-hover); border-color: var(--border-default); }
  .avatar {
    width: 28px; height: 28px;
    display: flex; align-items: center; justify-content: center;
    font-size: var(--fs-xs);
    font-weight: 700;
    letter-spacing: 0.02em;
    border-radius: 50%;
    background: var(--bg-chip-hover);
    color: var(--fg-primary);
  }
  .avatar-lg { width: 40px; height: 40px; font-size: var(--fs-sm); }
  .avatar[data-role='admin']    { background: var(--critical-bg); color: var(--critical); box-shadow: 0 0 0 1px color-mix(in srgb, var(--critical) 35%, transparent) inset; }
  .avatar[data-role='operator'] { background: var(--warn-bg);     color: var(--warn);     box-shadow: 0 0 0 1px color-mix(in srgb, var(--warn) 35%, transparent) inset; }
  .avatar[data-role='viewer']   { background: var(--pos-bg);      color: var(--pos);      box-shadow: 0 0 0 1px color-mix(in srgb, var(--pos) 35%, transparent) inset; }
  .user-meta {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    line-height: 1;
  }
  .user-name {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .user-role {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }

  .user-menu {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    min-width: 240px;
    padding: var(--s-3);
    z-index: var(--z-dropdown);
    box-shadow: var(--shadow-lg);
  }
  .menu-header {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    margin-bottom: var(--s-2);
  }
  .menu-header-text { display: flex; flex-direction: column; gap: var(--s-1); }
  .menu-name {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .menu-sub { display: flex; }
  .menu-items { display: flex; flex-direction: column; gap: 2px; }
  .menu-item {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: transparent;
    border: none;
    border-radius: var(--r-md);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
    text-align: left;
    text-decoration: none;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                color var(--dur-fast) var(--ease-out);
  }
  .menu-item > span { flex: 1; }
  .menu-item:hover { background: var(--bg-chip); color: var(--fg-primary); }
  .menu-item-danger:hover { background: var(--neg-bg); color: var(--neg); }

  .chip-role[data-role='admin']    { color: var(--critical); background: var(--critical-bg); border-color: color-mix(in srgb, var(--critical) 35%, transparent); }
  .chip-role[data-role='operator'] { color: var(--warn);     background: var(--warn-bg);     border-color: color-mix(in srgb, var(--warn) 35%, transparent); }
  .chip-role[data-role='viewer']   { color: var(--pos);      background: var(--pos-bg);      border-color: color-mix(in srgb, var(--pos) 35%, transparent); }
</style>
