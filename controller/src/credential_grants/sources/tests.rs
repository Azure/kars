// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SOURCE: &str = "/api/v1/namespaces/work/secrets/kars-credential-input-workspace";
const LEGACY: &str = "/api/v1/namespaces/work/secrets/kars-workspace-channels";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    patches: Vec<Value>,
    conflict: bool,
}
fn merge(value: &mut Value, patch: &Value) {
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

async fn fixture(
    existing_value: bool,
) -> (MockServer, Client, Arc<Mutex<State>>, KarsCredentialGrant) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State::default()));
    let grant:KarsCredentialGrant=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"workspaceUid":"workspace","writers":[],"enabled":true,"legacyImports":[{
            "sourceName":"kars-credential-input-workspace","namespace":"work","namespaceUid":"workspace",
            "secret":{"name":"kars-workspace-channels","uid":"legacy"},"resourceVersion":"1",
            "keys":["SLACK_BOT_TOKEN","TELEGRAM_BOT_TOKEN"]}]}
    })).unwrap();
    {
        let mut s = state.lock().unwrap();
        s.objects.insert(
            "/api/v1/namespaces/work".into(),
            json!({"metadata":{"name":"work","uid":"workspace","resourceVersion":"1"}}),
        );
        s.objects.insert(LEGACY.into(),json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-workspace-channels","namespace":"work","uid":"legacy","resourceVersion":"1"},
            "data":{"TELEGRAM_BOT_TOKEN":ByteString(b"legacy-token".to_vec()),"SLACK_BOT_TOKEN":ByteString(b"retained".to_vec())}}));
        s.objects.insert(SOURCE.into(),json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-credential-input-workspace","namespace":"work","uid":"source","resourceVersion":"1",
                "annotations":{PURPOSE:INPUT_PURPOSE,WORKSPACE:"work",TARGET_KIND:"Workspace",TARGET:"work",
                    GRANT_UID:"grant",INTENT:"explicit-reference-v2",REMOVED_KEYS:"[\"TELEGRAM_BOT_TOKEN\"]"}},
            "data":if existing_value {json!({"TELEGRAM_BOT_TOKEN":ByteString(b"pending-old".to_vec())})}else{json!({})}}));
    }
    let captured = state.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |r:&wiremock::Request| {
        let mut s=captured.lock().unwrap();let path=r.url.path();
        if r.method=="GET" && let Some(value)=s.objects.get(path) {return ResponseTemplate::new(200).set_body_json(value);}
        if r.method=="GET" && path=="/api/v1/namespaces/work/secrets" {
            let items = s.objects.values().filter(|value| value["kind"] == "Secret")
                .map(|value| json!({"metadata":value["metadata"]})).collect::<Vec<_>>();
            return ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList","metadata":{},"items":items}));
        }
        if r.method=="PATCH" && path.starts_with("/api/v1/namespaces/work/secrets/") {
            if s.conflict {
                return ResponseTemplate::new(409).set_body_json(json!({
                    "apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":409}));
            }
            let body:Value=r.body_json().unwrap();let value=s.objects.get_mut(path).unwrap();
            assert_eq!(value["metadata"]["uid"],body["metadata"]["uid"]);
            assert_eq!(value["metadata"]["resourceVersion"],body["metadata"]["resourceVersion"]);
            let revision=value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u64>().unwrap()+1;
            merge(value,&body);value["metadata"]["resourceVersion"]=revision.to_string().into();
            let result=value.clone();s.patches.push(body);
            return ResponseTemplate::new(200).set_body_json(result);
        }
        ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, grant)
}

#[tokio::test]
async fn credential_ownership_receipt_attests_only_the_exact_metadata_cas_and_expires_on_change() {
    const TARGET_SOURCE: &str =
        "/api/v1/namespaces/work/secrets/kars-credential-input-sandbox-agent";
    let (_server, client, state, mut grant) = fixture(false).await;
    {
        let mut state = state.lock().unwrap();
        state.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/agent".into(),
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                "metadata":{"name":"agent","namespace":"work","uid":"agent","resourceVersion":"1"},
                "spec":{"inferenceRef":{"name":"policy"}}}),
        );
        state.objects.insert(
            TARGET_SOURCE.into(),
            json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-credential-input-sandbox-agent","namespace":"work",
                "uid":"agent-source","resourceVersion":"1","annotations":{
                    PURPOSE:INPUT_PURPOSE,WORKSPACE:"work",TARGET_KIND:"KarsSandbox",TARGET:"agent",
                    TARGET_UID:"agent",GRANT_UID:"grant",INTENT:"explicit-reference-v2"}},
            "data":{"SLACK_BOT_TOKEN":ByteString(b"original".to_vec())}}),
        );
        state.conflict = true;
    }
    assert!(inventory(&client, &grant).await.is_err());
    assert!(state.lock().unwrap().patches.is_empty());
    state.lock().unwrap().conflict = false;
    let observed = inventory(&client, &grant).await.unwrap();
    let bound = observed
        .iter()
        .find(|entry| entry.uid == "agent-source")
        .unwrap();
    assert_eq!(bound.ownership_from_resource_version.as_deref(), Some("1"));
    assert_eq!(bound.resource_version, "2");
    {
        let state = state.lock().unwrap();
        assert_eq!(state.patches.len(), 1);
        assert_eq!(
            state.patches[0]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["metadata"]
        );
        assert_eq!(
            state.objects[TARGET_SOURCE]["data"]["SLACK_BOT_TOKEN"],
            json!(ByteString(b"original".to_vec()))
        );
    }
    grant.status = Some(CredentialGrantStatus {
        sources: observed,
        ..Default::default()
    });
    let repeated = inventory(&client, &grant).await.unwrap();
    assert_eq!(
        repeated
            .iter()
            .find(|entry| entry.uid == "agent-source")
            .unwrap()
            .ownership_from_resource_version
            .as_deref(),
        Some("1")
    );
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(TARGET_SOURCE).unwrap()["metadata"]["resourceVersion"] = "3".into();
        state.objects.get_mut(TARGET_SOURCE).unwrap()["data"]["SLACK_BOT_TOKEN"] =
            json!(ByteString(b"changed".to_vec()));
    }
    let changed = inventory(&client, &grant).await.unwrap();
    assert!(
        changed
            .iter()
            .find(|entry| entry.uid == "agent-source")
            .unwrap()
            .ownership_from_resource_version
            .is_none()
    );
}

