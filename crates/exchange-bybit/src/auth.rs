use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Sign a payload with HMAC-SHA256 for Bybit API.
pub fn sign(secret: &str, payload: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Build Bybit auth headers: timestamp + api_key + recv_window + query → signature.
pub fn auth_headers(api_key: &str, api_secret: &str, params: &str) -> (String, String, String) {
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();
    let recv_window = "5000";
    let sign_payload = format!("{timestamp}{api_key}{recv_window}{params}");
    let signature = sign(api_secret, &sign_payload);
    (timestamp, recv_window.to_string(), signature)
}
