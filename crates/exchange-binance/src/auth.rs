use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Sign a query string with HMAC-SHA256.
pub fn sign(secret: &str, payload: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Build a signed query string for Binance API.
pub fn signed_query(api_secret: &str, params: &str) -> String {
    let timestamp = chrono::Utc::now().timestamp_millis();
    let query = format!("{params}&timestamp={timestamp}");
    let signature = sign(api_secret, &query);
    format!("{query}&signature={signature}")
}
