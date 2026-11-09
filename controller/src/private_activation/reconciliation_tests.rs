// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NS: &str = "/api/v1/namespaces/kars-runtime";
const SOURCE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/runtime";
const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-runtime/deployments/runtime";
const REPLICAS: &str = "/apis/apps/v1/namespaces/kars-runtime/replicasets";

async fn fixture() -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    ReviewedConsumer,
) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut objects = BTreeMap::new();
    let mut activation = super::super::test_support::install(
        &mut objects,
        "core",
        "core-uid",
        "controller",
        &[("kars-runtime", "runtime-ns")],
    );
    let namespace = objects.get_mut(NS).unwrap();
    for (name, value) in [
        ("kars.azure.com/namespace-claim-version", "v1"),
        ("kars.azure.com/sandbox-name", "runtime"),
        ("kars.azure.com/sandbox-namespace", "work"),
        ("kars.azure.com/sandbox-uid", "sandbox"),
    ] {
        namespace["metadata"]["annotations"][name] = value.into();
    }
    let epoch = namespace["metadata"]["annotations"][EPOCH].clone();
    let template = json!({"metadata":{"labels":{"app":"runtime"},"annotations":{
        EPOCH:epoch,ROLLOUT_FIELDS[1]:"old-providers",ROLLOUT_FIELDS[2]:"old-credential",
        crate::credential_source::POD_VERSION:"projection-uid:1"}},
        "spec":{"containers":[{"name":"router","image":"reviewed"}]}});
    let consumer: ReviewedConsumer = serde_json::from_value(json!({"kind":"Deployment",
        "object":{"name":"runtime","uid":"deployment","resourceVersion":"1"},
        "templateDigest":template_digest(template.clone()).unwrap()}))
    .unwrap();
    activation["namespaces"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|scope| scope["namespace"]["name"] == "kars-runtime")
        .unwrap()["consumers"] = json!([consumer]);
    objects.insert(
        "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace".into(),
        json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1"},
            "spec":{"workspaceUid":"work-uid","writers":[],"privateActivation":activation}}),
    );
    objects.insert(SOURCE.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"runtime","namespace":"work","uid":"sandbox","resourceVersion":"1","generation":1,
            "annotations":{"kars.azure.com/namespace-uid":"runtime-ns"}},
        "spec":{"inferenceRef":{"name":"policy"}}}));
    objects.insert(DEPLOYMENT.into(),json!({"apiVersion":"apps/v1","kind":"Deployment",
        "metadata":{"name":"runtime","namespace":"kars-runtime","uid":"deployment","resourceVersion":"1"},
        "spec":{"replicas":1,"selector":{"matchLabels":{"app":"runtime"}},"template":template}}));
    objects.insert(
        REPLICAS.into(),
        json!({"apiVersion":"apps/v1","kind":"ReplicaSetList","metadata":{},"items":[]}),
    );
    let objects = Arc::new(Mutex::new(objects));
    let snapshot = objects.clone();
    let server = MockServer::start().await;
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        assert_eq!(request.method.as_str(), "GET", "qualification must be read-only");
        snapshot.lock().unwrap().get(request.url.path()).map(|value|
            ResponseTemplate::new(200).set_body_json(value)).unwrap_or_else(||
            ResponseTemplate::new(404).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404})))
    }).mount(&server).await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, objects, consumer)
}

