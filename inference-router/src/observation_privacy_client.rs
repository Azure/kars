// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    access_request::Scope,
    observation_privacy::{self as wire, Operation},
    service_observer::Binding,
};
use futures::StreamExt;
use k8s_openapi::api::{
    authorization::v1::SubjectAccessReview,
    core::v1::{ConfigMap, Namespace, Service, ServiceAccount},
};
use kube::{Api, Client, api::PostParams};
use std::net::{IpAddr, SocketAddr};

const ERROR: &str = "Private observation privacy verifier unavailable";

fn live(meta: &kube::api::ObjectMeta, uid: &str) -> bool {
    meta.uid.as_deref() == Some(uid) && meta.deletion_timestamp.is_none()
}

async fn address(
    client: &Client,
    endpoint: &wire::Endpoint,
    binding: &Binding,
    scope: &Scope,
) -> Result<SocketAddr, String> {
    let mut diagnostic = wire::Readiness::new("verifier_endpoint");
    if !endpoint.valid(chrono::Utc::now().timestamp()) {
        return Err(ERROR.into());
    }
    diagnostic.stage("verifier_namespace_read");
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&endpoint.namespace)
        .await
        .map_err(|error| {
            diagnostic.api(&error);
            ERROR
        })?;
    diagnostic.stage("verifier_account_read");
    let account = Api::<ServiceAccount>::namespaced(client.clone(), &endpoint.namespace)
        .get("kars-controller")
        .await
        .map_err(|error| {
            diagnostic.api(&error);
            ERROR
        })?;
    diagnostic.stage("verifier_identity_current");
    if !live(&namespace.metadata, &endpoint.namespace_uid)
        || !live(&account.metadata, &endpoint.controller_uid)
    {
        return Err(ERROR.into());
    }
    diagnostic.stage("verifier_descriptor_read");
    let descriptor = Api::<ConfigMap>::namespaced(client.clone(), &endpoint.namespace)
        .get(wire::DESCRIPTOR)
        .await
        .map_err(|error| {
            diagnostic.api(&error);
            ERROR
        })?;
    diagnostic.stage("verifier_descriptor_current");
    if !live(&descriptor.metadata, &endpoint.descriptor_uid)
        || descriptor
            .metadata
            .annotations
            .as_ref()
            .is_none_or(|annotations| {
                annotations.get(wire::CONTROLLER_UID) != Some(&endpoint.controller_uid)
                    || annotations.get(wire::NAMESPACE_UID) != Some(&endpoint.namespace_uid)
            })
        || descriptor
            .data
            .as_ref()
            .and_then(|data| data.get("config.json"))
            .and_then(|raw| serde_json::from_str::<wire::Endpoint>(raw).ok())
            .as_ref()
            != Some(endpoint)
    {
        return Err(ERROR.into());
    }
    diagnostic.stage("verifier_service_read");
    let service = Api::<Service>::namespaced(client.clone(), &endpoint.namespace)
        .get(wire::SERVICE)
        .await
        .map_err(|error| {
            diagnostic.api(&error);
            ERROR
        })?;
    diagnostic.stage("verifier_service_current");
    let spec = service.spec.as_ref().ok_or(ERROR)?;
    if !endpoint.service_matches(&service) {
        return Err(ERROR.into());
    }
    let ip = spec
        .cluster_ip
        .as_ref()
        .and_then(|ip| ip.parse::<IpAddr>().ok())
        .ok_or(ERROR)?;
    for review in wire::audience_tls_reviews(
        &endpoint.namespace,
        &binding.recipients,
        &format!("kars-{}", scope.identity.sandbox.name),
    ) {
        diagnostic.stage("verifier_audience_review");
        let request: SubjectAccessReview = serde_json::from_value(review).map_err(|_| ERROR)?;
        let response = Api::<SubjectAccessReview>::all(client.clone())
            .create(&PostParams::default(), &request)
            .await
            .map_err(|error| {
                diagnostic.api(&error);
                ERROR
            })?;
        diagnostic.stage("verifier_audience_denial");
        crate::sre_privacy::require_denial(&serde_json::to_value(response).map_err(|_| ERROR)?)
            .map_err(|_| ERROR)?;
    }
    diagnostic.finish();
    Ok(SocketAddr::new(ip, endpoint.port))
}

