//! HyperLiquid REST/WS wire types.
//!
//! Field names are short single letters because HL signs msgpack of the exact
//! payload — every byte matters, and longer names would produce an invalid
//! connection-id hash. Field order follows the HL Python SDK so msgpack maps
//! serialize to the same byte sequence.

use serde::{Deserialize, Serialize};

/// A single order in a `{type: "order", ...}` action.
///
/// Field order matches the HL Python SDK so `rmp_serde::to_vec_named`
/// produces identical msgpack bytes — any divergence would invalidate the
/// connection-id hash the signature is bound to.
#[derive(Debug, Clone, Serialize)]
pub struct HlOrder {
    /// Asset index (from `/info meta` universe).
    pub a: u32,
    /// is_buy
    pub b: bool,
    /// Limit price as a decimal string.
    pub p: String,
    /// Size (base asset) as a decimal string.
    pub s: String,
    /// reduce_only
    pub r: bool,
    /// Time-in-force / order type descriptor.
    pub t: HlOrderTif,
    /// Client order id — 0x-prefixed 128-bit hex. When `None`,
    /// `skip_serializing_if` drops the key from the msgpack map entirely
    /// so the byte shape (and thus the action hash) matches what HL
    /// expects for orders placed without a cloid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<String>,
}

/// Time-in-force variants. For a market-maker we always use `Alo` (Add
/// Liquidity Only = post-only). `Gtc` and `Ioc` are included for completeness.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HlOrderTif {
    Limit {
        limit: HlLimit,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct HlLimit {
    /// "Alo", "Gtc", or "Ioc".
    pub tif: String,
}

impl HlLimit {
    pub fn alo() -> Self {
        Self { tif: "Alo".into() }
    }
}

/// `{type: "order", orders: [...], grouping: "na"}` action body.
#[derive(Debug, Clone, Serialize)]
pub struct HlOrderAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub orders: Vec<HlOrder>,
    pub grouping: String,
}

impl HlOrderAction {
    pub fn new(orders: Vec<HlOrder>) -> Self {
        Self {
            type_: "order".into(),
            orders,
            grouping: "na".into(),
        }
    }
}

/// `{type: "cancel", cancels: [...]}` action body.
#[derive(Debug, Clone, Serialize)]
pub struct HlCancelAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub cancels: Vec<HlCancel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HlCancel {
    /// Asset index.
    pub a: u32,
    /// Exchange order id.
    pub o: u64,
}

impl HlCancelAction {
    pub fn new(cancels: Vec<HlCancel>) -> Self {
        Self {
            type_: "cancel".into(),
            cancels,
        }
    }
}

/// `{type: "cancelByCloid", cancels: [{asset, cloid}]}` action body.
///
/// Note: unlike `HlCancel`, the field names here are spelled out
/// (`asset`, `cloid`) — matching HL's Python SDK.
#[derive(Debug, Clone, Serialize)]
pub struct HlCancelByCloidAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub cancels: Vec<HlCancelByCloid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HlCancelByCloid {
    pub asset: u32,
    pub cloid: String,
}

impl HlCancelByCloidAction {
    pub fn new(cancels: Vec<HlCancelByCloid>) -> Self {
        Self {
            type_: "cancelByCloid".into(),
            cancels,
        }
    }
}

/// `{type: "modify", modifies: [...]}` action body.
#[derive(Debug, Clone, Serialize)]
pub struct HlModifyAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub modifies: Vec<HlModify>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HlModify {
    /// Exchange order id being modified.
    pub oid: u64,
    /// Replacement order.
    pub order: HlOrder,
}

impl HlModifyAction {
    pub fn new(modifies: Vec<HlModify>) -> Self {
        Self {
            type_: "modify".into(),
            modifies,
        }
    }
}

/// Envelope sent to `POST /exchange`.
#[derive(Debug, Serialize)]
pub struct HlExchangePayload<'a, A: Serialize> {
    pub action: &'a A,
    pub nonce: u64,
    pub signature: serde_json::Value,
    #[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
    pub vault_address: Option<String>,
}

/// `/info` meta response — the perp universe and per-asset decimals.
#[derive(Debug, Deserialize)]
pub struct HlMeta {
    pub universe: Vec<HlAssetMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HlAssetMeta {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u32,
    #[serde(default)]
    pub max_leverage: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_order(cloid: Option<&str>) -> HlOrder {
        HlOrder {
            a: 0,
            b: true,
            p: "42000".into(),
            s: "0.01".into(),
            r: false,
            t: HlOrderTif::Limit { limit: HlLimit::alo() },
            c: cloid.map(|s| s.to_string()),
        }
    }

    /// When `c` is `None`, msgpack must NOT contain the "c" key — otherwise
    /// the action hash diverges from what HL expects and signatures are
    /// rejected. This test pins that invariant.
    #[test]
    fn msgpack_omits_cloid_when_none() {
        let bytes = rmp_serde::to_vec_named(&sample_order(None)).unwrap();
        // Walk bytes and assert we never see an "a single-char 'c' key".
        // In msgpack, a 1-char string is encoded as 0xa1 0x63 ('c'=0x63).
        let needle = [0xa1u8, b'c'];
        assert!(
            !bytes.windows(2).any(|w| w == needle),
            "HlOrder with c=None should not emit 'c' key, but msgpack bytes contained it: {:?}",
            bytes
        );
    }

    #[test]
    fn msgpack_includes_cloid_when_some() {
        let bytes = rmp_serde::to_vec_named(&sample_order(Some("0xabcdef"))).unwrap();
        let needle = [0xa1u8, b'c'];
        assert!(
            bytes.windows(2).any(|w| w == needle),
            "HlOrder with c=Some should emit 'c' key, but msgpack bytes did not contain it: {:?}",
            bytes
        );
    }

    /// Two orders differing only in cloid presence must produce different
    /// msgpack — this guards against accidental pruning of `c=Some`.
    #[test]
    fn msgpack_differs_by_cloid() {
        let without = rmp_serde::to_vec_named(&sample_order(None)).unwrap();
        let with = rmp_serde::to_vec_named(&sample_order(Some("0xdead"))).unwrap();
        assert_ne!(without, with);
        // And `with` must be strictly longer (extra map entry).
        assert!(with.len() > without.len());
    }

    /// Field order in msgpack is declaration order — matches HL Python SDK.
    /// The first few bytes should encode the keys in order: a, b, p, s, r, t.
    #[test]
    fn msgpack_field_order_is_stable() {
        let bytes = rmp_serde::to_vec_named(&sample_order(None)).unwrap();
        // After the map header (0x86 = fixmap with 6 entries), the first
        // key is "a" → fixstr header 0xa1 then 0x61.
        assert_eq!(bytes[0], 0x86, "expected 6-entry fixmap header");
        assert_eq!(&bytes[1..3], &[0xa1, b'a']);
    }
}
