// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! An adopted intent may follow a qualified budget TLS rotation, not arbitrary trust.
use super::{BudgetError, KarsSRERegistration, KarsSandbox, Settings, StoreError};
use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{Api, Client, ResourceExt};
use rustls::{client::danger::ServerCertVerifier, pki_types::ServerName};

fn field<'a>(namespace: &'a Namespace, name: &str) -> Result<&'a str, BudgetError> {
    namespace
        .annotations()
        .get(&format!("{}{name}", crate::private_activation::PREFIX))
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(BudgetError::Authorization)
}

fn verify_identity(
    namespace: &Namespace,
    secret: &Secret,
    settings: &Settings,
    reg: &KarsSRERegistration,
    ca: &str,
) -> Result<(), BudgetError> {
    if namespace.name_any() != settings.accounting_namespace
        || namespace.uid().as_deref() != Some(reg.spec.controller.namespace.uid.as_str())
        || field(namespace, "root-namespace")? != reg.spec.controller.namespace.name
        || field(namespace, "root-namespace-uid")? != reg.spec.controller.namespace.uid
        || field(namespace, "root-deployment")? != reg.spec.controller.deployment.name
        || field(namespace, "root-deployment-uid")? != reg.spec.controller.deployment.uid
        || field(namespace, "budget-namespace")? != settings.accounting_namespace
        || field(namespace, "budget-namespace-uid")? != reg.spec.controller.namespace.uid
        || field(namespace, "budget-tls-name")? != settings.tls_secret
        || secret.name_any() != settings.tls_secret
        || secret.namespace().as_deref() != Some(settings.accounting_namespace.as_str())
        || secret.uid().as_deref() != Some(field(namespace, "budget-tls-uid")?)
        || secret.metadata.resource_version.as_deref()
            != Some(field(namespace, "budget-tls-version")?)
        || secret.metadata.deletion_timestamp.is_some()
        || secret.type_.as_deref() != Some("kubernetes.io/tls")
        || secret
            .annotations()
            .get("kars.azure.com/inference-budget-tls")
            .map(String::as_str)
            != Some("v1")
        || field(namespace, "budget-qualified-secret")? != field(namespace, "budget-tls-uid")?
        || field(namespace, "budget-qualified-bundle")? != field(namespace, "bundle-revision")?
        || field(namespace, "budget-qualified-key")? != field(namespace, "budget-key")?
        || ca.len() > 65_536
        || ca.contains("PRIVATE KEY")
    {
        return Err(BudgetError::Authorization);
    }
    let data = secret.data.as_ref().ok_or(BudgetError::Authorization)?;
    let cert = data.get("tls.crt").ok_or(BudgetError::Authorization)?;
    let key = data.get("tls.key").ok_or(BudgetError::Authorization)?;
    let certificates = rustls_pemfile::certs(&mut std::io::Cursor::new(&cert.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| BudgetError::Authorization)?;
    let private = rustls_pemfile::private_key(&mut std::io::Cursor::new(&key.0))
        .map_err(|_| BudgetError::Authorization)?
        .ok_or(BudgetError::Authorization)?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let identity = rustls::sign::CertifiedKey::from_der(certificates, private, &provider)
        .map_err(|_| BudgetError::Authorization)?;
    let public_key = identity
        .key
        .public_key()
        .ok_or(BudgetError::Authorization)?;
    if crate::providers::signing::sha256_hex(public_key.as_ref()) != field(namespace, "budget-key")?
    {
        return Err(BudgetError::Authorization);
    }
    let anchors = rustls_pemfile::certs(&mut std::io::Cursor::new(ca.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| BudgetError::Authorization)?;
    // Do not authorize appended, unrelated roots along with the rotated anchor.
    if anchors.len() != 1 {
        return Err(BudgetError::Authorization);
    }
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(anchors[0].clone())
        .map_err(|_| BudgetError::Authorization)?;
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        std::sync::Arc::new(roots),
        std::sync::Arc::new(provider),
    )
    .build()
    .map_err(|_| BudgetError::Authorization)?;
    let server_name = ServerName::try_from(format!(
        "kars-inference-budget.{}.svc",
        settings.accounting_namespace,
    ))
    .map_err(|_| BudgetError::Authorization)?;
    let (leaf, chain) = identity
        .cert
        .split_first()
        .ok_or(BudgetError::Authorization)?;
    verifier
        .verify_server_cert(
            leaf,
            chain,
            &server_name,
            &[],
            rustls::pki_types::UnixTime::now(),
        )
        .map_err(|_| BudgetError::Authorization)?;
    Ok(())
}

pub(super) async fn validate_ca(
    client: &Client,
    settings: &Settings,
    reg: &KarsSRERegistration,
    source: &KarsSandbox,
    original: Option<&str>,
    current: Option<&str>,
) -> Result<(), StoreError> {
    let original = original
        .filter(|value| !value.trim().is_empty())
        .ok_or(BudgetError::Authorization)?;
    let current = current
        .filter(|value| !value.trim().is_empty())
        .ok_or(BudgetError::Authorization)?;
    if original.trim() == current.trim() {
        return Ok(());
    }
    super::require_adopted_transition(client, reg, source).await?;
    let namespaces = Api::<Namespace>::all(client.clone());
    let namespace = namespaces
        .get(&settings.accounting_namespace)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    // SRE Ready depends on this validation. Use the independent operator-qualified
    // private root, never SRE's own readiness, to avoid a recovery dependency cycle.
    let epoch = crate::private_activation::namespace_epoch(client, &namespace)
        .await
        .map_err(|_| BudgetError::Authorization)?
        .ok_or(BudgetError::Authorization)?;
    crate::private_activation::inspect_namespace(client, &namespace, &epoch)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    crate::sre_authority::check_secret_denial(client, &settings.accounting_namespace)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let secret = Api::<Secret>::namespaced(client.clone(), &settings.accounting_namespace)
        .get(&settings.tls_secret)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    verify_identity(&namespace, &secret, settings, reg, current)?;
    let after = namespaces
        .get(&settings.accounting_namespace)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    if after.metadata.uid != namespace.metadata.uid
        || after.metadata.resource_version != namespace.metadata.resource_version
        || after.metadata.deletion_timestamp.is_some()
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "first_install_trust_tests.rs"]
mod tests;
