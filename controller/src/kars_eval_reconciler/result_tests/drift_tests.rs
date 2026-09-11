// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn condition(kind: &str, status: &str) -> serde_json::Value {
    json!({"type":kind,"status":status,"reason":"Existing","message":"retain",
        "observedGeneration":1,"lastTransitionTime":"2026-09-10T20:00:00Z"})
}

#[tokio::test]
async fn drift_updates_only_degraded_preserving_unrelated_conditions_and_transition_time() {
    for initial in [None, Some("False"), Some("True")] {
        let f = fixture::setup().await;
        let ready = condition("Ready", "True");
        let progressing = condition("Progressing", "False");
        let custom = condition("CustomEvidence", "Unknown");
        let mut prior = vec![ready.clone(), progressing.clone(), custom.clone()];
        if let Some(value) = initial {
            prior.push(condition("Degraded", value));
            prior.push(condition("Degraded", value));
        }
        {
            let mut store = f.store.lock().unwrap();
            store.target["metadata"]["generation"] = json!(2);
            store.target["status"] = json!({"phase":"Running","conditions":prior});
        }
        let api = Api::<KarsSandbox>::namespaced(f.client.clone(), "tenant");
        patch_sandbox_drift(&api, "demo", Some("target-uid"), "first")
            .await
            .unwrap();
        let first = f.store.lock().unwrap().target["status"].clone();
        patch_sandbox_drift(&api, "demo", Some("target-uid"), "updated")
            .await
            .unwrap();
        let second = f.store.lock().unwrap().target["status"].clone();
        for status in [&first, &second] {
            assert_eq!(status["phase"], "Running");
            let list = status["conditions"].as_array().unwrap();
            assert_eq!(list.len(), 4);
            assert_eq!(list[0], ready);
            assert_eq!(list[1], progressing);
            assert_eq!(list[2], custom);
            let degraded = &list[3];
            assert_eq!(degraded["type"], "Degraded");
            assert_eq!(degraded["status"], "True");
            assert_eq!(degraded["reason"], "ConformanceDrift");
            assert_eq!(degraded["observedGeneration"], 2);
        }
        assert_eq!(
            first["conditions"][3]["lastTransitionTime"],
            second["conditions"][3]["lastTransitionTime"]
        );
        assert_eq!(second["conditions"][3]["message"], "updated");
        if initial == Some("True") {
            assert_eq!(
                first["conditions"][3]["lastTransitionTime"],
                "2026-09-10T20:00:00Z"
            );
        } else {
            assert_ne!(
                first["conditions"][3]["lastTransitionTime"],
                "2026-09-10T20:00:00Z"
            );
        }
    }
}

#[tokio::test]
async fn drift_patch_keeps_uid_and_resource_version_conflicts_fail_closed() {
    for conflict in ["before-read", "uid", "rv"] {
        let f = fixture::setup().await;
        let prior = json!({"phase":"Running","conditions":[condition("Ready", "True"),
            condition("Progressing", "False")]});
        {
            let mut store = f.store.lock().unwrap();
            store.target["status"] = prior.clone();
            if conflict == "before-read" {
                store.target["metadata"]["uid"] = json!("replacement");
            } else {
                store.mutate_on_target_patch = Some(conflict);
            }
        }
        let api = Api::<KarsSandbox>::namespaced(f.client.clone(), "tenant");
        let result = patch_sandbox_drift(&api, "demo", Some("target-uid"), "drift").await;
        assert!(result.is_err(), "{conflict}");
        let store = f.store.lock().unwrap();
        assert_eq!(store.target["status"], prior);
        assert_eq!(
            store.target_status_writes,
            usize::from(conflict != "before-read")
        );
        if conflict != "before-read" {
            assert!(
                matches!(result, Err(ReconcileError::Kube(kube::Error::Api(error))) if error.code == 409)
            );
        }
    }
}
