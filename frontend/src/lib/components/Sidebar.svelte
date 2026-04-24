<script>
  import BrandMark from './BrandMark.svelte'
  import Icon from './Icon.svelte'

  let { route = $bindable('overview'), auth, connected = false, mode = 'paper' } = $props()

  // IA regroup (apr22) — split action vs. watch, lift venue/exec
  // observability out of the old Admin dumping ground, and make
  // Kill-switch / Rules / Calibration first-class nav entries.
  //
  //   Live         — watch-only dashboards.
  //   Operations   — act: deploy, retire, ack, investigate.
  //   Venues       — cross-venue execution quality (was AdminPage cards).
  //   Compliance   — review / surveillance.
  //   Configure    — author: strategy graphs + runtime rules.
  //   Admin        — system: kill-switch, platform, vault, users.
  //   Account      — personal (profile, 2FA).
  const groups = [
    {
      label: 'Live',
      items: [
        { id: 'overview',       label: 'Overview',       icon: 'overview',  roles: ['admin','operator','viewer'] },
        { id: 'orderbook',      label: 'Orderbook',      icon: 'orderbook', roles: ['admin','operator','viewer'] },
        { id: 'history',        label: 'History',        icon: 'history',   roles: ['admin','operator','viewer'] },
      ],
    },
    {
      label: 'Operations',
      items: [
        { id: 'fleet',          label: 'Fleet',          icon: 'pulse',   roles: ['admin','operator','viewer'] },
        { id: 'clients',        label: 'Clients',        icon: 'users',   roles: ['admin','operator','viewer'] },
        { id: 'reconciliation', label: 'Reconciliation', icon: 'shield',  roles: ['admin','operator','viewer'] },
        { id: 'incidents',      label: 'Incidents',      icon: 'alert',   roles: ['admin','operator'] },
      ],
    },
    {
      label: 'Venues & Execution',
      items: [
        { id: 'venues',      label: 'Venues',      icon: 'orderbook', roles: ['admin','operator'] },
        { id: 'calibration', label: 'Calibration', icon: 'graph',     roles: ['admin','operator'] },
      ],
    },
    {
      label: 'Compliance',
      items: [
        { id: 'compliance',   label: 'Compliance',   icon: 'doc',   roles: ['admin','operator','viewer'] },
      ],
    },
    {
      label: 'Configure',
      items: [
        { id: 'strategy', label: 'Strategy', icon: 'graph',    roles: ['admin','operator'] },
        { id: 'rules',    label: 'Rules',    icon: 'settings', roles: ['admin','operator'] },
      ],
    },
    {
      // UX-SURV-1 — standalone Surveillance demoted to admin-only
      // raw-diagnostic view. Operator-facing per-deployment scores
      // live in the drilldown's "Manipulation detectors" section,
      // where they have symbol + kill-level context.
      label: 'Admin',
      items: [
        { id: 'kill-switch',  label: 'Kill switch',  icon: 'alert',    roles: ['admin'] },
        { id: 'platform',     label: 'Platform',     icon: 'settings', roles: ['admin'] },
        { id: 'vault',        label: 'Vault',        icon: 'shield',   roles: ['admin'] },
        { id: 'users',        label: 'Users',        icon: 'users',    roles: ['admin'] },
        { id: 'login-audit',  label: 'Auth audit',   icon: 'history',  roles: ['admin'] },
        { id: 'surveillance', label: 'Surveillance', icon: 'alert',    roles: ['admin'] },
      ],
    },
    {
      label: 'Account',
      items: [
        { id: 'profile', label: 'My account', icon: 'users', roles: ['admin','operator','viewer'] },
      ],
    },
  ]

  const visibleGroups = $derived.by(() => {
    const role = auth?.state?.role || 'viewer'
    return groups
      .map(g => ({
        label: g.label,
        items: g.items.filter(i => i.roles.includes(role)),
      }))
      .filter(g => g.items.length > 0)
  })

  function go(id) { route = id }
</script>

<aside class="sidebar" class:connected>
  <div class="brand">
    <BrandMark size={22} />
    <span class="brand-name">Market Maker</span>
  </div>

  <nav class="nav">
    {#each visibleGroups as group (group.label)}
      <div class="nav-group">
        <div class="nav-group-head">{group.label}</div>
        {#each group.items as item (item.id)}
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
      </div>
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
    gap: var(--s-3);
    flex: 1;
  }
  /* 23-UX-11 grouped nav. Head labels only appear on hover-expand. */
  .nav-group { display: flex; flex-direction: column; gap: var(--s-1); }
  .nav-group-head {
    padding: 0 var(--s-3);
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-faint);
    font-weight: 600;
    opacity: 0;
    transition: opacity var(--dur-base) var(--ease-out);
    height: 1.5em;
  }
  .sidebar:hover .nav-group-head { opacity: 1; }
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
