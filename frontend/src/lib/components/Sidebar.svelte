<script>
  import BrandMark from './BrandMark.svelte'
  import Icon from './Icon.svelte'

  let { route = $bindable('overview'), auth, connected = false, mode = 'paper' } = $props()

  const items = [
    { id: 'overview',    label: 'Overview',    icon: 'overview',    roles: ['admin','operator','viewer'] },
    { id: 'orderbook',   label: 'Orderbook',   icon: 'orderbook',   roles: ['admin','operator','viewer'] },
    { id: 'history',     label: 'History',     icon: 'history',     roles: ['admin','operator','viewer'] },
    { id: 'calibration', label: 'Calibration', icon: 'calibration', roles: ['admin','operator'] },
    { id: 'compliance',  label: 'Compliance',  icon: 'compliance',  roles: ['admin','operator'] },
    { id: 'settings',    label: 'Settings',    icon: 'settings',    roles: ['admin','operator'] },
    { id: 'users',       label: 'Users',       icon: 'users',       roles: ['admin'] },
    { id: 'admin',       label: 'Admin',       icon: 'admin',       roles: ['admin'] },
  ]

  const visible = $derived(items.filter(i => i.roles.includes(auth?.state?.role || 'viewer')))

  function go(id) { route = id }
</script>

<aside class="sidebar" class:connected>
  <div class="brand">
    <BrandMark size={22} />
    <span class="brand-name">Market Maker</span>
  </div>

  <nav class="nav">
    {#each visible as item (item.id)}
      <button
        type="button"
        class="nav-item"
        class:active={route === item.id}
        onclick={() => go(item.id)}
        aria-label={item.label}
      >
        <span class="nav-icon"><Icon name={item.icon} size={18} /></span>
        <span class="nav-label">{item.label}</span>
      </button>
    {/each}
  </nav>

  <div class="foot">
    <a class="foot-link" href="https://github.com/iZonex/mg-market-maker" target="_blank" rel="noopener" aria-label="Docs">
      <Icon name="doc" size={16} />
      <span class="foot-label">Docs</span>
    </a>
  </div>
</aside>

<style>
  .sidebar {
    width: 64px;
    height: 100vh;
    position: sticky;
    top: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-raised);
    border-right: 1px solid var(--border-subtle);
    padding: var(--s-4) var(--s-2);
    transition: width var(--dur-base) var(--ease-out);
    z-index: var(--z-nav);
    overflow: hidden;
  }
  .sidebar:hover { width: 220px; }

  .brand {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    margin-bottom: var(--s-6);
    white-space: nowrap;
  }
  .mark {
    display: inline-flex;
    align-items: baseline;
    font-family: var(--font-sans);
    font-size: var(--fs-xl);
    font-weight: 700;
    color: var(--fg-primary);
    line-height: 1;
  }
  .mark-pipe { color: var(--accent); font-weight: 500; margin-left: 1px; }
  .brand-name {
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-secondary);
    white-space: nowrap;
    opacity: 0;
    transition: opacity var(--dur-base) var(--ease-out);
  }
  .sidebar:hover .brand-name { opacity: 1; }

  .nav {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
    flex: 1;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-3);
    background: transparent;
    border: none;
    border-radius: var(--r-lg);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    transition: background var(--dur-fast) var(--ease-out), color var(--dur-fast) var(--ease-out);
    position: relative;
  }
  .nav-item:hover { background: var(--bg-chip); color: var(--fg-primary); }
  .nav-item.active {
    background: var(--accent-dim);
    color: var(--accent);
  }
  .nav-item.active::before {
    content: '';
    position: absolute;
    left: -10px;
    top: 25%; bottom: 25%;
    width: 3px;
    background: var(--accent);
    border-radius: 0 var(--r-pill) var(--r-pill) 0;
  }
  .nav-icon {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
  }
  .nav-label {
    opacity: 0;
    transition: opacity var(--dur-base) var(--ease-out);
  }
  .sidebar:hover .nav-label { opacity: 1; }

  .foot {
    padding: var(--s-3) 0 0;
    border-top: 1px solid var(--border-subtle);
    margin-top: var(--s-3);
  }
  .foot-link {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    color: var(--fg-muted);
    text-decoration: none;
    border-radius: var(--r-md);
    font-size: var(--fs-sm);
    transition: background var(--dur-fast) var(--ease-out),
                color var(--dur-fast) var(--ease-out);
  }
  .foot-link:hover { background: var(--bg-chip); color: var(--fg-primary); }
  .foot-label {
    opacity: 0;
    transition: opacity var(--dur-base) var(--ease-out);
    white-space: nowrap;
  }
  .sidebar:hover .foot-label { opacity: 1; }
</style>
