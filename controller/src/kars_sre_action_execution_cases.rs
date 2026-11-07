// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[test]
fn strips_all_original_auth_and_keeps_connection_tls_settings() {
    let mut config = kube::Config::new("https://kubernetes.example".parse().unwrap());
    config.root_cert = Some(vec![vec![1, 2, 3]]);
    config.tls_server_name = Some("api.example".into());
    config.proxy_url = Some("http://proxy.example".parse().unwrap());
    config.auth_info = serde_json::from_value(json!({
        "username":"admin","password":"secret","token":"old-token","tokenFile":"must-not-read",
        "client-certificate":"must-not-read", "client-certificate-data":"old-cert", "client-key":"must-not-read",
        "client-key-data":"old-key","as":"admin","as-groups":["system:masters"],
        "auth-provider":{"name":"old-provider","config":{}},"exec":{"command":"must-not-execute"}
    })).unwrap();
    config.headers.push((
        "authorization".parse().unwrap(),
        "Bearer old-header".parse().unwrap(),
    ));
    config.headers.push((
        "impersonate-user".parse().unwrap(),
        "admin".parse().unwrap(),
    ));
    let sanitized = writer_connection(&config);
    assert_eq!(
        serde_json::to_value(&sanitized.auth_info).unwrap(),
        json!({})
    );
    assert!(sanitized.headers.is_empty());
    assert_eq!(sanitized.cluster_url, config.cluster_url);
    assert_eq!(sanitized.root_cert, config.root_cert);
    assert_eq!(sanitized.tls_server_name, config.tls_server_name);
    assert_eq!(sanitized.proxy_url, config.proxy_url);
    assert_eq!(sanitized.connect_timeout, config.connect_timeout);
}

#[tokio::test]
async fn every_action_uses_exact_named_scope_and_real_token_request_flow() {
    for (kind, workload, resource, verb) in [
        ("DeletePod", None, "pods", "delete"),
        ("DeleteResourceQuota", None, "resourcequotas", "delete"),
        ("ScaleDeployment", None, "deployments/scale", "patch"),
        ("PatchDeploymentImage", None, "deployments", "patch"),
        ("RolloutRestart", Some("Deployment"), "deployments", "patch"),
        (
            "RolloutRestart",
            Some("StatefulSet"),
            "statefulsets",
            "patch",
        ),
        ("RolloutRestart", Some("DaemonSet"), "daemonsets", "patch"),
    ] {
        let cr = action(kind, workload);
        let (server, cluster, ctx) = setup(&cr).await;
        drive(&cluster, &ctx).await.unwrap();
        drive(&cluster, &ctx).await.unwrap();
        let result = current(&cluster);
        assert_eq!(
            result.status.as_ref().unwrap().phase.as_deref(),
            Some(PHASE_APPLIED),
            "{kind}"
        );
        assert!(
            !result
                .status
                .as_ref()
                .unwrap()
                .execution
                .as_ref()
                .unwrap()
                .cleanup_pending
        );
        assert!(result.finalizers().is_empty());
        {
            let state = cluster.0.lock().unwrap();
            assert_eq!(state.writes, 1, "{kind}");
            assert_eq!(state.token_requests, 1);
            assert!(!state.objects.keys().any(|p| p.contains("/roles/")
                || p.contains("/rolebindings/")
                || p.contains("/serviceaccounts/")));
        }
        let requests = server.received_requests().await.unwrap();
        let role_request = requests
            .iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/roles"))
            .unwrap();
        let role: Value = serde_json::from_slice(&role_request.body).unwrap();
        assert_eq!(
            role["rules"],
            json!([{"apiGroups":[if resource=="pods" || resource=="resourcequotas" {""} else {"apps"}],
            "resources":[resource],"resourceNames":["workload"],"verbs":["get",verb]}])
        );
        let binding_request = requests
            .iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/rolebindings"))
            .unwrap();
        let binding: Value = serde_json::from_slice(&binding_request.body).unwrap();
        assert_eq!(binding["roleRef"]["kind"], "Role");
        assert_eq!(
            binding["subjects"][0]["name"],
            authority::authority_name(&cr).unwrap()
        );
        assert_eq!(binding["subjects"][0]["namespace"], "team-a");
        assert!(
            !requests
                .iter()
                .any(|r| r.url.path().contains("clusterrolebindings"))
        );
        for request in requests
            .iter()
            .filter(|r| r.url.path() == target_path(&cr) && r.method.as_str() != "GET")
        {
            assert!(
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Bearer writer-test-")
            );
            if kind == "ScaleDeployment" {
                assert_eq!(
                    request.headers["content-type"],
                    "application/merge-patch+json"
                );
            }
        }
        // A fresh controller context with a stale informer object cannot replay.
        assert!(reconcile(Arc::new(cr), ctx.clone()).await.is_err());
        assert_eq!(cluster.0.lock().unwrap().writes, 1);
    }
}

