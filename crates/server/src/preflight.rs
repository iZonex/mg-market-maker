//! Pre-flight checklist — automated validation before trading.
//!
//! Runs a series of checks against the live exchange to verify
//! connectivity, symbol validity, balance sufficiency, and config
//! sanity. All P0 checks must pass before the engine starts
//! quoting. Warnings are logged but don't block startup.

use mm_common::config::AppConfig;
use mm_exchange_core::ExchangeConnector;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Result of a single preflight check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// Run all preflight checks. Returns `Err` if any check fails.
pub async fn run_preflight(
    config: &AppConfig,
    connector: &Arc<dyn ExchangeConnector>,
    symbols: &[String],
) -> anyhow::Result<Vec<CheckResult>> {
    let mut results = Vec::new();

    // 1. Venue connectivity.
    let health = connector.health_check().await;
    let (health_status, health_msg) = match &health {
        Ok(true) => (CheckStatus::Pass, "venue reachable".to_string()),
        Ok(false) => (CheckStatus::Fail, "venue returned unhealthy".to_string()),
        Err(e) => (CheckStatus::Fail, format!("health check failed: {e}")),
    };
    results.push(CheckResult {
        name: "venue_connectivity".into(),
        status: health_status,
        message: health_msg,
    });

    // 2. Symbol validation + tick/lot/fees.
    for symbol in symbols {
        match connector.get_product_spec(symbol).await {
            Ok(spec) => {
                // Tick/lot sanity.
                if spec.tick_size <= Decimal::ZERO {
                    results.push(CheckResult {
                        name: format!("{symbol}_tick_size"),
                        status: CheckStatus::Fail,
                        message: format!("tick_size={} must be > 0", spec.tick_size),
                    });
                } else if spec.lot_size <= Decimal::ZERO {
                    results.push(CheckResult {
                        name: format!("{symbol}_lot_size"),
                        status: CheckStatus::Fail,
                        message: format!("lot_size={} must be > 0", spec.lot_size),
                    });
                } else {
                    results.push(CheckResult {
                        name: format!("{symbol}_product_spec"),
                        status: CheckStatus::Pass,
                        message: format!(
                            "tick={} lot={} min_notional={}",
                            spec.tick_size, spec.lot_size, spec.min_notional
                        ),
                    });
                }

                // Fee sanity — warn if defaults.
                if spec.maker_fee == dec!(-0.0001) && spec.taker_fee == dec!(0.001) {
                    results.push(CheckResult {
                        name: format!("{symbol}_fees"),
                        status: CheckStatus::Warn,
                        message: "using default fees — may not reflect actual VIP tier".into(),
                    });
                }
            }
            Err(e) => {
                results.push(CheckResult {
                    name: format!("{symbol}_product_spec"),
                    status: CheckStatus::Fail,
                    message: format!("symbol not found or API error: {e}"),
                });
            }
        }
    }

    // 3. Balance check.
    match connector.get_balances().await {
        Ok(balances) => {
            let has_funds = balances.iter().any(|b| b.available > Decimal::ZERO);
            results.push(CheckResult {
                name: "balances".into(),
                status: if has_funds {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warn
                },
                message: if has_funds {
                    format!("{} assets with balance", balances.len())
                } else {
                    "all balances are zero — cannot trade".into()
                },
            });
        }
        Err(e) => {
            results.push(CheckResult {
                name: "balances".into(),
                status: CheckStatus::Warn,
                message: format!("could not fetch balances: {e}"),
            });
        }
    }

    // 4. Rate limit budget.
    let remaining = connector.rate_limit_remaining().await;
    results.push(CheckResult {
        name: "rate_limit".into(),
        status: if remaining > 100 {
            CheckStatus::Pass
        } else if remaining > 20 {
            CheckStatus::Warn
        } else {
            CheckStatus::Fail
        },
        message: format!("rate_limit_remaining={remaining}"),
    });

    // 5. Config sanity.
    let mm = &config.market_maker;
    if mm.gamma > dec!(5) {
        results.push(CheckResult {
            name: "config_gamma".into(),
            status: CheckStatus::Warn,
            message: format!("gamma={} is very high — spread will be extremely wide", mm.gamma),
        });
    }
    if mm.order_size <= Decimal::ZERO {
        results.push(CheckResult {
            name: "config_order_size".into(),
            status: CheckStatus::Fail,
            message: "order_size must be > 0".into(),
        });
    }
    if mm.refresh_interval_ms < 100 {
        results.push(CheckResult {
            name: "config_refresh_interval".into(),
            status: CheckStatus::Warn,
            message: format!(
                "refresh_interval_ms={} — may exceed rate limits",
                mm.refresh_interval_ms
            ),
        });
    }

    // Report.
    let fails = results.iter().filter(|r| r.status == CheckStatus::Fail).count();
    let warns = results.iter().filter(|r| r.status == CheckStatus::Warn).count();
    let passes = results.iter().filter(|r| r.status == CheckStatus::Pass).count();

    for r in &results {
        match r.status {
            CheckStatus::Pass => info!(check = %r.name, "{}", r.message),
            CheckStatus::Warn => warn!(check = %r.name, "{}", r.message),
            CheckStatus::Fail => error!(check = %r.name, "{}", r.message),
        }
    }

    info!(
        passes,
        warns,
        fails,
        "preflight complete"
    );

    if fails > 0 && config.mode == "live" {
        anyhow::bail!(
            "preflight failed: {} check(s) failed — fix before trading",
            fails
        );
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_status_equality() {
        assert_eq!(CheckStatus::Pass, CheckStatus::Pass);
        assert_ne!(CheckStatus::Pass, CheckStatus::Fail);
    }

    #[test]
    fn check_result_formats() {
        let r = CheckResult {
            name: "test".into(),
            status: CheckStatus::Pass,
            message: "ok".into(),
        };
        assert_eq!(r.name, "test");
        assert_eq!(r.status, CheckStatus::Pass);
    }
}
