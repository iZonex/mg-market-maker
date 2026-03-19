use chrono::{DateTime, Utc};
use mm_common::types::OrderId;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

/// Persistent state checkpoint for crash recovery.
///
/// On every state change (fill, order placed/cancelled), we write a checkpoint.
/// On restart, we load the last checkpoint and reconcile with exchange state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub timestamp: DateTime<Utc>,
    /// Per-symbol state.
    pub symbols: HashMap<String, SymbolCheckpoint>,
    /// Global PnL tracking.
    pub daily_pnl: Decimal,
    pub total_realized_pnl: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolCheckpoint {
    pub symbol: String,
    /// Net inventory in base asset.
    pub inventory: Decimal,
    /// Average entry price.
    pub avg_entry_price: Decimal,
    /// Known open order IDs (to reconcile on restart).
    pub open_order_ids: Vec<OrderId>,
    /// Realized PnL for this symbol.
    pub realized_pnl: Decimal,
    /// Total volume traded.
    pub total_volume: Decimal,
    /// Total fills count.
    pub total_fills: u64,
}

impl Checkpoint {
    pub fn new() -> Self {
        Self {
            timestamp: Utc::now(),
            symbols: HashMap::new(),
            daily_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
        }
    }
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages checkpoint persistence to disk.
pub struct CheckpointManager {
    path: std::path::PathBuf,
    current: Checkpoint,
    /// Write counter — flush every N updates.
    write_count: u64,
    flush_every: u64,
}

impl CheckpointManager {
    /// Create a new checkpoint manager. Loads existing checkpoint if available.
    pub fn new(path: &Path, flush_every: u64) -> Self {
        let current = Self::load_from_disk(path).unwrap_or_else(|_| {
            info!("no existing checkpoint, starting fresh");
            Checkpoint::new()
        });

        Self {
            path: path.to_path_buf(),
            current,
            write_count: 0,
            flush_every,
        }
    }

    /// Get current checkpoint state.
    pub fn current(&self) -> &Checkpoint {
        &self.current
    }

    /// Update symbol state and optionally flush.
    pub fn update_symbol(&mut self, state: SymbolCheckpoint) {
        self.current.symbols.insert(state.symbol.clone(), state);
        self.current.timestamp = Utc::now();
        self.write_count += 1;

        if self.write_count >= self.flush_every {
            if let Err(e) = self.flush() {
                warn!(error = %e, "failed to flush checkpoint");
            }
            self.write_count = 0;
        }
    }

    /// Update global PnL.
    pub fn update_pnl(&mut self, daily: Decimal, total_realized: Decimal) {
        self.current.daily_pnl = daily;
        self.current.total_realized_pnl = total_realized;
    }

    /// Force flush to disk.
    pub fn flush(&self) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(&self.current)?;
        // Write to temp file first, then rename (atomic on most filesystems).
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        info!(
            symbols = self.current.symbols.len(),
            "checkpoint flushed to disk"
        );
        Ok(())
    }

    /// Load checkpoint from disk.
    fn load_from_disk(path: &Path) -> anyhow::Result<Checkpoint> {
        let content = std::fs::read_to_string(path)?;
        let checkpoint: Checkpoint = serde_json::from_str(&content)?;
        info!(
            timestamp = %checkpoint.timestamp,
            symbols = checkpoint.symbols.len(),
            "loaded checkpoint from disk"
        );
        Ok(checkpoint)
    }

    /// Get symbol state for reconciliation on restart.
    pub fn get_symbol(&self, symbol: &str) -> Option<&SymbolCheckpoint> {
        self.current.symbols.get(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_checkpoint_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("mm_test_checkpoint.json");

        let mut mgr = CheckpointManager::new(&path, 1);
        mgr.update_symbol(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0.05),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(42.5),
            total_volume: dec!(10000),
            total_fills: 100,
        });

        // Should auto-flush (flush_every = 1).
        // Load it back.
        let loaded = CheckpointManager::new(&path, 10);
        let sym = loaded.get_symbol("BTCUSDT").unwrap();
        assert_eq!(sym.inventory, dec!(0.05));
        assert_eq!(sym.realized_pnl, dec!(42.5));

        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }
}
