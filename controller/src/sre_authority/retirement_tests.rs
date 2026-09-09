// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::privacy_tests::admission_ready;
use super::tests::{State, fixture, registration};
use super::{bindings, reconcile};
use crate::sre_registration::{
    BindingReview, ConsumerReview, KarsSRERegistration, RUNTIME_NAMESPACE,
};
use k8s_openapi::api::rbac::v1::{ClusterRoleBinding, RoleBinding};
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const CRBS: &str = "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings";
const CONSUMER: &str = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
const RETIRED: &str = "kars.azure.com/sre-legacy-retired";

fn reviews(state: &Arc<Mutex<State>>, kind: &str) -> (KarsSRERegistration, String) {
    admission_ready(state);
    let mut locked = state.lock().unwrap();
    locked.binding["metadata"]["annotations"] = json!({"e2e-retained":"yes"});
    let local = kind == "RoleBinding";
    let path = if local {
        "/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/rolebindings/second".into()
    } else {
        format!("{CRBS}/second")
    };
    let mut second = json!({
        "apiVersion":"rbac.authorization.k8s.io/v1","kind":kind,
        "metadata":{"name":"second","uid":"second-binding","resourceVersion":"7",
            "annotations":{"e2e-retained":"yes"}},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":if local {"Role"} else {"ClusterRole"},
            "name":"custom-retirement-role"},"subjects":locked.binding["subjects"],
    });
    if local {
        second["metadata"]["namespace"] = RUNTIME_NAMESPACE.into();
    }
    let mut reg = registration();
    reg.spec.legacy_bindings.push(BindingReview {
        kind: kind.into(),
        namespace: local.then(|| RUNTIME_NAMESPACE.into()),
        name: "second".into(),
        uid: "second-binding".into(),
        resource_version: "7".into(),
        role_ref: serde_json::from_value(second["roleRef"].clone()).unwrap(),
        subjects: serde_json::from_value(second["subjects"].clone()).unwrap(),
    });
    reg.spec.legacy_consumer = Some(ConsumerReview {
        namespace: RUNTIME_NAMESPACE.into(),
        name: "sre".into(),
        uid: "consumer".into(),
        resource_version: "1".into(),
    });
    locked.objects.insert(path.clone(), second);
    locked.objects.insert(
        CONSUMER.into(),
        json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"sre","namespace":"kars-sre","uid":"consumer","resourceVersion":"1"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"sre"}},
                "template":{"metadata":{},"spec":{"containers":[],"serviceAccountName":"sandbox"}}}}),
    );
    locked
        .objects
        .insert(REG.into(), serde_json::to_value(&reg).unwrap());
    for (name, rule) in [
        (
            "kars-sre-private-diagnostics",
            json!({"apiGroups":[""],"resources":["pods"],"verbs":["get"]}),
        ),
        (
            "kars-sre-action-author",
            json!({"apiGroups":["kars.azure.com"],"resources":["karssreactions"],"verbs":["create"]}),
        ),
        (
            "kars-sre-router-renew",
            json!({"apiGroups":["kars.azure.com"],"resources":["karssreregistrations"],
                "resourceNames":["canonical"],"verbs":["get","renew"]}),
        ),
        (
            "custom-retirement-role",
            json!({"apiGroups":[""],"resources":["limitranges"],"verbs":["get"]}),
        ),
    ] {
        let role_path = if local && name == "custom-retirement-role" {
            format!("/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/roles/{name}")
        } else {
            format!("/apis/rbac.authorization.k8s.io/v1/clusterroles/{name}")
        };
        locked
            .objects
            .insert(role_path, json!({"metadata":{"name":name},"rules":[rule]}));
    }
    (reg, path)
}

