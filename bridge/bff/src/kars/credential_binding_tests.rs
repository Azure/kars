use super::*;
use std::collections::BTreeMap;

#[path = "credential_entrypoint_tests.rs"]
mod entrypoint;

const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const SOURCE: &str = "/api/v1/namespaces/work/secrets/kars-credential-input-workspace";
const REMOVED: &str = "kars.azure.com/credential-removed-keys";

pub(super) fn merge(value: &mut Value, patch: &Value) {
    if let Some(fields) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, item) in fields {
            if item.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut value[key], item);
            }
        }
    } else {
        *value = patch.clone();
    }
}

pub(super) fn acknowledge(state: &mut TestApi, source: &Value) {
    if let Some(grant) = state.objects.get_mut(GRANT) {
        let version = grant["metadata"]["resourceVersion"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            + 1;
        grant["metadata"]["resourceVersion"] = version.to_string().into();
        grant["status"]["sources"] = json!([{"name":source["metadata"]["name"],"uid":source["metadata"]["uid"],
            "resourceVersion":source["metadata"]["resourceVersion"],"phase":"Unbound","reason":"AwaitingImport",
            "keys":source["data"].as_object().map(|values|values.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()}]);
    }
}

fn grant(legacy: bool) -> Value {
    let review = json!({"sourceName":"kars-credential-input-workspace","namespace":"work","namespaceUid":"namespace",
        "secret":{"name":"kars-workspace-channels","uid":"legacy"},"resourceVersion":"1","keys":["TELEGRAM_BOT_TOKEN"]});
    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":[],"integrationStores":[],
            "legacyImports":if legacy{vec![review.clone()]}else{vec![]}},
        "status":{"phase":"Ready","observedGeneration":1,"sources":[],
            "legacySources":if legacy{vec![review]}else{vec![]}}})
}

#[tokio::test]
async fn credential_removal_before_first_import_persists_source_tombstone_and_is_idempotent() {
    let (cluster, state, server) = fixture().await;
    state
        .lock()
        .unwrap()
        .objects
        .insert(GRANT.into(), grant(true));
    cluster
        .write_agent_credentials(
            "work",
            "Workspace",
            "work",
            None,
            BTreeMap::new(),
            vec!["TELEGRAM_BOT_TOKEN".into()],
        )
        .await
        .unwrap();
    let uid = state.lock().unwrap().objects[SOURCE]["metadata"]["uid"].clone();
    for _ in 0..2 {
        let source = state.lock().unwrap().objects[SOURCE].clone();
        assert_eq!(
            source["metadata"]["annotations"][REMOVED],
            "[\"TELEGRAM_BOT_TOKEN\"]"
        );
        assert!(source["data"].get("TELEGRAM_BOT_TOKEN").is_none());
        cluster
            .write_agent_credentials(
                "work",
                "Workspace",
                "work",
                None,
                BTreeMap::new(),
                vec!["TELEGRAM_BOT_TOKEN".into()],
            )
            .await
            .unwrap();
    }
    assert_eq!(
        state.lock().unwrap().objects[SOURCE]["metadata"]["uid"],
        uid
    );
    cluster
        .write_agent_credentials(
            "work",
            "Workspace",
            "work",
            None,
            BTreeMap::from([("TELEGRAM_BOT_TOKEN".into(), "explicit-new".into())]),
            Vec::new(),
        )
        .await
        .unwrap();
    let s = state.lock().unwrap();
    assert_eq!(s.objects[SOURCE]["metadata"]["annotations"][REMOVED], "[]");
    assert!(
        s.objects[SOURCE]["data"]
            .get("TELEGRAM_BOT_TOKEN")
            .is_some()
    );
    assert!(s.calls.iter().all(|(method, _, _)| method != "DELETE"));
    server.abort();
}

