// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::Body,
    http::{Request, Response},
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::service_fn;

fn principal() -> Principal {
    Principal {
        sub: "reader-sub".into(),
        name: "Reader".into(),
        roles: vec!["user".into()],
    }
}

fn cluster(enrolled: bool, write: bool) -> (Cluster, Arc<Mutex<Vec<String>>>) {
    cluster_with_scopes(vec![(
        if enrolled {
            vec!["owner/repo"]
        } else {
            vec!["owner/other"]
        },
        write,
    )])
}

fn cluster_with_scopes(
    scopes: Vec<(Vec<&'static str>, bool)>,
) -> (Cluster, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let connection_name = crate::kars::github_connection_name(&principal().sub);
    let service = service_fn(move |request: Request<kube::client::Body>| {
        assert_eq!(
            request.method(),
            "GET",
            "preflight must not mutate authority"
        );
        let path = request.uri().path().to_string();
        recorded.lock().unwrap().push(path.clone());
        let body = if path == format!("/api/v1/namespaces/work/configmaps/{connection_name}") {
            json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":connection_name,"uid":"connection-uid"},
                "data":{"installation_id":"123", "account":"owner", "repos":"[\"owner/repo\",\"owner/other\"]"}})
        } else if path
            == "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace"
        {
            json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace", "uid":"grant-uid", "generation":1},
                "spec":{"enabled":true, "workspaceUid":"workspace-uid", "integrationStores":[], "githubConnections":scopes.iter().map(|(repositories, write)|json!({
                    "ownerSubject":"reader-sub", "connection":{"name":connection_name,"uid":"connection-uid"},
                    "installationId":123, "repositories":repositories, "write":write})).collect::<Vec<_>>()},
                "status":{"phase":"Ready", "observedGeneration":1, "sources":[]}})
        } else if path == "/api/v1/namespaces/work" {
            json!({"apiVersion":"v1", "kind":"Namespace", "metadata":{"name":"work","uid":"workspace-uid"}})
        } else {
            panic!("unexpected authority read: {path}")
        };
        async move {
            Ok::<_, std::io::Error>(
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        Cluster::for_test_client(kube::Client::new(service, "work")),
        calls,
    )
}

#[tokio::test]
async fn selected_repositories_reach_live_connection_and_workspace_authorization() {
    for (enrolled, grant_write, requested_write, expected) in [
        (true, false, false, CheckStatus::Pass),
        (false, false, false, CheckStatus::Fail),
        (true, false, true, CheckStatus::Fail),
        (true, true, true, CheckStatus::Pass),
    ] {
        let (cluster, calls) = cluster(enrolled, grant_write);
        let req: crate::routes::validate::ValidateRequest = serde_json::from_value(json!({
            "git_write_repos":["Owner/Repo"], "blueprint":{}
        }))
        .unwrap();
        let checks = check(
            &cluster,
            "work",
            &principal(),
            req.git_write_repos.as_deref(),
            requested_write,
            std::iter::empty(),
            None,
        )
        .await;
        assert_eq!(checks[0].status, expected);
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.ends_with("/karscredentialgrants/workspace"))
        );
        if expected == CheckStatus::Fail {
            assert!(checks[0].detail.contains("workspace administrator"));
            assert!(checks[0].detail.contains("Configuration → GitHub"));
        }
    }
}

#[tokio::test]
async fn separate_repository_scopes_do_not_combine_write_authority() {
    let mut scopes = vec![(vec!["owner/repo"], true), (vec!["owner/other"], false)];
    for _ in 0..2 {
        let (cluster, _) = cluster_with_scopes(scopes.clone());
        for (repos, write, allowed) in [
            (vec!["Owner/Repo"], true, true),
            (vec!["owner/repo"], false, true),
            (vec!["owner/other"], false, true),
            (vec!["owner/other"], true, false),
            (vec!["owner/repo", "owner/other"], false, false),
            (vec!["owner/repo", "owner/other"], true, false),
            (vec!["owner/foreign"], false, false),
        ] {
            let result = cluster
                .github_connection_grant(
                    "work",
                    &principal().sub,
                    repos.iter().map(|repo| (*repo).into()).collect(),
                    write,
                )
                .await;
            assert_eq!(result.is_ok(), allowed, "{repos:?}, write={write}");
            if let Ok(binding) = result {
                assert_eq!(
                    binding.repositories,
                    repos
                        .iter()
                        .map(|repo| repo.to_ascii_lowercase())
                        .collect::<Vec<_>>()
                );
                assert_eq!(binding.write, write);
            }
        }
        scopes.reverse();
    }
}

