// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{crd::KarsSandbox, mission_delivery::defer_until_ready};
use k8s_openapi::api::apps::v1::Deployment;

fn desired(state: &Arc<Mutex<State>>) -> (KarsSandbox, Deployment, Value) {
    let task = current(state);
    let s = state.lock().unwrap();
    let sandbox = serde_json::from_value(s.objects[SANDBOX].clone()).unwrap();
    let mut deployment: Deployment = serde_json::from_value(s.objects[DEPLOYMENT].clone()).unwrap();
    deployment.metadata.uid = None;
    deployment.metadata.resource_version = None;
    deployment
        .spec
        .as_mut()
        .unwrap()
        .template
        .spec
        .as_mut()
        .unwrap()
        .containers[0]
        .image = Some("changed:latest".into());
    (
        sandbox,
        deployment,
        json!({"task_authorization":task.envelope_digest(),"task_generation":task.metadata.generation}),
    )
}
fn writes(state: &Arc<Mutex<State>>) -> Vec<(String, String, Value)> {
    state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(method, path, _)| {
            method != "GET" && path.starts_with("/apis/apps/v1/namespaces/kars-run/deployments")
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn deferred_runtime_preserves_the_accepted_spec_through_actual_apply() {
    let (_server, ctx, state, _) = fixture().await;
    let (sandbox, mut deployment, identity) = desired(&state);
    let before = state.lock().unwrap().objects[DEPLOYMENT]["spec"].clone();
    defer_until_ready(&ctx.client, &sandbox, &mut deployment)
        .await
        .unwrap();
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().objects[DEPLOYMENT]["spec"], before);
    let calls = writes(&state);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "PATCH");
    assert_eq!(calls[0].2["metadata"]["uid"], "deployment-uid");
    assert_eq!(calls[0].2["metadata"]["resourceVersion"], "1");
}

#[tokio::test]
async fn deferred_runtime_rejects_concurrent_update_replacement_or_disappearance() {
    for race in ["update", "replacement", "disappearance"] {
        let (_server, ctx, state, _) = fixture().await;
        let (sandbox, mut deployment, identity) = desired(&state);
        defer_until_ready(&ctx.client, &sandbox, &mut deployment)
            .await
            .unwrap();
        {
            let mut s = state.lock().unwrap();
            if race == "disappearance" {
                s.objects.remove(DEPLOYMENT);
            } else {
                let live = s.objects.get_mut(DEPLOYMENT).unwrap();
                live["metadata"]["resourceVersion"] = json!("2");
                live["spec"]["replicas"] = json!(0);
                if race == "replacement" {
                    live["metadata"]["uid"] = json!("replacement-uid");
                }
            }
        }
        let before = state.lock().unwrap().objects.get(DEPLOYMENT).cloned();
        assert!(
            apply_deployment(&ctx.client, &sandbox, deployment, &identity)
                .await
                .is_err(),
            "{race}"
        );
        assert!(writes(&state).is_empty(), "{race}");
        assert_eq!(
            state.lock().unwrap().objects.get(DEPLOYMENT).cloned(),
            before
        );
    }
}

#[tokio::test]
async fn deferred_runtime_still_obeys_current_suspension_and_task_authority() {
    for pause in ["hold", "suspended", "unlaunched", "unready", "rebind"] {
        let (_server, ctx, state, _) = fixture().await;
        let (mut sandbox, mut deployment, identity) = desired(&state);
        defer_until_ready(&ctx.client, &sandbox, &mut deployment)
            .await
            .unwrap();
        {
            let mut s = state.lock().unwrap();
            match pause {
                "hold" => {
                    s.objects.get_mut(SANDBOX).unwrap()["metadata"]["annotations"][HOLD] =
                        json!("task-uid");
                }
                "suspended" => {
                    s.objects.get_mut(SANDBOX).unwrap()["spec"]["suspended"] = json!(true);
                }
                "unlaunched" => {
                    s.objects.get_mut(TASK).unwrap()["spec"]["execution"]["launch"] = json!(false);
                }
                "unready" => {
                    s.objects.get_mut(TASK).unwrap()["status"]["phase"] = json!("Pending");
                }
                "rebind" => {
                    s.objects.get_mut(TASK).unwrap()["metadata"]["annotations"][PENDING] =
                        json!("true");
                }
                _ => unreachable!(),
            }
            sandbox = serde_json::from_value(s.objects[SANDBOX].clone()).unwrap();
        }
        apply_deployment(&ctx.client, &sandbox, deployment, &identity)
            .await
            .unwrap();
        assert_eq!(
            state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
            0,
            "{pause}"
        );
    }
}

#[tokio::test]
async fn deferred_runtime_does_not_bypass_authorization_or_private_activation() {
    for private in [false, true] {
        let (_server, ctx, state, _) = fixture().await;
        if private {
            state.lock().unwrap().objects.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"] =
                json!({"annotations":{crate::private_activation::EPOCH:"unreviewed"}});
        }
        let (sandbox, mut deployment, mut identity) = desired(&state);
        defer_until_ready(&ctx.client, &sandbox, &mut deployment)
            .await
            .unwrap();
        if !private {
            identity["task_authorization"] = json!("stale");
        }
        assert!(
            apply_deployment(&ctx.client, &sandbox, deployment, &identity)
                .await
                .is_err()
        );
        assert!(writes(&state).is_empty());
    }
}

