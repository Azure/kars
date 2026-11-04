// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{credentials, identity_read_only};
use crate::{
    crd::KarsSandbox,
    credential_grants::observer_runtime,
    service_control::{self, TlsIdentity},
};
use k8s_openapi::api::core::v1::{Namespace, Pod, Secret};
use kube::{Api, Client, ResourceExt, api::ListParams};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Deserialize)]
pub(crate) struct RequestEntry {
    pub request_id: String,
    pub scope_id: String,
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub tier: Option<u8>,
    pub port: Option<u16>,
    pub status: String,
    pub expires_at_unix: u64,
}

#[derive(Clone, Deserialize)]
pub(crate) struct RequestScope {
    pub id: String,
    pub identity: Value,
    pub assignment_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Snapshot {
    schema_version: u32,
    sandbox: String,
    pub scope: RequestScope,
    pub entries: Vec<RequestEntry>,
}

impl Snapshot {
    pub(crate) fn active_entry(&self, scope: &str, id: &str) -> Option<&RequestEntry> {
        if self.scope.id != scope {
            return None;
        }
        self.entries.iter().find(|entry| {
            entry.scope_id == scope
                && entry.request_id == id
                && entry.expires_at_unix > chrono::Utc::now().timestamp().max(0) as u64
        })
    }
}

pub(super) struct ControlClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    token: String,
    identity: Value,
}

fn configuration(secret: &Secret) -> Result<Value, String> {
    serde_json::from_slice(
        &secret
            .data
            .as_ref()
            .and_then(|d| d.get("config.json"))
            .ok_or("Control TLS configuration missing")?
            .0,
    )
    .map_err(|_| "Control TLS configuration invalid".into())
}

fn version(secret: &Secret) -> Result<String, String> {
    crate::sre_registration::control_credential_version(secret)
}

impl ControlClient {
    #[cfg(test)]
    pub(super) fn for_test(endpoint: &str, identity: Value) -> Self {
        Self {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: format!("{endpoint}/internal/access-requests")
                .parse()
                .unwrap(),
            token: "test-control-token".into(),
            identity,
        }
    }

    pub(super) async fn connect(
        client: &Client,
        sandbox: &KarsSandbox,
        namespace: &Namespace,
    ) -> Result<Option<Self>, String> {
        let identity = identity_read_only(client, sandbox, namespace).await?;
        let Some(admin) =
            credentials::existing_secret(client, sandbox, namespace, credentials::ADMIN).await?
        else {
            return Ok(None);
        };
        let tls: TlsIdentity = serde_json::from_value(configuration(&admin)?)
            .map_err(|_| "Control TLS binding invalid")?;
        if !tls.valid_for(&identity, chrono::Utc::now().timestamp()) {
            return Err("Control TLS/current identity binding changed".into());
        }
        let runtime = namespace.name_any();
        let Some(deployment) =
            credentials::review_consumer(client, &runtime, &sandbox.name_any()).await?
        else {
            return Ok(None);
        };
        let pods = Api::<Pod>::namespaced(client.clone(), &runtime)
            .list(
                &ListParams::default()
                    .labels(&format!("kars.azure.com/sandbox={}", sandbox.name_any())),
            )
            .await
            .map_err(super::api_error)?
            .items;
        // A queue belongs to one router process, never a load-balanced Service.
        if pods.len() != 1 {
            return Ok(None);
        }
        let pod = &pods[0];
        if pod.metadata.deletion_timestamp.is_some()
            || pod.status.as_ref().and_then(|s| s.phase.as_deref()) != Some("Running")
        {
            return Ok(None);
        }
        if pod
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(credentials::ADMIN.version_annotation))
            != Some(&version(&admin)?)
        {
            return Ok(None);
        }
        observer_runtime::verify_consumer_lineage(client, &runtime, &deployment, pod).await?;
        let ip = pod
            .status
            .as_ref()
            .and_then(|s| s.pod_ip.as_deref())
            .and_then(|ip| ip.parse().ok())
            .ok_or("Control consumer address missing")?;
        let ca = reqwest::Certificate::from_pem(tls.ca_pem.as_bytes())
            .map_err(|_| "Control TLS CA invalid")?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .resolve(
                &tls.server_name,
                std::net::SocketAddr::new(ip, service_control::PORT),
            )
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .map_err(|_| "Control TLS client unavailable")?;
        let endpoint = format!(
            "https://{}:{}/internal/access-requests",
            tls.server_name,
            service_control::PORT
        )
        .parse()
        .map_err(|_| "Control TLS endpoint invalid")?;
        let token = String::from_utf8(
            admin
                .data
                .as_ref()
                .and_then(|d| d.get("control-token"))
                .ok_or("Control credential missing")?
                .0
                .clone(),
        )
        .map_err(|_| "Control credential invalid")?;
        Ok(Some(Self {
            http,
            endpoint,
            token,
            identity,
        }))
    }

    pub(super) async fn inspect(&self) -> Result<Snapshot, String> {
        let response = self
            .http
            .get(self.endpoint.clone())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| "Control queue transport unavailable")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("Control queue not available".into());
        }
        let value = observer_runtime::read_bounded_body(response, 1024 * 1024).await?;
        let snapshot: Snapshot =
            serde_json::from_value(value).map_err(|_| "Control queue response invalid")?;
        if snapshot.schema_version != 1
            || snapshot.scope.identity != self.identity
            || snapshot.sandbox != self.identity["sandbox"]["name"].as_str().unwrap_or("")
            || snapshot.entries.len() > 64
            || !crate::egress_approval_compile::valid_request_scope(&snapshot.scope.id)
        {
            return Err("Control queue scope identity changed".into());
        }
        Ok(snapshot)
    }

    pub(super) async fn decide(
        &self,
        scope: &str,
        id: &str,
        approve: bool,
        answer: Option<&str>,
    ) -> Result<(), String> {
        self.submit(
            scope,
            id,
            if approve { "approved" } else { "denied" },
            answer,
            None,
        )
        .await
    }

    pub(super) async fn cancel_reviewed_tier(
        &self,
        scope: &str,
        id: &str,
        explanation: &str,
    ) -> Result<(), String> {
        self.submit(scope, id, "cancelled", None, Some(explanation))
            .await
    }

    async fn submit(
        &self,
        scope: &str,
        id: &str,
        verdict: &str,
        answer: Option<&str>,
        explanation: Option<&str>,
    ) -> Result<(), String> {
        let current = self.inspect().await?;
        let entry = current
            .active_entry(scope, id)
            .ok_or("Control request is no longer current")?;
        if entry.status != "pending" {
            return Ok(());
        }
        if explanation.is_some() && entry.kind != "tier" {
            return Err("Reviewed cancellation requires an exact tier request".into());
        }
        let mut body = serde_json::json!({
            "scope_id":scope,"request_id":id,"verdict":verdict,"answer":answer,
        });
        if let Some(explanation) = explanation {
            body["explanation"] = Value::String(explanation.into());
        }
        let mut endpoint = self.endpoint.clone();
        endpoint.set_path("/internal/access-requests/decision");
        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "Control decision transport unavailable")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("Control decision was not accepted".into());
        }
        Ok(())
    }
}
