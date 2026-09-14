// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{api_error, live::Authority};
use crate::sre_registration::{
    AGENT_SECRET, EPOCH, KarsSRERegistration, OWNER, PRIVATE_SECRET, ROUTER_SA, RUNTIME_NAMESPACE,
};
use k8s_openapi::api::{
    authentication::v1::TokenRequest,
    core::v1::{Secret, ServiceAccount},
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn annotations(reg: &KarsSRERegistration) -> Value {
    json!({OWNER:reg.metadata.uid,EPOCH:reg.epoch(),
        "kars.azure.com/sandbox-uid":reg.spec.sandbox.uid,
        "kars.azure.com/namespace-uid":reg.spec.runtime_namespace.uid})
}

fn owned(meta: &kube::api::ObjectMeta, reg: &KarsSRERegistration) -> bool {
    meta.namespace.as_deref() == Some(RUNTIME_NAMESPACE)
        && meta.annotations.as_ref().is_some_and(|a| {
            a.get(OWNER) == reg.metadata.uid.as_ref()
                && a.get("kars.azure.com/sandbox-uid") == Some(&reg.spec.sandbox.uid)
                && a.get("kars.azure.com/namespace-uid") == Some(&reg.spec.runtime_namespace.uid)
        })
        && meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && meta
            .resource_version
            .as_deref()
            .is_some_and(|rv| !rv.is_empty())
        && meta.deletion_timestamp.is_none()
}

fn data(secret: &Secret, key: &str) -> Option<String> {
    String::from_utf8(secret.data.as_ref()?.get(key)?.0.clone()).ok()
}

pub(super) async fn review_targets(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    super::credential_guard::scan(client, reg).await?;
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(sa) = accounts
        .get_opt(ROUTER_SA)
        .await
        .map_err(|e| api_error("Preflight private SRE identity", e))?
        && (!owned(&sa.metadata, reg) || sa.automount_service_account_token != Some(false))
    {
        return Err("Reserved SRE ServiceAccount already belongs to another owner".into());
    }
    let secrets: Api<Secret> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    for name in [PRIVATE_SECRET, AGENT_SECRET] {
        if let Some(secret) = secrets
            .get_metadata_opt(name)
            .await
            .map_err(|e| api_error("Preflight SRE material ownership", e))?
            && !owned(&secret.metadata, reg)
        {
            return Err("Reserved SRE identity Secret already belongs to another owner".into());
        }
    }
    Ok(())
}

pub(super) async fn ensure_service_account(
    client: &Client,
    reg: &KarsSRERegistration,
    authority: &Authority,
) -> Result<ServiceAccount, String> {
    super::credential_guard::scan(client, reg).await?;
    if authority.namespace.metadata.uid.as_deref() != Some(&reg.spec.runtime_namespace.uid)
        || authority.sandbox.metadata.uid.as_deref() != Some(&reg.spec.sandbox.uid)
    {
        return Err("SRE authority changed before private identity creation".into());
    }
    let api: Api<ServiceAccount> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(sa) = api
        .get_opt(ROUTER_SA)
        .await
        .map_err(|e| api_error("Read private SRE ServiceAccount", e))?
    {
        if !owned(&sa.metadata, reg)
            || sa.automount_service_account_token != Some(false)
            || reg
                .status
                .as_ref()
                .and_then(|s| s.router_service_account_uid.as_deref())
                .is_some_and(|uid| Some(uid) != sa.metadata.uid.as_deref())
        {
            return Err("Private SRE ServiceAccount is unowned, replaced, or unsafe".into());
        }
        return Ok(sa);
    }
    let sa: ServiceAccount = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ServiceAccount","metadata":{
            "name":ROUTER_SA,"namespace":RUNTIME_NAMESPACE,"annotations":annotations(reg)},
        "automountServiceAccountToken":false,
    }))
    .map_err(|_| "Private SRE ServiceAccount serialization failed")?;
    api.create(&PostParams::default(), &sa)
        .await
        .map_err(|e| api_error("Create private SRE ServiceAccount", e))
}

async fn secret(client: &Client, reg: &KarsSRERegistration, name: &str) -> Result<Secret, String> {
    let api: Api<Secret> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(secret) = api
        .get_opt(name)
        .await
        .map_err(|e| api_error("Read owned SRE identity material", e))?
    {
        if !owned(&secret.metadata, reg) {
            return Err(
                "Existing SRE identity material has a different owner or namespace incarnation"
                    .into(),
            );
        }
        return Ok(secret);
    }
    let empty: Secret = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":name,"namespace":RUNTIME_NAMESPACE,"annotations":annotations(reg)},
    }))
    .map_err(|_| "SRE identity metadata serialization failed")?;
    api.create(&PostParams::default(), &empty)
        .await
        .map_err(|e| api_error("Create owned SRE identity metadata", e))
}