#[tokio::test]
async fn credential_rollout_reuses_qualified_structure_without_history_or_new_grant() {
    let (_server, client, objects, consumer) = fixture().await;
    let original = objects.lock().unwrap().clone();
    let mut namespace: Namespace = serde_json::from_value(original[NS].clone()).unwrap();
    let source: KarsSandbox = serde_json::from_value(original[SOURCE].clone()).unwrap();
    let baseline: Deployment = serde_json::from_value(original[DEPLOYMENT].clone()).unwrap();
    let epoch = original[NS]["metadata"]["annotations"][EPOCH]
        .as_str()
        .unwrap();
    namespace
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(format!("{PREFIX}parent-deployment"), epoch.into());
    let scope: crate::credential_grant::activation::NamespaceReview =
        serde_json::from_value(json!({
        "namespace":{"name":"kars-runtime","uid":"runtime-ns","resourceVersion":"1"},
        "consumers":[consumer],"epoch":epoch}))
        .unwrap();
    let (name, value) = record(&client, &source, &namespace, &baseline, &baseline, epoch)
        .await
        .unwrap()
        .unwrap();
    namespace
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(name.clone(), value);
    objects
        .lock()
        .unwrap()
        .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
    for index in 0..15 {
        let mut changed = original[DEPLOYMENT].clone();
        for field in ROLLOUT_FIELDS {
            changed["spec"]["template"]["metadata"]["annotations"][field] =
                json!(format!("rotation-{index}"));
        }
        changed["spec"]["template"]["metadata"]["annotations"]
            [crate::credential_source::POD_VERSION] =
            json!(format!("projection-uid:{}", index + 2));
        objects
            .lock()
            .unwrap()
            .insert(DEPLOYMENT.into(), changed.clone());
        let applied: Deployment = serde_json::from_value(changed.clone()).unwrap();
        let (_, value) = record(&client, &source, &namespace, &applied, &applied, epoch)
            .await
            .unwrap()
            .unwrap();
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(name.clone(), value);
        objects
            .lock()
            .unwrap()
            .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
        assert!(
            matches(
                &client,
                &namespace,
                &consumer,
                &serde_json::from_value(changed).unwrap(),
                epoch
            )
            .await
            .unwrap()
        );
        super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
            .await
            .unwrap();
    }
    for fault in [
        "image",
        "label",
        "unknown-annotation",
        "epoch",
        "source-uid",
        "source-generation",
        "namespace-uid",
        "record-field",
    ] {
        let mut state = objects.lock().unwrap().clone();
        let valid = state.clone();
        match fault {
            "image" => {
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"][0]["image"] =
                    "unreviewed".into()
            }
            "label" => {
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["labels"]["app"] =
                    "unreviewed".into()
            }
            "unknown-annotation" => {
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
                    ["unreviewed"] = "new".into()
            }
            "epoch" => {
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
                    [EPOCH] = "stale".into()
            }
            "source-uid" => {
                state.get_mut(SOURCE).unwrap()["metadata"]["uid"] = "replacement".into()
            }
            "source-generation" => {
                state.get_mut(SOURCE).unwrap()["metadata"]["generation"] = 2.into()
            }
            "namespace-uid" => state.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into(),
            "record-field" => {
                let mut receipt: Value = serde_json::from_str(
                    state[NS]["metadata"]["annotations"][&name]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                receipt["unreviewed"] = true.into();
                state.get_mut(NS).unwrap()["metadata"]["annotations"][&name] =
                    receipt.to_string().into();
            }
            _ => unreachable!(),
        }
        *objects.lock().unwrap() = state.clone();
        let ns = serde_json::from_value(state[NS].clone()).unwrap();
        let current = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        assert!(
            !matches(&client, &ns, &consumer, &current, epoch)
                .await
                .unwrap_or(false),
            "{fault}"
        );
        assert!(
            super::super::verification::reviewed_consumers(&client, &ns, &scope, epoch)
                .await
                .is_err(),
            "{fault}"
        );
        let applied: Deployment = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        assert!(
            !matches!(
                record(&client, &source, &ns, &applied, &applied, epoch).await,
                Ok(Some(_))
            ),
            "{fault}"
        );
        *objects.lock().unwrap() = valid;
    }
    for (field, replacement) in [
        ("uid", "replacement"),
        ("name", "foreign"),
        ("namespace", "foreign"),
    ] {
        let valid = objects.lock().unwrap().clone();
        objects.lock().unwrap().get_mut(DEPLOYMENT).unwrap()["metadata"][field] =
            replacement.into();
        assert!(
            super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
                .await
                .is_err(),
            "{field}"
        );
        *objects.lock().unwrap() = valid;
    }
    namespace
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(&name);
    assert!(
        super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
            .await
            .is_err(),
        "unrecorded consumers must remain strict"
    );
}

#[tokio::test]
async fn upgrade_recovery_requires_complete_pinned_template_and_owned_history() {
    for fault in [
        "none",
        "missing",
        "foreign-owner",
        "history-image",
        "current-image",
        "unexpected-rollout",
    ] {
        let (_server, client, objects, consumer) = fixture().await;
        let mut state = objects.lock().unwrap().clone();
        let mut template = state[DEPLOYMENT]["spec"]["template"].clone();
        template["metadata"]["labels"]["pod-template-hash"] = "rs-hash".into();
        let mut replica = json!({"apiVersion":"apps/v1","kind":"ReplicaSet",
            "metadata":{"name":"runtime-old","namespace":"kars-runtime","uid":"rs","resourceVersion":"1",
                "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"runtime","uid":"deployment","controller":true}]},
            "spec":{"template":template}});
        if fault == "foreign-owner" {
            replica["metadata"]["ownerReferences"][0]["uid"] = "foreign".into();
        }
        if fault == "history-image" {
            replica["spec"]["template"]["spec"]["containers"][0]["image"] = "foreign".into();
        }
        if fault != "missing" {
            state.get_mut(REPLICAS).unwrap()["items"] = json!([replica]);
        }
        state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
            [ROLLOUT_FIELDS[1]] = "new".into();
        if fault == "current-image" {
            state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"][0]["image"] =
                "foreign".into();
        }
        let ns = serde_json::from_value(state[NS].clone()).unwrap();
        let source = serde_json::from_value(state[SOURCE].clone()).unwrap();
        let applied: Deployment = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        let mut desired = applied.clone();
        if fault == "unexpected-rollout" {
            desired
                .spec
                .as_mut()
                .unwrap()
                .template
                .metadata
                .as_mut()
                .unwrap()
                .annotations
                .as_mut()
                .unwrap()
                .insert(ROLLOUT_FIELDS[1].into(), "different".into());
        }
        let epoch = state[NS]["metadata"]["annotations"][EPOCH]
            .as_str()
            .unwrap()
            .to_string();
        *objects.lock().unwrap() = state;
        let result = record(&client, &source, &ns, &desired, &applied, &epoch).await;
        assert_eq!(result.is_ok(), fault == "none", "{fault}");
        if let Ok(Some((_, value))) = result {
            let receipt: Reconciliation = serde_json::from_str(&value).unwrap();
            assert_eq!(receipt.baseline, consumer.template_digest);
        }
    }
}

#[tokio::test]
async fn model_rollouts_require_pinned_structure_and_current_policy_for_both_runtimes() {
    const GRANT: &str =
        "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
    const POLICY: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/inferencepolicies/policy";
    for runtime in ["agent", "openclaw"] {
        for fault in [
            "none",
            "missing-history",
            "foreign-history",
            "image",
            "label",
            "other-env",
            "value-from",
            "duplicate",
            "extra-field",
            "missing-model",
            "desired",
            "policy",
            "missing-policy",
            "source",
        ] {
            let (_server, client, objects, mut consumer) = fixture().await;
            let mut state = objects.lock().unwrap().clone();
            let mut baseline = state[DEPLOYMENT]["spec"]["template"].clone();
            baseline["spec"]["containers"] = json!([
                {"name":runtime,"image":"reviewed-agent","env":[{"name":"KARS_MODEL","value":"old"},{"name":"OTHER","value":"fixed"}]},
                {"name":"inference-router","image":"reviewed-router","env":[{"name":"AZURE_OPENAI_DEPLOYMENT","value":"old"},{"name":"DEFAULT_MODEL","value":"fixed"}]}
            ]);
            if runtime == "openclaw" {
                baseline["spec"]["containers"][0]["env"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name":"OPENCLAW_MODEL","value":"old"}));
            }
            consumer.template_digest = template_digest(baseline.clone()).unwrap();
            let legacy = Reconciliation {
                epoch: state[NS]["metadata"]["annotations"][EPOCH]
                    .as_str()
                    .unwrap()
                    .into(),
                baseline: consumer.template_digest.clone(),
                structure: structure(baseline.clone()).unwrap(),
                template: consumer.template_digest.clone(),
                sandbox_uid: "sandbox".into(),
                generation: 1,
                model_structure: None,
            };
            state.get_mut(NS).unwrap()["metadata"]["annotations"][key("deployment")] =
                serde_json::to_string(&legacy).unwrap().into();
            state.get_mut(GRANT).unwrap()["spec"]["privateActivation"]["namespaces"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|scope| scope["namespace"]["name"] == "kars-runtime")
                .unwrap()["consumers"] = json!([consumer]);
            let mut replica = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"old","namespace":"kars-runtime",
                "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"runtime","uid":"deployment","controller":true}]},"spec":{"template":baseline}});
            replica["spec"]["template"]["metadata"]["labels"]["pod-template-hash"] = "old".into();
            if fault == "foreign-history" {
                replica["metadata"]["ownerReferences"][0]["uid"] = "foreign".into();
            }
            if fault != "missing-history" {
                state.get_mut(REPLICAS).unwrap()["items"] = json!([replica]);
            }
            let mut changed = baseline;
            changed["spec"]["containers"][0]["env"][0]["value"] = "new".into();
            changed["spec"]["containers"][1]["env"][0]["value"] = "new".into();
            if runtime == "openclaw" {
                changed["spec"]["containers"][0]["env"][2]["value"] = "new".into();
            }
            let desired_template = changed.clone();
            match fault {
                "image" => changed["spec"]["containers"][0]["image"] = "foreign".into(),
                "label" => changed["metadata"]["labels"]["app"] = "foreign".into(),
                "other-env" => {
                    changed["spec"]["containers"][1]["env"][1]["value"] = "foreign".into()
                }
                "value-from" => {
                    changed["spec"]["containers"][0]["env"][0]["valueFrom"] =
                        json!({"fieldRef":{"fieldPath":"metadata.name"}})
                }
                "duplicate" => {
                    let entry = changed["spec"]["containers"][0]["env"][0].clone();
                    changed["spec"]["containers"][0]["env"]
                        .as_array_mut()
                        .unwrap()
                        .push(entry);
                }
                "extra-field" => {
                    changed["spec"]["containers"][0]["env"][0]["unexpected"] = true.into()
                }
                "missing-model" => {
                    changed["spec"]["containers"][0]["env"]
                        .as_array_mut()
                        .unwrap()
                        .remove(0);
                }
                _ => {}
            }
            state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"] = changed;
            state.insert(POLICY.into(), json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy",
                "metadata":{"name":"policy","namespace":"work","uid":"policy","resourceVersion":"1","generation":1},
                "spec":{"appliesTo":{},"modelPreference":{"primary":{"provider":"foundry","deployment":if fault == "policy" {"raced"} else {"new"}}}}}));
            if fault == "missing-policy" {
                state.remove(POLICY);
            }
            let source: KarsSandbox = serde_json::from_value(state[SOURCE].clone()).unwrap();
            if fault == "source" {
                state.get_mut(SOURCE).unwrap()["metadata"]["generation"] = 2.into();
            }
            let mut namespace: Namespace = serde_json::from_value(state[NS].clone()).unwrap();
            let epoch = state[NS]["metadata"]["annotations"][EPOCH]
                .as_str()
                .unwrap()
                .to_owned();
            let applied: Deployment = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
            let mut desired = applied.clone();
            desired.spec.as_mut().unwrap().template =
                serde_json::from_value(desired_template).unwrap();
            if fault == "desired" {
                desired
                    .spec
                    .as_mut()
                    .unwrap()
                    .template
                    .spec
                    .as_mut()
                    .unwrap()
                    .containers[0]
                    .env
                    .as_mut()
                    .unwrap()[0]
                    .value = Some("raced".into());
            }
            *objects.lock().unwrap() = state;
            let result = record(&client, &source, &namespace, &desired, &applied, &epoch).await;
            assert_eq!(
                result.is_ok(),
                fault == "none",
                "{runtime}/{fault}: {result:?}"
            );
            if fault != "none" {
                continue;
            }
            let (key, value) = result.unwrap().unwrap();
            namespace
                .metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(key.clone(), value);
            objects.lock().unwrap().get_mut(REPLICAS).unwrap()["items"] = json!([]);
            // Once recorded, repeated model changes need no retained ReplicaSet or new grant.
            for model in ["third", "fourth", "old"] {
                let mut state = objects.lock().unwrap().clone();
                for container in
                    state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"]
                        .as_array_mut()
                        .unwrap()
                {
                    for entry in container["env"].as_array_mut().unwrap() {
                        if matches!(
                            entry["name"].as_str(),
                            Some("KARS_MODEL" | "OPENCLAW_MODEL" | "AZURE_OPENAI_DEPLOYMENT")
                        ) {
                            entry["value"] = model.into();
                        }
                    }
                }
                state.get_mut(POLICY).unwrap()["spec"]["modelPreference"]["primary"]["deployment"] =
                    model.into();
                let applied: Deployment =
                    serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
                *objects.lock().unwrap() = state;
                let (_, value) = record(&client, &source, &namespace, &applied, &applied, &epoch)
                    .await
                    .unwrap()
                    .unwrap();
                namespace
                    .metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(key.clone(), value);
                let current =
                    serde_json::from_value(objects.lock().unwrap()[DEPLOYMENT].clone()).unwrap();
                assert!(
                    matches(&client, &namespace, &consumer, &current, &epoch)
                        .await
                        .unwrap()
                );
            }
        }
    }
}

#[tokio::test]
async fn replaced_consumers_and_resource_version_races_cannot_be_recorded() {
    for fault in [
        "replacement",
        "version",
        "absent-review",
        "absent-grant",
        "projection-render",
    ] {
        let (_server, client, objects, _) = fixture().await;
        let original = objects.lock().unwrap().clone();
        let ns = serde_json::from_value(original[NS].clone()).unwrap();
        let source = serde_json::from_value(original[SOURCE].clone()).unwrap();
        let mut applied: Deployment = serde_json::from_value(original[DEPLOYMENT].clone()).unwrap();
        let mut desired = applied.clone();
        let epoch = original[NS]["metadata"]["annotations"][EPOCH]
            .as_str()
            .unwrap();
        match fault {
            "replacement" => applied.metadata.uid = Some("replacement".into()),
            "version" => applied.metadata.resource_version = Some("different".into()),
            "projection-render" => {
                desired
                    .spec
                    .as_mut()
                    .unwrap()
                    .template
                    .metadata
                    .as_mut()
                    .unwrap()
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(
                        crate::credential_source::POD_VERSION.into(),
                        "projection-uid:2".into(),
                    );
            }
            "absent-review" => objects
                .lock()
                .unwrap()
                .get_mut(
                    "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace",
                )
                .unwrap()["spec"]["privateActivation"]["namespaces"] = json!([]),
            "absent-grant" => {
                objects.lock().unwrap().remove(
                    "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace",
                );
            }
            _ => unreachable!(),
        }
        assert!(
            !matches!(
                record(&client, &source, &ns, &desired, &applied, epoch).await,
                Ok(Some(_))
            ),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn projection_pinned_receipts_upgrade_before_authorizing_new_projection_revisions() {
    for model_binding in [false, true] {
        let (_server, client, objects, consumer) = fixture().await;
        let mut state = objects.lock().unwrap().clone();
        let mut legacy = state[DEPLOYMENT]["spec"]["template"].clone();
        let annotations = legacy["metadata"]["annotations"].as_object_mut().unwrap();
        annotations.remove("kars.azure.com/inference-providers-version");
        annotations.remove("kars.azure.com/services-credential-version");
        let legacy_structure = template_digest(legacy).unwrap();
        state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]["kars.azure.com/inference-providers-version"] =
            "previously-rotated".into();
        let epoch = state[NS]["metadata"]["annotations"][EPOCH]
            .as_str()
            .unwrap()
            .to_owned();
        let mut receipt = json!({"epoch":epoch,"baseline":consumer.template_digest,
            "structure":legacy_structure,"template":template_digest(state[DEPLOYMENT]["spec"]["template"].clone()).unwrap(),
            "sandbox_uid":"sandbox","generation":1});
        if model_binding {
            receipt["model_structure"] = legacy_structure.into();
        }
        state.get_mut(NS).unwrap()["metadata"]["annotations"][key("deployment")] =
            receipt.to_string().into();
        *objects.lock().unwrap() = state.clone();
        let mut namespace: Namespace = serde_json::from_value(state[NS].clone()).unwrap();
        let source = serde_json::from_value(state[SOURCE].clone()).unwrap();
        let current = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        assert!(
            matches(&client, &namespace, &consumer, &current, &epoch)
                .await
                .unwrap()
        );
        let applied = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        let (name, upgraded) = record(&client, &source, &namespace, &applied, &applied, &epoch)
            .await
            .unwrap()
            .unwrap();
        let parsed: Value = serde_json::from_str(&upgraded).unwrap();
        assert_eq!(
            parsed["structure"],
            structure(state[DEPLOYMENT]["spec"]["template"].clone()).unwrap()
        );
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(name.clone(), upgraded);
        state.insert(NS.into(), serde_json::to_value(&namespace).unwrap());
        state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
            [crate::credential_source::POD_VERSION] = "projection-uid:2".into();
        *objects.lock().unwrap() = state.clone();
        let current = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        assert!(
            !matches(&client, &namespace, &consumer, &current, &epoch)
                .await
                .unwrap()
        );
        let applied = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
        let (_, rotated) = record(&client, &source, &namespace, &applied, &applied, &epoch)
            .await
            .unwrap()
            .unwrap();
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(name, rotated);
        objects
            .lock()
            .unwrap()
            .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
        assert!(
            matches(&client, &namespace, &consumer, &current, &epoch)
                .await
                .unwrap()
        );
    }
}

#[test]
fn credential_rollout_structure_retains_all_non_rollout_fields() {
    let baseline = json!({"metadata":{"labels":{"app":"sre"},"annotations":{
        EPOCH:"old", "kars.azure.com/inference-providers-version":"old", "unrelated":"fixed"}},
        "spec":{"containers":[{"name":"router","image":"reviewed"}]}});
    for name in ROLLOUT_FIELDS.into_iter().chain([EPOCH]) {
        let mut changed = baseline.clone();
        changed["metadata"]["annotations"][name] = "new".into();
        assert_eq!(
            structure(baseline.clone()).unwrap(),
            structure(changed).unwrap()
        );
    }
    for path in [
        "/spec/containers/0/image",
        "/metadata/labels/app",
        "/metadata/annotations/unrelated",
    ] {
        let mut changed = baseline.clone();
        *changed.pointer_mut(path).unwrap() = "changed".into();
        assert_ne!(
            structure(baseline.clone()).unwrap(),
            structure(changed).unwrap()
        );
    }
    let mut invalid = baseline;
    invalid["metadata"]["annotations"][ROLLOUT_FIELDS[0]] = json!({"not":"a string"});
    assert!(structure(invalid).is_err());
}
