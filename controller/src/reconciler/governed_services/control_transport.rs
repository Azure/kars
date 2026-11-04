// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::credentials;
use crate::{
    crd::KarsSandbox,
    service_control::{self, TlsIdentity},
};
use k8s_openapi::api::core::v1::Namespace;
use kube::Client;
use serde_json::Value;

pub(super) async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    identity: &Value,
) -> Result<credentials::Projection, credentials::IssuanceError> {
    if !identity["task"].is_object() {
        return credentials::ensure(client, sandbox, namespace).await;
    }
    let existing =
        match credentials::existing_configuration(client, sandbox, namespace, credentials::ADMIN)
            .await
        {
            Ok(configuration) => configuration,
            Err(error @ credentials::IssuanceError::PrivacyPending) => return Err(error),
            Err(error) => {
                credentials::quarantine_on_privacy_loss(client, sandbox, namespace).await?;
                return Err(error);
            }
        };
    let existing = existing
        .map(serde_json::from_value::<TlsIdentity>)
        .transpose()
        .map_err(|_| "Control TLS configuration invalid")?;
    let tls = prepare(identity, existing, chrono::Utc::now().timestamp())?;
    let configuration =
        serde_json::to_string(&tls).map_err(|_| "Control TLS serialization failed")?;
    credentials::ensure_bound(
        client,
        sandbox,
        namespace,
        credentials::ADMIN,
        Some(&configuration),
        None,
    )
    .await
}

fn prepare(
    identity: &Value,
    existing: Option<TlsIdentity>,
    now: i64,
) -> Result<TlsIdentity, String> {
    if let Some(existing) = existing.filter(|tls| tls.valid_for(identity, now + 172800)) {
        return Ok(existing);
    }
    let server_name = service_control::server_name(identity)?;
    let issued = crate::providers::sre_tls::issue_for(vec![server_name.clone()])?;
    Ok(TlsIdentity {
        identity: identity.clone(),
        server_name,
        ca_pem: issued.ca,
        certificate_pem: issued.certificate,
        private_key_pem: issued.private_key,
        expires_at: issued.expires_at,
    })
}

pub(super) fn mount(pod: &mut Value) {
    if let Some(volume) = pod["volumes"].as_array_mut().and_then(|volumes| {
        volumes
            .iter_mut()
            .find(|volume| volume["name"] == "governed-services-control")
    }) {
        volume["secret"]["items"]
            .as_array_mut()
            .expect("control secret items")
            .push(serde_json::json!({"key":"config.json","path":"config.json"}));
    }
}

pub(crate) fn ingress() -> Value {
    serde_json::json!({
        "from":[{"namespaceSelector":{"matchLabels":{
            "app.kubernetes.io/name":"kars","app.kubernetes.io/component":"system"}},
            "podSelector":{"matchLabels":{"app.kubernetes.io/component":"controller"}}}],
        "ports":[{"port":service_control::PORT,"protocol":"TCP"}]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_tls_reuses_current_material_and_rotates_changed_task_or_near_expiry() {
        let identity = json!({"managed":true,"sandbox":{"uid":"sandbox-uid"},
            "namespace_uid":"namespace-uid","task":{"uid":"task-uid"},
            "task_generation":1,"task_authorization":"original"});
        let now = chrono::Utc::now().timestamp();
        let first = prepare(&identity, None, now).unwrap();
        assert!(first.valid_for(&identity, now));
        let same = prepare(&identity, Some(first.clone()), now).unwrap();
        assert_eq!(same.certificate_pem, first.certificate_pem);
        assert_eq!(same.private_key_pem, first.private_key_pem);
        let mut changed = identity.clone();
        changed["task_generation"] = json!(2);
        let rotated = prepare(&changed, Some(first.clone()), now).unwrap();
        assert!(rotated.valid_for(&changed, now));
        assert_ne!(rotated.ca_pem, first.ca_pem);
        let near_expiry = prepare(&identity, Some(first.clone()), first.expires_at - 60).unwrap();
        assert_ne!(near_expiry.ca_pem, first.ca_pem);
    }

    #[test]
    fn control_tls_uses_only_the_existing_private_router_mount_and_controller_ingress() {
        let mut pod = json!({"volumes":[],"containers":[
            {"name":"openclaw","volumeMounts":[]},{"name":"agent","volumeMounts":[]},
            {"name":"inference-router","volumeMounts":[]}]});
        super::super::mount(&mut pod);
        mount(&mut pod);
        assert_eq!(pod["containers"][0]["volumeMounts"], json!([]));
        assert_eq!(pod["containers"][1]["volumeMounts"], json!([]));
        assert_eq!(
            pod["volumes"][0]["secret"]["secretName"],
            "router-services-admin"
        );
        assert_eq!(
            pod["volumes"][0]["secret"]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ingress()["from"][0]["podSelector"]["matchLabels"]["app.kubernetes.io/component"],
            "controller"
        );
        assert_eq!(
            ingress()["ports"],
            json!([{"port":service_control::PORT,"protocol":"TCP"}])
        );
    }
}
