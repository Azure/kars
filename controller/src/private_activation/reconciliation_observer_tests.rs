// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";

fn issuance(version: &str) -> Value {
    json!({"capability":crate::service_observer::CAPABILITY,"phase":"Prepared","reason":"AwaitingPrivateVerifierCapability",
        "version":version,"grant":{"name":"workspace","uid":"grant"},
        "secret":{"name":crate::service_observer::SECRET,"uid":"observer"},
        "namespaceUid":"runtime-ns","privacyRevision":crate::sre_privacy::REVISION,
        "privacyEpoch":"privacy-epoch","deploymentUid":"deployment"})
}

fn render(baseline: &Value, source: &KarsSandbox) -> Value {
    let mut desired = baseline.clone();
    crate::credential_grants::mount_observations(&mut desired["spec"]["template"]["spec"], source);
    let mut deployment: Deployment = serde_json::from_value(desired).unwrap();
    crate::credential_grants::decorate_observations(&mut deployment, source);
    serde_json::to_value(deployment).unwrap()
}

#[tokio::test]
async fn observer_enable_rotation_and_removal_keep_original_review_and_exact_current_receipt() {
    for runtime in ["agent", "openclaw"] {
        let (_server, client, objects, mut consumer) = fixture().await;
        let mut state = objects.lock().unwrap().clone();
        state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"] = json!({
            "volumes":[{"name":"existing","emptyDir":{}}],
            "containers":[{"name":runtime,"image":"reviewed-agent"},
                {"name":"inference-router","image":"reviewed-router","env":[{"name":"EXISTING","value":"fixed"}],
                    "volumeMounts":[{"name":"existing","mountPath":"/existing"}]}]});
        let baseline = state[DEPLOYMENT].clone();
        consumer.template_digest = template_digest(baseline["spec"]["template"].clone()).unwrap();
        let grant = state.get_mut(GRANT).unwrap();
        for scope in grant["spec"]["privateActivation"]["namespaces"]
            .as_array_mut()
            .unwrap()
        {
            if scope["namespace"]["name"] == "kars-runtime" {
                scope["consumers"] = json!([consumer]);
            }
        }
        grant["spec"]["observationTargets"] =
            json!([{"kind":"KarsSandbox","namespace":"work","name":"runtime","uid":"sandbox"}]);
        *objects.lock().unwrap() = state.clone();
        let epoch = state[NS]["metadata"]["annotations"][EPOCH]
            .as_str()
            .unwrap()
            .to_owned();
        let mut ns: Namespace = serde_json::from_value(state[NS].clone()).unwrap();
        let source = serde_json::from_value(state[SOURCE].clone()).unwrap();
        let applied = serde_json::from_value(baseline.clone()).unwrap();
        let (key, receipt) = record(&client, &source, &ns, &applied, &applied, &epoch)
            .await
            .unwrap()
            .unwrap();
        ns.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(key.clone(), receipt);
        let baseline_structure = structure(baseline["spec"]["template"].clone()).unwrap();
        for revision in [Some("observer:1"), Some("observer:2"), None] {
            state.get_mut(SOURCE).unwrap()["status"]["serviceObservation"] =
                revision.map(issuance).unwrap_or(Value::Null);
            let source: KarsSandbox = serde_json::from_value(state[SOURCE].clone()).unwrap();
            let desired = render(&baseline, &source);
            let mut current = desired.clone();
            for volume in current["spec"]["template"]["spec"]["volumes"]
                .as_array_mut()
                .unwrap()
            {
                if volume.get("secret").is_some() {
                    volume["secret"]["defaultMode"] = 420.into();
                }
            }
            state.insert(DEPLOYMENT.into(), current.clone());
            *objects.lock().unwrap() = state.clone();
            assert!(
                !matches(
                    &client,
                    &ns,
                    &consumer,
                    &serde_json::from_value(current.clone()).unwrap(),
                    &epoch
                )
                .await
                .unwrap()
            );
            if revision.is_some() {
                // The pre-repair projection rejects exactly the actual renderer's structural additions.
                assert_ne!(
                    rollout_structure(current["spec"]["template"].clone(), true).unwrap(),
                    baseline_structure
                );
            }
            let desired: Deployment = serde_json::from_value(desired).unwrap();
            let applied: Deployment = serde_json::from_value(current.clone()).unwrap();
            let (_, receipt) = record(&client, &source, &ns, &desired, &applied, &epoch)
                .await
                .unwrap()
                .unwrap();
            let receipt_json: Value = serde_json::from_str(&receipt).unwrap();
            assert_eq!(receipt_json["baseline"], consumer.template_digest);
            assert_eq!(receipt_json["structure"], baseline_structure);
            assert_eq!(
                receipt_json["template"],
                template_digest(current["spec"]["template"].clone()).unwrap()
            );
            ns.metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(key.clone(), receipt);
            assert!(
                matches(
                    &client,
                    &ns,
                    &consumer,
                    &serde_json::from_value(current.clone()).unwrap(),
                    &epoch
                )
                .await
                .unwrap()
            );
            if revision != Some("observer:1") {
                continue;
            }
            for credentials in [true, false] {
                let mut pinned = receipt_json.clone();
                pinned["structure"] =
                    rollout_structure(current["spec"]["template"].clone(), credentials)
                        .unwrap()
                        .into();
                pinned["model_structure"] =
                    rollout_model_structure(current["spec"]["template"].clone(), credentials)
                        .unwrap()
                        .into();
                ns.metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(key.clone(), pinned.to_string());
                assert!(
                    matches(
                        &client,
                        &ns,
                        &consumer,
                        &serde_json::from_value(current.clone()).unwrap(),
                        &epoch
                    )
                    .await
                    .unwrap()
                );
                let (_, upgraded) = record(&client, &source, &ns, &desired, &applied, &epoch)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&upgraded).unwrap()["structure"],
                    baseline_structure
                );
                ns.metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(key.clone(), upgraded);
            }
            for fault in [
                "secret",
                "items",
                "mode",
                "mount",
                "env",
                "duplicate",
                "extra-volume",
                "agent-mount",
                "status-version",
                "status-privacy",
                "grant",
                "target",
                "namespace",
                "deployment",
                "privacy-revision",
                "desired",
            ] {
                let mut bad = state.clone();
                let mut bad_desired = desired.clone();
                let template = &mut bad.get_mut(DEPLOYMENT).unwrap()["spec"]["template"];
                match fault {
                    "secret" => {
                        template["spec"]["volumes"][1]["secret"]["secretName"] = "other".into()
                    }
                    "items" => {
                        template["spec"]["volumes"][1]["secret"]["items"][0]["path"] =
                            "other".into()
                    }
                    "mode" => template["spec"]["volumes"][1]["secret"]["defaultMode"] = 511.into(),
                    "mount" => {
                        template["spec"]["containers"][1]["volumeMounts"][1]["readOnly"] =
                            false.into()
                    }
                    "env" => {
                        template["spec"]["containers"][1]["env"][1]["valueFrom"] =
                            json!({"fieldRef":{"fieldPath":"metadata.name"}})
                    }
                    "duplicate" => {
                        let entry = template["spec"]["volumes"][1].clone();
                        template["spec"]["volumes"]
                            .as_array_mut()
                            .unwrap()
                            .push(entry);
                    }
                    "extra-volume" => {
                        template["spec"]["volumes"][1]["secret"]["optional"] = true.into();
                    }
                    "agent-mount" => {
                        template["spec"]["containers"][0]["volumeMounts"] =
                            json!([{"name":"service-observations","mountPath":"/stolen"}])
                    }
                    "status-version" => {
                        bad.get_mut(SOURCE).unwrap()["status"]["serviceObservation"]["version"] =
                            "observer:3".into()
                    }
                    "status-privacy" => {
                        bad.get_mut(SOURCE).unwrap()["status"]["serviceObservation"]["privacyEpoch"] =
                            "changed".into()
                    }
                    "grant" => bad.get_mut(GRANT).unwrap()["metadata"]["uid"] = "replaced".into(),
                    "target" => {
                        bad.get_mut(GRANT).unwrap()["spec"]["observationTargets"] = json!([])
                    }
                    "namespace" => {
                        bad.get_mut(SOURCE).unwrap()["status"]["serviceObservation"]["namespaceUid"] =
                            "replaced".into()
                    }
                    "deployment" => {
                        bad.get_mut(SOURCE).unwrap()["status"]["serviceObservation"]["deploymentUid"] =
                            "replaced".into()
                    }
                    "privacy-revision" => {
                        bad.get_mut(SOURCE).unwrap()["status"]["serviceObservation"]["privacyRevision"] =
                            "changed".into()
                    }
                    "desired" => bad_desired
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
                        .remove(observer::VERSION)
                        .map(|_| ())
                        .unwrap(),
                    _ => unreachable!(),
                }
                *objects.lock().unwrap() = bad.clone();
                let bad_applied = serde_json::from_value(bad[DEPLOYMENT].clone()).unwrap();
                let render_source =
                    if matches!(fault, "namespace" | "deployment" | "privacy-revision") {
                        serde_json::from_value(bad[SOURCE].clone()).unwrap()
                    } else {
                        source.clone()
                    };
                assert!(
                    record(
                        &client,
                        &render_source,
                        &ns,
                        &bad_desired,
                        &bad_applied,
                        &epoch
                    )
                    .await
                    .is_err(),
                    "{runtime}/{fault}"
                );
            }
            *objects.lock().unwrap() = state.clone();
        }
        assert!(state[REPLICAS]["items"].as_array().unwrap().is_empty());
    }
}