#[tokio::test]
async fn token_or_authorization_failure_never_falls_back_to_controller() {
    for failure in [
        "mint",
        "empty",
        "missing-token",
        "missing-expiry",
        "expired",
        "excessive",
        "forbidden",
    ] {
        let cr = action("ScaleDeployment", None);
        let (server, cluster, ctx) = setup(&cr).await;
        let name = authority::authority_name(&cr).unwrap();
        {
            let mut state = cluster.0.lock().unwrap();
            match failure {
                "mint" => {
                    state.failures.insert(
                        (
                            "POST".into(),
                            format!("/api/v1/namespaces/team-a/serviceaccounts/{name}/token"),
                        ),
                        500,
                    );
                }
                "empty" => state.empty_token = true,
                "missing-token" => state.missing_token = true,
                "missing-expiry" => state.missing_expiry = true,
                "expired" => state.token_ttl = -1,
                "excessive" => state.token_ttl = 3600,
                "forbidden" => {
                    state
                        .failures
                        .insert(("PATCH".into(), target_path(&cr)), 403);
                }
                _ => unreachable!(),
            }
        }
        drive(&cluster, &ctx).await.unwrap();
        drive(&cluster, &ctx).await.unwrap();
        assert_eq!(
            current(&cluster).status.unwrap().phase.as_deref(),
            Some(PHASE_FAILED)
        );
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
        let requests = server.received_requests().await.unwrap();
        assert!(!requests.iter().any(|r| r.url.path() == target_path(&cr)
            && r.method.as_str() == "PATCH"
            && r.headers["authorization"] == "Bearer controller-test"));
    }
}

