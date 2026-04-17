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
use rust_decimal_macros::dec;
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

        let same_sign =
            !self.qty.is_zero() && ((self.qty > Decimal::ZERO) == (fill_qty > Decimal::ZERO));

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
    /// Per-factor (base / quote asset) delta roll-up, seeded
    /// from the registered symbol→(base, quote) map. Epic C
    /// sub-component #1.
    #[serde(default)]
    pub per_factor: Vec<(String, Decimal)>,
    /// Per-strategy realised PnL in the reporting currency,
    /// keyed by `Strategy::name()`. Epic C sub-component #2.
    #[serde(default)]
    pub per_strategy: Vec<(String, Decimal)>,
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
    /// Symbol → (base asset, quote asset) map seeded by
    /// `register_symbol`. Used by [`Self::factor_delta`] and
    /// [`Self::factors`] (P2.3 Epic C sub-component #1) to
    /// aggregate per-base-asset exposure across symbols. A
    /// symbol with no registration contributes nothing to the
    /// factor view (best-effort — the old per-symbol snapshot
    /// path still works for unregistered symbols).
    symbol_assets: HashMap<String, (String, String)>,
    /// Per-strategy-class PnL accumulator (Epic C #2). Keys
    /// are owned `String`s because `Strategy::name()` returns
    /// `&str` not `&'static str`. Allocation only happens on
    /// the first fill of a new strategy class — subsequent
    /// fills on the same class re-hash and reuse the existing
    /// entry.
    per_strategy_pnl: HashMap<String, Decimal>,
    /// Dust threshold for the factor iterator. Factors with
    /// absolute delta below this are pruned from the
    /// [`Self::factors`] output so the dashboard doesn't show
    /// micro-residuals from fee rounding.
    dust_threshold: Decimal,
}

impl Portfolio {
    pub fn new(reporting_currency: impl Into<String>) -> Self {
        Self {
            reporting_currency: reporting_currency.into(),
            positions: HashMap::new(),
            marks_native: HashMap::new(),
            fx_to_reporting: HashMap::new(),
            symbol_assets: HashMap::new(),
            per_strategy_pnl: HashMap::new(),
            dust_threshold: dec!(0.00000001),
        }
    }

    /// Seed the per-factor aggregation with the (base, quote)
    /// asset decomposition for a symbol. Engine calls this once
    /// at startup from its `ProductSpec`. Idempotent — re-seeding
    /// with the same tuple is a no-op; re-seeding with a different
    /// tuple overwrites.
    ///
    /// Without this call a symbol still participates in PnL
    /// aggregation (via the old `per_asset` snapshot path) but
    /// contributes **nothing** to `factor_delta` / `factors`
    /// because the aggregator doesn't know which factor the
    /// symbol's base leg belongs to.
    pub fn register_symbol(&mut self, symbol: &str, base: &str, quote: &str) {
        self.symbol_assets
            .insert(symbol.to_string(), (base.to_string(), quote.to_string()));
    }