#[tokio::test]
async fn unselected_repositories_do_not_request_authority_or_forbid_unbound_egress() {
    let (cluster, calls) = cluster(false, false);
    for selection in [None, Some(vec![]), Some(vec!["  ".into()])] {
        assert!(
            check(
                &cluster,
                "work",
                &principal(),
                selection.as_deref(),
                true,
                ["api.github.com"].into_iter(),
                None
            )
            .await
            .is_empty()
        );
    }
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn connected_but_ungranted_repositories_and_opaque_egress_both_fail_before_creation() {
    let (cluster, _) = cluster(false, false);
    let repos = vec!["owner/repo".into()];
    for host in [
        "github.com",
        "api.github.com",
        "API.GITHUB.COM..",
        "*.github.com",
        "*.api.github.com",
        "*.com",
        "*",
        "*.COM.",
    ] {
        let checks = check(
            &cluster,
            "work",
            &principal(),
            Some(&repos),
            false,
            [host].into_iter(),
            None,
        )
        .await;
        assert_eq!(checks.len(), 2, "{host}");
        assert!(checks.iter().all(|check| check.status == CheckStatus::Fail));
        assert_eq!(checks[1].id, "github_egress");
        assert!(checks[1].detail.contains("Remove those entries"));
        assert!(checks[1].detail.contains("keep the repositories selected"));
    }
    for host in [
        "docs.example.com",
        "github.com.example.com",
        "raw.githubusercontent.com",
        "notgithub.com",
    ] {
        assert!(crate::routes::github::validate_bound_github_egress([host].into_iter()).is_ok());
    }
}

#[test]
fn team_and_mission_authority_derive_reviewed_capabilities() {
    let mut bp: BlueprintDto = serde_json::from_value(json!({"execution_plan":{
        "schema":"kars.execution-plan/v1", "roles":[], "max_parallel":1,
        "synthesis":{"objective":"Read evidence", "execution_mode":"native", "capabilities":["network"], "max_tool_calls":10}
    }})).unwrap();
    assert!(!write_enabled(None, &bp));
    assert!(!write_enabled(Some("mission"), &bp));
    assert!(!write_enabled(Some("team"), &bp));
    bp.execution_plan
        .as_mut()
        .unwrap()
        .synthesis
        .capabilities
        .push("github-pr".into());
    assert!(write_enabled(Some("mission"), &bp));
    assert!(write_enabled(Some("team"), &bp));
    assert!(write_enabled(None, &BlueprintDto::default()));
    assert!(write_enabled(Some("team"), &BlueprintDto::default()));
}

#[tokio::test]
async fn saved_task_validation_uses_stored_binding_egress_and_current_grant() {
    let (cluster, _) = cluster(true, true);
    let repositories = vec!["owner/repo".into()];
    let (_, binding) = crate::routes::github::authorize_github_access(
        &cluster,
        "work",
        &principal(),
        Some(&repositories),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    let mut task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"saved", "namespace":"work"},
        "spec":{"objective":"Read the repository", "envelope":{"tier":1,"authorityCeiling":1},
            "blueprint":{"githubBinding":binding, "egress":[{"host":"API.GITHUB.COM."}]}}
    }))
    .unwrap();
    let checks = check_stored(&cluster, "work", &principal(), &task).await;
    assert_eq!(checks[0].status, CheckStatus::Pass);
    assert_eq!(checks[1].id, "github_egress");
    assert_eq!(checks[1].status, CheckStatus::Fail);
    task.spec.blueprint.as_mut().unwrap().egress.clear();
    assert_eq!(
        check_stored(&cluster, "work", &principal(), &task)
            .await
            .len(),
        1
    );
    let (revoked, _) = self::cluster(false, true);
    assert_eq!(
        check_stored(&revoked, "work", &principal(), &task).await[0].status,
        CheckStatus::Fail
    );
    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .github_binding
        .as_mut()
        .unwrap()
        .repositories
        .push("owner/repo".into());
    assert_eq!(
        check_stored(&cluster, "work", &principal(), &task).await[0].status,
        CheckStatus::Fail
    );
    task.spec.blueprint.as_mut().unwrap().github_binding = None;
    assert!(
        check_stored(&cluster, "work", &principal(), &task)
            .await
            .is_empty()
    );
    task.spec.blueprint.as_mut().unwrap().git_write = Some(
        serde_json::from_value(json!({
            "connectionConfigMapRef":{"name":"legacy"}, "repos":["owner/repo"]
        }))
        .unwrap(),
    );
    assert_eq!(
        check_stored(&cluster, "work", &principal(), &task).await[0].status,
        CheckStatus::Fail
    );
}

#[tokio::test]
async fn stored_bindings_require_the_same_live_identities_and_current_permissions() {
    let (cluster, _) = cluster(true, true);
    let repos = vec!["owner/repo".into()];
    let (_, binding) = crate::routes::github::authorize_github_access(
        &cluster,
        "work",
        &principal(),
        Some(&repos),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    for changed in ["none", "grant", "connection", "repos", "write"] {
        let mut stored = binding.clone();
        match changed {
            "grant" => stored.grant.uid = "old-grant".into(),
            "connection" => stored.connection.uid = "old-connection".into(),
            "repos" => stored.repositories = vec!["owner/other".into()],
            "write" => stored.write = true,
            _ => {}
        }
        let checks = check(
            &cluster,
            "work",
            &principal(),
            Some(&repos),
            false,
            std::iter::empty(),
            Some(&stored),
        )
        .await;
        assert_eq!(
            checks[0].status,
            if changed == "none" {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            }
        );
    }
    let (revoked, _) = self::cluster(false, false);
    let checks = check(
        &revoked,
        "work",
        &principal(),
        Some(&repos),
        false,
        std::iter::empty(),
        Some(&binding),
    )
    .await;
    assert_eq!(checks[0].status, CheckStatus::Fail);
}
