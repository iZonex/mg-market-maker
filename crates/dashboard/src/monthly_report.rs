//! Aggregator bridging `DashboardState` → `MonthlyReportData`
//! (Epic A1 — operator-triggered MiCA monthly export).
//!
//! The scheduler path in `report_scheduler.rs` delegates to an
//! abstract `ReportJob` trait; nothing in the workspace actually
//! implements that trait yet. Until it does, on-demand HTTP is
//! the only surface that produces a MiCA monthly bundle, so this
//! module owns the aggregation:
//!
//! 1. Per-symbol summaries — roll up `SymbolState.pnl` +
//!    SLA metrics from `DashboardState::get_all` (or
//!    `get_client_state_snapshot` when a client filter is set).
//! 2. Fills — walk the per-client `recent_fills` VecDeque and
//!    filter by period. Note: the in-memory buffer is capped by
//!    `MAX_FILL_HISTORY`; exports longer than the buffer length
//!    fall back to the persistent fill log on disk when
//!    `set_fill_log_source` has been called on the state.
//! 3. Audit events — `mm_risk::audit_reader::read_audit_range`
//!    streams the hash-chained JSONL; no client filter is
//!    applied to the log because cross-client events (kill
//!    switch, global config) belong on every client's bundle.
//!
//! The result plugs straight into the pre-existing
//! `render_csv` / `render_xlsx` / `render_pdf` generators.

use crate::report_export::{
    build_manifest, AuditRow, FillRow, ManifestCounts, MonthlyReportData, ReportManifest,
    SymbolSummaryRow,
};
use crate::state::{DashboardState, FillRecord};
use chrono::{NaiveDate, Utc};
use std::path::Path;

/// Build a MiCA monthly bundle for `[from, to]` (both inclusive,
/// UTC day boundaries). When `client_id` is `None` the bundle
/// contains every symbol the dashboard knows about.
pub fn build_monthly_report(
    state: &DashboardState,
    client_id: Option<&str>,
    client_name: &str,
    from: NaiveDate,
    to: NaiveDate,
    audit_log_path: Option<&Path>,
) -> anyhow::Result<MonthlyReportData> {
    let from_ts = from
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc())
        .unwrap_or_else(Utc::now);
    // `to` is treated as inclusive — bump to end-of-day.
    let to_ts = to
        .and_hms_opt(23, 59, 59)
        .map(|dt| dt.and_utc())
        .unwrap_or_else(Utc::now);
    if to_ts < from_ts {
        anyhow::bail!("period_to must be >= period_from");
    }

    // ── Summaries ───────────────────────────────────────────
    // `SymbolState` only reflects the *current* session —
    // there's no per-period PnL breakdown stored separately, so
    // the export is "state as of generated_at scoped to
    // symbols matching the client". Documented as such.
    let all_symbols = state.get_all();
    let mut summaries = Vec::with_capacity(all_symbols.len());
    for s in &all_symbols {
        if let Some(cid) = client_id {
            if state.get_client_for_symbol(&s.symbol).as_deref() != Some(cid) {
                continue;
            }
        }
        summaries.push(SymbolSummaryRow {
            symbol: s.symbol.clone(),
            total_pnl: s.pnl.total,
            spread_pnl: s.pnl.spread,
            inventory_pnl: s.pnl.inventory,
            fees_paid: s.pnl.fees,
            rebates_earned: s.pnl.rebates,
            round_trips: s.pnl.round_trips,
            total_volume: s.pnl.volume,
            presence_pct: s.presence_pct_24h,
            two_sided_pct: s.two_sided_pct_24h,
            uptime_pct: s.sla_uptime_pct,
            sla_violations: 0,
        });
    }

    // ── Fills ───────────────────────────────────────────────
    let raw_fills = match client_id {
        Some(cid) => state.get_client_fills(cid, usize::MAX),
        None => state.get_recent_fills(None, usize::MAX),
    };
    let fills: Vec<FillRow> = raw_fills
        .into_iter()
        .filter(|f| f.timestamp >= from_ts && f.timestamp <= to_ts)
        .map(fill_record_to_row)
        .collect();

    // ── Audit events ────────────────────────────────────────
    // Distributed path: when a fleet audit fetcher is wired
    // (server/main.rs hooks one up when both controller and
    // dashboard run in the same process), call it on the
    // current runtime to fan out across every deployment's
    // audit JSONL and merge the results. Falls back to the
    // local-file reader when no fetcher is set — preserves
    // standalone test / single-process dev flows.
    let audit_events = if let Some(fetcher) = state.audit_range_fetcher() {
        let from_ms = from_ts.timestamp_millis();
        let until_ms = to_ts.timestamp_millis();
        // Cap the per-request limit — MiCA months can have
        // hundreds of thousands of events in busy deployments,
        // but a single run shouldn't chew hours of engine time.
        // Controller caps at 5000 per deployment via the
        // `audit_tail` topic handler; the fleet merge can exceed
        // that across deployments. 50 000 is a generous global
        // cap that still keeps the in-memory JSON manageable.
        const FLEET_AUDIT_LIMIT: usize = 50_000;
        let events = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // We're inside a tokio runtime (HTTP handler path).
                // `block_in_place` keeps the runtime responsive
                // while we await on the fan-out future — the
                // fetcher spins out independent tokio tasks per
                // agent, so this doesn't deadlock.
                tokio::task::block_in_place(|| {
                    handle.block_on(fetcher(from_ms, until_ms, FLEET_AUDIT_LIMIT))
                })
            }
            Err(_) => Vec::new(),
        };
        events
            .into_iter()
            .filter_map(|v| serde_json::from_value::<mm_risk::audit::AuditEvent>(v).ok())
            .map(audit_event_to_row)
            .collect()
    } else {
        match audit_log_path {
            Some(p) if p.exists() => {
                let events =
                    mm_risk::audit_reader::read_audit_range(p, from_ts, to_ts).unwrap_or_default();
                events.into_iter().map(audit_event_to_row).collect()
            }
            _ => Vec::new(),
        }
    };

    Ok(MonthlyReportData {
        client_id: client_id.unwrap_or("all").to_string(),
        client_name: client_name.to_string(),
        period_from: from,
        period_to: to,
        generated_at: Utc::now(),
        summaries,
        fills,
        audit_events,
    })
}

