//! Ticker normalisation.
//!
//! Raw asset tokens coming out of RSS / CryptoPanic / LLM
//! output are noisy — "bitcoin", "Bitcoin", "BTC", "xbt",
//! "bitcoin (BTC)". Canonicalising to a single form
//! (`"BTC"`) is what makes the downstream mention counter
//! actually increment the same bucket regardless of
//! capitalisation and naming drift.
//!
//! Alias table mirrors `~/santiment/src/analyzer/sentiment.py`
//! `TICKER_ALIASES` — crypto, indices, FX, commodities all in
//! one map. Missing keys pass through upper-cased so a new
//! asset that wasn't in the table still routes somewhere.

use std::collections::HashMap;
use std::sync::OnceLock;

fn alias_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let pairs: &[(&str, &str)] = &[
            // ── Indices
            ("s&p 500", "SPX"),
            ("s&p500", "SPX"),
            ("^gspc", "SPX"),
            ("^spx", "SPX"),
            ("sp500", "SPX"),
            ("lse:spx", "SPX"),
            ("dow", "DJI"),
            ("djia", "DJI"),
            ("^dji", "DJI"),
            ("^dia", "DJI"),
            ("dow jones", "DJI"),
            ("nasdaq", "NDX"),
            ("^ixic", "NDX"),
            ("compx", "NDX"),
            ("qqq", "NDX"),
            // ── Crypto
            ("bitcoin", "BTC"),
            ("xbt", "BTC"),
            ("ethereum", "ETH"),
            ("ether", "ETH"),
            ("solana", "SOL"),
            ("ripple", "XRP"),
            ("dogecoin", "DOGE"),
            ("cardano", "ADA"),
            ("polkadot", "DOT"),
            ("avalanche", "AVAX"),
            ("chainlink", "LINK"),
            ("litecoin", "LTC"),
            ("tether", "USDT"),
            // ── Commodities
            ("gold", "XAU"),
            ("xau/usd", "XAU"),
            ("gc=f", "XAU"),
            ("silver", "XAG"),
            ("xag/usd", "XAG"),
            ("xagusd", "XAG"),
            ("slv", "XAG"),
            ("oil", "WTI"),
            ("crude", "WTI"),
            ("cl=f", "WTI"),
            ("brent", "BRENT"),
            ("diesel", "DIESEL"),
            // ── FX
            ("eur/usd", "EURUSD"),
            ("eurusd", "EURUSD"),
            ("gbp/usd", "GBPUSD"),
            ("gbpusd", "GBPUSD"),
            ("cable", "GBPUSD"),
            ("stg", "GBPUSD"),
            ("usd/jpy", "USDJPY"),
            ("usdjpy", "USDJPY"),
            ("dollar", "DXY"),
            ("usd index", "DXY"),
            ("euro", "EUR"),
            ("swiss franc", "CHF"),
            ("pound", "GBP"),
        ];
        pairs.iter().copied().collect()
    })
}

/// Canonicalise a single raw ticker / asset name. Unknown
/// inputs pass through upper-cased so a newly-listed asset
/// still routes to a stable bucket (and an alias can be
/// added later without mention-counter drift).
pub fn normalize_ticker(raw: &str) -> String {
    let key = raw.trim().to_lowercase();
    if let Some(v) = alias_table().get(key.as_str()) {
        return (*v).to_string();
    }
    raw.trim().to_uppercase()
}

/// Scan free-form text for ticker / asset mentions and return
/// the canonical set. Matches both the alias table (any
/// case-insensitive variant in the table) and explicit upper-
/// case tokens on `whitelist` (so operator-configured
/// `monitored_assets = ["BTC", "SOL"]` keeps detecting those
/// even when the article uses the bare symbol).
///
/// Conservative: we only consider word-boundary substrings,
/// never partial matches — "bitcoin" picks up "BTC" but
/// "bitcoincash" does not (it has no alias → unrecognised).
pub fn extract_tickers(text: &str, whitelist: &[String]) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    // Split into rough word tokens: ASCII alphanumerics stay
    // together, other chars separate. Simple and cheap.
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '&')
        .filter(|t| !t.is_empty())
        .collect();
    let whitelist_set: std::collections::HashSet<String> = whitelist
        .iter()
        .map(|w| normalize_ticker(w))
        .collect();

    let mut out: Vec<String> = Vec::new();
    for tok in &tokens {
        if let Some(canon) = alias_table().get(*tok) {
            let c = (*canon).to_string();
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    // Check multi-word aliases (e.g., "s&p 500", "dow jones").
    for (k, canon) in alias_table().iter() {
        if k.contains(' ') && lower.contains(k) {
            let c = (*canon).to_string();
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    // Upper-case whitelist hits — catch bare "BTC", "SOL".
    for tok in &tokens {
        let upper = tok.to_uppercase();
        if whitelist_set.contains(&upper) && !out.contains(&upper) {
            out.push(upper);
        }
    }
    out
}

/// Dedup + canonicalise a list of raw assets. Preserves first-
/// seen order so audit-log reads stay reproducible.
pub fn normalize_asset_list(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for r in raw {
        let n = normalize_ticker(r);
        if n.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == &n) {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_crypto_aliases_canonicalise() {
        assert_eq!(normalize_ticker("bitcoin"), "BTC");
        assert_eq!(normalize_ticker("Bitcoin"), "BTC");
        assert_eq!(normalize_ticker(" BITCOIN "), "BTC");
        assert_eq!(normalize_ticker("xbt"), "BTC");
        assert_eq!(normalize_ticker("ETHEREUM"), "ETH");
    }

    #[test]
    fn unknown_tickers_pass_through_uppercased() {
        assert_eq!(normalize_ticker("wifusd"), "WIFUSD");
        assert_eq!(normalize_ticker("NewToken"), "NEWTOKEN");
    }

    #[test]
    fn dedup_preserves_order() {
        let input = ["bitcoin".into(), "BTC".into(), "eth".into(), "BITCOIN".into()];
        let out = normalize_asset_list(&input);
        assert_eq!(out, vec!["BTC", "ETH"]);
    }

    #[test]
    fn fx_aliases() {
        assert_eq!(normalize_ticker("EUR/USD"), "EURUSD");
        assert_eq!(normalize_ticker("cable"), "GBPUSD");
        assert_eq!(normalize_ticker("dollar"), "DXY");
    }

    #[test]
    fn index_aliases() {
        assert_eq!(normalize_ticker("S&P 500"), "SPX");
        assert_eq!(normalize_ticker("Nasdaq"), "NDX");
    }
}