#[tokio::test]
async fn cleanup_failure_is_visible_and_retried_without_reexecution() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    let binding = format!(
        "/apis/rbac.authorization.k8s.io/v1/namespaces/team-a/rolebindings/{}",
        authority::authority_name(&cr).unwrap()
    );
    cluster
        .0
        .lock()
        .unwrap()
        .failures
        .insert(("DELETE".into(), binding.clone()), 500);
    drive(&cluster, &ctx).await.unwrap();
    assert!(drive(&cluster, &ctx).await.is_err());
    let result = current(&cluster);
    assert!(
        result
            .finalizers()
            .contains(&authority::FINALIZER.to_string())
    );
    let status = result.status.unwrap();
    assert_eq!(status.phase.as_deref(), Some(PHASE_APPLIED));
    assert!(status.execution.unwrap().cleanup_pending);
    assert!(
        status
            .conditions
            .iter()
            .any(|c| c.type_ == "AuthorityCleaned" && c.status == "False")
    );
    assert!(
        !cluster
            .0
            .lock()
            .unwrap()
            .objects
            .keys()
            .any(|p| p.contains("/serviceaccounts/"))
    );
    cluster.0.lock().unwrap().failures.clear();
    drive(&cluster, &ctx).await.unwrap();
    assert!(
        !current(&cluster)
            .status
            .unwrap()
            .execution
            .unwrap()
            .cleanup_pending
    );
    assert_eq!(cluster.0.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn accepted_delete_with_finalizer_does_not_claim_revocation() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    cluster.0.lock().unwrap().held_deletions = true;
    drive(&cluster, &ctx).await.unwrap();
    assert!(drive(&cluster, &ctx).await.is_err());
    assert!(
        current(&cluster)
            .status
            .unwrap()
            .execution
            .unwrap()
            .cleanup_pending
    );
    cluster.0.lock().unwrap().held_deletions = false;
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(cluster.0.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn foreign_collision_is_not_overwritten_or_deleted() {
    for resource in ["serviceaccounts", "roles", "rolebindings"] {
        let cr = action("ScaleDeployment", None);
        let (server, cluster, ctx) = setup(&cr).await;
        let prefix = if resource == "serviceaccounts" {
            "/api/v1"
        } else {
            "/apis/rbac.authorization.k8s.io/v1"
        };
        let key = format!(
            "{prefix}/namespaces/team-a/{resource}/{}",
            authority::authority_name(&cr).unwrap()
        );
        let kind = match resource {
            "serviceaccounts" => "ServiceAccount",
            "roles" => "Role",
            _ => "RoleBinding",
        };
        let mut foreign = json!({"apiVersion":"v1","kind":kind,"metadata":{"name":authority::authority_name(&cr).unwrap(),
            "namespace":"team-a","uid":"foreign","resourceVersion":"1"}});
        if resource == "rolebindings" {
            foreign["roleRef"] =
                json!({"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"foreign"});
        }
        cluster
            .0
            .lock()
            .unwrap()
            .objects
            .insert(key.clone(), foreign.clone());
        drive(&cluster, &ctx).await.unwrap();
        assert!(drive(&cluster, &ctx).await.is_err());
        assert_eq!(cluster.0.lock().unwrap().objects[&key], foreign);
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
        assert!(!server.received_requests().await.unwrap().iter().any(
            |r| r.url.path() == key && ["DELETE", "PATCH", "PUT"].contains(&r.method.as_str())
        ));
    }
}

#[tokio::test]
async fn restart_and_deletion_clean_consumed_attempts_without_replay() {
    for deleting in [false, true] {
        let cr = action("ScaleDeployment", None);
        let (_server, cluster, ctx) = setup(&cr).await;
        let claim = claimed(&cluster, &ctx).await;
        let execution = claim.status.as_ref().unwrap().execution.as_ref().unwrap();
        let _writer = authority::writer(&ctx.client, &ctx.connection, &claim, execution)
            .await
            .unwrap();
        // A second replica must not revoke a still-active attempt.
        drive(&cluster, &ctx).await.unwrap();
        assert_eq!(cluster.0.lock().unwrap().token_requests, 1);
        expire(&cluster);
        if deleting {
            cluster.0.lock().unwrap().objects.get_mut(CR_PATH).unwrap()["metadata"]["deletionTimestamp"] =
                json!(Utc::now().to_rfc3339());
        }
        drive(&cluster, &ctx).await.unwrap();
        let result = current(&cluster);
        assert!(result.finalizers().is_empty());
        assert_eq!(
            result.status.as_ref().unwrap().phase.as_deref(),
            Some(PHASE_FAILED)
        );
        assert!(!result.status.unwrap().execution.unwrap().cleanup_pending);
        drive(&cluster, &ctx).await.unwrap();
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
        assert_eq!(cluster.0.lock().unwrap().token_requests, 1);
    }
}

#[tokio::test]
async fn replacement_target_and_managed_quota_fail_before_mutation() {
    for kind in [
        "ScaleDeployment",
        "DeletePod",
        "DeleteResourceQuota",
        "RolloutRestart",
        "PatchDeploymentImage",
    ] {
        let cr = action(kind, None);
        let (_server, cluster, ctx) = setup(&cr).await;
        let claim = claimed(&cluster, &ctx).await;
        cluster
            .0
            .lock()
            .unwrap()
            .objects
            .get_mut(&target_path(&cr))
            .unwrap()["metadata"]["uid"] = json!("replacement");
        assert!(
            apply_action(&ctx, &claim)
                .await
                .unwrap_err()
                .to_string()
                .contains("UID changed")
        );
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
    }
    let cr = action("DeleteResourceQuota", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    cluster
        .0
        .lock()
        .unwrap()
        .objects
        .get_mut(&target_path(&cr))
        .unwrap()["metadata"]["labels"] = json!({"kars.azure.com/managed-by":"controller"});
    drive(&cluster, &ctx).await.unwrap();
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(
        current(&cluster).status.unwrap().phase.as_deref(),
        Some(PHASE_FAILED)
    );
    assert_eq!(cluster.0.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn replacement_between_dry_run_and_mutation_is_fenced_for_every_action() {
    for kind in [
        "ScaleDeployment",
        "PatchDeploymentImage",
        "RolloutRestart",
        "DeletePod",
        "DeleteResourceQuota",
    ] {
        let cr = action(kind, None);
        let (_server, cluster, ctx) = setup(&cr).await;
        cluster.0.lock().unwrap().replace_target_after_dry_run = true;
        drive(&cluster, &ctx).await.unwrap();
        drive(&cluster, &ctx).await.unwrap();
        assert_eq!(
            current(&cluster).status.unwrap().phase.as_deref(),
            Some(PHASE_FAILED)
        );
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
        assert_eq!(
            cluster.0.lock().unwrap().objects[&target_path(&cr)]["metadata"]["uid"],
            "replacement-after-dry-run"
        );
    }
}

#[tokio::test]
async fn legacy_authority_is_detected_but_never_deleted_without_ownership() {
    for present in [true, false] {
        let cr = action("ScaleDeployment", None);
        let (server, cluster, ctx) = setup(&cr).await;
        let path = "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/legacy-writer";
        {
            let mut state = cluster.0.lock().unwrap();
            state.objects.get_mut(CR_PATH).unwrap()["status"] =
                json!({"writerCrbName":"legacy-writer"});
            if present {
                state.objects.insert(path.into(),json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
                    "metadata":{"name":"legacy-writer","uid":"legacy-uid"},
                    "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"legacy-role"}}));
            }
        }
        assert_eq!(drive(&cluster, &ctx).await.is_err(), present);
        assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path() == path && r.method.as_str() != "GET")
        );
        assert_eq!(
            current(&cluster).status.unwrap().writer_crb_name.is_some(),
            present
        );
    }
}

#[tokio::test]
async fn lost_claim_response_consumes_attempt_without_issuing_authority() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    cluster.0.lock().unwrap().fail_after_claim_commit = true;
    drive(&cluster, &ctx).await.unwrap();
    assert!(drive(&cluster, &ctx).await.is_err());
    expire(&cluster);
    drive(&cluster, &ctx).await.unwrap();
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(
        current(&cluster).status.unwrap().phase.as_deref(),
        Some(PHASE_FAILED)
    );
    assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
    assert_eq!(cluster.0.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn lost_completion_response_leaves_durable_cleanup_and_no_replay() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    cluster.0.lock().unwrap().fail_after_status_commit = true;
    drive(&cluster, &ctx).await.unwrap();
    assert!(drive(&cluster, &ctx).await.is_err());
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(cluster.0.lock().unwrap().writes, 1);
    assert_eq!(cluster.0.lock().unwrap().token_requests, 1);
    assert!(
        !current(&cluster)
            .status
            .unwrap()
            .execution
            .unwrap()
            .cleanup_pending
    );
}

#[tokio::test]
async fn concurrent_action_identities_do_not_union_authority() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    let first = claimed(&cluster, &ctx).await;
    let mut second = first.clone();
    second.metadata.uid = Some("11111111-aaaa-bbbb-cccc-dddddddddddd".into());
    second.spec.action = action("DeletePod", None).spec.action;
    let execution = second.status.as_mut().unwrap().execution.as_mut().unwrap();
    execution.authority_name = format!("sre-action-{}", second.metadata.uid.as_deref().unwrap());
    let (a, b) = tokio::join!(
        authority::writer(
            &ctx.client,
            &ctx.connection,
            &first,
            first.status.as_ref().unwrap().execution.as_ref().unwrap()
        ),
        authority::writer(
            &ctx.client,
            &ctx.connection,
            &second,
            second.status.as_ref().unwrap().execution.as_ref().unwrap()
        )
    );
    let a = a.unwrap();
    let b = b.unwrap();
    // The mock server enforces each minted identity's actual Role resource/name.
    assert!(
        authority::target_metadata(&a, &second.spec.action, "team-a", "workload")
            .await
            .is_err()
    );
    assert!(
        authority::target_metadata(&b, &first.spec.action, "team-a", "workload")
            .await
            .is_err()
    );
    assert_eq!(cluster.0.lock().unwrap().tokens.len(), 2);
}

#[tokio::test]
async fn actual_writer_requests_ignore_original_auth_files_exec_and_headers() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, mut ctx) = setup(&cr).await;
    let connection = &mut Arc::get_mut(&mut ctx).unwrap().connection;
    connection.auth_info = serde_json::from_value(json!({
        "token":"controller-test","tokenFile":"missing-original-token-file",
        "username":"admin","password":"original-password",
        "client-certificate":"missing-original-certificate","client-key":"missing-original-key",
        "as":"admin","as-groups":["system:masters"],
        "auth-provider":{"name":"unsupported-provider","config":{}},
        "exec":{"command":"must-not-execute"}
    }))
    .unwrap();
    connection.headers.push((
        "authorization".parse().unwrap(),
        "Bearer controller-test".parse().unwrap(),
    ));
    connection.headers.push((
        "impersonate-user".parse().unwrap(),
        "admin".parse().unwrap(),
    ));
    drive(&cluster, &ctx).await.unwrap();
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(cluster.0.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn namespace_replacement_is_not_touched_by_execution_or_cleanup() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    let claim = claimed(&cluster, &ctx).await;
    cluster.0.lock().unwrap().objects.get_mut(NS_PATH).unwrap()["metadata"]["uid"] =
        json!("new-namespace");
    assert!(apply_action(&ctx, &claim).await.is_err());
    expire(&cluster);
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
    assert!(
        !current(&cluster)
            .status
            .unwrap()
            .execution
            .unwrap()
            .cleanup_pending
    );
}

#[tokio::test]
async fn authority_creation_failure_cleans_partial_objects() {
    for resource in ["serviceaccounts", "roles", "rolebindings"] {
        let cr = action("ScaleDeployment", None);
        let (_server, cluster, ctx) = setup(&cr).await;
        let prefix = if resource == "serviceaccounts" {
            "/api/v1"
        } else {
            "/apis/rbac.authorization.k8s.io/v1"
        };
        cluster.0.lock().unwrap().failures.insert(
            (
                "POST".into(),
                format!("{prefix}/namespaces/team-a/{resource}"),
            ),
            500,
        );
        drive(&cluster, &ctx).await.unwrap();
        drive(&cluster, &ctx).await.unwrap();
        let status = current(&cluster).status.unwrap();
        assert_eq!(status.phase.as_deref(), Some(PHASE_FAILED));
        assert!(!status.execution.unwrap().cleanup_pending);
        assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
        assert!(
            !cluster
                .0
                .lock()
                .unwrap()
                .objects
                .keys()
                .any(|p| p.contains("/roles/")
                    || p.contains("/rolebindings/")
                    || p.contains("/serviceaccounts/"))
        );
    }
}

#[tokio::test]
async fn concurrent_claims_only_consume_one_attempt() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    drive(&cluster, &ctx).await.unwrap();
    let cr = current(&cluster);
    let api = Api::<KarsSREAction>::namespaced(ctx.client.clone(), "kars-sre");
    let (a, b) = tokio::join!(
        claim_execution(&api, &ctx, &cr),
        claim_execution(&api, &ctx, &cr)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
}

#[tokio::test]
async fn approval_change_after_claim_refuses_mutation_and_retains_cleanup() {
    let cr = action("ScaleDeployment", None);
    let (_server, cluster, ctx) = setup(&cr).await;
    let claim = claimed(&cluster, &ctx).await;
    cluster.0.lock().unwrap().objects.get_mut(CR_PATH).unwrap()["spec"]["approval"]["state"] =
        json!(APPROVAL_REJECTED);
    assert!(apply_action(&ctx, &claim).await.is_err());
    expire(&cluster);
    drive(&cluster, &ctx).await.unwrap();
    assert_eq!(cluster.0.lock().unwrap().writes, 0);
    assert!(
        !current(&cluster)
            .status
            .unwrap()
            .execution
            .unwrap()
            .cleanup_pending
    );
}

#[tokio::test]
async fn pending_rejected_and_protected_actions_never_issue_authority() {
    for variant in ["pending", "rejected", "namespace", "kind", "replicas"] {
        let mut cr = action("ScaleDeployment", None);
        match variant {
            "pending" => cr.spec.approval.state = APPROVAL_PENDING.into(),
            "rejected" => cr.spec.approval.state = APPROVAL_REJECTED.into(),
            "namespace" => {
                cr.spec
                    .action
                    .params
                    .insert("namespace".into(), json!("kars-sre"));
            }
            "kind" => {
                cr.spec.action.kind = "RolloutRestart".into();
                cr.spec.action.params.insert("kind".into(), json!("Secret"));
            }
            "replicas" => {
                cr.spec.action.params.insert("replicas".into(), json!(51));
            }
            _ => unreachable!(),
        }
        // Setup's target discovery intentionally uses a valid supported action.
        let (_server, cluster, ctx) = setup(&action("ScaleDeployment", None)).await;
        cluster
            .0
            .lock()
            .unwrap()
            .objects
            .insert(CR_PATH.into(), serde_json::to_value(cr).unwrap());
        drive(&cluster, &ctx).await.unwrap();
        assert_eq!(cluster.0.lock().unwrap().token_requests, 0);
        assert_eq!(cluster.0.lock().unwrap().writes, 0);
    }
}