/// Convenience — compute a manifest without mutating the data.
pub fn manifest_for(
    data: &MonthlyReportData,
    formats: &[&str],
    secret: &[u8],
) -> anyhow::Result<ReportManifest> {
    build_manifest(data, formats, secret)
}

fn fill_record_to_row(f: FillRecord) -> FillRow {
    FillRow {
        timestamp: f.timestamp,
        client_id: f.client_id.unwrap_or_else(|| "default".into()),
        symbol: f.symbol,
        side: f.side,
        price: f.price,
        qty: f.qty,
        fee: f.fee,
        is_maker: f.is_maker,
        slippage_bps: f.slippage_bps,
    }
}

fn audit_event_to_row(e: mm_risk::audit::AuditEvent) -> AuditRow {
    AuditRow {
        timestamp: e.timestamp,
        seq: e.seq,
        event_type: format!("{:?}", e.event_type),
        symbol: e.symbol,
        client_id: e.client_id,
        detail: e.detail,
        prev_hash: e.prev_hash,
    }
}

// Manifest-count helper preserved for external callers that want
// the count block without re-deriving from the data struct.
pub fn counts_of(data: &MonthlyReportData) -> ManifestCounts {
    ManifestCounts {
        summaries: data.summaries.len(),
        fills: data.fills.len(),
        audit_events: data.audit_events.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_produces_empty_report() {
        let state = DashboardState::new();
        let from = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let r = build_monthly_report(&state, None, "Unassigned", from, to, None).expect("build ok");
        assert_eq!(r.client_id, "all");
        assert!(r.summaries.is_empty());
        assert!(r.fills.is_empty());
        assert!(r.audit_events.is_empty());
        assert_eq!(r.period_from, from);
        assert_eq!(r.period_to, to);
    }

    #[test]
    fn inverted_period_rejected() {
        let state = DashboardState::new();
        let from = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let err = build_monthly_report(&state, None, "x", from, to, None).unwrap_err();
        assert!(err.to_string().contains("period_to must be >="));
    }
}
