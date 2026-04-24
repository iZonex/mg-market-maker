//! MiCA Article 17 algorithmic trading report (Epic 5 item 5.4).
//!
//! Generates the structured data required for MiCA Article 17
//! compliance: strategy description, OTR statistics, risk
//! controls, and SLA obligations.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;

/// MiCA Article 17 algorithmic trading report.
#[derive(Debug, Clone, Serialize)]
pub struct MicaAlgoReport {
    /// Reporting period start.
    pub period_from: DateTime<Utc>,
    /// Reporting period end.
    pub period_to: DateTime<Utc>,
    /// Strategy type in use.
    pub strategy_description: String,
    /// Order-to-trade ratio statistics for the period.
    pub otr_statistics: OtrPeriodStats,
    /// Risk controls configuration.
    pub risk_controls: RiskControlSummary,
    /// SLA obligation parameters.
    pub sla_obligations: SlaSummary,
    /// HMAC signature for tamper detection.
    pub signature: String,
    /// Report generation timestamp.
    pub generated_at: DateTime<Utc>,
}

/// OTR statistics over the reporting period.
#[derive(Debug, Clone, Serialize)]
pub struct OtrPeriodStats {
    /// Average OTR across the period.
    pub avg_otr: Decimal,
    /// Peak OTR observed.
    pub max_otr: Decimal,
    /// Number of OTR snapshots in the period.
    pub sample_count: u64,
}

/// Summary of risk controls in place.
#[derive(Debug, Clone, Serialize)]
pub struct RiskControlSummary {
    pub daily_loss_limit: Decimal,
    pub max_position_value: Decimal,
    pub max_inventory: Decimal,
    pub kill_switch_levels: u8,
    pub circuit_breaker_enabled: bool,
    pub vpin_threshold: Decimal,
    pub max_spread_bps: Decimal,
}

/// SLA obligation parameters.
#[derive(Debug, Clone, Serialize)]
pub struct SlaSummary {
    pub max_spread_bps: Decimal,
    pub min_depth_quote: Decimal,
    pub min_uptime_pct: Decimal,
    pub two_sided_required: bool,
}

impl MicaAlgoReport {
    /// Build a MiCA report from config and current state.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        period_from: DateTime<Utc>,
        period_to: DateTime<Utc>,
        strategy: &str,
        config: &mm_common::config::AppConfig,
        avg_otr: Decimal,
        max_otr: Decimal,
        otr_samples: u64,
        secret: &str,
    ) -> Self {
        let risk_controls = RiskControlSummary {
            daily_loss_limit: config.kill_switch.daily_loss_limit,
            max_position_value: config.kill_switch.max_position_value,
            max_inventory: config.risk.max_inventory,
            kill_switch_levels: 5,
            circuit_breaker_enabled: true,
            vpin_threshold: config.toxicity.vpin_threshold,
            max_spread_bps: config.risk.max_spread_bps,
        };

        let sla = SlaSummary {
            max_spread_bps: config.sla.max_spread_bps,
            min_depth_quote: config.sla.min_depth_quote,
            min_uptime_pct: config.sla.min_uptime_pct,
            two_sided_required: config.sla.two_sided_required,
        };

        // Build the body first WITHOUT the signature, serialise
        // the full tamper-relevant payload, and sign that. Epic
        // 36.4 — the previous scheme signed four tokens (period
        // × strategy × sample_count) and left `avg_otr`,
        // `max_otr`, risk_controls, sla_obligations unsigned, so
        // a regulator / internal auditor could tamper with
        // anything outside the four signed fields and the
        // signature stayed valid.
        let generated_at = Utc::now();
        let unsigned = UnsignedReport {
            period_from,
            period_to,
            strategy_description: strategy.to_string(),
            otr_statistics: OtrPeriodStats {
                avg_otr,
                max_otr,
                sample_count: otr_samples,
            },
            risk_controls,
            sla_obligations: sla,
            generated_at,
        };
        let body =
            serde_json::to_string(&unsigned).expect("MicaAlgoReport fields are all serde-safe");
        let signature = hmac_sign(secret, &body);

        Self {
            period_from: unsigned.period_from,
            period_to: unsigned.period_to,
            strategy_description: unsigned.strategy_description,
            otr_statistics: unsigned.otr_statistics,
            risk_controls: unsigned.risk_controls,
            sla_obligations: unsigned.sla_obligations,
            signature,
            generated_at: unsigned.generated_at,
        }
    }

    /// Verify the signature covers the full body. Returns `Ok(())`
    /// when the caller-provided secret produces the recorded
    /// signature over the deserialised fields (in the canonical
    /// order used at build time), `Err` otherwise.
    pub fn verify(&self, secret: &str) -> anyhow::Result<()> {
        use subtle::ConstantTimeEq;
        let unsigned = UnsignedReport {
            period_from: self.period_from,
            period_to: self.period_to,
            strategy_description: self.strategy_description.clone(),
            otr_statistics: self.otr_statistics.clone(),
            risk_controls: self.risk_controls.clone(),
            sla_obligations: self.sla_obligations.clone(),
            generated_at: self.generated_at,
        };
        let body = serde_json::to_string(&unsigned)?;
        let expected = hmac_sign(secret, &body);
        if expected
            .as_bytes()
            .ct_eq(self.signature.as_bytes())
            .unwrap_u8()
            == 1
        {
            Ok(())
        } else {
            anyhow::bail!("MiCA report signature verification failed");
        }
    }
}