async fn update(
    client: &Client,
    secret: &Secret,
    values: BTreeMap<String, String>,
    annotations: Value,
) -> Result<(), String> {
    let api: Api<Secret> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    api.patch(&secret.name_any(),&PatchParams::default(),&Patch::Merge(json!({
        "metadata":{"uid":secret.metadata.uid,"resourceVersion":secret.metadata.resource_version,"annotations":annotations},
        "type":"Opaque","stringData":values,
    }))).await.map_err(|e|api_error("Update owned SRE identity material",e))?;
    Ok(())
}

fn cluster_connection() -> Result<(String, String), String> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST")
        .map_err(|_| "Kubernetes service host is unavailable")?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS").unwrap_or_else(|_| "443".into());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let url = reqwest::Url::parse(&format!("https://{host}:{port}"))
        .map_err(|_| "Kubernetes service address is invalid")?;
    let ca = std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/ca.crt")
        .map_err(|_| "Kubernetes service CA is unavailable")?;
    Ok((url.to_string(), ca))
}

pub(super) async fn ensure(
    client: &Client,
    reg: &KarsSRERegistration,
    authority: &Authority,
    sa: &ServiceAccount,
) -> Result<(), String> {
    ensure_with_connection(client, reg, authority, sa, cluster_connection).await
}

async fn ensure_with_connection(
    client: &Client,
    reg: &KarsSRERegistration,
    _authority: &Authority,
    sa: &ServiceAccount,
    connection: impl FnOnce() -> Result<(String, String), String>,
) -> Result<(), String> {
    super::live::verify(client, reg).await?;
    super::check_secret_denial(client, RUNTIME_NAMESPACE).await?;
    super::credential_guard::scan(client, reg).await?;
    let runtime = Api::<k8s_openapi::api::core::v1::Namespace>::all(client.clone())
        .get(RUNTIME_NAMESPACE)
        .await
        .map_err(|e| api_error("Read private activation namespace", e))?;
    let source =
        Api::<crate::crd::KarsSandbox>::namespaced(client.clone(), &reg.spec.sandbox.namespace)
            .get(&reg.spec.sandbox.name)
            .await
            .map_err(|e| api_error("Read private activation source", e))?;
    if source.metadata.uid.as_deref() != Some(reg.spec.sandbox.uid.as_str()) {
        return Err("Private activation source was replaced".into());
    }
    let consumption_epoch =
        crate::private_activation::for_sandbox(client, &source, &runtime).await?;
    let mut private = secret(client, reg, PRIVATE_SECRET).await?;
    if private
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(EPOCH))
        != Some(&reg.epoch())
        || !crate::private_activation::stamp_matches(&private, consumption_epoch.as_deref())
        || (reg
            .status
            .as_ref()
            .is_some_and(|status| status.phase == "Blocked")
            && data(&private, "kube-token").is_some())
    {
        // Replacing the bound Secret UID invalidates old private JWTs, not just
        // the current file contents, when upgrading an underguarded epoch.
        let api: Api<Secret> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
        api.delete(
            PRIVATE_SECRET,
            &DeleteParams {
                preconditions: Some(Preconditions {
                    uid: private.metadata.uid.clone(),
                    resource_version: private.metadata.resource_version.clone(),
                }),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| api_error("Retire prior-epoch SRE token anchor", e))?;
        private = secret(client, reg, PRIVATE_SECRET).await?;
    }
    let agent = secret(client, reg, AGENT_SECRET).await?;
    let mut private_values = BTreeMap::new();
    let now = chrono::Utc::now().timestamp();
    let epoch = reg.epoch();
    let tls_expiry = private
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kars.azure.com/sre-tls-expiry"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_default();
    let same_epoch = private
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(EPOCH))
        == Some(&epoch)
        && crate::private_activation::stamp_matches(&private, consumption_epoch.as_deref());
    let tls_valid = same_epoch
        && tls_expiry > now + 172_800
        && [
            "server-cert.pem",
            "server-key.pem",
            "agent-token",
            "agent-ca.crt",
        ]
        .iter()
        .all(|key| data(&private, key).is_some());
    let mut private_annotations = annotations(reg);
    if let Some(epoch) = &consumption_epoch {
        private_annotations[crate::private_activation::EPOCH] = epoch.clone().into();
    }
    if !tls_valid {
        let identity = crate::providers::sre_tls::issue()?;
        let proxy_token = crate::providers::signing::generate_service_token();
        private_values.insert("server-cert.pem".into(), identity.certificate);
        private_values.insert("server-key.pem".into(), identity.private_key);
        private_values.insert("agent-ca.crt".into(), identity.ca.clone());
        private_values.insert("agent-token".into(), proxy_token.clone());
        private_annotations["kars.azure.com/sre-tls-expiry"] =
            identity.expires_at.to_string().into();
        // Token and CA change together. The legacy Hermes client rebuilds its
        // TLS context when it observes the opaque token change.
        update(
            client,
            &agent,
            BTreeMap::from([
                ("token".into(), proxy_token),
                ("ca.crt".into(), identity.ca),
                ("namespace".into(), RUNTIME_NAMESPACE.into()),
            ]),
            annotations(reg),
        )
        .await?;
    } else if data(&agent, "token") != data(&private, "agent-token")
        || data(&agent, "ca.crt") != data(&private, "agent-ca.crt")
    {
        update(
            client,
            &agent,
            BTreeMap::from([
                (
                    "token".into(),
                    data(&private, "agent-token").ok_or("SRE proxy token missing")?,
                ),
                (
                    "ca.crt".into(),
                    data(&private, "agent-ca.crt").ok_or("SRE proxy CA missing")?,
                ),
                ("namespace".into(), RUNTIME_NAMESPACE.into()),
            ]),
            annotations(reg),
        )
        .await?;
    }
    let kube_expiry = data(&private, "kube-expires-at")
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
        .map(|time| time.timestamp())
        .unwrap_or_default();
    if !same_epoch || kube_expiry < now + 600 {
        let request: TokenRequest = serde_json::from_value(json!({
            "apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
            "spec":{"audiences":[],"expirationSeconds":3600,
                "boundObjectRef":{"apiVersion":"v1","kind":"Secret","name":PRIVATE_SECRET,"uid":private.metadata.uid}},
        })).map_err(|_|"SRE TokenRequest serialization failed")?;
        let token = Api::<ServiceAccount>::namespaced(client.clone(), RUNTIME_NAMESPACE)
            .create_token_request(ROUTER_SA, &PostParams::default(), &request)
            .await
            .map_err(|e| api_error("Issue private SRE Kubernetes token", e))?;
        let token = token
            .status
            .ok_or("Kubernetes TokenRequest omitted its token/expiry")?;
        let expiry = token.expiration_timestamp.0.to_string();
        if token.token.is_empty()
            || chrono::DateTime::parse_from_rfc3339(&expiry)
                .map_err(|_| "TokenRequest expiry was invalid")?
                .timestamp()
                <= now + 300
        {
            return Err("TokenRequest returned unusable private credentials".into());
        }
        let (url, ca) = connection()?;
        private_values.insert("kube-token".into(), token.token);
        private_values.insert("kube-expires-at".into(), expiry);
        private_values.insert("kube-ca.crt".into(), ca);
        private_values.insert(
            "config.json".into(),
            serde_json::to_string(&json!({
                "schema":"kars.azure.com/sre-api/v1","kubeUrl":url,
                "registrationUid":reg.metadata.uid,"privacyEpoch":epoch,
                "source":reg.spec.sandbox,"namespaceUid":reg.spec.runtime_namespace.uid,
                "runtimeNamespace":RUNTIME_NAMESPACE,"serviceAccountUid":sa.metadata.uid,
                "secretUid":private.metadata.uid,
            }))
            .map_err(|_| "SRE proxy configuration serialization failed")?,
        );
    }

    if !private_values.is_empty() {
        super::live::verify(client, reg).await?;
        super::check_secret_denial(client, RUNTIME_NAMESPACE).await?;
        super::credential_guard::scan(client, reg).await?;
        update(client, &private, private_values, private_annotations).await?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) async fn ensure_for_test(
    client: &Client,
    reg: &KarsSRERegistration,
    authority: &Authority,
    sa: &ServiceAccount,
) -> Result<(), String> {
    ensure_with_connection(client, reg, authority, sa, || {
        Ok(("https://kubernetes.default.svc".into(), "test-ca".into()))
    })
    .await
}