    /// Record a fill. `symbol` is the instrument identifier (e.g.
    /// `BTCUSDT`). `qty` is signed. `strategy_class` is the
    /// tag from `Strategy::name()` that tells the portfolio
    /// which strategy-level bucket the PnL belongs to (Epic C
    /// #2). Pass `"unclassified"` from test code or legacy
    /// call sites that do not know the strategy.
    pub fn on_fill(&mut self, symbol: &str, qty: Decimal, price: Decimal, strategy_class: &str) {
        let pos = self.positions.entry(symbol.to_string()).or_default();
        let prior_realised = pos.realised_pnl_native;
        pos.apply_fill(qty, price);
        // Strategy-class attribution only tracks the *realised*
        // PnL delta from this fill — unrealised MTM is still
        // reported per-symbol via `snapshot()`. For pure
        // opening fills the realised delta is zero and the
        // strategy-class bucket sees nothing; for closing fills
        // the delta equals the realised PnL on the overlap.
        let realised_delta = pos.realised_pnl_native - prior_realised;
        if !realised_delta.is_zero() {
            // Apply FX to the strategy PnL the same way the
            // snapshot path does — realised PnL is in the
            // symbol's native quote currency, we report in the
            // reporting currency.
            let fx = self
                .fx_to_reporting
                .get(symbol)
                .copied()
                .unwrap_or(Decimal::ONE);
            *self
                .per_strategy_pnl
                .entry(strategy_class.to_string())
                .or_insert(Decimal::ZERO) += realised_delta * fx;
        }
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
            per_factor: self.factors(),
            per_strategy: self.per_strategy_sorted(),
        }
    }

    pub fn positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }

    /// Per-factor delta for the named base / quote asset. Sums
    /// contributions from every registered symbol:
    ///
    /// - A symbol whose **base asset** is `asset` contributes
    ///   `+qty` (a long BTCUSDT contributes +BTC).
    /// - A symbol whose **quote asset** is `asset` contributes
    ///   `-qty · mark_native` (a long ETHBTC implicitly sells
    ///   BTC equal to the notional of the ETH leg, so it
    ///   contributes a negative BTC delta proportional to the
    ///   ETH mark).
    ///
    /// Symbols without a prior `register_symbol` call are
    /// silently skipped — the aggregator only sees registered
    /// symbols.
    pub fn factor_delta(&self, asset: &str) -> Decimal {
        let mut total = Decimal::ZERO;
        for (symbol, pos) in &self.positions {
            let Some((base, quote)) = self.symbol_assets.get(symbol) else {
                continue;
            };
            if base == asset {
                total += pos.qty;
            }
            if quote == asset {
                let mark = self
                    .marks_native
                    .get(symbol)
                    .copied()
                    .unwrap_or(pos.avg_entry);
                total -= pos.qty * mark;
            }
        }
        total
    }

    /// Iterator-style access to every non-dust factor delta.
    /// Returns a vector of `(asset, delta)` pairs pruned by the
    /// `dust_threshold`. Used by the dashboard daily report so
    /// operators see an honest per-factor view instead of
    /// noise-level residuals.
    pub fn factors(&self) -> Vec<(String, Decimal)> {
        // Collect the distinct set of base + quote assets across
        // registered symbols.
        let mut assets: Vec<String> = Vec::new();
        for (base, quote) in self.symbol_assets.values() {
            if !assets.iter().any(|a| a == base) {
                assets.push(base.clone());
            }
            if !assets.iter().any(|a| a == quote) {
                assets.push(quote.clone());
            }
        }
        let mut out = Vec::new();
        for asset in assets {
            let delta = self.factor_delta(&asset);
            if delta.abs() >= self.dust_threshold {
                out.push((asset, delta));
            }
        }
        // Deterministic order for the daily report.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Read-only view of the per-strategy PnL accumulator.
    /// Returns strategy-class tag → realised PnL (in the
    /// reporting currency) since engine start. Keys are
    /// whatever `Strategy::name()` implementations return,
    /// plus the literal `"unclassified"` bucket used by
    /// legacy and test call sites.
    pub fn per_strategy_pnl(&self) -> &HashMap<String, Decimal> {
        &self.per_strategy_pnl
    }

    /// Iterator-style access to the per-strategy PnL, sorted by
    /// strategy class for deterministic daily-report output.
    pub fn per_strategy_sorted(&self) -> Vec<(String, Decimal)> {
        let mut v: Vec<(String, Decimal)> = self
            .per_strategy_pnl
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
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
        p.apply_fill(dec!(1), dec!(80)); // cover 1 at 80 → +20
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

    const UNCLASSIFIED: &str = "unclassified";

    #[test]
    fn portfolio_aggregates_multiple_symbols_in_reporting_currency() {
        let mut pf = Portfolio::new("USDT");
        // BTCUSDT: buy 0.1 at 50000, mark at 55000.
        pf.on_fill("BTCUSDT", dec!(0.1), dec!(50000), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(55000));
        // ETHBTC: buy 1 at 0.05 (BTC-quoted). Mark at 0.055.
        pf.on_fill("ETHBTC", dec!(1), dec!(0.05), UNCLASSIFIED);
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
        pf.on_fill("BTCUSDT", dec!(1), dec!(100), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(110));
        let snap = pf.snapshot();
        // (110 - 100) * 1 = 10 USDT.
        assert_eq!(snap.total_unrealised_pnl, dec!(10));
    }

    #[test]
    fn total_equity_is_realised_plus_unrealised() {
        let mut pf = Portfolio::new("USDT");
        pf.on_fill("BTCUSDT", dec!(2), dec!(100), UNCLASSIFIED);
        pf.on_fill("BTCUSDT", dec!(-1), dec!(110), UNCLASSIFIED); // realise +10
        pf.mark_price("BTCUSDT", dec!(120));
        let snap = pf.snapshot();
        // Realised 10, unrealised (120 - 100) * 1 = 20.
        assert_eq!(snap.total_realised_pnl, dec!(10));
        assert_eq!(snap.total_unrealised_pnl, dec!(20));
        assert_eq!(snap.total_equity, dec!(30));
    }

    // ---- Epic C sub-component #1: per-factor delta tests ----

    /// Single USDT-quoted long contributes +qty to the base
    /// factor and a negative USDT number to the quote factor
    /// (the implicit short of the reporting currency). The
    /// reporting currency side is reported for completeness —
    /// operators can filter it client-side.
    #[test]
    fn factor_delta_single_usdt_quoted_long() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(0.5), dec!(50000), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(50000));
        assert_eq!(pf.factor_delta("BTC"), dec!(0.5));
        // Quote leg: long 0.5 BTC at 50000 sells 25_000 USDT.
        assert_eq!(pf.factor_delta("USDT"), dec!(-25000));
    }

    /// Two BTC-contributing symbols (BTCUSDT long + ETHBTC long)
    /// must aggregate additively. ETHBTC long contributes
    /// +ETH to the ETH factor AND a negative BTC contribution
    /// (-qty · mark) to the BTC factor because the ETHBTC
    /// quote leg is an implicit BTC short.
    #[test]
    fn factor_delta_cross_quote_aggregates_correctly() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.register_symbol("ETHBTC", "ETH", "BTC");
        pf.on_fill("BTCUSDT", dec!(1), dec!(50000), UNCLASSIFIED);
        pf.on_fill("ETHBTC", dec!(10), dec!(0.05), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(50000));
        pf.mark_price("ETHBTC", dec!(0.05));
        // ETH factor = +10 from the ETHBTC base leg.
        assert_eq!(pf.factor_delta("ETH"), dec!(10));
        // BTC factor = +1 from BTCUSDT base leg, and -0.5 from
        // the ETHBTC quote leg (10 ETH × 0.05 BTC/ETH = 0.5 BTC
        // implicitly sold). Net = +0.5.
        assert_eq!(pf.factor_delta("BTC"), dec!(0.5));
    }

    /// `factors()` iterator returns every distinct factor from
    /// the registered symbols, skips dust, and sorts
    /// deterministically by asset name.
    #[test]
    fn factors_iterator_is_sorted_and_dust_pruned() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.register_symbol("ETHUSDT", "ETH", "USDT");
        pf.on_fill("BTCUSDT", dec!(0.5), dec!(50000), UNCLASSIFIED);
        pf.on_fill("ETHUSDT", dec!(2), dec!(3000), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(50000));
        pf.mark_price("ETHUSDT", dec!(3000));
        let factors = pf.factors();
        // Deterministic alphabetical order.
        let keys: Vec<String> = factors.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, vec!["BTC", "ETH", "USDT"]);
        assert_eq!(factors[0], ("BTC".to_string(), dec!(0.5)));
        assert_eq!(factors[1], ("ETH".to_string(), dec!(2)));
        // USDT = -(0.5 × 50000 + 2 × 3000) = -31_000
        assert_eq!(factors[2], ("USDT".to_string(), dec!(-31000)));
    }

    /// Unregistered symbols are silently skipped by the
    /// per-factor aggregator. The old per-symbol PnL path
    /// still works for them via `snapshot()` — the factor
    /// view just doesn't see them.
    #[test]
    fn factor_delta_ignores_unregistered_symbols() {
        let mut pf = Portfolio::new("USDT");
        // Register BTCUSDT but NOT ETHUSDT.
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(0.5), dec!(50000), UNCLASSIFIED);
        pf.on_fill("ETHUSDT", dec!(2), dec!(3000), UNCLASSIFIED); // unregistered
        pf.mark_price("BTCUSDT", dec!(50000));
        pf.mark_price("ETHUSDT", dec!(3000));
        assert_eq!(pf.factor_delta("BTC"), dec!(0.5));
        // ETH factor gets nothing — ETHUSDT was never registered.
        assert_eq!(pf.factor_delta("ETH"), dec!(0));
        // USDT factor only reflects BTCUSDT, not ETHUSDT.
        assert_eq!(pf.factor_delta("USDT"), dec!(-25000));
    }

    /// `register_symbol` is idempotent: calling it twice with
    /// the same tuple is a no-op, calling it with a new tuple
    /// overwrites. Catches the regression where a late
    /// re-registration would double-count a factor.
    #[test]
    fn register_symbol_is_idempotent_and_overwrites() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT"); // idempotent
        pf.on_fill("BTCUSDT", dec!(1), dec!(50000), UNCLASSIFIED);
        assert_eq!(pf.factor_delta("BTC"), dec!(1));

        // Now overwrite with a different classification — the
        // contributions flip immediately.
        pf.register_symbol("BTCUSDT", "BTC2", "USDT");
        assert_eq!(pf.factor_delta("BTC"), dec!(0));
        assert_eq!(pf.factor_delta("BTC2"), dec!(1));
    }

    /// Signed cancellation: long BTCUSDT on venue A + short
    /// BTCUSDT on venue B nets to zero BTC-delta. Modelled as
    /// two separate symbols that both resolve to the BTC
    /// factor.
    #[test]
    fn factor_delta_signed_cancellation_across_venues() {
        let mut pf = Portfolio::new("USDT");
        // "BINANCE:BTCUSDT" and "BYBIT:BTCUSDT" — distinct
        // symbol strings, both map to BTC/USDT factors.
        pf.register_symbol("BINANCE:BTCUSDT", "BTC", "USDT");
        pf.register_symbol("BYBIT:BTCUSDT", "BTC", "USDT");
        pf.on_fill("BINANCE:BTCUSDT", dec!(1), dec!(50000), UNCLASSIFIED);
        pf.on_fill("BYBIT:BTCUSDT", dec!(-1), dec!(50000), UNCLASSIFIED);
        pf.mark_price("BINANCE:BTCUSDT", dec!(50000));
        pf.mark_price("BYBIT:BTCUSDT", dec!(50000));
        assert_eq!(pf.factor_delta("BTC"), dec!(0));
    }

    /// Dust threshold prunes factors with near-zero delta from
    /// the `factors()` output. A BTC factor that ends at 1e-9
    /// BTC (below the default 1e-8 threshold) is silently
    /// dropped.
    #[test]
    fn factors_iterator_prunes_dust() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        // A dust-sized position — below the default threshold.
        pf.on_fill("BTCUSDT", dec!(0.000000001), dec!(50000), UNCLASSIFIED);
        pf.mark_price("BTCUSDT", dec!(50000));
        let factors = pf.factors();
        // The BTC bucket is dust → pruned. The USDT quote leg
        // is also dust-scale (50000 × 1e-9 = 5e-5, which is
        // above the 1e-8 threshold) so it shows up.
        let btc = factors.iter().find(|(k, _)| k == "BTC");
        assert!(btc.is_none(), "BTC dust should be pruned");
    }

    /// Snapshot exposes `per_factor` alongside the legacy
    /// `per_asset` map — downstream dashboards can consume
    /// either without a second call.
    #[test]
    fn snapshot_exposes_per_factor_and_per_strategy() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(2), dec!(100), "basis");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(110), "basis"); // realise +10
        pf.mark_price("BTCUSDT", dec!(120));
        let snap = pf.snapshot();
        let btc = snap
            .per_factor
            .iter()
            .find(|(k, _)| k == "BTC")
            .expect("BTC factor present");
        assert_eq!(btc.1, dec!(1));
        // per_strategy is non-empty and carries the basis class.
        let basis = snap
            .per_strategy
            .iter()
            .find(|(k, _)| k == "basis")
            .expect("basis bucket present");
        assert_eq!(basis.1, dec!(10));
    }

    // ---- Epic C sub-component #2: per-strategy labeling tests ----

    /// Single strategy class: realised PnL accumulates into
    /// exactly one bucket.
    #[test]
    fn per_strategy_pnl_single_class() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(2), dec!(100), "avellaneda");
        pf.on_fill("BTCUSDT", dec!(-2), dec!(110), "avellaneda");
        let map = pf.per_strategy_pnl();
        assert_eq!(map.get("avellaneda").copied(), Some(dec!(20)));
    }

    /// Multiple strategy classes quoting the same symbol do
    /// NOT commingle — each gets its own bucket. Models the
    /// funding-arb driver and the basis engine pushing into
    /// the same Portfolio on the same leg.
    #[test]
    fn per_strategy_pnl_multi_class_does_not_commingle() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        // Basis engine buys 1 at 100 then sells 1 at 105 → +5.
        pf.on_fill("BTCUSDT", dec!(1), dec!(100), "basis");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(105), "basis");
        // Funding-arb driver buys 1 at 102 then sells 1 at 108 → +6.
        // Shares the same BTCUSDT position bucket, so the
        // second buy re-opens the position at avg 102 before
        // the second sell closes it at 108.
        pf.on_fill("BTCUSDT", dec!(1), dec!(102), "funding_arb");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(108), "funding_arb");
        let map = pf.per_strategy_pnl();
        assert_eq!(map.get("basis").copied(), Some(dec!(5)));
        assert_eq!(map.get("funding_arb").copied(), Some(dec!(6)));
        // Sum matches total realised.
        assert_eq!(map.values().sum::<Decimal>(), dec!(11));
    }

    /// Unknown strategy classes get their own bucket. No
    /// silent dropping, no "unknown" catch-all — every string
    /// the caller passes becomes a first-class key.
    #[test]
    fn per_strategy_pnl_unknown_class_becomes_own_bucket() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(1), dec!(100), "experimental_v2");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(110), "experimental_v2");
        assert_eq!(
            pf.per_strategy_pnl().get("experimental_v2").copied(),
            Some(dec!(10))
        );
    }

    /// Per-strategy PnL sorted output is deterministic —
    /// daily reports need stable ordering.
    #[test]
    fn per_strategy_sorted_is_alphabetical() {
        let mut pf = Portfolio::new("USDT");
        pf.register_symbol("BTCUSDT", "BTC", "USDT");
        pf.on_fill("BTCUSDT", dec!(1), dec!(100), "zebra");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(101), "zebra");
        pf.on_fill("BTCUSDT", dec!(1), dec!(100), "alpha");
        pf.on_fill("BTCUSDT", dec!(-1), dec!(102), "alpha");
        let sorted = pf.per_strategy_sorted();
        let keys: Vec<&str> = sorted.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["alpha", "zebra"]);
    }

    // ── Property-based tests (Epic 14) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn price_strat()(cents in 100i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    prop_compose! {
        fn qty_strat()(units in -10_000i64..10_000i64) -> Decimal {
            // Signed qty strategy: positive = buy, negative = sell.
            Decimal::new(units, 4)
        }
    }

    proptest! {
        /// Round-tripping a position to flat ALWAYS reports zero
        /// unrealised PnL regardless of intermediate fills. The
        /// final realised figure accumulates everything. Mirrors
        /// the inventory-module property on `InventoryManager`
        /// but crosses the Portfolio's native→reporting FX path.
        #[test]
        fn closed_position_has_zero_unrealised(
            opens in proptest::collection::vec((price_strat(), qty_strat()), 1..10),
            close_price in price_strat(),
            mark in price_strat(),
        ) {
            let mut pf = Portfolio::new("USDT");
            pf.register_symbol("TEST", "BASE", "USDT");

            let mut net_qty = dec!(0);
            for (p, q) in &opens {
                pf.on_fill("TEST", *q, *p, "test");
                net_qty += *q;
            }
            // Close the net position at close_price.
            if !net_qty.is_zero() {
                pf.on_fill("TEST", -net_qty, close_price, "test");
            }
            pf.mark_price("TEST", mark);

            let snap = pf.snapshot();
            prop_assert_eq!(snap.total_unrealised_pnl, dec!(0),
                "flat portfolio unrealised = {}", snap.total_unrealised_pnl);
        }

        /// FX conversion is linear: doubling the FX rate doubles
        /// the reporting-currency realised PnL for a non-zero
        /// realised figure. Catches a multiplication order
        /// regression.
        #[test]
        fn fx_scales_realised_pnl_linearly(
            fill1 in (price_strat(), qty_strat()),
            fill2_price in price_strat(),
            fx_raw in 1i64..1000i64,
        ) {
            let (p1, q1) = fill1;
            prop_assume!(!q1.is_zero());
            let fx = Decimal::new(fx_raw, 2);  // 0.01 .. 10.0
            let mut pf_a = Portfolio::new("USDT");
            pf_a.register_symbol("TEST", "BASE", "NATIVE");
            pf_a.set_fx("TEST", fx);
            pf_a.on_fill("TEST", q1, p1, "s");
            pf_a.on_fill("TEST", -q1, fill2_price, "s");
            let snap_a = pf_a.snapshot();

            let mut pf_b = Portfolio::new("USDT");
            pf_b.register_symbol("TEST", "BASE", "NATIVE");
            pf_b.set_fx("TEST", fx * dec!(2));
            pf_b.on_fill("TEST", q1, p1, "s");
            pf_b.on_fill("TEST", -q1, fill2_price, "s");
            let snap_b = pf_b.snapshot();

            prop_assert_eq!(snap_b.total_realised_pnl, snap_a.total_realised_pnl * dec!(2),
                "FX doubling should double realised: a={} b={}",
                snap_a.total_realised_pnl, snap_b.total_realised_pnl);
        }

        /// Sum of per-asset realised equals the global
        /// total_realised. Catches a weighting bug across
        /// multiple symbols / FX rates.
        #[test]
        fn per_asset_realised_sums_to_total(
            fills in proptest::collection::vec(
                (price_strat(), qty_strat(), price_strat()),
                1..15,
            ),
        ) {
            let mut pf = Portfolio::new("USDT");
            pf.register_symbol("A", "BA", "USDT");
            pf.register_symbol("B", "BB", "USDT");

            // Alternate fills across two symbols.
            for (i, (p, q, close)) in fills.iter().enumerate() {
                let sym = if i % 2 == 0 { "A" } else { "B" };
                prop_assume!(!q.is_zero());
                pf.on_fill(sym, *q, *p, "test");
                pf.on_fill(sym, -*q, *close, "test");
            }
            let snap = pf.snapshot();
            let sum: Decimal = snap.per_asset.values()
                .map(|a| a.realised_pnl_reporting)
                .sum();
            prop_assert_eq!(snap.total_realised_pnl, sum);
        }

        /// Per-strategy PnL sums to the global total_realised.
        /// The strategy attribution must partition realised PnL
        /// exactly — every dollar is tagged to exactly one
        /// bucket.
        #[test]
        fn per_strategy_partitions_realised(
            opens in proptest::collection::vec(
                (price_strat(), qty_strat(), price_strat()),
                1..10,
            ),
        ) {
            let mut pf = Portfolio::new("USDT");
            pf.register_symbol("T", "BA", "USDT");
            for (i, (p, q, close)) in opens.iter().enumerate() {
                prop_assume!(!q.is_zero());
                let strat = if i % 2 == 0 { "alpha" } else { "beta" };
                pf.on_fill("T", *q, *p, strat);
                pf.on_fill("T", -*q, *close, strat);
            }
            let snap = pf.snapshot();
            let sum: Decimal = snap.per_strategy.iter()
                .map(|(_, pnl)| *pnl)
                .sum();
            prop_assert_eq!(snap.total_realised_pnl, sum);
        }
    }
}