pub(crate) async fn verify(
    client: &Client,
    binding: &Binding,
    token: &str,
    version: &str,
    scope: &Scope,
    operation: Operation,
) -> Result<(), String> {
    let mut diagnostic = wire::Readiness::new("verifier_binding");
    let verifier = binding.verifier.as_ref().ok_or(ERROR)?;
    diagnostic.stage("verifier_address");
    let address = address(client, verifier, binding, scope).await?;
    diagnostic.stage("verifier_request");
    let nonce: String = rand::random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let request = wire::Request {
        capability: wire::CAPABILITY.into(),
        purpose: wire::PURPOSE.into(),
        target: wire::Target {
            workspace: scope.identity.sandbox.namespace.clone(),
            workspace_uid: binding.workspace_uid.clone(),
            name: scope.identity.sandbox.name.clone(),
            uid: scope.identity.sandbox.uid.clone(),
            namespace_uid: scope.identity.namespace_uid.clone(),
        },
        grant_uid: binding.grant.uid.clone(),
        grant_generation: binding.grant.generation,
        recipients: binding.recipients.clone(),
        credential_version: version.into(),
        identity: serde_json::to_value(&scope.identity).map_err(|_| ERROR)?,
        scope_id: scope.id.clone(),
        operation,
        epoch: binding.privacy_epoch.clone(),
        nonce,
        verifier: verifier.clone(),
    };
    if !request.valid(chrono::Utc::now().timestamp()) {
        return Err(ERROR.into());
    }
    diagnostic.stage("verifier_exchange");
    exchange(verifier, address, token, &request).await?;
    diagnostic.finish();
    Ok(())
}

async fn exchange(
    endpoint: &wire::Endpoint,
    address: SocketAddr,
    token: &str,
    request: &wire::Request,
) -> Result<(), String> {
    let mut diagnostic = wire::Readiness::new("verifier_tls_client");
    let ca = reqwest::Certificate::from_pem(endpoint.ca_pem.as_bytes()).map_err(|_| ERROR)?;
    // Deliberately no shared client/proof cache: each request re-pins the current
    // descriptor and establishes TLS to the current canonical Service.
    let http = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .tls_built_in_root_certs(false)
        .add_root_certificate(ca)
        .resolve(&endpoint.server_name, address)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(wire::DEADLINE_SECONDS + 2))
        .build()
        .map_err(|_| ERROR)?;
    diagnostic.stage("verifier_transport");
    let response = http
        .post(format!(
            "https://{}:{}{}",
            endpoint.server_name,
            address.port(),
            wire::PATH
        ))
        .bearer_auth(token)
        .json(request)
        .send()
        .await
        .map_err(|error| {
            diagnostic.transport(&error);
            ERROR
        })?;
    diagnostic.stage("verifier_http");
    diagnostic.status(response.status().as_u16());
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|n| n > wire::MAX_BODY as u64)
    {
        return Err(ERROR.into());
    }
    diagnostic.stage("verifier_body");
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|error| {
            diagnostic.transport(&error);
            ERROR
        })?;
        if bytes.len() + part.len() > wire::MAX_BODY {
            return Err(ERROR.into());
        }
        bytes.extend_from_slice(&part);
    }
    diagnostic.stage("verifier_proof_json");
    let proof: wire::Proof = serde_json::from_slice(&bytes).map_err(|_| ERROR)?;
    diagnostic.stage("verifier_proof_binding");
    if !proof.matches(request) || !endpoint.valid(chrono::Utc::now().timestamp()) {
        return Err(ERROR.into());
    }
    diagnostic.finish();
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