#[tokio::test]
async fn credential_pending_import_update_keeps_unrelated_values_and_uid_fenced_removal_intent() {
    let (cluster, state, server) = fixture().await;
    let source = json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":"kars-credential-input-workspace","namespace":"work","uid":"pending","resourceVersion":"1",
            "annotations":{"customer":"keep"}},"data":{"SLACK_BOT_TOKEN":k8s_openapi::ByteString(b"keep".to_vec())}});
    {
        let mut s = state.lock().unwrap();
        s.objects.insert(GRANT.into(), grant(true));
        s.objects.insert(SOURCE.into(), source.clone());
        acknowledge(&mut s, &source);
    }
    cluster
        .write_agent_credentials(
            "work",
            "Workspace",
            "work",
            None,
            BTreeMap::new(),
            vec!["TELEGRAM_BOT_TOKEN".into()],
        )
        .await
        .unwrap();
    let s = state.lock().unwrap();
    assert_eq!(s.objects[SOURCE]["metadata"]["uid"], "pending");
    assert_eq!(
        s.objects[SOURCE]["metadata"]["annotations"]["customer"],
        "keep"
    );
    assert_eq!(
        s.objects[SOURCE]["data"]["SLACK_BOT_TOKEN"],
        source["data"]["SLACK_BOT_TOKEN"]
    );
    assert_eq!(
        s.objects[SOURCE]["metadata"]["annotations"][REMOVED],
        "[\"TELEGRAM_BOT_TOKEN\"]"
    );
    let patch = &s
        .calls
        .iter()
        .find(|(method, path, _)| method == "PATCH" && path == SOURCE)
        .unwrap()
        .2;
    assert_eq!(patch[0]["path"], "/metadata/uid");
    assert_eq!(patch[1]["path"], "/metadata/resourceVersion");
    server.abort();
}

fn sandbox(name: &str, spec: Value, ready: bool) -> Value {
    let mut value = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":name,"namespace":"work","uid":format!("uid-{name}"),"resourceVersion":"1"},"spec":spec});
    if ready {
        value["status"] = json!({"phase":"Running"});
    }
    value
}
fn bindings(grant: &str) -> Value {
    json!({"grant":{"name":"workspace","uid":grant},"sources":[{"scope":"workspace",
        "source":{"name":"kars-credential-input-workspace","uid":"source"},"keys":[]}]})
}

#[tokio::test]
async fn workspace_rebinding_preserves_v1_and_unbounded_consumers_and_updates_only_fresh_or_opted_in()
 {
    let (cluster, state, server) = fixture().await;
    let path =
        |name: &str| format!("/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/{name}");
    let legacy = sandbox(
        "v1",
        json!({"credentialsRef":{"name":"kars-credential-source-v1","uid":"v1-source"}}),
        true,
    );
    let unbounded = sandbox("unbounded", json!({}), true);
    {
        let mut s = state.lock().unwrap();
        s.objects.insert(path("v1"), legacy.clone());
        s.objects.insert(path("unbounded"), unbounded.clone());
        s.objects
            .insert(path("fresh"), sandbox("fresh", json!({}), false));
        s.objects.insert(
            path("v2"),
            sandbox("v2", json!({"credentialBindings":bindings("grant")}), true),
        );
    }
    cluster
        .bind_workspace_credentials(
            "work",
            &Identity {
                name: "workspace".into(),
                uid: "grant".into(),
            },
            &Identity {
                name: "kars-credential-input-workspace".into(),
                uid: "source".into(),
            },
            vec!["SLACK_BOT_TOKEN".into()],
        )
        .await
        .unwrap();
    let s = state.lock().unwrap();
    assert_eq!(s.objects[&path("v1")], legacy);
    assert_eq!(s.objects[&path("unbounded")], unbounded);
    for name in ["fresh", "v2"] {
        assert_eq!(
            s.objects[&path(name)]["spec"]["credentialBindings"]["sources"][0]["keys"],
            json!(["SLACK_BOT_TOKEN"])
        );
    }
    assert_eq!(
        s.calls
            .iter()
            .filter(|(method, _, _)| method == "PATCH")
            .count(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn workspace_consumer_plan_rejects_late_conflicts_before_converting_any_target() {
    let (cluster, state, server) = fixture().await;
    {
        let mut s = state.lock().unwrap();
        s.objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/first".into(),json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
            "metadata":{"name":"first","namespace":"work","uid":"first","resourceVersion":"1"},"spec":{"blueprint":{}}}));
        s.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/late".into(),
            sandbox(
                "late",
                json!({"credentialBindings":bindings("foreign")}),
                true,
            ),
        );
    }
    assert!(
        cluster
            .bind_workspace_credentials(
                "work",
                &Identity {
                    name: "workspace".into(),
                    uid: "grant".into()
                },
                &Identity {
                    name: "kars-credential-input-workspace".into(),
                    uid: "source".into()
                },
                vec!["SLACK_BOT_TOKEN".into()]
            )
            .await
            .is_err()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    server.abort();
}
