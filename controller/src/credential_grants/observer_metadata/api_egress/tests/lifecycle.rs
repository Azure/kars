// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::credential_grants::observer_metadata as metadata;

#[tokio::test]
async fn observer_api_retirement_finds_orphans_without_other_metadata_and_keeps_only_approved_generation()
 {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    retire(&client, &grant, true).await.unwrap();
    assert!(state.lock().unwrap().objects.contains_key(POLICY));
    let mut next = grant.clone();
    next.metadata.generation = Some(2);
    retire(&client, &next, true).await.unwrap();
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert_eq!(
        state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
        "v1"
    );
    retire(&client, &next, true).await.unwrap();
    let deletes: Vec<_> = mutations(&state)
        .into_iter()
        .filter(|(method, _, _)| method == "DELETE")
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(
        deletes[0].2["preconditions"],
        json!({"uid":"policy-uid","resourceVersion":"10"})
    );
}

#[tokio::test]
async fn observer_api_removed_disabled_and_full_revoke_remove_even_same_generation_policies() {
    for mode in ["removed", "disabled", "revoke"] {
        let (_server, client, state, mut grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        if mode == "removed" {
            grant.spec.observation_targets.clear();
        }
        if mode == "disabled" {
            grant.spec.enabled = false;
        }
        retire(&client, &grant, mode != "revoke").await.unwrap();
        assert!(
            !state.lock().unwrap().objects.contains_key(POLICY),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn observer_api_cleanup_conflicts_pending_deletes_and_api_errors_remain_retryable() {
    for mode in ["conflict", "pending", "forbidden"] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.delete_conflict = mode == "conflict";
            state.retain_deleted = mode == "pending";
            if mode == "forbidden" {
                state.errors.insert(POLICIES.into(), 403);
            }
        }
        assert!(retire(&client, &grant, false).await.is_err());
        assert!(state.lock().unwrap().objects.contains_key(POLICY));
        assert_eq!(
            state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
            "v1"
        );
        {
            let mut state = state.lock().unwrap();
            state.delete_conflict = false;
            state.retain_deleted = false;
            state.errors.clear();
        }
        retire(&client, &grant, false).await.unwrap();
        assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    }
}

#[tokio::test]
async fn observer_api_partial_other_metadata_failure_cannot_skip_cnp_revocation() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    state.lock().unwrap().errors.insert(
        "/apis/rbac.authorization.k8s.io/v1/rolebindings".into(),
        403,
    );
    assert!(metadata::revoke(&client, &grant).await.is_err());
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert_eq!(
        state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
        "v1"
    );
    state.lock().unwrap().errors.clear();
    metadata::revoke(&client, &grant).await.unwrap();
}

#[tokio::test]
async fn observer_api_namespace_race_before_create_never_writes_into_the_replacement() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    state.lock().unwrap().namespace_replacement_on_policy_read = Some(POLICY.into());
    assert!(
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .is_err()
    );
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert!(
        !mutations(&state)
            .iter()
            .any(|(_, path, _)| path.contains("ciliumnetworkpolicies"))
    );
}

#[tokio::test]
async fn observer_api_retirement_preserves_foreign_owners_and_namespace_replacements() {
    for mode in ["grant", "owner", "target", "namespace", "workspace"] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            match mode {
                "grant" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["annotations"][GRANT_OWNER] =
                        "foreign".into()
                }
                "owner" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
                        "foreign".into()
                }
                "target" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["annotations"]
                        [claim::SOURCE_UID] = "foreign".into()
                }
                "namespace" => {
                    state.objects.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                _ => {
                    state.objects.get_mut(NS).unwrap()["metadata"]["annotations"]
                        [claim::SOURCE_NAMESPACE] = "foreign".into()
                }
            }
            state.calls.clear();
        }
        let result = retire(&client, &grant, false).await;
        if mode != "workspace" {
            assert!(result.is_err(), "{mode}");
        }
        assert!(state.lock().unwrap().objects.contains_key(POLICY));
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn observer_api_ordinary_namespaces_do_not_probe_cilium_or_gain_an_index() {
    let (_server, client, state, mut grant, _, _) = fixture().await;
    grant.spec.observation_targets.clear();
    state.lock().unwrap().errors.insert(API.into(), 403);
    retire(&client, &grant, true).await.unwrap();
    assert!(mutations(&state).is_empty());
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, path, _)| path == API)
    );
}

