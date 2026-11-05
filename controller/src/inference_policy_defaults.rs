// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::{Value, json};

use crate::inference_policy::InferencePolicy;

pub const SELECTION: &str = "kars.azure.com/model-default-source";
pub const REGISTRY: &str = "kars-inference-providers";
pub const NAMESPACE: &str = "kars-system";

pub fn inherits(policy: &InferencePolicy) -> bool {
    policy.namespace().as_deref() == Some(NAMESPACE)
        && matches!(
            policy.name_any().as_str(),
            "sre-inference" | "bridge-orchestrator-inference"
        )
        && policy.spec.bundle_ref.is_none()
        && policy
            .metadata
            .owner_references
            .as_ref()
            .is_none_or(Vec::is_empty)
        && policy
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(SELECTION))
            .map(String::as_str)
            == Some("cluster")
}

fn default_patch(policy: &InferencePolicy, secret: &Secret) -> Result<Option<Value>, String> {
    if !inherits(policy) {
        return Ok(None);
    }
    let raw = secret
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kars.azure.com/default-model"))
        .ok_or("Select a cluster default before enabling component inheritance")?;
    let model: Value = serde_json::from_str(raw).map_err(|_| "Invalid cluster model default")?;
    if model.as_object().is_none_or(|m| m.len() != 2)
        || ["provider", "deployment"]
            .iter()
            .any(|key| model[*key].as_str().is_none_or(|s| s.trim().is_empty()))
    {
        return Err("Invalid cluster model default".into());
    }
    let current = policy
        .spec
        .model_preference
        .as_ref()
        .map(|p| json!({"provider":p.primary.provider,"deployment":p.primary.deployment}));
    if current.as_ref() == Some(&model) {
        return Ok(None);
    }
    let uid = policy
        .uid()
        .filter(|v| !v.is_empty())
        .ok_or("Policy identity unavailable")?;
    let version = policy
        .resource_version()
        .filter(|v| !v.is_empty())
        .ok_or("Policy version unavailable")?;
    Ok(Some(
        json!({"metadata":{"uid":uid,"resourceVersion":version},"spec":{"modelPreference":{"primary":model}}}),
    ))
}

fn invalid_default(message: String) -> kube::Error {
    kube::Error::Api(Box::new(kube::core::Status {
        reason: "InvalidModelDefault".into(),
        code: 422,
        message,
        ..Default::default()
    }))
}

