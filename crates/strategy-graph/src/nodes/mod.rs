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

pub mod logic;
pub mod math;
pub mod risk;
pub mod sinks;
pub mod sources;
pub mod stats;
