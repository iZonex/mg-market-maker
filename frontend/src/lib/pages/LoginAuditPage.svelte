<script>
  /*
   * Wave H4 — login / logout / password-reset audit UI.
   *
   * Reads the admin `/api/admin/auth/audit` endpoint which
   * surfaces LoginSucceeded / LoginFailed / LogoutSucceeded /
   * PasswordResetIssued / PasswordResetCompleted rows from the
   * shared MiCA audit trail. Displayed newest-first with a
   * substring filter so operators can narrow by user_id or IP
   * without server-side grep.
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const DEFAULT_WINDOW_HOURS = 24
  const WINDOW_OPTIONS = [
    { label: '1 h', hours: 1 },
    { label: '24 h', hours: 24 },
    { label: '7 d', hours: 24 * 7 },
    { label: '30 d', hours: 24 * 30 },
  ]

  let windowHours = $state(DEFAULT_WINDOW_HOURS)
  let limit = $state(200)
  let contains = $state('')
  let rows = $state([])
  let loading = $state(false)
  let error = $state('')

  async function refresh() {
    loading = true
    error = ''
    try {
      const until = Date.now()
      const from = until - windowHours * 60 * 60 * 1000
      const params = new URLSearchParams()
      params.set('from_ms', String(from))
      params.set('until_ms', String(until))
      params.set('limit', String(limit))
      if (contains.trim()) params.set('contains', contains.trim())
      rows = await api.getJson(`/api/admin/auth/audit?${params.toString()}`)
    } catch (e) {
      error = e.message
      rows = []
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 30_000)
    return () => clearInterval(id)
  })

  function typeClass(t) {
    switch (t) {
      case 'login_succeeded':
      case 'password_reset_completed':
        return 'chip-pos'
      case 'login_failed':
        return 'chip-neg'
      case 'logout_succeeded':
        return 'chip-muted'
      case 'password_reset_issued':
        return 'chip-warn'
      default:
        return 'chip-muted'
    }
  }

  function formatTs(ts) {
    if (!ts) return '—'
    try { return new Date(ts).toLocaleString() } catch (_) { return ts }
  }

  const summary = $derived.by(() => {
    const acc = { succeeded: 0, failed: 0, logout: 0, reset_issued: 0, reset_completed: 0 }
    for (const r of rows) {
      switch (r.event_type) {
        case 'login_succeeded': acc.succeeded++; break
        case 'login_failed': acc.failed++; break
        case 'logout_succeeded': acc.logout++; break
        case 'password_reset_issued': acc.reset_issued++; break
        case 'password_reset_completed': acc.reset_completed++; break
      }
    }
    return acc
  })
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Auth audit" subtitle="Login, logout, password-reset events" span={3}>
      {#snippet children()}
        <div class="toolbar">
          <div class="seg">
            {#each WINDOW_OPTIONS as opt (opt.hours)}
              <button
                type="button"
                class="seg-btn"
                class:active={windowHours === opt.hours}
                onclick={() => { windowHours = opt.hours; refresh() }}
              >{opt.label}</button>
            {/each}
          </div>
          <input
            type="text"
            class="text-input"
            placeholder="filter substring (user_id, ip, role)"
            bind:value={contains}
            onkeydown={(e) => { if (e.key === 'Enter') refresh() }}
          />
          <button type="button" class="btn btn-sm btn-ghost" onclick={refresh} disabled={loading}>
            {#if loading}<span class="spinner"></span>Refreshing…{:else}<Icon name="check" size={12} />Refresh{/if}
          </button>
        </div>

        <div class="summary">
          <span class="stat stat-pos" title="LoginSucceeded">{summary.succeeded} ok</span>
          <span class="stat stat-neg" title="LoginFailed">{summary.failed} fail</span>
          <span class="stat stat-muted" title="LogoutSucceeded">{summary.logout} logout</span>
          <span class="stat stat-warn" title="PasswordResetIssued">{summary.reset_issued} issued</span>
          <span class="stat stat-pos" title="PasswordResetCompleted">{summary.reset_completed} reset</span>
        </div>

        {#if error}
          <div class="error-line">
            <Icon name="alert" size={12} />
            <span>{error}</span>
          </div>
        {/if}

        {#if rows.length === 0 && !loading}
          <div class="empty-state" style="padding: var(--s-4)">
            <span class="empty-state-title">No events in this window</span>
            <span class="empty-state-hint">
              Try a wider time window, or clear the filter. Rows
              are read from the MiCA audit file — first-run
              deployments have no history until the first login.
            </span>
          </div>
        {:else}
          <table class="tbl">
            <thead>
              <tr>
                <th>When</th>
                <th>Event</th>
                <th>Detail</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as r (r.seq + ':' + r.timestamp)}
                <tr>
                  <td class="mono">{formatTs(r.timestamp)}</td>
                  <td>
                    <span class="chip {typeClass(r.event_type)}">{r.event_type}</span>
                  </td>
                  <td class="detail mono">{r.detail || ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }

  .toolbar {
    display: flex; flex-wrap: wrap; align-items: center; gap: var(--s-3);
    margin-bottom: var(--s-3);
  }
  .seg { display: inline-flex; border: 1px solid var(--border-subtle); border-radius: var(--r-sm); overflow: hidden; }
  .seg-btn {
    padding: 4px 10px; background: transparent; border: none;
    color: var(--fg-secondary); font-size: var(--fs-2xs); cursor: pointer;
    font-family: var(--font-sans);
  }
  .seg-btn.active { background: var(--bg-chip); color: var(--fg-primary); }
  .text-input {
    flex: 1; min-width: 220px;
    padding: 6px 10px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }

  .summary {
    display: flex; gap: var(--s-2); flex-wrap: wrap;
    margin-bottom: var(--s-3);
  }
  .stat {
    padding: 2px 10px; border-radius: var(--r-pill);
    font-size: var(--fs-2xs); font-weight: 500;
  }
  .stat-pos { background: color-mix(in srgb, var(--pos) 18%, transparent); color: var(--pos); }
  .stat-neg { background: color-mix(in srgb, var(--neg) 18%, transparent); color: var(--neg); }
  .stat-warn { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .stat-muted { background: var(--bg-chip); color: var(--fg-muted); }

  .error-line {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
    color: var(--neg);
    margin-bottom: var(--s-3);
  }

  .mono { font-family: var(--font-mono); font-size: var(--fs-2xs); color: var(--fg-secondary); }
  .detail { word-break: break-all; }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0,0,0,0.25);
    border-top-color: #001510;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