pub(super) async fn retire(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    for name in [PRIVATE_SECRET, AGENT_SECRET] {
        if let Some(secret) = secrets
            .get_opt(name)
            .await
            .map_err(|e| api_error("Read retiring SRE identity", e))?
        {
            if secret
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(OWNER))
                != reg.metadata.uid.as_ref()
            {
                continue;
            }
            if !owned(&secret.metadata, reg) {
                return Err("SRE identity material has conflicting incarnation metadata".into());
            }
            secrets
                .delete(
                    name,
                    &DeleteParams {
                        preconditions: Some(Preconditions {
                            uid: secret.metadata.uid,
                            resource_version: secret.metadata.resource_version,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| api_error("Retire owned SRE identity", e))?;
        }
    }
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(sa) = accounts
        .get_opt(ROUTER_SA)
        .await
        .map_err(|e| api_error("Read retiring private SRE identity", e))?
    {
        if sa.metadata.annotations.as_ref().and_then(|a| a.get(OWNER)) != reg.metadata.uid.as_ref()
        {
            return Ok(());
        }
        if !owned(&sa.metadata, reg) {
            return Err("Private SRE identity has conflicting incarnation metadata".into());
        }
        if reg
            .status
            .as_ref()
            .and_then(|status| status.router_service_account_uid.as_deref())
            .is_some_and(|uid| sa.metadata.uid.as_deref() != Some(uid))
        {
            return Err("Private SRE ServiceAccount UID changed; replacement preserved".into());
        }
        accounts
            .delete(
                ROUTER_SA,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: sa.metadata.uid,
                        resource_version: sa.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Retire private SRE identity", e))?;
    }
    Ok(())
}