fn survivors() -> Value {
    json!([{"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}])
}

#[tokio::test]
async fn retirement_http_fixture_dry_runs_never_mutate_either_binding_store() {
    for kind in ["ClusterRoleBinding", "RoleBinding"] {
        let (_server, client, state) = fixture().await;
        let (_reg, path) = reviews(&state, kind);
        let (before, second) = {
            let locked = state.lock().unwrap();
            (locked.binding.clone(), locked.objects[&path].clone())
        };
        let params = PatchParams {
            dry_run: true,
            ..Default::default()
        };
        let patch = json!({"metadata":{"uid":"binding","resourceVersion":"1",
            "annotations":{RETIRED:"registration"}},"subjects":survivors()});
        let projected = Api::<ClusterRoleBinding>::all(client.clone())
            .patch("legacy", &params, &Patch::Merge(&patch))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(projected).unwrap()["subjects"],
            survivors()
        );
        let patch = json!({"metadata":{"uid":"second-binding","resourceVersion":"7",
            "annotations":{RETIRED:"registration"}},"subjects":survivors()});
        if kind == "RoleBinding" {
            Api::<RoleBinding>::namespaced(client, RUNTIME_NAMESPACE)
                .patch("second", &params, &Patch::Merge(&patch))
                .await
                .unwrap();
        } else {
            Api::<ClusterRoleBinding>::all(client)
                .patch("second", &params, &Patch::Merge(&patch))
                .await
                .unwrap();
        }
        let locked = state.lock().unwrap();
        assert_eq!(locked.binding, before);
        assert_eq!(locked.objects[&path], second);
        assert_eq!(locked.binding_patches.len(), 2);
        assert!(
            locked
                .binding_patches
                .iter()
                .all(|(_, dry_run, _)| *dry_run)
        );
    }
}

#[tokio::test]
async fn retirement_all_preflights_precede_identical_cas_writes_and_preserve_survivors() {
    for kind in ["ClusterRoleBinding", "RoleBinding"] {
        let (_server, client, state) = fixture().await;
        let (reg, path) = reviews(&state, kind);
        let before = state.lock().unwrap().objects.clone();
        let reviewed = bindings::review(&client, &reg).await.unwrap();
        bindings::retire_legacy(&client, &reg, &reviewed)
            .await
            .unwrap();
        {
            let locked = state.lock().unwrap();
            let patches = &locked.binding_patches;
            assert_eq!(patches.len(), 4);
            assert_eq!(
                patches
                    .iter()
                    .map(|(_, dry_run, _)| *dry_run)
                    .collect::<Vec<_>>(),
                [true, true, false, false]
            );
            for (preflight, write) in patches[..2].iter().zip(&patches[2..]) {
                assert_eq!(preflight.0, write.0);
                assert_eq!(preflight.2, write.2);
                assert_eq!(preflight.2["subjects"], survivors());
                assert_eq!(
                    preflight.2["metadata"]["annotations"][RETIRED],
                    "registration"
                );
            }
            assert_eq!(patches[0].0, format!("{CRBS}/legacy"));
            assert_eq!(patches[1].0, path);
            assert_eq!(patches[0].2["metadata"]["uid"], "binding");
            assert_eq!(patches[0].2["metadata"]["resourceVersion"], "1");
            assert_eq!(patches[1].2["metadata"]["uid"], "second-binding");
            assert_eq!(patches[1].2["metadata"]["resourceVersion"], "7");
            for binding in [&locked.binding, &locked.objects[&path]] {
                assert_eq!(binding["subjects"], survivors());
                assert_eq!(binding["metadata"]["annotations"]["e2e-retained"], "yes");
                assert_eq!(binding["metadata"]["annotations"][RETIRED], "registration");
            }
            assert_eq!(locked.binding["metadata"]["resourceVersion"], "2");
            assert_eq!(locked.objects[&path]["metadata"]["resourceVersion"], "8");
            assert_eq!(
                locked.binding["roleRef"],
                serde_json::to_value(&reg.spec.legacy_bindings[0].role_ref).unwrap()
            );
            assert_eq!(locked.objects[&path]["roleRef"], before[&path]["roleRef"]);
            assert_eq!(locked.objects[CONSUMER], before[CONSUMER]);
            assert!(
                locked
                    .calls
                    .iter()
                    .all(|(method, _, _)| method == "GET" || method == "PATCH")
            );
        }
        let reviewed = bindings::review(&client, &reg).await.unwrap();
        bindings::retire_legacy(&client, &reg, &reviewed)
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().binding_patches.len(), 4);
    }
}

#[tokio::test]
async fn retirement_second_binding_forbidden_preserves_all_grants_roles_and_consumer() {
    for kind in ["ClusterRoleBinding", "RoleBinding"] {
        let (_server, client, state) = fixture().await;
        let (reg, path) = reviews(&state, kind);
        let (before, mut before_objects) = {
            let mut locked = state.lock().unwrap();
            locked
                .binding_patch_errors
                .insert((path.clone(), true), 403);
            (locked.binding.clone(), locked.objects.clone())
        };
        let error = reconcile(&client, &reg).await.unwrap_err();
        assert_eq!(
            error,
            format!("Preflight reviewed SRE {kind} retirement: Kubernetes status 403")
        );
        assert!(!error.contains("PRIVATE_SENTINEL"));
        let locked = state.lock().unwrap();
        assert_eq!(locked.binding, before);
        assert_eq!(locked.objects[REG]["status"]["phase"], "Blocked");
        let mut after_objects = locked.objects.clone();
        before_objects.remove(REG);
        after_objects.remove(REG);
        assert_eq!(after_objects, before_objects);
        assert_eq!(locked.binding_patches.len(), 2);
        assert!(
            locked
                .binding_patches
                .iter()
                .all(|(_, dry_run, _)| *dry_run)
        );
        assert_eq!(locked.binding_patches[0].0, format!("{CRBS}/legacy"));
        assert_eq!(locked.binding_patches[1].0, path);
        assert!(
            locked
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET" || method == "PATCH")
        );
        assert!(
            !locked
                .calls
                .iter()
                .any(|(method, path, _)| method == "PATCH" && path == CONSUMER)
        );
    }
}

#[tokio::test]
async fn retirement_preflight_rejects_stale_uid_or_resource_version_before_any_write() {
    for kind in ["ClusterRoleBinding", "RoleBinding"] {
        for (field, changed, code) in [("uid", "replacement", 422), ("resourceVersion", "8", 409)] {
            let (_server, client, state) = fixture().await;
            let (reg, path) = reviews(&state, kind);
            let reviewed = bindings::review(&client, &reg).await.unwrap();
            let before = {
                let mut locked = state.lock().unwrap();
                locked.objects.get_mut(&path).unwrap()["metadata"][field] = changed.into();
                (locked.binding.clone(), locked.objects.clone())
            };
            let error = bindings::retire_legacy(&client, &reg, &reviewed)
                .await
                .unwrap_err();
            assert_eq!(
                error,
                format!("Preflight reviewed SRE {kind} retirement: Kubernetes status {code}")
            );
            let locked = state.lock().unwrap();
            assert_eq!(locked.binding, before.0);
            assert_eq!(locked.objects, before.1);
            assert_eq!(locked.binding_patches.len(), 2);
            assert!(
                locked
                    .binding_patches
                    .iter()
                    .all(|(_, dry_run, _)| *dry_run)
            );
        }
    }
}

#[tokio::test]
async fn retirement_preflight_propagates_other_api_failures_without_writes() {
    for code in [409, 422, 500] {
        let (_server, client, state) = fixture().await;
        let (reg, path) = reviews(&state, "ClusterRoleBinding");
        let reviewed = bindings::review(&client, &reg).await.unwrap();
        let before = {
            let mut locked = state.lock().unwrap();
            locked.binding_patch_errors.insert((path, true), code);
            (locked.binding.clone(), locked.objects.clone())
        };
        let error = bindings::retire_legacy(&client, &reg, &reviewed)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            format!(
                "Preflight reviewed SRE ClusterRoleBinding retirement: Kubernetes status {code}"
            )
        );
        let locked = state.lock().unwrap();
        assert_eq!(locked.binding, before.0);
        assert_eq!(locked.objects, before.1);
        assert!(
            locked
                .binding_patches
                .iter()
                .all(|(_, dry_run, _)| *dry_run)
        );
    }
}

#[tokio::test]
async fn retirement_write_phase_races_fail_closed_without_claiming_atomic_rollback() {
    for code in [403, 409] {
        let (_server, client, state) = fixture().await;
        let (reg, path) = reviews(&state, "RoleBinding");
        let reviewed = bindings::review(&client, &reg).await.unwrap();
        let before = {
            let mut locked = state.lock().unwrap();
            locked
                .binding_patch_errors
                .insert((path.clone(), false), code);
            locked.objects.clone()
        };
        let error = bindings::retire_legacy(&client, &reg, &reviewed)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            format!("Retire reviewed SRE RoleBinding: Kubernetes status {code}")
        );
        let locked = state.lock().unwrap();
        assert_eq!(locked.binding["subjects"], survivors());
        assert_eq!(locked.objects, before);
        assert_eq!(locked.binding_patches.len(), 4);
        assert_eq!(
            locked
                .binding_patches
                .iter()
                .map(|(_, dry_run, _)| *dry_run)
                .collect::<Vec<_>>(),
            [true, true, false, false]
        );
    }
}
