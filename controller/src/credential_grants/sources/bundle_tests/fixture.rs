// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::*;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const NAMESPACE: &str = "/api/v1/namespaces/work";
pub const GRANT: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
pub const SECRETS: &str = "/api/v1/namespaces/work/secrets";

pub struct State {
    pub objects: BTreeMap<String, Value>,
    pub calls: Vec<(String, String, Value)>,
    pub target_path: String,
    pub source_path: String,
    pub bundle_path: String,
    pub after_create: Option<&'static str>,
    pub after_anchor: Option<&'static str>,
    pub create_response: Option<&'static str>,
    pub create_error: Option<u16>,
    pub lose_create_ack: bool,
    pub malformed_create_ack: bool,
    pub anchor_error: Option<(usize, u16)>,
    pub lose_anchor_ack: Option<usize>,
    pub persistent_conflict: bool,
    pub fail_recovery_get: Option<&'static str>,
    pub creates: usize,
    pub anchors: usize,
    pub writes: usize,
    pub cas_conflicts: usize,
}

pub fn bump(value: &mut Value) {
    let rv = value["metadata"]["resourceVersion"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    value["metadata"]["resourceVersion"] = json!(rv.to_string());
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

pub fn mutate(state: &mut State, mutation: &str) {
    let target = state.target_path.clone();
    let source = state.source_path.clone();
    let bundle = state.bundle_path.clone();
    let created_uid = state
        .objects
        .get(&bundle)
        .map(|value| value["metadata"]["uid"].clone());
    let object: &str = if mutation.starts_with("bundle-") {
        &bundle
    } else if mutation.starts_with("source-") {
        &source
    } else if mutation.starts_with("grant-") {
        GRANT
    } else if mutation.starts_with("namespace-") {
        NAMESPACE
    } else {
        &target
    };
    let value = state.objects.get_mut(object).unwrap();
    match mutation {
        "suspension" => {
            value["spec"]["suspended"] = json!(false);
            value["metadata"]["generation"] = json!(2);
        }
        "status" => value["status"] = json!({"phase":"Pending","reason":"ConcurrentReconcile"}),
        "target-uid" | "source-uid" | "grant-uid" | "namespace-uid" | "bundle-uid" => {
            value["metadata"]["uid"] = json!("replacement")
        }
        "target-delete" | "source-delete" | "grant-delete" | "namespace-delete"
        | "bundle-delete" => value["metadata"]["deletionTimestamp"] = json!("2026-09-12T00:00:00Z"),
        "target-bindings" => value["spec"]["credentialBindings"]["sources"][0]["keys"] = json!([]),
        "target-direct" => {
            value["spec"]["credentialsRef"] = json!({"name":"legacy","uid":"legacy"})
        }
        "target-spec" => value["spec"]["inferenceRef"]["name"] = json!("different-policy"),
        "target-owner" => {
            value["metadata"]["ownerReferences"] = json!([{
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"foreign","uid":"foreign","controller":true
            }])
        }
        "target-anchor" => {
            value["metadata"]["annotations"][bundle::UID_ANNOTATION] = json!("foreign")
        }
        "target-anchor-current" => {
            value["metadata"]["annotations"][bundle::UID_ANNOTATION] =
                created_uid.expect("bundle CREATE must precede its target anchor")
        }
        "target-annotation" => value["metadata"]["annotations"]["authority"] = json!("changed"),
        "target-suspension-type" => value["spec"]["suspended"] = json!("not-a-boolean"),
        "grant-spec" => {
            value["spec"]["enabled"] = json!(false);
            value["metadata"]["generation"] = json!(2);
        }
        "grant-unready" => value["status"]["phase"] = json!("Blocked"),
        "source-value" => {
            value["data"]["TELEGRAM_BOT_TOKEN"] = json!(ByteString(b"fresh-value".to_vec()))
        }
        "bundle-data" => {
            value["data"] = json!({"TELEGRAM_BOT_TOKEN":ByteString(b"foreign-value".to_vec())})
        }
        "bundle-string-data" => value["stringData"] = json!({"TELEGRAM_BOT_TOKEN":"foreign-value"}),
        "bundle-owner" => value["metadata"]["ownerReferences"][0]["uid"] = json!("foreign"),
        "bundle-purpose" => value["metadata"]["annotations"][PURPOSE] = json!("foreign"),
        "bundle-kind" => value["metadata"]["annotations"][TARGET_KIND] = json!("KarsTask"),
        "bundle-target" => value["metadata"]["annotations"][TARGET] = json!("foreign"),
        "bundle-workspace" => value["metadata"]["annotations"][WORKSPACE] = json!("foreign"),
        "bundle-grant" => value["metadata"]["annotations"][GRANT_UID] = json!("foreign"),
        "bundle-type" => value["type"] = json!("kubernetes.io/tls"),
        "bundle-state" => value["metadata"]["annotations"][INPUT_STATE] = json!("already-consumed"),
        "bundle-immutable" => value["immutable"] = json!(true),
        "bundle-no-uid" => value["metadata"]["uid"] = Value::Null,
        "bundle-no-rv" => {
            value["metadata"]["resourceVersion"] = Value::Null;
            return;
        }
        "bundle-namespace" => value["metadata"]["namespace"] = json!("foreign"),
        "bundle-name" => value["metadata"]["name"] = json!("foreign"),
        "source-rv" | "grant-rv" | "bundle-rv" => {}
        _ => panic!("unknown bundle fixture mutation"),
    }
    bump(value);
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion":"v1","kind":"Status","status":"Failure","code":code,
        "reason":if code==404 {"NotFound"} else if code==409 {"Conflict"} else {"Error"},
        "message":"PRIVATE_SERVER_DETAIL",
    }))
}

