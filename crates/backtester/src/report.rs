use mm_indicators::Candle;
use mm_risk::pnl::PnlAttribution;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Results of a backtest run.
#[derive(Debug, Clone)]
pub struct BacktestReport {
    pub strategy_name: String,
    pub total_events: u64,
    pub total_ticks: u64,
    pub total_quotes: u64,
    pub total_fills: u64,
    pub final_inventory: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_pnl: Decimal,
    pub pnl_attribution: PnlAttribution,
    /// 22W-6 — resampled candles emitted during the run. Empty
    /// when the simulator wasn't configured with
    /// `with_candles(...)`. Operators export these for offline
    /// alpha research, hyperopt, or as input to candle-based
    /// indicators (RSI / Bollinger / ATR) that prefer OHLC bars
    /// over raw trades.
    pub completed_candles: Vec<Candle>,
}

impl BacktestReport {
    pub fn print(&self) {
        println!("═══════════════════════════════════════════");
        println!("  BACKTEST REPORT: {}", self.strategy_name);
        println!("═══════════════════════════════════════════");
        println!("  Events:          {}", self.total_events);
        println!("  Book updates:    {}", self.total_ticks);
        println!("  Quote cycles:    {}", self.total_quotes);
        println!("  Fills:           {}", self.total_fills);
        println!("───────────────────────────────────────────");
        println!("  Final inventory: {}", self.final_inventory);
        println!("  Realized PnL:    {}", self.realized_pnl);
        println!("  Unrealized PnL:  {}", self.unrealized_pnl);
        println!("  TOTAL PnL:       {}", self.total_pnl);
        println!("───────────────────────────────────────────");
        println!("  PnL Attribution:");
        println!("    Spread:        {}", self.pnl_attribution.spread_pnl);
        println!("    Inventory:     {}", self.pnl_attribution.inventory_pnl);
        println!("    Rebates:       {}", self.pnl_attribution.rebate_income);
        println!("    Fees:         -{}", self.pnl_attribution.fees_paid);
        println!("    Round trips:   {}", self.pnl_attribution.round_trips);
        println!("    Volume:        {}", self.pnl_attribution.total_volume);
        let efficiency = if self.pnl_attribution.total_volume > dec!(0) {
            self.total_pnl / self.pnl_attribution.total_volume * dec!(10_000)
        } else {
            dec!(0)
        };
        println!("    Efficiency:    {} bps", efficiency);
        if !self.completed_candles.is_empty() {
            println!("───────────────────────────────────────────");
            println!("  Candles:         {}", self.completed_candles.len());
            if let (Some(first), Some(last)) = (
                self.completed_candles.first(),
                self.completed_candles.last(),
            ) {
                println!("    Span:          {} → {}", first.open_ts_ms, last.close_ts_ms);
                println!("    Open:          {}", first.open);
                println!("    Close:         {}", last.close);
                let total_vol: Decimal = self
                    .completed_candles
                    .iter()
                    .map(|c| c.volume())
                    .sum();
                println!("    Total volume:  {}", total_vol);
            }
        }
        println!("═══════════════════════════════════════════");
    }
}
