//! Build an [`ExchangeConnector`] from a [`ResolvedCredential`].
//!
//! Mirrors the venue-dispatch logic that lives in
//! `crates/server/src/main.rs::create_hedge_connector`, but
//! takes a resolved credential (exchange + product + api keys
//! already materialised from env) rather than the legacy
//! monolithic `AppConfig`. This is the shape the agent's
//! reconcile loop expects: it holds only credential IDs on the
//! wire, resolves them at spawn time, and builds the connector
//! via this factory.
//!
//! Kept deliberately thin — one match, no side effects, no async.
//! Constructors on the concrete connectors are synchronous and
//! do no network IO, so the factory is safe to call from inside
//! a reconcile pass without blocking.

use std::sync::Arc;

use anyhow::Result;
use mm_common::config::{ExchangeType, ProductType};
use mm_common::settings::ResolvedCredential;
use mm_exchange_core::connector::ExchangeConnector;

/// Build a connector for `resolved`. Returns an owned `Arc` so
/// the caller can share it with the reconcile task and still
/// drop it when the task exits.
pub fn build_connector(resolved: &ResolvedCredential) -> Result<Arc<dyn ExchangeConnector>> {
    let api_key = resolved.api_key.as_str();
    let api_secret = resolved.api_secret.as_str();

    match resolved.exchange {
        ExchangeType::Custom => {
            anyhow::bail!(
                "custom exchange requires rest_url/ws_url — not derivable from \
                 a ResolvedCredential. Use a typed venue (binance/bybit/hyperliquid) \
                 or extend CredentialSpec with explicit urls."
            );
        }
        ExchangeType::Binance | ExchangeType::BinanceTestnet => {
            let testnet = matches!(resolved.exchange, ExchangeType::BinanceTestnet);
            match resolved.product {
                ProductType::Spot => {
                    let c = if testnet {
                        mm_exchange_binance::BinanceConnector::testnet(api_key, api_secret)
                    } else {
                        mm_exchange_binance::BinanceConnector::new(
                            "https://api.binance.com",
                            "wss://stream.binance.com:9443",
                            api_key,
                            api_secret,
                        )
                    };
                    Ok(Arc::new(c))
                }
                ProductType::LinearPerp => Ok(Arc::new(
                    mm_exchange_binance::BinanceFuturesConnector::new(api_key, api_secret),
                )),
                ProductType::InversePerp => {
                    anyhow::bail!("Binance inverse (COIN-M) is not supported — use linear_perp")
                }
            }
        }
        ExchangeType::Bybit | ExchangeType::BybitTestnet => {
            let testnet = matches!(resolved.exchange, ExchangeType::BybitTestnet);
            let c = match (resolved.product, testnet) {
                (ProductType::Spot, false) => {
                    mm_exchange_bybit::BybitConnector::spot(api_key, api_secret)
                }
                (ProductType::Spot, true) => {
                    mm_exchange_bybit::BybitConnector::testnet_spot(api_key, api_secret)
                }
                (ProductType::LinearPerp, false) => {
                    mm_exchange_bybit::BybitConnector::linear(api_key, api_secret)
                }
                (ProductType::LinearPerp, true) => {
                    mm_exchange_bybit::BybitConnector::testnet(api_key, api_secret)
                }
                (ProductType::InversePerp, false) => {
                    mm_exchange_bybit::BybitConnector::inverse(api_key, api_secret)
                }
                (ProductType::InversePerp, true) => {
                    mm_exchange_bybit::BybitConnector::testnet_inverse(api_key, api_secret)
                }
            };
            Ok(Arc::new(c))
        }
        ExchangeType::HyperLiquid => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::new(api_secret)?,
        )),
        ExchangeType::HyperLiquidTestnet => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::testnet(api_secret)?,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_exchange_core::connector::{VenueId, VenueProduct};

    fn cred(ex: ExchangeType, product: ProductType) -> ResolvedCredential {
        ResolvedCredential {
            id: format!("{ex:?}-{product:?}"),
            exchange: ex,
            product,
            api_key: "dummy-key".into(),
            // HyperLiquid constructor parses the secret as an EIP-712
            // private key — use a known-valid hex string so the
            // test doesn't hit a parse error during factory dispatch.
            // 32 bytes of 0x11 is a valid-length hex scalar.
            api_secret: "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
            max_notional_quote: None,
            default_symbol: None,
        }
    }

    #[test]
    fn binance_spot_produces_connector() {
        let c = build_connector(&cred(ExchangeType::Binance, ProductType::Spot)).unwrap();
        assert_eq!(c.venue_id(), VenueId::Binance);
        assert_eq!(c.product(), VenueProduct::Spot);
    }

    #[test]
    fn binance_linear_perp_produces_futures_connector() {
        let c = build_connector(&cred(ExchangeType::Binance, ProductType::LinearPerp)).unwrap();
        assert_eq!(c.venue_id(), VenueId::Binance);
        assert_eq!(c.product(), VenueProduct::LinearPerp);
    }

    #[test]
    fn binance_inverse_rejected() {
        let err = build_connector(&cred(ExchangeType::Binance, ProductType::InversePerp));
        assert!(err.is_err());
    }

    #[test]
    fn bybit_spot_produces_connector() {
        let c = build_connector(&cred(ExchangeType::Bybit, ProductType::Spot)).unwrap();
        assert_eq!(c.venue_id(), VenueId::Bybit);
        assert_eq!(c.product(), VenueProduct::Spot);
    }

    #[test]
    fn bybit_linear_perp_produces_connector() {
        let c = build_connector(&cred(ExchangeType::Bybit, ProductType::LinearPerp)).unwrap();
        assert_eq!(c.venue_id(), VenueId::Bybit);
        assert_eq!(c.product(), VenueProduct::LinearPerp);
    }

    #[test]
    fn bybit_testnet_produces_connector() {
        let c =
            build_connector(&cred(ExchangeType::BybitTestnet, ProductType::LinearPerp)).unwrap();
        assert_eq!(c.venue_id(), VenueId::Bybit);
    }

    #[test]
    fn hyperliquid_produces_connector() {
        let c = build_connector(&cred(ExchangeType::HyperLiquid, ProductType::LinearPerp)).unwrap();
        assert_eq!(c.venue_id(), VenueId::HyperLiquid);
    }

    #[test]
    fn custom_exchange_type_rejected() {
        let err = build_connector(&cred(ExchangeType::Custom, ProductType::Spot));
        assert!(
            err.is_err(),
            "custom requires explicit URLs, not supported via credential"
        );
    }
}