fn recreate(state: &Arc<Mutex<State>>) -> (KarsSandbox, Deployment, Value) {
    state.lock().unwrap().objects.get_mut(DEPLOYMENT).unwrap()["spec"]["strategy"] =
        json!({"type":"RollingUpdate","rollingUpdate":{"maxSurge":"25%","maxUnavailable":"25%"}});
    let (sandbox, mut deployment, identity) = desired(state);
    deployment.spec.as_mut().unwrap().strategy =
        Some(serde_json::from_value(json!({"type":"Recreate"})).unwrap());
    (sandbox, deployment, identity)
}

#[tokio::test]
async fn recreate_removes_defaulted_strategy_before_applying_and_is_idempotent() {
    let (_server, ctx, state, _) = fixture().await;
    let (sandbox, deployment, identity) = recreate(&state);
    let before = state.lock().unwrap().objects[DEPLOYMENT].clone();
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    let calls = writes(&state);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].2,
        json!({"metadata":{"uid":"deployment-uid","resourceVersion":"1"},
        "spec":{"strategy":{"type":"Recreate","rollingUpdate":null}}})
    );
    assert_eq!(calls[1].2["metadata"]["resourceVersion"], "2");
    {
        let s = state.lock().unwrap();
        assert_eq!(
            s.objects[DEPLOYMENT]["metadata"]["uid"],
            before["metadata"]["uid"]
        );
        assert_eq!(
            s.objects[DEPLOYMENT]["spec"]["strategy"],
            json!({"type":"Recreate"})
        );
        assert_eq!(s.objects[DEPLOYMENT]["spec"]["replicas"], 1);
    }
    state.lock().unwrap().calls.clear();
    let (sandbox, deployment, identity) = desired(&state);
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    assert_eq!(writes(&state).len(), 1);
}

#[tokio::test]
async fn recreate_strategy_conflict_does_not_apply_or_replace_the_runtime() {
    let (_server, ctx, state, _) = fixture().await;
    let (sandbox, deployment, identity) = recreate(&state);
    state.lock().unwrap().strategy_conflict = true;
    let before = state.lock().unwrap().objects[DEPLOYMENT].clone();
    assert!(
        apply_deployment(&ctx.client, &sandbox, deployment, &identity)
            .await
            .is_err()
    );
    assert_eq!(writes(&state).len(), 1);
    assert_eq!(state.lock().unwrap().objects[DEPLOYMENT], before);
}

#[tokio::test]
async fn recreate_rechecks_task_fence_after_strategy_transition() {
    let (_server, ctx, state, _) = fixture().await;
    let (sandbox, deployment, identity) = recreate(&state);
    state.lock().unwrap().hold_after_strategy = true;
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
        0
    );
    assert_eq!(writes(&state).len(), 2);
}

#[tokio::test]
async fn recreate_does_not_bypass_authorization_ownership_or_private_activation() {
    for failure in ["authority", "owner", "private"] {
        let (_server, ctx, state, _) = fixture().await;
        let (sandbox, mut deployment, mut identity) = recreate(&state);
        match failure {
            "authority" => identity["task_authorization"] = json!("stale"),
            "owner" => {
                state.lock().unwrap().objects.get_mut(DEPLOYMENT).unwrap()["metadata"]["labels"]["kars.azure.com/sandbox"] =
                    json!("foreign")
            }
            "private" => {
                deployment.spec.as_mut().unwrap().template.metadata = Some(
                    serde_json::from_value(
                        json!({"annotations":{crate::private_activation::EPOCH:"unreviewed"}}),
                    )
                    .unwrap(),
                )
            }
            _ => unreachable!(),
        }
        assert!(
            apply_deployment(&ctx.client, &sandbox, deployment, &identity)
                .await
                .is_err()
        );
        assert!(writes(&state).is_empty(), "{failure}");
    }
}

#[tokio::test]
async fn creating_a_runtime_requires_current_namespace_custody() {
    let (_server, ctx, state, _) = fixture().await;
    let (sandbox, deployment, identity) = desired(&state);
    {
        let mut s = state.lock().unwrap();
        s.objects.remove(DEPLOYMENT);
        s.objects.get_mut(RUNTIME).unwrap()["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] =
            json!("foreign-sandbox");
    }
    assert!(
        apply_deployment(&ctx.client, &sandbox, deployment, &identity)
            .await
            .is_err()
    );
    assert!(writes(&state).is_empty());
    assert!(!state.lock().unwrap().objects.contains_key(DEPLOYMENT));
}

#[tokio::test]
async fn first_runtime_uses_create_and_does_not_adopt_a_racing_deployment() {
    for race in [false, true] {
        let (_server, ctx, state, _) = fixture().await;
        let (sandbox, deployment, identity) = desired(&state);
        {
            let mut s = state.lock().unwrap();
            s.objects.remove(DEPLOYMENT);
            s.create_deployment_on_post = race;
        }
        let result = apply_deployment(&ctx.client, &sandbox, deployment, &identity).await;
        assert_eq!(result.is_ok(), !race);
        let calls = writes(&state);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "POST");
        if race {
            assert_eq!(
                state.lock().unwrap().objects[DEPLOYMENT]["metadata"]["uid"],
                "competing-deployment-uid"
            );
            assert_eq!(
                state.lock().unwrap().objects[DEPLOYMENT]["metadata"]["resourceVersion"],
                "50"
            );
        }
    }
}
