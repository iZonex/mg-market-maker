//! Node catalog — closed set of built-in `NodeKind` implementations.
//!
//! One file per category for navigability. Phase 1 ships just enough
//! to prove the architecture end-to-end:
//!
//!   math   — `Math.Add`
//!   sinks  — `Out.SpreadMult`, `Out.SizeMult`, `Out.KillEscalate`
//!
//! Phases 2–4 fill in the rest of the catalog from the architecture
//! doc.

pub mod exec;
pub mod indicators;
pub mod logic;
pub mod math;
pub mod plan;
pub mod quotes;
pub mod risk;
pub mod sinks;
pub mod sources;
pub mod stats;
pub mod strategies;

use rust_decimal::Decimal;
use std::str::FromStr;

/// Parse one optional decimal-valued config field — the shared
/// scaffolding behind the catalog's `from_config` builders.
///
/// `None` (field absent) yields `default`. A present value is parsed
/// from its string form and kept only if `valid` returns `true`.
/// Returns `None` — config rejected — on a parse failure or a value
/// the predicate rejects, matching the `from_config` contract.
pub(crate) fn decimal_field(
    raw: Option<String>,
    default: Decimal,
    valid: impl Fn(Decimal) -> bool,
) -> Option<Decimal> {
    match raw {
        None => Some(default),
        Some(s) => {
            let v = Decimal::from_str(&s).ok()?;
            valid(v).then_some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decimal_field;
    use rust_decimal_macros::dec;

    #[test]
    fn decimal_field_default_parse_and_reject() {
        // Absent → default.
        assert_eq!(decimal_field(None, dec!(0.1), |_| true), Some(dec!(0.1)));
        // Present + valid → parsed value.
        assert_eq!(
            decimal_field(Some("0.5".into()), dec!(0.1), |v| v > dec!(0)),
            Some(dec!(0.5))
        );
        // Present but the predicate rejects → None.
        assert_eq!(
            decimal_field(Some("-1".into()), dec!(0.1), |v| v > dec!(0)),
            None
        );
        // Unparseable → None.
        assert_eq!(decimal_field(Some("abc".into()), dec!(0.1), |_| true), None);
    }
}
