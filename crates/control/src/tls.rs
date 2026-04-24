//! TLS helpers for the control channel.
//!
//! PR-2f-tls wraps the listener side of [`crate::ws_transport`]
//! so production deployments can bind `wss://` instead of plain
//! `ws://`. The client side is already TLS-capable — when a
//! caller passes `wss://host/` to [`crate::WsTransport::connect`],
//! `tokio-tungstenite`'s `rustls-tls-webpki-roots` feature
//! terminates TLS against the system trust anchors.
//!
//! Key rotation is operator-driven: the controller reads cert + key
//! paths from env on startup, stream-terminates TLS for every
//! accepted connection. Hot reload is not wired — a key
//! rotation requires a controller restart, which is fine in practice
//! because the reconnect loop on agents handles a restart
//! transparently.

use std::io;
use std::path::Path;
use std::sync::Arc;

use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("io error loading tls material: {0}")]
    Io(#[from] io::Error),
    #[error("tls cert pem file contained no certificates")]
    EmptyCerts,
    #[error("tls key pem file contained no private key")]
    NoPrivateKey,
    #[error("tls config build failed: {0}")]
    Rustls(String),
}

/// Load a PEM-encoded certificate chain from disk.
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let raw = std::fs::read(path)?;
    let mut slice: &[u8] = &raw;
    let certs: Vec<_> = rustls_pemfile::certs(&mut slice).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        return Err(TlsError::EmptyCerts);
    }
    Ok(certs)
}

/// Load a PEM-encoded PKCS8 / RSA / ECDSA private key from disk.
/// The PEM may contain multiple keys — we take the first.
fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let raw = std::fs::read(path)?;
    let mut slice: &[u8] = &raw;
    match rustls_pemfile::private_key(&mut slice)? {
        Some(key) => Ok(key),
        None => Err(TlsError::NoPrivateKey),
    }
}

/// Build a rustls server config + TlsAcceptor from PEM files on
/// disk. Callers pass the resulting `TlsAcceptor` to
/// [`crate::WsListener::bind_tls`] below.
pub fn build_acceptor(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::Rustls(e.to_string()))?;
    Ok(TlsAcceptor::from(Arc::new(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_self_signed(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        // rcgen produces a freshly-generated self-signed cert
        // pair for the listed SANs — plenty for the acceptor-
        // build test, and exactly the shape an operator would
        // hand the controller via PEM files.
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("rcgen generates a self-signed cert");
        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        let mut f = std::fs::File::create(&cert_path).unwrap();
        f.write_all(cert_pem.as_bytes()).unwrap();
        let mut f = std::fs::File::create(&key_path).unwrap();
        f.write_all(key_pem.as_bytes()).unwrap();
        (cert_path, key_path)
    }

    #[test]
    fn build_acceptor_from_generated_pem() {
        // Install the aws_lc_rs provider once — rustls 0.23
        // requires a default CryptoProvider at runtime.
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let (cert, key) = write_self_signed(tmp.path());
        let acceptor = build_acceptor(&cert, &key);
        assert!(
            acceptor.is_ok(),
            "build_acceptor failed: {:?}",
            acceptor.err()
        );
    }

    #[test]
    fn load_certs_empty_pem_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.pem");
        std::fs::write(&path, b"").unwrap();
        let err = load_certs(&path).unwrap_err();
        assert!(matches!(err, TlsError::EmptyCerts));
    }

    #[test]
    fn load_key_missing_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.pem");
        std::fs::write(&path, b"").unwrap();
        let err = load_key(&path).unwrap_err();
        assert!(matches!(err, TlsError::NoPrivateKey));
    }
}
