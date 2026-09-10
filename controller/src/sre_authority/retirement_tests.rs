// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::privacy_tests::admission_ready;
use super::tests::{State, fixture, registration};
use super::{bindings, migration, reconcile};
use crate::sre_registration::{
    BindingReview, ConsumerReview, EPOCH, KarsSRERegistration, OWNER, RUNTIME_NAMESPACE,
    RegistrationStatus,
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

fn stopped_owned_consumer(reg: &KarsSRERegistration) -> Value {
    json!({"apiVersion":"apps/v1","kind":"Deployment",
        "metadata":{"name":"sre","namespace":RUNTIME_NAMESPACE,"uid":"owned-consumer","resourceVersion":"5"},
        "spec":{"replicas":0,"selector":{"matchLabels":{"app":"sre"}},
            "template":{"metadata":{"annotations":{OWNER:reg.metadata.uid,EPOCH:reg.epoch()}},
                        "spec":{"containers":[],"serviceAccountName":"sandbox"}}}})
}

fn seed_stopped_consumer(state: &Arc<Mutex<State>>, reg: &KarsSRERegistration) {
    let mut locked = state.lock().unwrap();
    locked
        .objects
        .insert(CONSUMER.into(), stopped_owned_consumer(reg));
    locked.objects.insert(
        "/api/v1/namespaces/kars-sre/pods".into(),
        json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[]}),
    );
}

#[tokio::test]
async fn disabled_retirement_removes_only_quiesced_owned_deployment_with_uid_rv_fences() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    reg.spec.enabled = false;
    seed_stopped_consumer(&state, &reg);
    migration::stop_registered_consumer_for_retirement(&client, &reg)
        .await
        .unwrap();
    migration::stop_registered_consumer_for_retirement(&client, &reg)
        .await
        .unwrap();
    let locked = state.lock().unwrap();
    assert!(!locked.objects.contains_key(CONSUMER));
    let deletes: Vec<_> = locked
        .calls
        .iter()
        .filter(|(method, _, _)| method == "DELETE")
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].1, CONSUMER);
    assert_eq!(
        deletes[0].2["preconditions"],
        json!({"uid":"owned-consumer","resourceVersion":"5"})
    );
}

#[tokio::test]
async fn retired_audit_record_repairs_its_leftover_owned_consumer_without_reissuing_authority() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    reg.spec.enabled = false;
    reg.status = Some(RegistrationStatus {
        phase: "Retired".into(),
        observed_generation: reg.metadata.generation.unwrap_or_default(),
        privacy_revision: Some(crate::sre_privacy::REVISION.into()),
        ..Default::default()
    });
    seed_stopped_consumer(&state, &reg);
    reconcile(&client, &reg).await.unwrap();
    let locked = state.lock().unwrap();
    assert!(!locked.objects.contains_key(CONSUMER));
    assert!(locked.calls.iter().all(|(method, path, _)| method == "GET"
        || method == "DELETE"
        || method == "POST" && path.ends_with("/subjectaccessreviews")));
}

#[tokio::test]
async fn retirement_preserves_foreign_namespace_or_unowned_consumer() {
    for foreign_namespace in [true, false] {
        let (_server, client, state) = fixture().await;
        let mut reg = registration();
        reg.spec.enabled = false;
        seed_stopped_consumer(&state, &reg);
        {
            let mut locked = state.lock().unwrap();
            if foreign_namespace {
                locked.namespace["metadata"]["uid"] = "foreign-namespace".into();
            } else {
                locked.objects.get_mut(CONSUMER).unwrap()["spec"]["template"]["metadata"]["annotations"]
                    [OWNER] = "foreign-registration".into();
            }
        }
        let before = state.lock().unwrap().objects[CONSUMER].clone();
        migration::stop_registered_consumer_for_retirement(&client, &reg)
            .await
            .unwrap();
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects[CONSUMER], before);
        assert!(locked.calls.iter().all(|(method, _, _)| method == "GET"));
    }
}

#[tokio::test]
async fn retirement_rechecks_uid_ownership_and_quiescence_before_deleting() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for field in ["uid", "owner", "replicas"] {
        let (server, client, state) = fixture().await;
        let mut reg = registration();
        reg.spec.enabled = false;
        seed_stopped_consumer(&state, &reg);
        let value = stopped_owned_consumer(&reg);
        let reads = AtomicUsize::new(0);
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(CONSUMER))
            .respond_with(move |_: &wiremock::Request| {
                let mut current = value.clone();
                if reads.fetch_add(1, Ordering::SeqCst) >= 3 {
                    match field {
                        "uid" => current["metadata"]["uid"] = "replacement".into(),
                        "owner" => {
                            current["spec"]["template"]["metadata"]["annotations"][OWNER] =
                                "foreign".into()
                        }
                        _ => current["spec"]["replicas"] = 1.into(),
                    }
                }
                wiremock::ResponseTemplate::new(200).set_body_json(current)
            })
            .with_priority(1)
            .mount(&server)
            .await;
        let error = migration::stop_registered_consumer_for_retirement(&client, &reg)
            .await
            .unwrap_err();
        assert!(error.contains("changed after quiescence"));
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method != "DELETE")
        );
    }
}

#[tokio::test]
async fn retirement_delete_conflict_is_not_retried_or_forced() {
    for code in [403, 409] {
        let (server, client, state) = fixture().await;
        let mut reg = registration();
        reg.spec.enabled = false;
        seed_stopped_consumer(&state, &reg);
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path(CONSUMER))
            .respond_with(wiremock::ResponseTemplate::new(code).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure",
                "reason":if code == 409 {"Conflict"} else {"Forbidden"},"code":code,
                "message":"PRIVATE_SENTINEL"})))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let error = migration::stop_registered_consumer_for_retirement(&client, &reg)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            format!("Remove stopped owned SRE consumer: Kubernetes status {code}")
        );
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert!(state.lock().unwrap().objects.contains_key(CONSUMER));
    }
}

#[tokio::test]
async fn retirement_waits_for_deletion_without_removing_foreign_finalizers() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    reg.spec.enabled = false;
    seed_stopped_consumer(&state, &reg);
    {
        let mut locked = state.lock().unwrap();
        let deployment = locked.objects.get_mut(CONSUMER).unwrap();
        deployment["metadata"]["deletionTimestamp"] = "2026-09-09T00:00:00Z".into();
        deployment["metadata"]["finalizers"] = json!(["e2e.example/foreign"]);
    }
    let error = migration::stop_registered_consumer_for_retirement(&client, &reg)
        .await
        .unwrap_err();
    assert!(migration::is_waiting(&error));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    assert_eq!(
        state.lock().unwrap().objects[CONSUMER]["metadata"]["finalizers"],
        json!(["e2e.example/foreign"])
    );
}

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
