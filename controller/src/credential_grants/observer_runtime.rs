// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{crd::KarsSandbox, reconciler::governed_services, service_observer::Binding};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    core::v1::Pod,
};
use std::net::{IpAddr, SocketAddr};

pub(super) async fn expiry(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    mut binding: Binding,
) -> Result<Binding, String> {
    let now = chrono::Utc::now().timestamp();
    binding.expires_at = now + crate::observation_privacy::MAX_TOKEN_SECONDS;
    if let Some(raw) = governed_services::credentials::existing_configuration(
        client,
        sandbox,
        namespace,
        governed_services::credentials::OBSERVER,
    )
    .await?
        && let Ok(mut old) = serde_json::from_value::<Binding>(raw)
    {
        let expiry = old.expires_at;
        old.expires_at = binding.expires_at;
        if expiry > now + 300
            && expiry <= binding.expires_at
            && serde_json::to_value(&old).ok() == serde_json::to_value(&binding).ok()
        {
            binding.expires_at = expiry;
        }
    }
    Ok(binding)
}

pub(super) async fn probe(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    deployment: &Deployment,
    binding: &Binding,
    version: &str,
) -> Result<bool, String> {
    crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| "Observation probe namespace changed")?;
    let runtime = namespace.name_any();
    let secret = Api::<Secret>::namespaced(client.clone(), &runtime)
        .get(crate::service_observer::SECRET)
        .await
        .map_err(|e| api_error("Read exact observation probe credential", e))?;
    governed_services::credentials::validate(
        &secret,
        sandbox
            .metadata
            .uid
            .as_deref()
            .ok_or("Sandbox UID missing")?,
        namespace,
        governed_services::credentials::OBSERVER,
    )?;
    if version
        != format!(
            "{}:{}",
            secret.uid().ok_or("Observation UID missing")?,
            secret
                .resource_version()
                .ok_or("Observation revision missing")?
        )
    {
        return Ok(false);
    }
    let token = std::str::from_utf8(
        &secret
            .data
            .as_ref()
            .and_then(|d| d.get(crate::service_observer::TOKEN_KEY))
            .ok_or("Observation token missing")?
            .0,
    )
    .map_err(|_| "Observation token invalid")?;
    let pods = Api::<Pod>::namespaced(client.clone(), &runtime)
        .list(
            &ListParams::default()
                .labels(&format!("kars.azure.com/sandbox={}", sandbox.name_any())),
        )
        .await
        .map_err(|e| api_error("Read current observation consumers", e))?;
    let mut seen = false;
    for pod in pods {
        if pod.metadata.deletion_timestamp.is_some() {
            return Ok(false);
        }
        if pod.status.as_ref().and_then(|s| s.phase.as_deref()) != Some("Running")
            || pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(governed_services::credentials::OBSERVER.version_annotation))
                .map(String::as_str)
                != Some(version)
        {
            return Ok(false);
        }
        let owner = pod
            .metadata
            .owner_references
            .as_ref()
            .and_then(|owners| {
                owners.iter().find(|owner| {
                    owner.kind == "ReplicaSet"
                        && owner.api_version == "apps/v1"
                        && owner.controller == Some(true)
                })
            })
            .ok_or("Observation consumer lineage missing")?;
        let set = Api::<ReplicaSet>::namespaced(client.clone(), &runtime)
            .get(&owner.name)
            .await
            .map_err(|e| api_error("Read observation consumer lineage", e))?;
        if set.uid().as_deref() != Some(owner.uid.as_str())
            || set.metadata.deletion_timestamp.is_some()
            || set.metadata.owner_references.as_ref().is_none_or(|owners| {
                !owners.iter().any(|owner| {
                    owner.kind == "Deployment"
                        && owner.api_version == "apps/v1"
                        && owner.controller == Some(true)
                        && Some(&owner.uid) == deployment.metadata.uid.as_ref()
                })
            })
        {
            return Err("Observation consumer lineage changed".into());
        }
        let Some(ip) = pod
            .status
            .as_ref()
            .and_then(|s| s.pod_ip.as_deref())
            .and_then(|ip| ip.parse::<IpAddr>().ok())
        else {
            return Ok(false);
        };
        let ca = reqwest::Certificate::from_pem(binding.ca_pem.as_bytes())
            .map_err(|_| "Observation CA invalid")?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .resolve(
                &binding.server_name,
                SocketAddr::new(ip, crate::service_observer::PORT),
            )
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .map_err(|_| "Observation probe TLS unavailable")?;
        let Ok(response) = http
            .get(format!(
                "https://{}:{}/internal/observations/scope",
                binding.server_name,
                crate::service_observer::PORT
            ))
            .bearer_auth(token)
            .send()
            .await
        else {
            return Ok(false);
        };
        if response.status() != reqwest::StatusCode::OK {
            return Ok(false);
        }
        let Ok(value) = read_body(response).await else {
            return Ok(false);
        };
        if value["capability"] != crate::service_observer::CAPABILITY
            || value["privacy_verifier"] != crate::observation_privacy::CAPABILITY
            || value["identity"] != binding.identity
            || value["scope_id"].as_str().is_none_or(str::is_empty)
        {
            return Ok(false);
        }
        seen = true;
    }
    Ok(seen)
}

async fn read_body(mut response: reqwest::Response) -> Result<serde_json::Value, &'static str> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Observation probe body transport failed")?
    {
        if body.len().saturating_add(chunk.len()) > crate::observation_privacy::MAX_BODY {
            return Err("Observation probe body exceeds its limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "Observation probe body is not valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn payload(
        bytes: Vec<u8>,
        declared_length: usize,
    ) -> Result<serde_json::Value, &'static str> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192);
                request.push(stream.read_u8().await.unwrap());
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let result = read_body(response).await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn observer_runtime_body_accepts_the_exact_limit_and_rejects_excess() {
        let mut bytes = b"{}".to_vec();
        bytes.resize(crate::observation_privacy::MAX_BODY, b' ');
        assert_eq!(
            payload(bytes.clone(), bytes.len()).await.unwrap(),
            serde_json::json!({})
        );
        bytes.push(b' ');
        assert_eq!(
            payload(bytes.clone(), bytes.len()).await.unwrap_err(),
            "Observation probe body exceeds its limit"
        );
    }

    #[tokio::test]
    async fn observer_runtime_body_rejects_truncated_transport_even_after_valid_json() {
        assert_eq!(
            payload(b"{}".to_vec(), 4).await.unwrap_err(),
            "Observation probe body transport failed"
        );
    }

    #[tokio::test]
    async fn observer_runtime_body_rejects_invalid_json() {
        assert_eq!(
            payload(b"invalid".to_vec(), 7).await.unwrap_err(),
            "Observation probe body is not valid JSON"
        );
    }
}
