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
    issue_for(vec!["localhost".into(), "127.0.0.1".into()])
}

pub fn issue_for(names: Vec<String>) -> Result<Identity, String> {
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
    let mut leaf = CertificateParams::new(names).map_err(|_| "SRE TLS parameters are invalid")?;
    leaf.not_before = not_before;
    leaf.distinguished_name
        .push(rcgen::DnType::CommonName, "Kars SRE loopback API");
    leaf.not_after = expiry;
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.use_authority_key_identifier_extension = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command};

    struct Certificates(PathBuf);

    impl Drop for Certificates {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn issued_certificates_pass_strict_server_verification() {
        let files = Certificates(std::env::temp_dir().join(format!(
            "kars-sre-tls-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        )));
        std::fs::create_dir(&files.0).unwrap();
        let ca = files.0.join("ca.pem");
        let leaf = files.0.join("leaf.pem");
        let unrelated_ca = files.0.join("unrelated-ca.pem");
        std::fs::write(&unrelated_ca, issue().unwrap().ca).unwrap();
        for (identity, flag, name) in [
            (issue().unwrap(), "-verify_ip", "127.0.0.1"),
            (
                issue_for(vec!["kars-private.kars-system.svc".into()]).unwrap(),
                "-verify_hostname",
                "kars-private.kars-system.svc",
            ),
        ] {
            std::fs::write(&ca, identity.ca).unwrap();
            std::fs::write(&leaf, identity.certificate).unwrap();
            let verify = |trust: &std::path::Path, flag: &str, name: &str| {
                Command::new("openssl")
                    .args([
                        "verify",
                        "-x509_strict",
                        "-purpose",
                        "sslserver",
                        flag,
                        name,
                        "-CAfile",
                    ])
                    .arg(trust)
                    .arg(&leaf)
                    .output()
                    .expect("OpenSSL is required for strict certificate interoperability tests")
            };
            let valid = verify(&ca, flag, name);
            assert!(
                valid.status.success(),
                "Strict server verification failed: {}",
                String::from_utf8_lossy(&valid.stderr)
            );
            assert!(!verify(&unrelated_ca, flag, name).status.success());
            assert!(
                !verify(&ca, "-verify_hostname", "unrelated.invalid")
                    .status
                    .success()
            );
        }
    }
}
