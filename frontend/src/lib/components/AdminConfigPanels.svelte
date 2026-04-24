<script>
  /*
   * UI-7 — admin config panels. One collapsible sub-panel
   * each for the four backend-ready surfaces that had no
   * frontend before: webhooks, alert rules, loans, sentiment
   * headline overrides. Every panel does a minimal list + add
   * round-trip against its `/api/admin/*` endpoint; nothing
   * tries to be a full management console, just enough so the
   * operator can touch these from the UI.
   */
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  // ── Webhooks ──────────────────────────────────────────────
  let webhooks = $state({ url_count: 0, events_sent: 0, events_failed: 0 })
  let webhookUrl = $state('')
  let webhookError = $state('')
  let webhookBusy = $state(false)

  async function refreshWebhooks() {
    try {
      webhooks = await api.getJson('/api/admin/webhooks')
      webhookError = ''
    } catch (e) {
      webhookError = String(e)
    }
  }
  async function addWebhook() {
    if (!webhookUrl) return
    webhookBusy = true
    try {
      webhooks = await api.postJson('/api/admin/webhooks', { url: webhookUrl })
      webhookUrl = ''
      webhookError = ''
    } catch (e) {
      webhookError = String(e)
    } finally {
      webhookBusy = false
    }
  }

  // ── Alert rules ───────────────────────────────────────────
  let alerts = $state([])
  let alertKind = $state('max_spread_bps')
  let alertSymbol = $state('BTCUSDT')
  let alertThreshold = $state(50)
  let alertError = $state('')
  let alertBusy = $state(false)

  async function refreshAlerts() {
    try {
      alerts = await api.getJson('/api/admin/alerts')
      alertError = ''
    } catch (e) {
      alertError = String(e)
    }
  }
  async function addAlert() {
    alertBusy = true
    try {
      const rule = {
        kind: alertKind,
        symbol: alertSymbol,
        threshold: Number(alertThreshold),
      }
      alerts = await api.postJson('/api/admin/alerts', rule)
      alertError = ''
    } catch (e) {
      alertError = String(e)
    } finally {
      alertBusy = false
    }
  }

  // ── Loans ─────────────────────────────────────────────────
  let loans = $state([])
  let loanError = $state('')
  let loanBusy = $state(false)
  let loanSymbol = $state('BTCUSDT')
  let loanClient = $state('default')
  let loanQty = $state(1)
  let loanCost = $state(0)
  let loanApr = $state(10)
  let loanCounterparty = $state('')
  let loanStart = $state('')
  let loanEnd = $state('')

  async function refreshLoans() {
    try {
      loans = await api.getJson('/api/admin/loans')
      loanError = ''
    } catch (e) {
      loanError = String(e)
    }
  }
  async function addLoan() {
    loanBusy = true
    try {
      const req = {
        symbol: loanSymbol,
        client_id: loanClient,
        total_qty: Number(loanQty),
        cost_basis_per_token: Number(loanCost),
        annual_rate_pct: Number(loanApr),
        counterparty: loanCounterparty,
        start_date: loanStart,
        end_date: loanEnd,
        installments: [],
      }
      await api.postJson('/api/admin/loans', req)
      loanError = ''
      await refreshLoans()
    } catch (e) {
      loanError = String(e)
    } finally {
      loanBusy = false
    }
  }

  // ── Sentiment headline override ───────────────────────────
  // Posts to /api/admin/sentiment/headline — the handler lives
  // on the controller now and fans the News override out to
  // every running deployment via per-deployment variables
  // PATCH (the agent translator maps `news` → ConfigOverride::News).
  // Response carries per-deployment dispatch counts.
  let sentimentHeadline = $state('')
  let sentimentError = $state('')
  let sentimentStatus = $state('')
  let sentimentBusy = $state(false)

  async function pushHeadline() {
    if (!sentimentHeadline.trim()) return
    sentimentBusy = true
    try {
      const resp = await api.postJson('/api/admin/sentiment/headline', { text: sentimentHeadline })
      sentimentStatus = `pushed @ ${new Date().toLocaleTimeString()} · recipients: ${resp.recipients ?? 0}`
      sentimentHeadline = ''
      sentimentError = ''
    } catch (e) {
      sentimentError = String(e)
    } finally {
      sentimentBusy = false
    }
  }

  // Lazy-load on mount.
  $effect(() => {
    refreshWebhooks()
    refreshAlerts()
    refreshLoans()
  })