fn respond(state: &mut State, request: &Request) -> ResponseTemplate {
    let path = request.url.path();
    let method = request.method.as_str();
    let body: Value = if request.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&request.body).unwrap()
    };
    state.calls.push((method.into(), path.into(), body.clone()));
    if method == "GET" {
        let failing = state.fail_recovery_get.is_some_and(|kind| {
            path == match kind {
                "target" => state.target_path.as_str(),
                "source" => state.source_path.as_str(),
                "bundle" => state.bundle_path.as_str(),
                "grant" => GRANT,
                "namespace" => NAMESPACE,
                _ => panic!("unknown read failure"),
            }
        });
        if state.creates > 0 && failing {
            return failure(500);
        }
        return state.objects.get(path).map_or_else(
            || failure(404),
            |value| ResponseTemplate::new(200).set_body_json(value),
        );
    }
    if method == "POST" && path == SECRETS {
        state.creates += 1;
        if let Some(code) = state.create_error {
            return failure(code);
        }
        if state.objects.contains_key(&state.bundle_path) {
            return failure(409);
        }
        let mut created = body;
        assert!(created.get("data").is_none());
        created["metadata"]["uid"] = json!("exclusive-bundle-uid");
        created["metadata"]["resourceVersion"] = json!("30");
        created["metadata"]["creationTimestamp"] = json!("2026-09-12T00:00:00Z");
        state
            .objects
            .insert(state.bundle_path.clone(), created.clone());
        if let Some(mutation) = state.create_response.take() {
            mutate(state, mutation);
            created = state.objects[&state.bundle_path].clone();
        }
        if let Some(mutation) = state.after_create.take() {
            mutate(state, mutation);
            let target = state.objects.get_mut(&state.target_path).unwrap();
            bump(target);
        }
        if state.lose_create_ack {
            return failure(500);
        }
        if state.malformed_create_ack {
            return ResponseTemplate::new(201).set_body_string("{invalid");
        }
        return ResponseTemplate::new(201).set_body_json(created);
    }
    if method == "PATCH" && path == state.target_path {
        state.anchors += 1;
        assert_eq!(
            body.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["metadata"]
        );
        assert_eq!(
            body["metadata"]["annotations"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![bundle::UID_ANNOTATION]
        );
        if let Some((attempt, code)) = state.anchor_error
            && attempt == state.anchors
        {
            return failure(code);
        }
        let target = state.objects.get_mut(path).unwrap();
        if state.persistent_conflict {
            bump(target);
        }
        if body["metadata"]["uid"] != target["metadata"]["uid"]
            || body["metadata"]["resourceVersion"] != target["metadata"]["resourceVersion"]
        {
            state.cas_conflicts += 1;
            return failure(409);
        }
        merge(target, &body);
        bump(target);
        let response = target.clone();
        if let Some(mutation) = state.after_anchor.take() {
            mutate(state, mutation);
        }
        if state.lose_anchor_ack == Some(state.anchors) {
            return failure(500);
        }
        return ResponseTemplate::new(200).set_body_json(response);
    }
    if method == "PATCH" && path == state.bundle_path {
        let secret = state.objects.get_mut(path).unwrap();
        if body["metadata"]["uid"] != secret["metadata"]["uid"]
            || body["metadata"]["resourceVersion"] != secret["metadata"]["resourceVersion"]
        {
            state.cas_conflicts += 1;
            return failure(409);
        }
        assert!(body.get("data").is_some());
        merge(secret, &body);
        bump(secret);
        state.writes += 1;
        return ResponseTemplate::new(200).set_body_json(secret.clone());
    }
    panic!("unexpected bundle fixture operation: {method} {path}");
}

pub struct Fixture {
    pub _server: MockServer,
    pub client: Client,
    pub state: Arc<Mutex<State>>,
    pub target: CredentialTarget,
    pub bindings: CredentialBindings,
}

