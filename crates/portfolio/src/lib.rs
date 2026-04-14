//! Multi-currency portfolio tracker.
//!
//! The existing `mm-risk` PnL tracker assumes a single quote
//! currency — fine while we only run `BTCUSDT`, broken the moment we
//! quote `BTCUSDT` and `ETHBTC` at the same time because the second
//! pair's PnL denominates in BTC rather than USDT.
//!
//! This crate tracks positions and realised PnL across multiple base
//! currencies, then converts everything to a single **reporting
//! currency** (typically USDT or USDC) using reference prices the
//! caller supplies.
//!
//! Design:
//!
//! - One [`Portfolio`] owns a map `asset → Position` plus a map of
//!   `asset → reference_price_in_reporting_currency`.
//! - Positions update incrementally on fills; realised and
//!   unrealised PnL are cached snapshots read via `snapshot()`.
//! - Nothing is asynchronous, nothing clocks wall time. Drive it
//!   from the engine's main tick loop.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A single asset position with weighted-average cost basis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    /// Current quantity of the asset (signed; negative = short).
    pub qty: Decimal,
    /// Weighted-average entry price in the asset's native quote
    /// currency (the quote asset that was used to open the position).
    pub avg_entry: Decimal,
    /// Realised PnL in the asset's native quote currency.
    pub realised_pnl_native: Decimal,
}

impl Position {
    /// Apply a fill. `fill_qty` is signed: positive = buy, negative
    /// = sell.
    pub fn apply_fill(&mut self, fill_qty: Decimal, fill_price: Decimal) {
        let new_qty = self.qty + fill_qty;

        let same_sign = !self.qty.is_zero()
            && ((self.qty > Decimal::ZERO) == (fill_qty > Decimal::ZERO));

        if self.qty.is_zero() || same_sign {
            // Opening or increasing the position → recompute
            // weighted-average entry.
            let total_cost = self.avg_entry * self.qty.abs() + fill_price * fill_qty.abs();
            let total_qty = self.qty.abs() + fill_qty.abs();
            self.avg_entry = if total_qty > Decimal::ZERO {
                total_cost / total_qty
            } else {
                Decimal::ZERO
            };
        } else {
            // Reducing or flipping the position → realise PnL on the
            // overlap portion.
            let closed = fill_qty.abs().min(self.qty.abs());
            let direction = if self.qty > Decimal::ZERO {
                Decimal::ONE
            } else {
                Decimal::NEGATIVE_ONE
            };
            let pnl = direction * (fill_price - self.avg_entry) * closed;
            self.realised_pnl_native += pnl;

            if fill_qty.abs() > self.qty.abs() {
                // We crossed zero: the remainder opens a fresh
                // position at the fill price.
                self.avg_entry = fill_price;
            }
            // Otherwise avg_entry stays the same.
        }

        self.qty = new_qty;
        if self.qty.is_zero() {
            self.avg_entry = Decimal::ZERO;
        }
    }
}

/// Snapshot of the full portfolio, denominated in the reporting
/// currency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortfolioSnapshot {
    pub reporting_currency: String,
    pub total_equity: Decimal,
    pub total_realised_pnl: Decimal,
    pub total_unrealised_pnl: Decimal,
    pub per_asset: HashMap<String, AssetSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSnapshot {
    pub qty: Decimal,
    pub avg_entry: Decimal,
    pub mark_price: Decimal,
    pub realised_pnl_native: Decimal,
    pub realised_pnl_reporting: Decimal,
    pub unrealised_pnl_native: Decimal,
    pub unrealised_pnl_reporting: Decimal,
    /// Exchange rate applied when converting native → reporting.
    pub fx_to_reporting: Decimal,
}

pub struct Portfolio {
    reporting_currency: String,
    positions: HashMap<String, Position>,
    /// Mark prices in the position's native quote currency.
    marks_native: HashMap<String, Decimal>,
    /// Conversion factor: 1 unit of the asset's native quote currency
    /// → N units of the reporting currency. For USDT-quoted pairs
    /// where the reporting currency is USDT this stays `1`.
    fx_to_reporting: HashMap<String, Decimal>,
}

impl Portfolio {
    pub fn new(reporting_currency: impl Into<String>) -> Self {
        Self {
            reporting_currency: reporting_currency.into(),
            positions: HashMap::new(),
            marks_native: HashMap::new(),
            fx_to_reporting: HashMap::new(),
        }
    }