#[tokio::test]
async fn observer_api_portable_policy_is_revoked_when_last_target_is_removed() {
    let (_server, client, state, mut grant, sandbox, namespace) = fixture().await;
    let name = format!(
        "{}-rpc",
        policy_prefix(&grant, &sandbox.uid().unwrap()).unwrap()
    );
    let path = format!("/apis/networking.k8s.io/v1/namespaces/kars-agent/networkpolicies/{name}");
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    apply_runtime(
        &client,
        &grant,
        &namespace,
        "NetworkPolicy",
        &name,
        json!({
            "spec":{"podSelector":{"matchLabels":{"kars.azure.com/sandbox":"agent"}},
                "policyTypes":["Egress"],"egress":plan.rules}
        }),
    )
    .await
    .unwrap();
    state.lock().unwrap().calls.clear();
    state.lock().unwrap().errors.insert(API.into(), 403);
    grant.spec.observation_targets.clear();
    metadata::revoke_stale(&client, &grant).await.unwrap();
    assert!(!state.lock().unwrap().objects.contains_key(&path));
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, path, _)| path == API)
    );
    assert_eq!(
        mutations(&state)[0].2["preconditions"],
        json!({"uid":"policy-uid","resourceVersion":"10"})
    );
}

fn runtime_policy(
    kind: &str,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    plan: &Plan,
) -> (String, Value) {
    if kind == KIND {
        (
            format!("{POLICIES}/fenced"),
            json!({"spec":spec(sandbox, namespace, plan).unwrap()}),
        )
    } else {
        (
            "/apis/networking.k8s.io/v1/namespaces/kars-agent/networkpolicies/fenced".into(),
            json!({"spec":{"podSelector":{"matchLabels":{"kars.azure.com/sandbox":"agent"}},
                "policyTypes":["Egress"],"egress":plan.rules}}),
        )
    }
}

#[tokio::test]
async fn observer_api_both_policy_kinds_keep_original_namespace_uid_across_create_update_and_noop()
{
    for kind in ["NetworkPolicy", KIND] {
        for operation in ["create", "update", "noop"] {
            for replacement in ["before-namespace-read", "after-policy-read"] {
                let (_server, client, state, grant, sandbox, namespace) = fixture().await;
                let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
                let (path, desired) = runtime_policy(kind, &sandbox, &namespace, &plan);
                if operation != "create" {
                    let mut initial = desired.clone();
                    if operation == "update" {
                        initial["spec"]["egress"] = json!([]);
                    }
                    apply_runtime(&client, &grant, &namespace, kind, "fenced", initial)
                        .await
                        .unwrap();
                }
                let original = {
                    let mut state = state.lock().unwrap();
                    let original = state.objects.get(&path).cloned();
                    state.calls.clear();
                    if replacement == "before-namespace-read" {
                        state.objects.get_mut(NS).unwrap()["metadata"]["uid"] =
                            "replacement-uid".into();
                    } else {
                        state.namespace_replacement_on_policy_read = Some(path.clone());
                    }
                    original
                };
                let error = apply_runtime(&client, &grant, &namespace, kind, "fenced", desired)
                    .await
                    .unwrap_err();
                assert!(
                    error.contains("namespace"),
                    "{kind}/{operation}/{replacement}"
                );
                assert!(
                    mutations(&state).is_empty(),
                    "{kind}/{operation}/{replacement}"
                );
                assert_eq!(state.lock().unwrap().objects.get(&path), original.as_ref());
                assert_eq!(namespace.uid().as_deref(), Some("runtime-uid"));
            }
        }
    }
}

#[tokio::test]
async fn observer_api_both_policy_kinds_require_existing_uid_rv_and_preserve_conflicted_objects() {
    for kind in ["NetworkPolicy", KIND] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        let (path, desired) = runtime_policy(kind, &sandbox, &namespace, &plan);
        let mut initial = desired.clone();
        initial["spec"]["egress"] = json!([]);
        apply_runtime(&client, &grant, &namespace, kind, "fenced", initial)
            .await
            .unwrap();
        let original = state.lock().unwrap().objects[&path].clone();
        for missing in ["uid", "resourceVersion"] {
            {
                let mut state = state.lock().unwrap();
                state.objects.insert(path.clone(), original.clone());
                state.objects.get_mut(&path).unwrap()["metadata"][missing] = Value::Null;
                state.calls.clear();
            }
            assert!(
                apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
                    .await
                    .is_err(),
                "{kind}/{missing}"
            );
            assert!(mutations(&state).is_empty());
        }
        {
            let mut state = state.lock().unwrap();
            state.objects.insert(path.clone(), original.clone());
            state.replace_conflict = true;
            state.calls.clear();
        }
        let error = apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
            .await
            .unwrap_err();
        assert!(error.contains("409"));
        assert_eq!(state.lock().unwrap().objects[&path], original);
        let requests = mutations(&state);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "PUT");
        assert_eq!(requests[0].2["metadata"]["uid"], "policy-uid");
        assert_eq!(requests[0].2["metadata"]["resourceVersion"], "10");
        assert_eq!(
            requests[0].2["metadata"]["annotations"][NAMESPACE_UID],
            "runtime-uid"
        );
        {
            let mut state = state.lock().unwrap();
            state.replace_conflict = false;
            state.calls.clear();
        }
        apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
            .await
            .unwrap();
        assert_eq!(
            state.lock().unwrap().objects[&path]["spec"],
            desired["spec"]
        );
        assert_eq!(
            state.lock().unwrap().objects[&path]["metadata"]["uid"],
            "policy-uid"
        );
    }
}
