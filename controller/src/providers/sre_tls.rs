// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standard X.509 issuance for a pod-local compatibility endpoint.

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};

pub struct Identity {
    pub ca: String,
    pub certificate: String,
    pub private_key: String,
    pub expires_at: i64,
}

pub fn issue() -> Result<Identity, String> {
    let now = time::OffsetDateTime::now_utc();
    let not_before = now - time::Duration::hours(1);
    let expiry = now + time::Duration::days(30);
    let mut root = CertificateParams::new(Vec::<String>::new())
        .map_err(|_| "SRE CA parameters are invalid")?;
    root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root.distinguished_name
        .push(rcgen::DnType::CommonName, "Kars SRE loopback CA");
    root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    root.not_before = not_before;
    root.not_after = expiry;
    let root_key = KeyPair::generate().map_err(|_| "SRE CA key generation failed")?;
    let ca = root
        .self_signed(&root_key)
        .map_err(|_| "SRE CA issuance failed")?;
    let mut leaf = CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
        .map_err(|_| "SRE TLS parameters are invalid")?;
    leaf.not_before = not_before;
    leaf.distinguished_name
        .push(rcgen::DnType::CommonName, "Kars SRE loopback API");
    leaf.not_after = expiry;
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let key = KeyPair::generate().map_err(|_| "SRE TLS key generation failed")?;
    let certificate = leaf
        .signed_by(&key, &ca, &root_key)
        .map_err(|_| "SRE TLS issuance failed")?;
    Ok(Identity {
        ca: ca.pem(),
        certificate: certificate.pem(),
        private_key: key.serialize_pem(),
        expires_at: expiry.unix_timestamp(),
    })
}
