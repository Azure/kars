// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Upstream mTLS to the inference-router (port 8444).
//!
//! Loads:
//! - the gateway's client cert + key (presented to the router),
//! - the CA bundle used to *verify* the router's server cert.
//!
//! The router-side mTLS port (`inference-router::main` :8444) is the
//! only sink the gateway is allowed to forward to. Both sides pin
//! certificates against the same CA bundle so a stolen leaf on
//! either end is detectable by cert-rotation alone.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{RootCertStore, client::WebPkiServerVerifier};

#[derive(Debug, thiserror::Error)]
pub enum MtlsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no client cert in {0}")]
    NoCert(PathBuf),
    #[error("no client key in {0}")]
    NoKey(PathBuf),
    #[error("no trust anchor in CA bundle {0}")]
    NoCa(PathBuf),
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("verifier: {0}")]
    Verifier(#[from] rustls::client::VerifierBuilderError),
}

#[derive(Debug)]
pub struct MtlsConfig {
    pub client: Arc<ClientConfig>,
}

pub fn load(
    client_cert: &Path,
    client_key: &Path,
    ca_bundle: &Path,
) -> Result<MtlsConfig, MtlsError> {
    crate::tls::install_default_crypto_provider_for_tests();
    let cert_bytes = std::fs::read(client_cert)?;
    let key_bytes = std::fs::read(client_key)?;
    let ca_bytes = std::fs::read(ca_bundle)?;

    let mut cr = std::io::BufReader::new(&cert_bytes[..]);
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cr).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        return Err(MtlsError::NoCert(client_cert.to_path_buf()));
    }

    let mut kr = std::io::BufReader::new(&key_bytes[..]);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut kr)?
        .ok_or_else(|| MtlsError::NoKey(client_key.to_path_buf()))?;

    let mut roots = RootCertStore::empty();
    let mut ar = std::io::BufReader::new(&ca_bytes[..]);
    let ca_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut ar).collect::<Result<_, _>>()?;
    if ca_certs.is_empty() {
        return Err(MtlsError::NoCa(ca_bundle.to_path_buf()));
    }
    for c in ca_certs {
        roots.add(c)?;
    }

    let verifier = WebPkiServerVerifier::builder(Arc::new(roots)).build()?;
    let cfg = ClientConfig::builder()
        .with_webpki_verifier(verifier)
        .with_client_auth_cert(certs, key)?;

    Ok(MtlsConfig {
        client: Arc::new(cfg),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> (PathBuf, PathBuf, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cert = include_bytes!("../testdata/test-cert.pem");
        let key = include_bytes!("../testdata/test-key.pem");
        let cp = dir.path().join("client.crt");
        let kp = dir.path().join("client.key");
        let ca = dir.path().join("ca.pem");
        std::fs::write(&cp, cert).unwrap();
        std::fs::write(&kp, key).unwrap();
        std::fs::write(&ca, cert).unwrap();
        (cp, kp, ca, dir)
    }

    #[test]
    fn load_succeeds_with_valid_pem() {
        let (cp, kp, ca, _d) = fixtures();
        let cfg = load(&cp, &kp, &ca).expect("valid pem");
        let _: Arc<ClientConfig> = cfg.client; // smoke
    }

    #[test]
    fn load_rejects_missing_ca() {
        let (cp, kp, _ca, d) = fixtures();
        let absent = d.path().join("absent-ca.pem");
        let err = load(&cp, &kp, &absent).unwrap_err();
        assert!(matches!(err, MtlsError::Io(_)));
    }

    #[test]
    fn load_rejects_empty_ca_bundle() {
        let (cp, kp, _ca, d) = fixtures();
        let empty = d.path().join("empty-ca.pem");
        std::fs::write(&empty, b"").unwrap();
        let err = load(&cp, &kp, &empty).unwrap_err();
        assert!(matches!(err, MtlsError::NoCa(_)));
    }

    #[test]
    fn load_rejects_missing_client_cert() {
        let (_cp, kp, ca, d) = fixtures();
        let absent = d.path().join("absent.crt");
        let err = load(&absent, &kp, &ca).unwrap_err();
        assert!(matches!(err, MtlsError::Io(_)));
    }

    #[test]
    fn load_rejects_missing_client_key() {
        let (cp, _kp, ca, d) = fixtures();
        let absent = d.path().join("absent.key");
        let err = load(&cp, &absent, &ca).unwrap_err();
        assert!(matches!(err, MtlsError::Io(_)));
    }
}