    /// Record a fill. `symbol` is the instrument identifier (e.g.
    /// `BTCUSDT`). `qty` is signed.
    pub fn on_fill(&mut self, symbol: &str, qty: Decimal, price: Decimal) {
        let pos = self.positions.entry(symbol.to_string()).or_default();
        pos.apply_fill(qty, price);
    }

    /// Update the current mark price for a symbol, in its own native
    /// quote currency.
    pub fn mark_price(&mut self, symbol: &str, price: Decimal) {
        self.marks_native.insert(symbol.to_string(), price);
    }

    /// Set (or override) the FX conversion factor from the symbol's
    /// native quote currency to the reporting currency. For symbols
    /// already denominated in the reporting currency, the default is
    /// `1` and you don't need to call this.
    pub fn set_fx(&mut self, symbol: &str, factor: Decimal) {
        self.fx_to_reporting.insert(symbol.to_string(), factor);
    }

    pub fn snapshot(&self) -> PortfolioSnapshot {
        let mut per_asset = HashMap::new();
        let mut total_realised = Decimal::ZERO;
        let mut total_unrealised = Decimal::ZERO;

        for (symbol, pos) in &self.positions {
            let mark = self
                .marks_native
                .get(symbol)
                .copied()
                .unwrap_or(pos.avg_entry);
            let fx = self
                .fx_to_reporting
                .get(symbol)
                .copied()
                .unwrap_or(Decimal::ONE);

            let unrealised_native = (mark - pos.avg_entry) * pos.qty;
            let realised_reporting = pos.realised_pnl_native * fx;
            let unrealised_reporting = unrealised_native * fx;

            total_realised += realised_reporting;
            total_unrealised += unrealised_reporting;

            per_asset.insert(
                symbol.clone(),
                AssetSnapshot {
                    qty: pos.qty,
                    avg_entry: pos.avg_entry,
                    mark_price: mark,
                    realised_pnl_native: pos.realised_pnl_native,
                    realised_pnl_reporting: realised_reporting,
                    unrealised_pnl_native: unrealised_native,
                    unrealised_pnl_reporting: unrealised_reporting,
                    fx_to_reporting: fx,
                },
            );
        }

        PortfolioSnapshot {
            reporting_currency: self.reporting_currency.clone(),
            total_equity: total_realised + total_unrealised,
            total_realised_pnl: total_realised,
            total_unrealised_pnl: total_unrealised,
            per_asset,
        }
    }

    pub fn positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn opening_long_sets_avg_entry() {
        let mut p = Position::default();
        p.apply_fill(dec!(1), dec!(100));
        assert_eq!(p.qty, dec!(1));
        assert_eq!(p.avg_entry, dec!(100));
    }

    #[test]
    fn adding_to_long_weights_average() {
        let mut p = Position::default();
        p.apply_fill(dec!(1), dec!(100));
        p.apply_fill(dec!(3), dec!(200));
        assert_eq!(p.qty, dec!(4));
        // (1*100 + 3*200) / 4 = 700/4 = 175
        assert_eq!(p.avg_entry, dec!(175));
    }

    #[test]
    fn reducing_long_realises_pnl() {
        let mut p = Position::default();
        p.apply_fill(dec!(2), dec!(100));
        p.apply_fill(dec!(-1), dec!(150));
        assert_eq!(p.qty, dec!(1));
        // Closed 1 unit at 150 vs avg 100 → +50.
        assert_eq!(p.realised_pnl_native, dec!(50));
        assert_eq!(p.avg_entry, dec!(100));
    }

    #[test]
    fn flipping_long_to_short_realises_and_opens_fresh() {
        let mut p = Position::default();
        p.apply_fill(dec!(2), dec!(100));
        // Sell 3 at 150 → close 2 at +100, then open short 1 at 150.
        p.apply_fill(dec!(-3), dec!(150));
        assert_eq!(p.qty, dec!(-1));
        assert_eq!(p.realised_pnl_native, dec!(100));
        assert_eq!(p.avg_entry, dec!(150));
    }

    #[test]
    fn short_then_cover_realises_positive_pnl_on_drop() {
        let mut p = Position::default();
        p.apply_fill(dec!(-2), dec!(100)); // short 2 at 100
        p.apply_fill(dec!(1), dec!(80));   // cover 1 at 80 → +20
        assert_eq!(p.qty, dec!(-1));
        assert_eq!(p.realised_pnl_native, dec!(20));
    }

