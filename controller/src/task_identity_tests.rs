// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

fn task(name: &str, parent: Option<&str>, ready: bool) -> KarsTask {
    let mut task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":name,"namespace":"workspace","uid":format!("{name}-uid"),"resourceVersion":"1","generation":1},
        "spec":{
            "objective":"Fixture", "envelope":{"tier":if parent.is_some(){2}else{3},
                "authorityCeiling":if parent.is_some(){2}else{3},"delegationDepth":if parent.is_some(){1}else{2}},
            "blueprint":{"model":{"provider":"azure-openai","deployment":"fixture"}},
            "parentRef":parent.map(|name| json!({"name":name}))
        }
    })).unwrap();
    task.status = Some(
        serde_json::from_value(json!({
            "phase":if ready{"Ready"}else{"Pending"}, "observedGeneration":1,
            "envelopeDigest":task.envelope_digest(),
            "conditions":[{"type":"Ready","status":if ready{"True"}else{"False"},
                "reason":"Fixture","message":"Fixture","lastTransitionTime":"2026-09-08T00:00:00Z"}]
        }))
        .unwrap(),
    );
    task
}

async fn setup() -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/namespaces/workspace"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":"workspace","uid":"workspace-uid","resourceVersion":"1"}
        })))
        .mount(&server)
        .await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client)
}

async fn serve_task(server: &MockServer, task: &KarsTask) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/{}",
            task.name_any()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(task))
        .mount(server)
        .await;
}

#[tokio::test]
async fn returns_uid_order_and_exact_canonical_authorization_without_budget_coupling() {
    let (server, client) = setup().await;
    let root = task("root", None, true);
    let leaf = task("leaf", Some("root"), true);
    serve_task(&server, &root).await;
    serve_task(&server, &leaf).await;
    let lineage = resolve(&client, &leaf, LeafReadiness::RequireReady)
        .await
        .unwrap();
    assert_eq!(lineage.workspace_uid, "workspace-uid");
    assert!(lineage.team.is_none());
    assert_eq!(
        lineage
            .nodes
            .iter()
            .map(|node| node.pin.task.uid.as_str())
            .collect::<Vec<_>>(),
        ["root-uid", "leaf-uid"]
    );
    assert_eq!(
        lineage.nodes[1].pin.parent_task_uid.as_deref(),
        Some("root-uid")
    );
    assert_eq!(lineage.nodes[1].pin.root_task_uid, "root-uid");
    assert_eq!(
        lineage.nodes[1].authorization_digest,
        leaf.envelope_digest()
    );
    assert_eq!(lineage.nodes[1].generation, 1);
    lineage
        .verify_pins(&[lineage.nodes[1].pin.clone()])
        .unwrap();
    let mut stale = lineage.nodes[1].pin.clone();
    stale.parent_task_uid = Some("recreated-root".into());
    assert!(lineage.verify_pins(&[stale]).is_err());
    let pin = lineage.nodes[1].pin.clone();
    assert!(lineage.verify_pins(&[pin.clone(), pin]).is_err());
}

#[tokio::test]
async fn pending_leaf_is_explicit_but_pending_ancestors_never_grant_authority() {
    let (server, client) = setup().await;
    let root = task("root", None, true);
    let leaf = task("leaf", Some("root"), false);
    serve_task(&server, &root).await;
    serve_task(&server, &leaf).await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::RequireReady).await,
        Err(Error::NotReady)
    ));
    assert!(
        resolve(&client, &leaf, LeafReadiness::AllowPending)
            .await
            .is_ok()
    );

    let (server, client) = setup().await;
    serve_task(&server, &task("root", None, false)).await;
    serve_task(&server, &leaf).await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::AllowPending).await,
        Err(Error::NotReady)
    ));
}

#[derive(Clone)]
struct ReplacedOnRecheck {
    calls: Arc<AtomicUsize>,
    task: KarsTask,
}

impl Respond for ReplacedOnRecheck {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut task = self.task.clone();
        if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            task.metadata.uid = Some("recreated-root-uid".into());
        }
        ResponseTemplate::new(200).set_body_json(task)
    }
}

#[tokio::test]
async fn double_inventory_rejects_recreated_uid_instead_of_mixing_ancestry() {
    let (server, client) = setup().await;
    let leaf = task("leaf", Some("root"), true);
    serve_task(&server, &leaf).await;
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/root",
        ))
        .respond_with(ReplacedOnRecheck {
            calls: Arc::new(AtomicUsize::new(0)),
            task: task("root", None, true),
        })
        .mount(&server)
        .await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::RequireReady).await,
        Err(Error::Changed)
    ));
}

#[tokio::test]
async fn stale_leaf_generation_and_foreign_namespace_are_not_adopted() {
    let (server, client) = setup().await;
    let leaf = task("leaf", None, true);
    let mut replaced = leaf.clone();
    replaced.metadata.generation = Some(2);
    serve_task(&server, &replaced).await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::RequireReady).await,
        Err(Error::Changed)
    ));
    let mut invalid = leaf;
    invalid.metadata.namespace = Some("../foreign".into());
    assert!(matches!(
        resolve(&client, &invalid, LeafReadiness::RequireReady).await,
        Err(Error::Identity)
    ));
}

#[tokio::test]
async fn non404_parent_failure_propagates_without_authority_or_writes() {
    let (server, client) = setup().await;
    let leaf = task("leaf", Some("root"), true);
    serve_task(&server, &leaf).await;
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/root",
        ))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "kind":"Status","apiVersion":"v1","status":"Failure","code":503,
            "reason":"ServiceUnavailable","message":"fixture"
        })))
        .mount(&server)
        .await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::RequireReady).await,
        Err(Error::Api {
            code: Some(503),
            ..
        })
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET")
    );
}

#[tokio::test]
async fn conflicting_team_owner_uid_cannot_select_another_lifetime_root() {
    let (server, client) = setup().await;
    let root = task("root", None, true);
    let mut leaf = task("leaf", Some("root"), true);
    leaf.metadata.owner_references = Some(
        serde_json::from_value(json!([{
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
            "name":"foreign-team","uid":"foreign-team-uid","controller":true
        }]))
        .unwrap(),
    );
    serve_task(&server, &root).await;
    serve_task(&server, &leaf).await;
    assert!(matches!(
        resolve(&client, &leaf, LeafReadiness::RequireReady).await,
        Err(Error::Identity)
    ));
}

#[tokio::test]
async fn team_owner_is_live_uid_bound_and_not_inferred_from_display_names() {
    let (server, client) = setup().await;
    let mut root = task("root", None, true);
    let mut leaf = task("leaf", Some("root"), true);
    let owner = serde_json::from_value(json!([{
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "name":"team","uid":"team-uid","controller":true
    }]))
    .unwrap();
    root.metadata.owner_references = Some(owner);
    leaf.metadata.owner_references = root.metadata.owner_references.clone();
    serve_task(&server, &root).await;
    serve_task(&server, &leaf).await;
    Mock::given(method("GET"))
        .and(path("/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsteams/team"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
            "metadata":{"name":"team","namespace":"workspace","uid":"team-uid","resourceVersion":"1","generation":1},
            "spec":{"charter":"Fixture","envelope":{"tier":3,"authorityCeiling":3,"delegationDepth":2}}
        }))).mount(&server).await;
    let lineage = resolve(&client, &leaf, LeafReadiness::RequireReady)
        .await
        .unwrap();
    assert_eq!(lineage.team.unwrap().uid().as_deref(), Some("team-uid"));
    assert_eq!(lineage.nodes[1].pin.root_task_uid, "root-uid");
}