/// Canonical signed shape: every tamper-relevant field EXCEPT
/// the signature itself. Serialised stably via serde so the
/// sign-then-verify round-trip is deterministic.
#[derive(Serialize)]
struct UnsignedReport {
    period_from: DateTime<Utc>,
    period_to: DateTime<Utc>,
    strategy_description: String,
    otr_statistics: OtrPeriodStats,
    risk_controls: RiskControlSummary,
    sla_obligations: SlaSummary,
    generated_at: DateTime<Utc>,
}

fn hmac_sign(secret: &str, body: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn build_mica_report() {
        let config = mm_common::config::AppConfig::default();
        let report = MicaAlgoReport::build(
            Utc::now() - chrono::Duration::days(30),
            Utc::now(),
            "avellaneda_stoikov",
            &config,
            dec!(5.2),
            dec!(12.1),
            720,
            "test-secret",
        );
        assert_eq!(report.strategy_description, "avellaneda_stoikov");
        assert_eq!(report.otr_statistics.sample_count, 720);
        assert!(!report.signature.is_empty());
    }

    #[test]
    fn risk_controls_from_config() {
        let config = mm_common::config::AppConfig::default();
        let report = MicaAlgoReport::build(
            Utc::now(),
            Utc::now(),
            "grid",
            &config,
            dec!(0),
            dec!(0),
            0,
            "secret",
        );
        assert_eq!(report.risk_controls.kill_switch_levels, 5);
        assert!(report.risk_controls.circuit_breaker_enabled);
    }

    /// Post Epic 36.4 the signature covers `generated_at`, which
    /// is `Utc::now()` at build time — so two sequential builds
    /// yield *different* signatures by construction. The test
    /// still asserts that each build's signature *verifies*
    /// against the same secret; determinism across processes is
    /// no longer the property we care about.
    #[test]
    fn signature_verifies_round_trip() {
        let config = mm_common::config::AppConfig::default();
        let from = Utc::now();
        let to = Utc::now();
        let r = MicaAlgoReport::build(from, to, "test", &config, dec!(0), dec!(0), 0, "key");
        assert!(r.verify("key").is_ok());
        assert!(r.verify("wrong-secret").is_err());
    }

    #[test]
    fn tampered_field_breaks_signature() {
        let config = mm_common::config::AppConfig::default();
        let from = Utc::now();
        let to = Utc::now();
        let mut r = MicaAlgoReport::build(from, to, "test", &config, dec!(0), dec!(0), 0, "key");
        r.otr_statistics.avg_otr = dec!(9999); // tamper outside the 4-token legacy scope
        assert!(
            r.verify("key").is_err(),
            "Epic 36.4: full-body signature must reject tampered avg_otr"
        );
    }
}