</script>

<div class="acp">
  <!-- Webhooks -->
  <div class="panel">
    <div class="panel-head">
      <span class="name">Webhooks</span>
      <span class="stats">
        {webhooks.url_count} url(s) · {webhooks.events_sent} sent · {webhooks.events_failed} failed
      </span>
    </div>
    <div class="panel-body">
      <div class="row">
        <input
          type="url"
          placeholder="https://example.com/webhook"
          bind:value={webhookUrl}
          disabled={webhookBusy}
        />
        <Button variant="primary" onclick={addWebhook} disabled={webhookBusy || !webhookUrl}>
          {#snippet children()}Add{/snippet}
        </Button>
      </div>
      {#if webhookError}<div class="error">{webhookError}</div>{/if}
      {#if webhooks.url_count === 0}
        <div class="muted small">No webhook URLs registered — alerts won't fan out until you add one.</div>
      {/if}
    </div>
  </div>

  <!-- Alerts -->
  <div class="panel">
    <div class="panel-head">
      <span class="name">Alert rules</span>
      <span class="stats">{alerts.length} rule(s)</span>
    </div>
    <div class="panel-body">
      <div class="row wrap">
        <label class="field">
          <span class="k">kind</span>
          <select bind:value={alertKind} disabled={alertBusy}>
            <option value="max_spread_bps">max_spread_bps</option>
            <option value="min_uptime_pct">min_uptime_pct</option>
            <option value="max_inventory">max_inventory</option>
            <option value="drawdown_quote">drawdown_quote</option>
          </select>
        </label>
        <label class="field">
          <span class="k">symbol</span>
          <input type="text" bind:value={alertSymbol} disabled={alertBusy} />
        </label>
        <label class="field">
          <span class="k">threshold</span>
          <input type="number" bind:value={alertThreshold} disabled={alertBusy} step="0.01" />
        </label>
        <Button variant="primary" onclick={addAlert} disabled={alertBusy || !alertSymbol}>
          {#snippet children()}Add{/snippet}
        </Button>
      </div>
      {#if alertError}<div class="error">{alertError}</div>{/if}
      {#if alerts.length}
        <table class="rules">
          <thead>
            <tr><th>Kind</th><th>Symbol</th><th>Threshold</th></tr>
          </thead>
          <tbody>
            {#each alerts as a, i (i)}
              <tr>
                <td><code>{a.kind}</code></td>
                <td><code>{a.symbol}</code></td>
                <td class="mono">{a.threshold}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <div class="muted small">No alert rules configured — add the first one above.</div>
      {/if}
    </div>
  </div>

  <!-- Loans -->
  <div class="panel">
    <div class="panel-head">
      <span class="name">Loan agreements</span>
      <span class="stats">{loans.length} active</span>
    </div>
    <div class="panel-body">
      <div class="row wrap">
        <label class="field"><span class="k">symbol</span><input type="text" bind:value={loanSymbol} disabled={loanBusy} /></label>
        <label class="field"><span class="k">client</span><input type="text" bind:value={loanClient} disabled={loanBusy} /></label>
        <label class="field"><span class="k">qty</span><input type="number" step="0.0001" bind:value={loanQty} disabled={loanBusy} /></label>
        <label class="field"><span class="k">cost/unit</span><input type="number" step="0.01" bind:value={loanCost} disabled={loanBusy} /></label>
        <label class="field"><span class="k">APR%</span><input type="number" step="0.1" bind:value={loanApr} disabled={loanBusy} /></label>
        <label class="field"><span class="k">counterparty</span><input type="text" bind:value={loanCounterparty} disabled={loanBusy} /></label>
        <label class="field"><span class="k">start</span><input type="date" bind:value={loanStart} disabled={loanBusy} /></label>
        <label class="field"><span class="k">end</span><input type="date" bind:value={loanEnd} disabled={loanBusy} /></label>
        <Button variant="primary" onclick={addLoan} disabled={loanBusy || !loanSymbol}>
          {#snippet children()}Create{/snippet}
        </Button>
      </div>
      {#if loanError}<div class="error">{loanError}</div>{/if}
      {#if loans.length}
        <table class="rules">
          <thead>
            <tr><th>ID</th><th>Symbol</th><th>Qty</th><th>APR</th><th>Status</th></tr>
          </thead>
          <tbody>
            {#each loans.slice(0, 8) as l (l.id)}
              <tr>
                <td class="mono">{l.id.slice(0, 8)}…</td>
                <td><code>{l.symbol}</code></td>
                <td class="mono">{l.terms?.total_qty ?? '—'}</td>
                <td class="mono">{l.terms?.annual_rate_pct ?? '—'}%</td>
                <td><code>{l.status}</code></td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if loans.length > 8}
          <div class="muted small">{loans.length - 8} more loan(s) truncated</div>
        {/if}
      {:else}
        <div class="muted small">No active loan agreements — fill the form above to create one.</div>
      {/if}
    </div>
  </div>

  <!-- Sentiment headline override -->
  <div class="panel">
    <div class="panel-head">
      <span class="name">Sentiment headline</span>
      <span class="stats">push ad-hoc news into every deployment's NewsRetreat</span>
    </div>
    <div class="panel-body">
      <div class="row">
        <input
          type="text"
          placeholder="e.g. SEC approves spot ETF — bull trigger"
          bind:value={sentimentHeadline}
          disabled={sentimentBusy}
        />
        <Button variant="primary" onclick={pushHeadline} disabled={sentimentBusy || !sentimentHeadline.trim()}>
          {#snippet children()}Push{/snippet}
        </Button>
      </div>
      {#if sentimentError}<div class="error">{sentimentError}</div>{/if}
      {#if sentimentStatus}<div class="muted small">{sentimentStatus}</div>{/if}
    </div>
  </div>
</div>

<style>
  .acp { display: flex; flex-direction: column; gap: var(--s-3); }
  .panel {
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .panel-head {
    display: flex; justify-content: space-between; align-items: center;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-xs);
  }
  .name { font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-tight); }
  .stats { color: var(--fg-muted); font-size: var(--fs-2xs); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .panel-body { padding: var(--s-3); display: flex; flex-direction: column; gap: var(--s-2); }
  .row { display: flex; gap: var(--s-2); align-items: flex-end; }
  .row.wrap { flex-wrap: wrap; }
  .field { display: flex; flex-direction: column; gap: 2px; min-width: 100px; }
  .field .k { font-size: var(--fs-2xs); text-transform: uppercase; color: var(--fg-muted); letter-spacing: var(--tracking-label); }
  input, select {
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    padding: 4px var(--s-2);
    font-size: var(--fs-xs);
  }
  input[type=url], input[type=text]:not(.field input), .row > input:first-child {
    flex: 1 1 auto;
  }
  .btn:hover:not(:disabled) { background: var(--accent); color: var(--bg-base); }
  .error { color: var(--danger); font-size: var(--fs-xs); }
  .muted { color: var(--fg-muted); }
  .small { font-size: var(--fs-xs); }
  .rules { width: 100%; border-collapse: collapse; margin-top: var(--s-2); }
  .rules th, .rules td { padding: 4px var(--s-2); font-size: var(--fs-2xs); text-align: left; border-bottom: 1px solid var(--border-subtle); }
  .rules th { color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
