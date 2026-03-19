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
        println!("═══════════════════════════════════════════");
    }
}
