// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    crd::KarsSandbox,
    sre_registration::{AGENT_SECRET, EPOCH, KarsSRERegistration, NAME, OWNER, PRIVATE_SECRET},
};
use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{Api, Client, ResourceExt};
use serde_json::{Value, json};

pub(crate) struct Projection {
    pub epoch: String,
    pub registration_uid: String,
    pub tls_expiry: String,
    pub credential_uid: String,
}

pub(crate) async fn authorize(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<Projection>, String> {
    let reserved = sandbox.name_any() == "sre"
        || sandbox
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/role"))
            .map(String::as_str)
            == Some("sre");
    if !reserved {
        return Ok(None);
    }
    let registrations: Api<KarsSRERegistration> = Api::all(client.clone());
    let reg = registrations
        .get_opt(NAME)
        .await
        .map_err(|e| super::api_error("Read SRE enrollment", e))?
        .ok_or("SRE source is staged but not enrolled by a cluster registrar")?;
    if reg.spec.sandbox.uid != sandbox.uid().unwrap_or_default()
        || reg.spec.sandbox.namespace != sandbox.namespace().unwrap_or_default()
        || reg.spec.runtime_namespace.uid != namespace.uid().unwrap_or_default()
    {
        return Err("This SRE occupant is not the registered canonical source; no private identity is granted".into());
    }
    let epoch = super::privacy_epoch(client, &reg.spec.runtime_namespace.name)
        .await?
        .ok_or("SRE privacy epoch is absent")?;
    if !reg.spec.enabled
        || reg.status.as_ref().is_none_or(|status| {
            status.phase != "Ready"
                || status.observed_generation != reg.metadata.generation.unwrap_or_default()
                || status.privacy_epoch.as_deref() != Some(epoch.as_str())
                || !status.legacy_secret_access_denied
        })
    {
        return Err("SRE authority migration has not reached Ready".into());
    }
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &reg.spec.runtime_namespace.name);
    let secret = secrets
        .get_metadata(PRIVATE_SECRET)
        .await
        .map_err(|e| super::api_error("Read SRE TLS revision", e))?;
    let annotations = secret
        .metadata
        .annotations
        .as_ref()
        .ok_or("SRE private identity annotations missing")?;
    if annotations.get(OWNER) != reg.metadata.uid.as_ref() || annotations.get(EPOCH) != Some(&epoch)
    {
        return Err("SRE private identity does not match the registered privacy epoch".into());
    }
    Ok(Some(Projection {
        epoch,
        registration_uid: reg.uid().ok_or("SRE registration UID missing")?,
        credential_uid: secret.uid().ok_or("SRE private credential UID missing")?,
        tls_expiry: annotations
            .get("kars.azure.com/sre-tls-expiry")
            .cloned()
            .ok_or("SRE TLS expiry missing")?,
    }))
}

fn env(container: &mut Value, name: &str, value: &str) {
    let env = container["env"]
        .as_array_mut()
        .expect("controller container environment");
    env.retain(|entry| entry["name"] != name);
    env.push(json!({"name":name,"value":value}));
}

pub(crate) fn project(pod: &mut Value) {
    // Do not change serviceAccountName: Azure federation remains
    // system:serviceaccount:<namespace>:sandbox. Only the K8s diagnostic client
    // uses the separate private identity.
    pod["automountServiceAccountToken"] = false.into();
    let volumes = pod["volumes"]
        .as_array_mut()
        .expect("controller pod volumes");
    volumes.extend([
        json!({"name":"sre-api-private","secret":{"secretName":PRIVATE_SECRET}}),
        json!({"name":"sre-api-agent","secret":{"secretName":AGENT_SECRET,
            "items":[{"key":"token","path":"token"},{"key":"ca.crt","path":"ca.crt"},{"key":"namespace","path":"namespace"}]}}),
        json!({"name":"router-kubernetes","projected":{"sources":[
            {"serviceAccountToken":{"path":"token","expirationSeconds":3600}},
            {"configMap":{"name":"kube-root-ca.crt","items":[{"key":"ca.crt","path":"ca.crt"}]}},
            {"downwardAPI":{"items":[{"path":"namespace","fieldRef":{"fieldPath":"metadata.namespace"}}]}}
        ]}}),
    ]);
    for container in pod["containers"]
        .as_array_mut()
        .expect("controller pod containers")
    {
        let router = container["name"] == "inference-router";
        container["volumeMounts"]
            .as_array_mut()
            .expect("controller mounts")
            .push(json!({
                "name":if router {"router-kubernetes"} else {"sre-api-agent"},
                "mountPath":"/var/run/secrets/kubernetes.io/serviceaccount","readOnly":true,
            }));
        if router {
            container["volumeMounts"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "name":"sre-api-private","mountPath":"/etc/kars/sre-api","readOnly":true,
                }));
            env(container, "KARS_SRE_API_ENABLED", "true");
            container["readinessProbe"] = json!({
                "exec":{"command":["kars-inference-router","sre-ready"]},
                "initialDelaySeconds":3,"periodSeconds":5,"timeoutSeconds":5,
            });
        } else {
            let prior = container["env"]
                .as_array()
                .and_then(|values| values.iter().find(|entry| entry["name"] == "NO_PROXY"))
                .and_then(|entry| entry["value"].as_str())
                .unwrap_or_default();
            let no_proxy = format!("127.0.0.1,localhost,{prior}");
            env(container, "KUBERNETES_SERVICE_HOST", "127.0.0.1");
            env(container, "KUBERNETES_SERVICE_PORT", "9446");
            env(container, "KUBERNETES_SERVICE_PORT_HTTPS", "9446");
            env(container, "NO_PROXY", &no_proxy);
            env(container, "no_proxy", &no_proxy);
        }
    }
}

pub(crate) fn annotations(projection: &Projection, agent_name: &str) -> Value {
    json!({
        OWNER:projection.registration_uid,EPOCH:projection.epoch,
        "kars.azure.com/sre-tls-expiry":projection.tls_expiry,
        "kars.azure.com/sre-private-credential-uid":projection.credential_uid,
        "azure.workload.identity/skip-containers":format!("{agent_name},egress-guard"),
    })
}
