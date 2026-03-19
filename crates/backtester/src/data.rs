use chrono::{DateTime, Utc};
use mm_common::types::{Price, PriceLevel, Qty, Side};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single recorded market event for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RecordedEvent {
    BookSnapshot {
        timestamp: DateTime<Utc>,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: u64,
    },
    Trade {
        timestamp: DateTime<Utc>,
        price: Price,
        qty: Qty,
        taker_side: Side,
    },
}

/// Load recorded events from a JSONL file (one JSON object per line).
pub fn load_events(path: &Path) -> anyhow::Result<Vec<RecordedEvent>> {
    let content = std::fs::read_to_string(path)?;
    let mut events = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event: RecordedEvent = serde_json::from_str(line)?;
        events.push(event);
    }
    tracing::info!(count = events.len(), path = %path.display(), "loaded recorded events");
    Ok(events)
}

/// Record live events to a JSONL file for later replay.
pub struct EventRecorder {
    writer: std::io::BufWriter<std::fs::File>,
}

impl EventRecorder {
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
        })
    }

    pub fn record(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        use std::io::Write;
        let json = serde_json::to_string(event)?;
        writeln!(self.writer, "{json}")?;
        Ok(())
    }

    pub fn flush(&mut self) -> anyhow::Result<()> {
        use std::io::Write;
        self.writer.flush()?;
        Ok(())
    }
}
