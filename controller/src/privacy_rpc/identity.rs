// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::observation_privacy::{self as wire, Endpoint};
use k8s_openapi::api::{
    admissionregistration::v1::{ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding},
    authorization::v1::SubjectAccessReview,
    core::v1::{ConfigMap, Namespace, Secret, Service, ServiceAccount},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::{Value, json};

pub(super) struct Identity {
    pub endpoint: Endpoint,
    pub certificate: String,
    pub key: String,
}

pub(super) async fn private_key_denial(client: &Client, namespace: &str) -> Result<(), String> {
    access_denial(client, wire::tls_access_reviews(namespace)).await
}

pub(super) async fn access_denial(client: &Client, reviews: Vec<Value>) -> Result<(), String> {
    for review in reviews {
        let review: SubjectAccessReview =
            serde_json::from_value(review).map_err(|_| "Privacy authorization request invalid")?;
        let response = Api::<SubjectAccessReview>::all(client.clone())
            .create(&PostParams::default(), &review)
            .await
            .map_err(|_| "Privacy authorization unavailable")?;
        crate::sre_privacy::require_denial(
            &serde_json::to_value(response).map_err(|_| "Privacy authorization invalid")?,
        )
        .map_err(str::to_string)?;
    }
    Ok(())
}

pub(super) async fn admission(client: &Client) -> Result<(), String> {
    for name in [
        "kars-observation-privacy-material",
        "kars-observation-privacy-pods",
        "kars-observation-privacy-service",
    ] {
        let policy = Api::<ValidatingAdmissionPolicy>::all(client.clone())
            .get(name)
            .await
            .map_err(|_| "Privacy RPC admission unavailable")?;
        let binding = Api::<ValidatingAdmissionPolicyBinding>::all(client.clone())
            .get(name)
            .await
            .map_err(|_| "Privacy RPC admission binding unavailable")?;
        if policy.metadata.deletion_timestamp.is_some()
            || binding.metadata.deletion_timestamp.is_some()
            || policy
                .spec
                .as_ref()
                .and_then(|s| s.failure_policy.as_deref())
                != Some("Fail")
            || policy.status.as_ref().is_none_or(|s| {
                s.observed_generation != policy.metadata.generation
                    || s.type_checking.as_ref().is_none_or(|t| {
                        t.expression_warnings
                            .as_ref()
                            .is_some_and(|v| !v.is_empty())
                    })
            })
            || binding.spec.as_ref().is_none_or(|s| {
                s.policy_name.as_deref() != Some(name)
                    || s.validation_actions
                        .as_ref()
                        .is_none_or(|a| !a.iter().any(|v| v == "Deny"))
            })
        {
            return Err("Privacy RPC admission is not currently enforced".into());
        }
    }
    Ok(())
}

pub(super) fn metadata(namespace: &str, ns_uid: &str, sa_uid: &str, name: &str) -> Value {
    json!({"name":name,"namespace":namespace,"annotations":{wire::CONTROLLER_UID:sa_uid,wire::NAMESPACE_UID:ns_uid},
        "labels":{"app.kubernetes.io/managed-by":"kars-controller"},
        "ownerReferences":[{"apiVersion":"v1","kind":"ServiceAccount","name":"kars-controller","uid":sa_uid,
            "controller":true,"blockOwnerDeletion":false}]})
}

pub(super) async fn prepare(client: &Client, namespace: &str) -> Result<Identity, String> {
    const ERROR: &str = "Private verifier identity unavailable";
    admission(client).await?;
    let ns = Api::<Namespace>::all(client.clone())
        .get(namespace)
        .await
        .map_err(|_| ERROR)?;
    let sa = Api::<ServiceAccount>::namespaced(client.clone(), namespace)
        .get("kars-controller")
        .await
        .map_err(|_| ERROR)?;
    if ns.metadata.deletion_timestamp.is_some() || sa.metadata.deletion_timestamp.is_some() {
        return Err(ERROR.into());
    }
    let ns_uid = ns.uid().ok_or(ERROR)?;
    let sa_uid = sa.uid().ok_or(ERROR)?;
    let epoch = crate::sre_authority::privacy_epoch(client, namespace).await?;
    private_key_denial(client, namespace).await?;
    let service = Api::<Service>::namespaced(client.clone(), namespace)
        .get(wire::SERVICE)
        .await
        .map_err(|_| ERROR)?;
    if service.metadata.deletion_timestamp.is_some()
        || service.spec.as_ref().is_none_or(|s| {
            s.type_.as_deref().unwrap_or("ClusterIP") != "ClusterIP"
                || s.ports
                    .as_ref()
                    .is_none_or(|ports| ports.len() != 1 || ports[0].port != i32::from(wire::PORT))
                || s.selector.as_ref().is_none_or(|selector| {
                    selector.get("app.kubernetes.io/name").map(String::as_str) != Some("kars")
                        || selector
                            .get("app.kubernetes.io/component")
                            .map(String::as_str)
                            != Some("controller")
                })
        })
    {
        return Err(ERROR.into());
    }
    let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
    let existing = secrets.get_opt(wire::SECRET).await.map_err(|_| ERROR)?;
    if existing.as_ref().is_some_and(|secret| {
        secret.type_.as_deref() != Some("Opaque")
            || !super::discovery::owned(&secret.metadata, &ns_uid, &sa_uid)
    }) {
        return Err("Foreign privacy TLS identity preserved".into());
    }
    let now = chrono::Utc::now().timestamp();
    let server_name = format!("privacy-{ns_uid}.kars.internal");
    let parsed = existing
        .as_ref()
        .and_then(|s| s.data.as_ref())
        .and_then(|d| d.get("config.json"))
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes.0).ok());
    let reusable = parsed.as_ref().is_some_and(|config| {
        config["serverName"] == server_name
            && config["epoch"] == json!(epoch)
            && config["privacyRevision"] == crate::sre_privacy::REVISION
            && config["expiresAt"]
                .as_i64()
                .is_some_and(|expiry| expiry > now + 172800)
    });
    let configuration = if reusable {
        parsed.ok_or(ERROR)?
    } else {
        let issued = crate::providers::sre_tls::issue_for(vec![server_name.clone()])?;
        json!({"serverName":server_name,"caPem":issued.ca,"certificatePem":issued.certificate,
            "privateKeyPem":issued.private_key,"expiresAt":issued.expires_at,"epoch":epoch,
            "privacyRevision":crate::sre_privacy::REVISION})
    };
    let raw = serde_json::to_string(&configuration).map_err(|_| ERROR)?;
    let secret = match existing {
        Some(existing) if reusable => existing,
        Some(existing) => secrets.patch(wire::SECRET, &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"uid":existing.metadata.uid,"resourceVersion":existing.metadata.resource_version},
                "stringData":{"config.json":raw}}))).await.map_err(|_| ERROR)?,
        None => {
            let secret: Secret = serde_json::from_value(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                "metadata":metadata(namespace,&ns_uid,&sa_uid,wire::SECRET),"stringData":{"config.json":raw}})).map_err(|_| ERROR)?;
            secrets.create(&PostParams::default(), &secret).await.map_err(|_| ERROR)?
        }
    };
    if !super::discovery::owned(&secret.metadata, &ns_uid, &sa_uid) {
        return Err(ERROR.into());
    }
    let descriptors = Api::<ConfigMap>::namespaced(client.clone(), namespace);
    let descriptor = match descriptors
        .get_opt(wire::DESCRIPTOR)
        .await
        .map_err(|_| ERROR)?
    {
        Some(value) if super::discovery::owned(&value.metadata, &ns_uid, &sa_uid) => value,
        Some(_) => return Err("Foreign privacy descriptor preserved".into()),
        None => descriptors
            .create(
                &PostParams::default(),
                &serde_json::from_value(json!({"apiVersion":"v1","kind":"ConfigMap",
                "metadata":metadata(namespace,&ns_uid,&sa_uid,wire::DESCRIPTOR)}))
                .map_err(|_| ERROR)?,
            )
            .await
            .map_err(|_| ERROR)?,
    };
    let string = |field: &str| {
        configuration[field]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ERROR.to_string())
    };
    let endpoint = Endpoint {
        capability: wire::CAPABILITY.into(),
        namespace: namespace.into(),
        namespace_uid: ns_uid,
        controller_uid: sa_uid,
        service_uid: service.uid().ok_or(ERROR)?,
        port: wire::PORT,
        descriptor_uid: descriptor.uid().ok_or(ERROR)?,
        tls_uid: secret.uid().ok_or(ERROR)?,
        tls_version: secret.resource_version().ok_or(ERROR)?,
        server_name,
        ca_pem: string("caPem")?,
        expires_at: configuration["expiresAt"].as_i64().ok_or(ERROR)?,
    };
    if !endpoint.valid(now) {
        return Err(ERROR.into());
    }
    Ok(Identity {
        endpoint,
        certificate: string("certificatePem")?,
        key: string("privateKeyPem")?,
    })
}
