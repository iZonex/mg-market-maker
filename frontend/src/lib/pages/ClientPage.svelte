<script>
  /*
   * Wave B4 — per-client drilldown page.
   *
   * Joins data from several sources:
   *   - /api/admin/clients                  — registered client directory
   *   - /api/v1/client/{id}/pnl             — fleet-aggregated PnL
   *   - /api/v1/client/{id}/sla             — fleet-aggregated SLA
   *   - /api/v1/client/{id}/sla/certificate — signed SLA certificate
   *   - /api/v1/fleet + /api/v1/approvals   — which agents carry
   *                                           this tenant (profile.client_id)
   *
   * Cards (components/client/*) are presentational; this file
   * owns the fetch loops + join.
   */
  import Card from '../components/Card.svelte'
  import EmptyStateGuide from '../components/EmptyStateGuide.svelte'
  import ClientOnboardingPanel from '../components/ClientOnboardingPanel.svelte'
  import ClientsListCard from '../components/client/ClientsListCard.svelte'
  import ClientPnlCard from '../components/client/ClientPnlCard.svelte'
  import ClientSlaCard from '../components/client/ClientSlaCard.svelte'
  import ClientInviteCard from '../components/client/ClientInviteCard.svelte'
  import WebhookDeliveriesCard from '../components/client/WebhookDeliveriesCard.svelte'
  import TenantAgentsCard from '../components/client/TenantAgentsCard.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 5000

  let clients = $state([])
  let selected = $state(null)
  let pnl = $state(null)
  let sla = $state(null)
  let cert = $state(null)
  let webhookDeliveries = $state([])
  let webhookTestBusy = $state(false)
  let webhookTestStatus = $state(null)
  let invite = $state(null)
  let inviteBusy = $state(false)
  let fleet = $state([])
  let approvals = $state([])
  let error = $state(null)
  let loading = $state(true)

  async function refreshClients() {
    try {
      const [list, f, a] = await Promise.all([
        api.getJson('/api/admin/clients'),
        api.getJson('/api/v1/fleet'),
        api.getJson('/api/v1/approvals').catch(() => []),
      ])
      clients = Array.isArray(list) ? list : []
      fleet = Array.isArray(f) ? f : []
      approvals = Array.isArray(a) ? a : []
      if (!selected && clients.length > 0) {
        selected = clients[0].client_id || clients[0].id
      }
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  async function refreshSelected() {
    if (!selected) {
      pnl = null; sla = null; cert = null; webhookDeliveries = []
      return
    }
    try {
      const [p, s, c, wh] = await Promise.all([
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/pnl`).catch(() => null),
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/sla`).catch(() => null),
        api.getJson(`/api/v1/client/${encodeURIComponent(selected)}/sla/certificate`).catch(() => null),
        api.getJson(`/api/admin/clients/${encodeURIComponent(selected)}/webhooks/deliveries`).catch(() => ({ deliveries: [] })),
      ])
      pnl = p; sla = s; cert = c
      webhookDeliveries = wh?.deliveries || []
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  async function generateInvite() {
    if (!selected || inviteBusy) return
    inviteBusy = true
    try {
      const r = await api.authedFetch(`/api/admin/clients/${encodeURIComponent(selected)}/invite`, {
        method: 'POST',
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json()
      invite = {
        url: `${window.location.origin}${body.invite_url}`,
        token: body.invite_token,
        expires_at: body.expires_at,
      }
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      inviteBusy = false
    }
  }

  async function copyInvite() {
    if (!invite) return
    await navigator.clipboard.writeText(invite.url).catch(() => {})
  }

  async function testWebhooks() {
    if (!selected || webhookTestBusy) return
    webhookTestBusy = true
    webhookTestStatus = { phase: 'pending', text: 'firing test payloads…' }
    try {
      const r = await api.authedFetch(`/api/admin/clients/${encodeURIComponent(selected)}/webhooks/test`, {
        method: 'POST',
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json()
      webhookTestStatus = {
        phase: body.attempted === 0 ? 'info' : body.succeeded === body.attempted ? 'ok' : 'warn',
        text: body.attempted === 0
          ? 'No webhook URLs configured for this client.'
          : `${body.succeeded}/${body.attempted} URLs acknowledged`,
      }
      await refreshSelected()
    } catch (e) {
      webhookTestStatus = { phase: 'err', text: e?.message || String(e) }
    } finally {
      webhookTestBusy = false
    }
  }

  $effect(() => {
    refreshClients()
    const iv = setInterval(refreshClients, REFRESH_MS)
    return () => clearInterval(iv)
  })

  $effect(() => {
    // Re-fetch whenever selection changes.
    selected
    refreshSelected()
    const iv = setInterval(refreshSelected, REFRESH_MS)
    return () => clearInterval(iv)
  })

  // Approvals join: find every accepted agent whose
  // profile.client_id matches the selected tenant. Fleet row
  // lookup by agent_id gives us deployment counts + live state.
  const tenantAgents = $derived.by(() => {
    if (!selected) return []
    const fpByClient = new Map()
    for (const a of approvals || []) {
      if (a.profile?.client_id === selected) {
        fpByClient.set(a.fingerprint, a)
      }
    }
    return (fleet || [])
      .filter((a) => fpByClient.has(a.pubkey_fingerprint))
      .map((a) => ({ ...a, approval: fpByClient.get(a.pubkey_fingerprint) || null }))
  })
</script>

<div class="page scroll">
  {#if !loading && clients.length === 0}
    <EmptyStateGuide
      title="No clients registered yet"
      message="Tenants are created once per business relationship. Each client gets their own PnL bucket, SLA tracking, webhook delivery log, and portal login."
      steps={[
        {
          title: 'Register the client',
          description: 'Use the "Register client" card below. Enter client_id, name, symbols they trade, jurisdiction, and webhook URLs.',
        },
        {
          title: 'Tag credentials with client_id',
          description: 'Vault credentials have an optional client_id — only agents tagged for this client can receive them. Cross-tenant leak is gated at deploy time.',
          action: { label: 'Open Vault', route: 'vault' },
        },
        {
          title: 'Generate a portal invite',
          description: 'Select the client on this page and click "Generate invite URL". The single-use 24h link lets them sign up for a ClientReader account and see their own data only.',
        },
      ]}
      {onNavigate}
    />
    <div class="grid onboard-only">
      <Card title="Register client" subtitle="jurisdiction-gated onboarding" span={3}>
        {#snippet children()}<ClientOnboardingPanel {auth} />{/snippet}
      </Card>
    </div>
  {:else}
    <div class="grid">
      <ClientsListCard
        {clients}
        {selected}
        {loading}
        {error}
        onSelect={(id) => (selected = id)}
      />

      {#if selected}
        <ClientPnlCard {selected} {pnl} />
        <ClientSlaCard {sla} {cert} />
        <ClientInviteCard
          {selected}
          {invite}
          busy={inviteBusy}
          onGenerate={generateInvite}
          onCopy={copyInvite}
        />
        <WebhookDeliveriesCard
          deliveries={webhookDeliveries}
          testBusy={webhookTestBusy}
          testStatus={webhookTestStatus}
          onTest={testWebhooks}
        />
        <Card title="Register new client" subtitle="jurisdiction-gated onboarding" span={3}>
          {#snippet children()}<ClientOnboardingPanel {auth} />{/snippet}
        </Card>
        <TenantAgentsCard {selected} agents={tenantAgents} />
      {/if}
    </div>
  {/if}
</div>

<style>
  .page { padding: var(--s-4); }
  .scroll { overflow-y: auto; }
  .grid {
    display: grid;
    grid-template-columns: minmax(240px, 1fr) repeat(2, minmax(0, 1fr));
    gap: var(--s-3);
  }
  .grid.onboard-only { margin-top: var(--s-3); }
</style>
