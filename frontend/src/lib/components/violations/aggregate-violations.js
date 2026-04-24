/*
 * Pure aggregator: four fleet streams → one sorted violation list.
 *
 * Keeping this out of the Svelte file makes it testable and
 * trims the panel down to presentation + side-effects.
 *
 * Streams:
 *   - SLA rows (/api/v1/sla)
 *   - fleet rows (/api/v1/fleet)
 *   - reconciliation rows (/api/v1/reconciliation/fleet)
 *   - manipulation rows (/api/v1/surveillance/fleet)
 */

export function aggregateViolations({
  slaRows = [],
  fleetRows = [],
  reconRows = [],
  manipulationRows = [],
}) {
  const out = []

  // Symbol → deployments map so SLA/presence rows can target
  // concrete deployments, not just "the symbol".
  const symbolToDeps = new Map()
  for (const a of fleetRows) {
    for (const d of a.deployments || []) {
      if (!symbolToDeps.has(d.symbol)) symbolToDeps.set(d.symbol, [])
      symbolToDeps.get(d.symbol).push({
        agent_id: a.agent_id, deployment_id: d.deployment_id,
      })
    }
  }

  // SLA uptime + presence breaches (MiCA obligation).
  for (const s of slaRows) {
    const uptime = Number(s.uptime_pct ?? 0)
    const deps = symbolToDeps.get(s.symbol) || []
    if (uptime > 0 && uptime < 95) {
      out.push({
        key: `sla#${s.symbol}`,
        severity: uptime < 90 ? 'high' : 'med',
        category: 'SLA', target: s.symbol,
        metric: `uptime ${uptime.toFixed(2)}%`,
        detail: 'Below 95% presence floor (MiCA obligation).',
        deployments: deps,
      })
    }
    const presence = Number(s.presence_pct_24h ?? 0)
    if (presence > 0 && presence < 95) {
      out.push({
        key: `presence#${s.symbol}`,
        severity: presence < 90 ? 'high' : 'med',
        category: 'presence', target: s.symbol,
        metric: `24h presence ${presence.toFixed(2)}%`,
        detail: 'Per-pair two-sided presence breaches the MiCA rolling window.',
        deployments: deps,
      })
    }
  }

  // Kill-ladder escalations.
  for (const a of fleetRows) {
    for (const d of a.deployments || []) {
      if ((d.kill_level || 0) > 0) {
        out.push({
          key: `kill#${a.agent_id}/${d.deployment_id}`,
          severity: d.kill_level >= 4 ? 'high' : d.kill_level >= 2 ? 'med' : 'low',
          category: 'kill',
          target: `${a.agent_id} · ${d.symbol}`,
          metric: `L${d.kill_level}`,
          detail: 'Kill ladder escalated — strategy is not running normally.',
          deployments: [{ agent_id: a.agent_id, deployment_id: d.deployment_id }],
        })
      }
    }
  }

  // Reconciliation drift.
  for (const r of reconRows) {
    if (!r.has_drift) continue
    const bits = []
    if (r.ghost_orders?.length > 0) bits.push(`${r.ghost_orders.length} ghost`)
    if (r.phantom_orders?.length > 0) bits.push(`${r.phantom_orders.length} phantom`)
    if (r.balance_mismatches?.length > 0) bits.push(`${r.balance_mismatches.length} bal Δ`)
    if (r.orders_fetch_failed) bits.push('fetch fail')
    out.push({
      key: `recon#${r.agent_id}/${r.deployment_id}`,
      severity: r.orders_fetch_failed ? 'high' : 'med',
      category: 'recon',
      target: `${r.agent_id} · ${r.symbol}`,
      metric: bits.join(' · '),
      detail: 'Order/balance reconciliation cycle reported drift this tick.',
      deployments: [{ agent_id: r.agent_id, deployment_id: r.deployment_id }],
    })
  }

  // Manipulation detector escalations.
  for (const m of manipulationRows) {
    const score = Number(m.combined || 0)
    if (score >= 0.8) {
      out.push({
        key: `manip#${m.agent_id}/${m.deployment_id}`,
        severity: score >= 0.95 ? 'high' : 'med',
        category: 'manip',
        target: `${m.agent_id} · ${m.symbol}`,
        metric: `combined ${(score * 100).toFixed(0)}%`,
        detail: 'Manipulation detector score breached alert threshold (0.8).',
        deployments: [{ agent_id: m.agent_id, deployment_id: m.deployment_id }],
      })
    }
  }

  // High severity first; ties break by category then target.
  const rank = { high: 0, med: 1, low: 2 }
  out.sort((a, b) => {
    const r = rank[a.severity] - rank[b.severity]
    if (r !== 0) return r
    return a.category.localeCompare(b.category) || a.target.localeCompare(b.target)
  })
  return out
}
