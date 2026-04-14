use std::sync::Arc;

use mm_common::types::InstrumentPair;
use mm_exchange_core::connector::ExchangeConnector;

/// Bundle of exchange connectors the engine drives for a single
/// logical strategy.
///
/// Single-connector mode is the default and leaves `hedge` unset.
/// Cross-product strategies (basis trade, funding arb) set `hedge`
/// to a second connector (usually a perp/futures venue) and
/// describe the symbol mapping via `pair`.
///
/// The engine treats the primary as the quoting leg — the place
/// where `OrderManager` submits maker orders — and the hedge leg
/// as a price-reference / hedge-execution venue. The asymmetry is
/// deliberate: a basis strategy quotes spot and hedges perp; a
/// single-venue MM never touches the hedge side.
#[derive(Clone)]
pub struct ConnectorBundle {
    pub primary: Arc<dyn ExchangeConnector>,
    pub hedge: Option<Arc<dyn ExchangeConnector>>,
    pub pair: Option<InstrumentPair>,
}

impl ConnectorBundle {
    /// Single-connector mode: engine behaves byte-for-byte the
    /// same as the pre-Sprint-G code path.
    pub fn single(primary: Arc<dyn ExchangeConnector>) -> Self {
        Self {
            primary,
            hedge: None,
            pair: None,
        }
    }

    /// Dual-connector mode with an explicit instrument pair.
    pub fn dual(
        primary: Arc<dyn ExchangeConnector>,
        hedge: Arc<dyn ExchangeConnector>,
        pair: InstrumentPair,
    ) -> Self {
        Self {
            primary,
            hedge: Some(hedge),
            pair: Some(pair),
        }
    }

    pub fn is_dual(&self) -> bool {
        self.hedge.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockConnector;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use rust_decimal_macros::dec;

    fn stub(venue: VenueId, product: VenueProduct) -> Arc<dyn ExchangeConnector> {
        Arc::new(MockConnector::new(venue, product))
    }

    #[test]
    fn single_has_no_hedge() {
        let b = ConnectorBundle::single(stub(VenueId::Binance, VenueProduct::Spot));
        assert!(!b.is_dual());
        assert!(b.hedge.is_none());
        assert!(b.pair.is_none());
    }

    #[test]
    fn dual_stores_both_connectors_and_pair() {
        let pair = InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTCUSDT".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        };
        let bundle = ConnectorBundle::dual(
            stub(VenueId::Binance, VenueProduct::Spot),
            stub(VenueId::Binance, VenueProduct::LinearPerp),
            pair.clone(),
        );
        assert!(bundle.is_dual());
        assert_eq!(bundle.primary.venue_id(), VenueId::Binance);
        assert_eq!(bundle.primary.product(), VenueProduct::Spot);
        let hedge = bundle.hedge.as_ref().unwrap();
        assert_eq!(hedge.product(), VenueProduct::LinearPerp);
        let stored = bundle.pair.as_ref().unwrap();
        assert_eq!(stored.primary_symbol, pair.primary_symbol);
        assert_eq!(stored.hedge_symbol, pair.hedge_symbol);
    }
}
