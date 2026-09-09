// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const CAPABILITY: &str = "kars.azure.com/observation-privacy/v1";
pub const PURPOSE: &str = "read-only-observation-privacy";
pub const PATH: &str = "/internal/observations/verify-privacy";
pub const SERVICE: &str = "kars-observation-privacy";
pub const DESCRIPTOR: &str = "kars-observation-privacy";
pub const SECRET: &str = "kars-observation-privacy-tls";
pub const PORT: u16 = 9448;
pub const MAX_BODY: usize = 32768;
pub const DEADLINE_SECONDS: u64 = 8;
pub const MAX_TOKEN_SECONDS: i64 = 3600;
pub const REVISION_LABEL: &str = "kars.azure.com/observation-privacy-revision";
pub const CONTROLLER_UID: &str = "kars.azure.com/privacy-controller-uid";
pub const NAMESPACE_UID: &str = "kars.azure.com/privacy-namespace-uid";

pub fn name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_alphanumeric()
                && part.as_bytes()[part.len() - 1].is_ascii_alphanumeric()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    pub capability: String,
    pub namespace: String,
    pub namespace_uid: String,
    pub controller_uid: String,
    pub service_uid: String,
    pub port: u16,
    pub descriptor_uid: String,
    pub tls_uid: String,
    pub tls_version: String,
    pub server_name: String,
    pub ca_pem: String,
    pub expires_at: i64,
}

impl Endpoint {
    pub fn valid(&self, now: i64) -> bool {
        self.capability == CAPABILITY
            && name(&self.namespace, 63)
            && self.port >= 1024
            && [
                &self.namespace_uid,
                &self.controller_uid,
                &self.service_uid,
                &self.descriptor_uid,
                &self.tls_uid,
            ]
            .iter()
            .all(|value| name(value, 128))
            && !self.tls_version.is_empty()
            && self.tls_version.len() <= 128
            && self.server_name == format!("privacy-{}.kars.internal", self.namespace_uid)
            && self.ca_pem.starts_with("-----BEGIN CERTIFICATE-----")
            && self.ca_pem.len() <= 8192
            && self.expires_at > now
    }

    pub fn service_matches(&self, service: &k8s_openapi::api::core::v1::Service) -> bool {
        let endpoint = self;
        use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
        service.metadata.name.as_deref()==Some(SERVICE) && service.metadata.namespace.as_deref()==Some(endpoint.namespace.as_str())
            && service.metadata.uid.as_deref()==Some(endpoint.service_uid.as_str()) && service.metadata.deletion_timestamp.is_none()
            && service.spec.as_ref().is_some_and(|spec| {
                spec.type_.as_deref().unwrap_or("ClusterIP")=="ClusterIP" && spec.external_name.is_none()
                    && spec.external_ips.as_ref().is_none_or(Vec::is_empty)
                    && spec.cluster_ip.as_ref().and_then(|ip| ip.parse::<std::net::IpAddr>().ok()).is_some()
                    && spec.ports.as_ref().is_some_and(|ports| ports.len()==1 && ports[0].port==i32::from(endpoint.port)
                        && ports[0].protocol.as_deref().unwrap_or("TCP")=="TCP"
                        && ports[0].target_port.as_ref().is_none_or(|port|matches!(port,IntOrString::Int(port) if *port==i32::from(endpoint.port))))
                    && spec.selector.as_ref().is_some_and(|selector| selector.len()==3
                        && selector.get("app.kubernetes.io/name").map(String::as_str)==Some("kars")
                        && selector.get("app.kubernetes.io/component").map(String::as_str)==Some("controller")
                        && selector.get(REVISION_LABEL)==Some(&endpoint.revision()))
            })
    }
    pub fn revision(&self) -> String {
        digest(self)[..32].into()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub workspace: String,
    pub workspace_uid: String,
    pub name: String,
    pub uid: String,
    pub namespace_uid: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    Scope,
    Learned,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub capability: String,
    pub purpose: String,
    pub target: Target,
    pub grant_uid: String,
    pub grant_generation: i64,
    pub recipients: Vec<crate::service_observer::Recipient>,
    pub credential_version: String,
    pub identity: Value,
    pub scope_id: String,
    pub operation: Operation,
    pub epoch: Option<String>,
    pub nonce: String,
    pub verifier: Endpoint,
}

impl Request {
    pub fn valid(&self, now: i64) -> bool {
        self.capability == CAPABILITY
            && self.purpose == PURPOSE
            && name(&self.target.workspace, 63)
            && name(&self.target.name, 58)
            && [
                &self.target.workspace_uid,
                &self.target.uid,
                &self.target.namespace_uid,
                &self.grant_uid,
            ]
            .iter()
            .all(|value| name(value, 128))
            && self.grant_generation > 0
            && !self.recipients.is_empty()
            && self.recipients.len() <= 16
            && self.recipients.iter().all(|r| {
                name(&r.namespace, 63)
                    && name(&r.name, 253)
                    && name(&r.uid, 128)
                    && name(&r.namespace_uid, 128)
            })
            && !self.credential_version.is_empty()
            && self.credential_version.len() <= 256
            && !self.scope_id.is_empty()
            && self.scope_id.len() <= 256
            && self.nonce.len() == 64
            && self.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            && self.identity["managed"] == true
            && self.verifier.valid(now)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proof {
    pub capability: String,
    pub purpose: String,
    pub allowed: bool,
    pub request_digest: String,
    pub nonce: String,
    pub epoch: Option<String>,
}

impl Proof {
    pub fn allow(request: &Request, epoch: Option<String>) -> Self {
        Self {
            capability: CAPABILITY.into(),
            purpose: PURPOSE.into(),
            allowed: true,
            request_digest: digest(request),
            nonce: request.nonce.clone(),
            epoch,
        }
    }
    pub fn matches(&self, request: &Request) -> bool {
        self.allowed
            && self.capability == CAPABILITY
            && self.purpose == PURPOSE
            && self.request_digest == digest(request)
            && self.nonce == request.nonce
            && self.epoch == request.epoch
    }
}

pub fn digest(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("privacy wire types serialize"))
    )
}

pub fn tls_access_reviews(namespace: &str) -> Vec<Value> {
    crate::sre_privacy::secret_access_reviews(namespace)
        .into_iter()
        .filter_map(|mut review| {
            if review["spec"]["resourceAttributes"]["name"] != "router-services-admin" {
                return None;
            }
            review["spec"]["resourceAttributes"]["name"] = SECRET.into();
            Some(review)
        })
        .collect()
}

pub fn audience_tls_reviews(
    namespace: &str,
    recipients: &[crate::service_observer::Recipient],
    runtime: &str,
) -> Vec<Value> {
    let base = tls_access_reviews(namespace);
    let mut reviews = base.clone();
    for (ns, name, uid) in recipients
        .iter()
        .map(|r| (r.namespace.as_str(), r.name.as_str(), Some(r.uid.as_str())))
        .chain(std::iter::once((runtime, "sandbox", None)))
    {
        for mut review in base.clone() {
            review["spec"]["user"] = format!("system:serviceaccount:{ns}:{name}").into();
            review["spec"]["groups"] = serde_json::json!([
                "system:authenticated",
                "system:serviceaccounts",
                format!("system:serviceaccounts:{ns}")
            ]);
            if let Some(uid) = uid {
                review["spec"]["uid"] = uid.into();
            }
            reviews.push(review);
        }
    }
    reviews
}