/// Materialize only opted-in standing policies; task and Team routes stay sealed.
pub async fn reconcile(client: &Client, policy: &InferencePolicy) -> Result<bool, kube::Error> {
    if !inherits(policy) {
        return Ok(false);
    }
    use crate::credential_grant::{KarsCredentialGrant, NAME, ObjectIdentity};
    let grant = Api::<KarsCredentialGrant>::namespaced(client.clone(), NAMESPACE)
        .get(NAME)
        .await?;
    let grant = crate::credential_grants::current(
        client,
        NAMESPACE,
        &ObjectIdentity {
            name: NAME.into(),
            uid: grant.uid().unwrap_or_default(),
        },
    )
    .await
    .map_err(invalid_default)?;
    let store = grant
        .spec
        .integration_stores
        .iter()
        .find(|store| store.purpose == "providers" && store.secret.name == REGISTRY)
        .ok_or_else(|| {
            invalid_default(
                "Cluster model registry is not enrolled in the workspace credential grant".into(),
            )
        })?;
    let secret = Api::<Secret>::namespaced(client.clone(), NAMESPACE)
        .get(REGISTRY)
        .await?;
    if crate::credential_grants::identity(&secret.metadata)
        .map_err(invalid_default)?
        .0
        != store.secret.uid
        || secret.type_.as_deref() != Some("Opaque")
    {
        return Err(invalid_default(
            "Cluster model registry identity or type changed".into(),
        ));
    }
    let patch = default_patch(policy, &secret).map_err(invalid_default)?;
    if let Some(patch) = patch {
        Api::<InferencePolicy>::namespaced(client.clone(), NAMESPACE)
            .patch(
                &policy.name_any(),
                &PatchParams::default(),
                &Patch::Merge(patch),
            )
            .await?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> InferencePolicy {
        serde_json::from_value(json!({"metadata":{"name":"sre-inference","namespace":NAMESPACE,"uid":"policy","resourceVersion":"7","annotations":{SELECTION:"cluster"}},"spec":{"appliesTo":{},"modelPreference":{"primary":{"provider":"azure-openai","deployment":"old"},"fallback":[]}}})).unwrap()
    }
    fn secret() -> Secret {
        serde_json::from_value(json!({"metadata":{"annotations":{"kars.azure.com/default-model":"{\"provider\":\"foundry\",\"deployment\":\"new\"}"}}})).unwrap()
    }
    #[test]
    fn inherited_primary_is_qualified_cas_and_preserves_other_policy_fields() {
        let patch = default_patch(&policy(), &secret()).unwrap().unwrap();
        assert_eq!(
            patch,
            json!({"metadata":{"uid":"policy","resourceVersion":"7"},"spec":{"modelPreference":{"primary":{"provider":"foundry","deployment":"new"}}}})
        );
    }
    #[test]
    fn explicit_and_reviewed_policies_are_not_rewritten() {
        let mut p = policy();
        p.metadata.annotations = None;
        assert!(default_patch(&p, &secret()).unwrap().is_none());
        p = policy();
        p.metadata.name = Some("team-task-inference".into());
        assert!(default_patch(&p, &secret()).unwrap().is_none());
        p = policy();
        p.metadata.namespace = Some("tenant".into());
        assert!(default_patch(&p, &secret()).unwrap().is_none());
    }
    #[test]
    fn unchanged_default_does_not_write_or_roll_pods() {
        let mut p = policy();
        let m = p.spec.model_preference.as_mut().unwrap();
        m.primary.provider = "foundry".into();
        m.primary.deployment = "new".into();
        assert!(default_patch(&p, &secret()).unwrap().is_none());
    }
    #[tokio::test]
    async fn synchronization_requires_current_enrolled_authority_and_preserves_policy() {
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let grant_path =
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karscredentialgrants/workspace";
        let secret_path = "/api/v1/namespaces/kars-system/secrets/kars-inference-providers";
        let namespace_path = "/api/v1/namespaces/kars-system";
        let policy_path =
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/inferencepolicies/sre-inference";
        for scenario in [
            "valid",
            "disabled",
            "stale",
            "unenrolled",
            "replaced",
            "namespace",
            "denied",
            "conflict",
        ] {
            let server = MockServer::start().await;
            let mut grant = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace","namespace":NAMESPACE,"uid":"grant","resourceVersion":"1","generation":1},
                "spec":{"enabled":true,"workspaceUid":"workspace","writers":[],"integrationStores":[{"purpose":"providers","secret":{"name":REGISTRY,"uid":"registry"}}]},
                "status":{"phase":"Ready","observedGeneration":1,"reason":"Verified fixture"}});
            let mut source = secret();
            source.metadata.name = Some(REGISTRY.into());
            source.metadata.uid = Some("registry".into());
            source.metadata.resource_version = Some("1".into());
            source.type_ = Some("Opaque".into());
            let mut namespace = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":NAMESPACE,"uid":"workspace","resourceVersion":"1"}});
            match scenario {
                "disabled" => grant["spec"]["enabled"] = json!(false),
                "stale" => grant["status"]["observedGeneration"] = json!(0),
                "unenrolled" => grant["spec"]["integrationStores"] = json!([]),
                "replaced" => source.metadata.uid = Some("replacement".into()),
                "namespace" => namespace["metadata"]["uid"] = json!("replacement"),
                _ => {}
            }
            let objects = BTreeMap::from([
                (grant_path, grant),
                (secret_path, serde_json::to_value(source).unwrap()),
                (namespace_path, namespace),
            ]);
            let writes = Arc::new(Mutex::new(Vec::new()));
            let captured = writes.clone();
            Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
                if scenario != "denied" && request.method == "GET"
                    && let Some(value) = objects.get(request.url.path())
                {
                    return ResponseTemplate::new(200).set_body_json(value);
                }
                if request.method == "PATCH" && request.url.path() == policy_path {
                    let body: Value = request.body_json().unwrap();
                    captured.lock().unwrap().push(body);
                    if scenario != "conflict" {
                        return ResponseTemplate::new(200).set_body_json(policy());
                    }
                }
                let code = if scenario == "conflict" { 409 } else { 403 };
                ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Rejected","code":code}))
            }).mount(&server).await;
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let client =
                Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
            let result = reconcile(&client, &policy()).await;
            assert_eq!(
                result.is_ok(),
                scenario == "valid",
                "{scenario}: {result:?}"
            );
            let patches = writes.lock().unwrap();
            if matches!(scenario, "valid" | "conflict") {
                assert_eq!(
                    patches.as_slice(),
                    &[default_patch(&policy(), &secret()).unwrap().unwrap()]
                );
            } else {
                assert!(patches.is_empty(), "{scenario}");
            }
        }
    }
    #[test]
    fn malformed_or_missing_default_is_not_guessed() {
        for raw in [
            "bad",
            "{}",
            "{\"provider\":\"foundry\",\"deployment\":\"\"}",
        ] {
            let mut s = secret();
            s.metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert("kars.azure.com/default-model".into(), raw.into());
            assert!(default_patch(&policy(), &s).is_err());
        }
        assert!(default_patch(&policy(), &Secret::default()).is_err());
    }
}