#[tokio::test]
async fn credential_deletion_tombstone_wins_over_first_import_and_existing_pending_values_idempotently()
 {
    for pending_value in [false, true] {
        let (_server, client, state, grant) = fixture(pending_value).await;
        let target = CredentialTarget {
            kind: "KarsSandbox".into(),
            namespace: "work".into(),
            name: "agent".into(),
            uid: "agent".into(),
        };
        let selection = CredentialSelection {
            scope: CredentialScope::Workspace,
            source: ObjectIdentity {
                name: "kars-credential-input-workspace".into(),
                uid: "source".into(),
            },
            keys: vec!["TELEGRAM_BOT_TOKEN".into(), "SLACK_BOT_TOKEN".into()],
            owner: None,
        };
        let source = read_input(&client, &grant, &target, &selection)
            .await
            .unwrap();
        assert!(
            !source
                .data
                .as_ref()
                .unwrap()
                .contains_key("TELEGRAM_BOT_TOKEN")
        );
        assert_eq!(
            source.data.as_ref().unwrap()["SLACK_BOT_TOKEN"].0,
            b"retained"
        );
        let patches = state.lock().unwrap().patches.len();
        read_input(&client, &grant, &target, &selection)
            .await
            .unwrap();
        let s = state.lock().unwrap();
        assert_eq!(s.patches.len(), patches);
        assert_eq!(s.objects[SOURCE]["metadata"]["uid"], "source");
        assert!(
            s.objects[SOURCE]["data"]
                .get("TELEGRAM_BOT_TOKEN")
                .is_none()
        );
        assert_eq!(s.objects[LEGACY]["metadata"]["uid"], "legacy");
        assert_eq!(
            s.objects[LEGACY]["data"]["TELEGRAM_BOT_TOKEN"],
            json!(ByteString(b"legacy-token".to_vec()))
        );
        assert!(s.patches.iter().any(|patch| {
            patch["data"]
                .as_object()
                .is_some_and(|data| data.get("TELEGRAM_BOT_TOKEN") == Some(&Value::Null))
        }));
    }
}

#[test]
fn credential_attenuation_rejects_both_revealing_overridden_values_and_revealing_absent_masks() {
    let workspace = CredentialSelection {
        scope: CredentialScope::Workspace,
        source: ObjectIdentity {
            name: format!("{INPUT_PREFIX}workspace"),
            uid: "workspace".into(),
        },
        keys: vec!["TELEGRAM_BOT_TOKEN".into()],
        owner: None,
    };
    let later = CredentialSelection {
        scope: CredentialScope::Team,
        source: ObjectIdentity {
            name: format!("{INPUT_PREFIX}team-team"),
            uid: "team".into(),
        },
        keys: workspace.keys.clone(),
        owner: Some(CredentialTarget {
            kind: "KarsTeam".into(),
            namespace: "work".into(),
            name: "team".into(),
            uid: "team".into(),
        }),
    };
    let parent = CredentialBindings {
        grant: ObjectIdentity {
            name: NAME.into(),
            uid: "grant".into(),
        },
        sources: vec![workspace.clone(), later.clone()],
    };
    let child = CredentialBindings {
        grant: parent.grant.clone(),
        sources: vec![workspace.clone()],
    };
    let early: Secret = serde_json::from_value(
        json!({"data":{"TELEGRAM_BOT_TOKEN":ByteString(b"hidden".to_vec())}}),
    )
    .unwrap();
    for overridden in [false, true] {
        let late: Secret = serde_json::from_value(if overridden {
            json!({"data":{"TELEGRAM_BOT_TOKEN":ByteString(b"override".to_vec())}})
        } else {
            json!({"data":{}})
        })
        .unwrap();
        let mut parent_values = BTreeMap::new();
        apply_selection(&mut parent_values, &early, &workspace);
        apply_selection(&mut parent_values, &late, &later);
        let mut child_values = BTreeMap::new();
        apply_selection(&mut child_values, &early, &workspace);
        assert_ne!(parent_values, child_values);
        assert!(!attenuates(Some(&child), Some(&parent)));
        assert!(attenuates(Some(&parent), Some(&parent)));
    }
}