    #[test]
    fn closing_to_zero_resets_avg_entry() {
        let mut p = Position::default();
        p.apply_fill(dec!(1), dec!(100));
        p.apply_fill(dec!(-1), dec!(110));
        assert!(p.qty.is_zero());
        assert!(p.avg_entry.is_zero());
        assert_eq!(p.realised_pnl_native, dec!(10));
    }

    /// Walk a three-leg scenario by hand:
    ///
    /// 1. Buy  2 @ 100 → qty = +2, avg = 100.
    /// 2. Sell 3 @ 110 → closes 2 at +10 each (realised +20) then
    ///    flips short: qty = -1, avg = 110.
    /// 3. Buy  1 @ 105 → covers the short: realised += (110-105)×1
    ///    = +5, new realised = +25. qty = 0 → avg resets to 0.
    ///
    /// This is the canonical long-flip-cover sequence used in every
    /// weighted-average cost-basis tutorial (e.g. Natenberg's
    /// *Option Volatility and Pricing*, §"Position Accounting").
    /// Pinning all three intermediate states catches any off-by-one
    /// in the realised-PnL sign, the closed-quantity clamp, or the
    /// avg-entry reset.
    #[test]
    fn canonical_long_flip_cover_scenario() {
        let mut p = Position::default();
        p.apply_fill(dec!(2), dec!(100));
        assert_eq!(p.qty, dec!(2));
        assert_eq!(p.avg_entry, dec!(100));
        assert_eq!(p.realised_pnl_native, dec!(0));

        p.apply_fill(dec!(-3), dec!(110));
        assert_eq!(p.qty, dec!(-1));
        assert_eq!(p.avg_entry, dec!(110));
        assert_eq!(p.realised_pnl_native, dec!(20));

        p.apply_fill(dec!(1), dec!(105));
        assert_eq!(p.qty, dec!(0));
        assert_eq!(p.avg_entry, dec!(0));
        assert_eq!(p.realised_pnl_native, dec!(25));
    }

    #[test]
    fn portfolio_aggregates_multiple_symbols_in_reporting_currency() {
        let mut pf = Portfolio::new("USDT");
        // BTCUSDT: buy 0.1 at 50000, mark at 55000.
        pf.on_fill("BTCUSDT", dec!(0.1), dec!(50000));
        pf.mark_price("BTCUSDT", dec!(55000));
        // ETHBTC: buy 1 at 0.05 (BTC-quoted). Mark at 0.055.
        pf.on_fill("ETHBTC", dec!(1), dec!(0.05));
        pf.mark_price("ETHBTC", dec!(0.055));
        pf.set_fx("ETHBTC", dec!(55000)); // 1 BTC = 55000 USDT at mark

        let snap = pf.snapshot();
        assert_eq!(snap.reporting_currency, "USDT");
        // BTCUSDT unrealised: (55000 - 50000) * 0.1 = 500 USDT
        // ETHBTC unrealised native: (0.055 - 0.05) * 1 = 0.005 BTC
        // ETHBTC unrealised reporting: 0.005 * 55000 = 275 USDT
        assert_eq!(snap.total_unrealised_pnl, dec!(775));
    }

    #[test]
    fn fx_default_is_one() {
        let mut pf = Portfolio::new("USDT");
        pf.on_fill("BTCUSDT", dec!(1), dec!(100));
        pf.mark_price("BTCUSDT", dec!(110));
        let snap = pf.snapshot();
        // (110 - 100) * 1 = 10 USDT.
        assert_eq!(snap.total_unrealised_pnl, dec!(10));
    }

    #[test]
    fn total_equity_is_realised_plus_unrealised() {
        let mut pf = Portfolio::new("USDT");
        pf.on_fill("BTCUSDT", dec!(2), dec!(100));
        pf.on_fill("BTCUSDT", dec!(-1), dec!(110)); // realise +10
        pf.mark_price("BTCUSDT", dec!(120));
        let snap = pf.snapshot();
        // Realised 10, unrealised (120 - 100) * 1 = 20.
        assert_eq!(snap.total_realised_pnl, dec!(10));
        assert_eq!(snap.total_unrealised_pnl, dec!(20));
        assert_eq!(snap.total_equity, dec!(30));
    }
}
