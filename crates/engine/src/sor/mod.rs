//! Smart Order Router — Epic A.
//!
//! Cost-aware cross-venue dispatcher. Takes a
//! `(side, qty, urgency)` target plus a `ConnectorBundle`
//! and recommends how to split the fill across the
//! available venues to minimise total cost (taker fee +
//! maker queue wait + slippage).
//!
//! # Module layout
//!
//! - [`cost`] — per-venue cost model
//!   ([`cost::VenueCostModel`], [`cost::RouteCost`])
//! - [`venue_state`] — per-venue snapshot aggregator
//!   ([`venue_state::VenueSnapshot`],
//!   [`venue_state::VenueStateAggregator`],
//!   [`venue_state::VenueSeed`])
//! - `router` — greedy cost-minimising router (Sprint A-3)
//!
//! v1 is **advisory**: `MarketMakerEngine::recommend_route`
//! returns a `router::RouteDecision` but the engine does
//! not auto-dispatch. Operators run the recommendation
//! through the dashboard / CLI / audit trail and decide
//! whether to act. Stage-2 wires inline dispatch through an
//! `ExecAlgorithm` once the cost model is validated against
//! real fills.

pub mod cost;
pub mod dispatch;
pub mod router;
pub mod trade_rate;
pub mod venue_state;