impl Fixture {
    pub async fn prepare(&self) -> Result<Secret, String> {
        super::super::prepare(&self.client, &self.target, &self.bindings).await
    }

    pub fn bundle(&self) -> Value {
        let state = self.state.lock().unwrap();
        state.objects[&state.bundle_path].clone()
    }
}

pub async fn setup(kind: &str) -> Fixture {
    let target = CredentialTarget {
        kind: kind.into(),
        namespace: "work".into(),
        name: "agent".into(),
        uid: "target-uid".into(),
    };
    let source_name = input_name(kind, "agent").unwrap();
    let bindings = CredentialBindings {
        grant: ObjectIdentity {
            name: NAME.into(),
            uid: "grant-uid".into(),
        },
        sources: vec![CredentialSelection {
            scope: CredentialScope::Target,
            source: ObjectIdentity {
                name: source_name.clone(),
                uid: "source-uid".into(),
            },
            keys: vec!["TELEGRAM_BOT_TOKEN".into()],
            owner: Some(target.clone()),
        }],
    };
    let plural = match kind {
        "KarsSandbox" => "karssandboxes",
        "KarsTask" => "karstasks",
        "KarsTeam" => "karsteams",
        _ => panic!("unsupported fixture kind"),
    };
    let target_path = format!("/apis/kars.azure.com/v1alpha1/namespaces/work/{plural}/agent");
    let source_path = format!("{SECRETS}/{source_name}");
    let bundle_path = format!("{SECRETS}/{}", bundle_name(&target));
    let mut object = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":kind,
        "metadata":{"name":"agent","namespace":"work","uid":target.uid,"resourceVersion":"10","generation":1},
        "spec":if kind=="KarsSandbox" { json!({"suspended":true,"inferenceRef":{"name":"policy"},"credentialBindings":bindings}) }
            else { json!({"objective":"Test credential bundle","envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":1},
                "execution":{"launch":true},"blueprint":{"model":{"provider":"azure-openai","deployment":"test"},
                    "credentialBindings":bindings}}) }});
    if kind == "KarsTask" {
        let task: crate::kars_task::KarsTask = serde_json::from_value(object.clone()).unwrap();
        object["status"] = json!({"phase":"Ready","observedGeneration":1,"envelopeDigest":task.envelope_digest(),
            "conditions":[{"type":"Ready","status":"True","reason":"Reconciled","message":"Fixture",
                "lastTransitionTime":"2026-09-12T00:00:00Z"}]});
        let ready: crate::kars_task::KarsTask = serde_json::from_value(object.clone()).unwrap();
        assert!(crate::kars_task_reconciler::task_is_ready(&ready));
    }
    let state = Arc::new(Mutex::new(State {
        objects: BTreeMap::from([
            (target_path.clone(), object),
            (
                GRANT.into(),
                json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace","namespace":"work","uid":"grant-uid","resourceVersion":"20","generation":1},
                "spec":{"workspaceUid":"workspace-uid","writers":[],"enabled":true},
                "status":{"phase":"Ready","observedGeneration":1,"reason":"Fixture"}}),
            ),
            (
                NAMESPACE.into(),
                json!({"apiVersion":"v1","kind":"Namespace",
                "metadata":{"name":"work","uid":"workspace-uid","resourceVersion":"1"}}),
            ),
            (
                source_path.clone(),
                json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                "metadata":{"name":source_name,"namespace":"work","uid":"source-uid","resourceVersion":"40",
                    "ownerReferences":[owner_ref(&target)],"annotations":{PURPOSE:INPUT_PURPOSE,
                        WORKSPACE:"work",TARGET_KIND:kind,TARGET:"agent",TARGET_UID:target.uid,
                        GRANT_UID:"grant-uid",INTENT:"explicit-reference-v2","kars.azure.com/credential-import-revision":"enrolled"}},
                "data":{"TELEGRAM_BOT_TOKEN":ByteString(b"initial-value".to_vec())}}),
            ),
        ]),
        calls: vec![],
        target_path,
        source_path,
        bundle_path,
        after_create: None,
        after_anchor: None,
        create_response: None,
        create_error: None,
        lose_create_ack: false,
        malformed_create_ack: false,
        anchor_error: None,
        lose_anchor_ack: None,
        persistent_conflict: false,
        fail_recovery_get: None,
        creates: 0,
        anchors: 0,
        writes: 0,
        cas_conflicts: 0,
    }));
    let server = MockServer::start().await;
    let handler = state.clone();
    Mock::given(|_: &Request| true)
        .respond_with(move |request: &Request| respond(&mut handler.lock().unwrap(), request))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    Fixture {
        _server: server,
        client,
        state,
        target,
        bindings,
    }
}
