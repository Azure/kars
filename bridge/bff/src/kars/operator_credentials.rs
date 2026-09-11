// Private observations are separate from legacy admin, control and App credentials.

use super::{cluster::Cluster, credentials::failure};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    authentication::v1::SelfSubjectReview,
    core::v1::{Namespace, Pod, Secret, ServiceAccount},
};
use kube::{
    Api, ResourceExt,
    api::{ListParams, PostParams},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};

const CAPABILITY: &str = "kars.azure.com/egress-observation/v1";
const PRIVACY_VERIFIER: &str = "kars.azure.com/observation-privacy/v1";
const SECRET: &str = "router-services-observer";
const PORT: u16 = 9447;

fn unavailable() -> kube::Error {
    failure(
        "Private read-only observation capability is unavailable; legacy admin credentials are not a fallback",
    )
}

impl Cluster {
    pub async fn private_learned_domains(&self, name: &str) -> Result<Vec<String>, kube::Error> {
        let workspace = self.core_namespace();
        let grant = self.credential_grant(&workspace).await?;
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsSandbox",
        ));
        let sandbox_api =
            Api::<DynamicObject>::namespaced_with(self.client.clone(), &workspace, &resource);
        let sandbox = sandbox_api.get(name).await?;
        let uid = sandbox.uid().ok_or_else(unavailable)?;
        if !grant.document.data["spec"]["observationTargets"]
            .as_array()
            .is_some_and(|targets| {
                targets.iter().any(|target| {
                    target["kind"] == "KarsSandbox"
                        && target["namespace"] == workspace
                        && target["name"] == name
                        && target["uid"] == uid
                })
            })
        {
            return Err(unavailable());
        }
        let observation = &sandbox.data["status"]["serviceObservation"];
        if sandbox.metadata.deletion_timestamp.is_some()
            || observation["capability"] != CAPABILITY
            || observation["phase"] != "Ready"
            || observation["grant"]["uid"] != grant.identity.uid
        {
            return Err(unavailable());
        }
        let runtime = format!("kars-{name}");
        let namespace = Api::<Namespace>::all(self.client.clone())
            .get(&runtime)
            .await?;
        let namespace_uid = namespace.uid().ok_or_else(unavailable)?;
        let annotations = namespace
            .metadata
            .annotations
            .as_ref()
            .ok_or_else(unavailable)?;
        if namespace.metadata.deletion_timestamp.is_some()
            || annotations
                .get("kars.azure.com/namespace-claim-version")
                .map(String::as_str)
                != Some("v1")
            || annotations.get("kars.azure.com/sandbox-namespace") != Some(&workspace)
            || annotations
                .get("kars.azure.com/sandbox-name")
                .map(String::as_str)
                != Some(name)
            || annotations.get("kars.azure.com/sandbox-uid") != Some(&uid)
            || sandbox
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/namespace-uid"))
                != Some(&namespace_uid)
            || observation["namespaceUid"] != namespace_uid
        {
            return Err(unavailable());
        }
        let secret = Api::<Secret>::namespaced(self.client.clone(), &runtime)
            .get(SECRET)
            .await?;
        let secret_uid = secret.uid().ok_or_else(unavailable)?;
        let version = format!(
            "{}:{}",
            secret_uid,
            secret.resource_version().ok_or_else(unavailable)?
        );
        if secret.metadata.deletion_timestamp.is_some()
            || secret.type_.as_deref() != Some("Opaque")
            || observation["secret"]["name"] != SECRET
            || observation["secret"]["uid"] != secret_uid
            || observation["version"] != version
            || secret
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/sandbox-uid"))
                != Some(&uid)
            || secret
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/namespace-uid"))
                != Some(&namespace_uid)
        {
            return Err(unavailable());
        }
        let data = secret.data.as_ref().ok_or_else(unavailable)?;
        if data
            .keys()
            .any(|key| !["observation-token", "config.json"].contains(&key.as_str()))
        {
            return Err(unavailable());
        }
        let token = String::from_utf8(
            data.get("observation-token")
                .ok_or_else(unavailable)?
                .0
                .clone(),
        )
        .map_err(|_| unavailable())?;
        let config: Value =
            serde_json::from_slice(&data.get("config.json").ok_or_else(unavailable)?.0)
                .map_err(|_| unavailable())?;
        if token.len() != 64
            || config["capability"] != CAPABILITY
            || config["verifier"]["capability"] != PRIVACY_VERIFIER
            || config["expiresAt"]
                .as_i64()
                .is_none_or(|expiry| expiry <= chrono::Utc::now().timestamp())
            || config["workspaceUid"] != grant.document.data["spec"]["workspaceUid"]
            || config["identity"]["sandbox"]["uid"] != uid
            || config["identity"]["namespace_uid"] != namespace_uid
            || config["grant"]["uid"] != grant.identity.uid
            || config["grant"]["generation"] != json!(grant.document.metadata.generation)
            || config["privacyRevision"] != observation["privacyRevision"]
            || config["privacyEpoch"] != observation["privacyEpoch"]
        {
            return Err(unavailable());
        }
        let caller = Api::<SelfSubjectReview>::all(self.client.clone())
            .create(&PostParams::default(), &SelfSubjectReview::default())
            .await?;
        let caller = serde_json::to_value(caller).map_err(|_| unavailable())?;
        let caller_uid = caller["status"]["userInfo"]["uid"]
            .as_str()
            .ok_or_else(unavailable)?;
        let caller_name = caller["status"]["userInfo"]["username"]
            .as_str()
            .ok_or_else(unavailable)?;
        let recipient = config["recipients"]
            .as_array()
            .and_then(|recipients| {
                recipients.iter().find(|recipient| {
                    recipient["uid"] == caller_uid
                        && caller_name
                            == format!(
                                "system:serviceaccount:{}:{}",
                                recipient["namespace"].as_str().unwrap_or_default(),
                                recipient["name"].as_str().unwrap_or_default()
                            )
                })
            })
            .ok_or_else(unavailable)?;
        let receiver_namespace = recipient["namespace"].as_str().ok_or_else(unavailable)?;
        let receiver_name = recipient["name"].as_str().ok_or_else(unavailable)?;
        let receiver_ns = Api::<Namespace>::all(self.client.clone())
            .get(receiver_namespace)
            .await?;
        let receiver_sa =
            Api::<ServiceAccount>::namespaced(self.client.clone(), receiver_namespace)
                .get(receiver_name)
                .await?;
        if recipient["namespaceUid"] != json!(receiver_ns.metadata.uid)
            || recipient["uid"] != json!(receiver_sa.metadata.uid)
            || receiver_ns.metadata.deletion_timestamp.is_some()
            || receiver_sa.metadata.deletion_timestamp.is_some()
        {
            return Err(unavailable());
        }
        let deployment = Api::<Deployment>::namespaced(self.client.clone(), &runtime)
            .get(name)
            .await?;
        if observation["deploymentUid"] != json!(deployment.metadata.uid)
            || deployment.metadata.deletion_timestamp.is_some()
        {
            return Err(unavailable());
        }
        let pods = Api::<Pod>::namespaced(self.client.clone(), &runtime)
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={name}")))
            .await?;
        let mut address = None;
        for pod in pods {
            if pod.metadata.deletion_timestamp.is_some()
                || pod.status.as_ref().and_then(|s| s.phase.as_deref()) != Some("Running")
                || pod
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/services-observer-version"))
                    != Some(&version)
            {
                continue;
            }
            let Some(owner) = pod.metadata.owner_references.as_ref().and_then(|owners| {
                owners
                    .iter()
                    .find(|owner| owner.kind == "ReplicaSet" && owner.controller == Some(true))
            }) else {
                continue;
            };
            let set = Api::<ReplicaSet>::namespaced(self.client.clone(), &runtime)
                .get(&owner.name)
                .await?;
            if set.uid().as_deref() != Some(owner.uid.as_str())
                || set.metadata.deletion_timestamp.is_some()
                || set.metadata.owner_references.as_ref().is_none_or(|owners| {
                    !owners.iter().any(|owner| {
                        owner.kind == "Deployment"
                            && owner.controller == Some(true)
                            && Some(owner.uid.as_str()) == deployment.metadata.uid.as_deref()
                    })
                })
            {
                continue;
            }
            if let Some(ip) = pod
                .status
                .as_ref()
                .and_then(|s| s.pod_ip.as_deref())
                .and_then(|ip| ip.parse::<IpAddr>().ok())
            {
                address = Some(SocketAddr::new(ip, PORT));
                break;
            }
        }
        let address = address.ok_or_else(unavailable)?;
        let host = config["serverName"].as_str().ok_or_else(unavailable)?;
        if host != format!("observer-{uid}.kars.internal") {
            return Err(unavailable());
        }
        let ca = reqwest::Certificate::from_pem(
            config["caPem"].as_str().ok_or_else(unavailable)?.as_bytes(),
        )
        .map_err(|_| unavailable())?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, address)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| unavailable())?;
        let origin = format!("https://{host}:{PORT}");
        let scope = client
            .get(format!("{origin}/internal/observations/scope"))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !scope.status().is_success() {
            return Err(unavailable());
        }
        let scope: Value = scope.json().await.map_err(|_| unavailable())?;
        if scope["capability"] != CAPABILITY
            || scope["identity"] != config["identity"]
            || scope["privacy_verifier"] != PRIVACY_VERIFIER
        {
            return Err(unavailable());
        }
        let response = client
            .get(format!("{origin}/internal/observations/egress/learned"))
            .bearer_auth(&token)
            .header(
                "x-kars-service-scope",
                scope["scope_id"].as_str().ok_or_else(unavailable)?,
            )
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
        let value: Value = response.json().await.map_err(|_| unavailable())?;
        if value["capability"] != CAPABILITY || value["scope_id"] != scope["scope_id"] {
            return Err(unavailable());
        }
        Ok(value["domains"]
            .as_array()
            .ok_or_else(unavailable)?
            .iter()
            .filter_map(|domain| {
                domain
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| domain["domain"].as_str().map(str::to_string))
            })
            .collect())
    }
}
