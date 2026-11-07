// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_sre_action::{ActionSpec, ApprovalSpec, KarsSREActionSpec};
use std::{collections::BTreeMap, sync::Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::any};

const CR_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions/fix";
const NS_PATH: &str = "/api/v1/namespaces/team-a";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    failures: BTreeMap<(String, String), u16>,
    tokens: BTreeMap<String, String>,
    writes: usize,
    token_requests: usize,
    revision: u64,
    token_ttl: i64,
    empty_token: bool,
    missing_token: bool,
    missing_expiry: bool,
    fail_after_status_commit: bool,
    fail_after_claim_commit: bool,
    held_deletions: bool,
    replace_target_after_dry_run: bool,
}

#[derive(Clone)]
struct Cluster(Arc<Mutex<State>>);

fn response(code: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
}

fn error(code: u16) -> ResponseTemplate {
    let reason = match code {
        403 => "Forbidden",
        404 => "NotFound",
        409 => "Conflict",
        _ => "InternalError",
    };
    response(
        code,
        json!({"apiVersion":"v1","kind":"Status","status":"Failure",
        "message":"test API failure","reason":reason,"code":code}),
    )
}

fn merge(value: &mut Value, patch: &Value) {
    if let Some(map) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, child) in map {
            if child.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut value[key], child);
            }
        }
    } else {
        *value = patch.clone();
    }
}

impl Respond for Cluster {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        let path = request.url.path().to_string();
        let method = request.method.as_str();
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let controller = authorization == "Bearer controller-test";
        let writer_target = state.tokens.get(authorization).cloned();
        if !controller && writer_target.as_deref() != Some(path.as_str()) {
            return error(403);
        }
        if request.headers.contains_key("impersonate-user")
            || request.headers.contains_key("impersonate-group")
        {
            return error(403);
        }
        if let Some(code) = state.failures.get(&(method.into(), path.clone())) {
            return error(*code);
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let dry_run = request
            .url
            .query_pairs()
            .any(|(k, v)| k == "dryRun" && v == "All");
        if path.ends_with("/token") {
            if !controller || method != "POST" {
                return error(403);
            }
            assert_eq!(body["spec"]["expirationSeconds"], 600);
            assert_eq!(body["spec"]["audiences"], json!([]));
            assert!(body["spec"].get("boundObjectRef").is_none());
            let account_path = path.trim_end_matches("/token");
            if !state.objects.contains_key(account_path) {
                return error(404);
            }
            let name = account_path.rsplit('/').next().unwrap();
            let role_path =
                format!("/apis/rbac.authorization.k8s.io/v1/namespaces/team-a/roles/{name}");
            let role = state.objects.get(&role_path).unwrap();
            let resource = role["rules"][0]["resources"][0].as_str().unwrap();
            let group = role["rules"][0]["apiGroups"][0].as_str().unwrap();
            let target = role["rules"][0]["resourceNames"][0].as_str().unwrap();
            let (resource, suffix) = resource
                .strip_suffix("/scale")
                .map(|r| (r, "/scale"))
                .unwrap_or((resource, ""));
            let prefix = if group.is_empty() {
                "/api/v1".into()
            } else {
                format!("/apis/{group}/v1")
            };
            let target_path = format!("{prefix}/namespaces/team-a/{resource}/{target}{suffix}");
            state.token_requests += 1;
            let token = if state.empty_token {
                String::new()
            } else {
                format!("writer-test-{name}")
            };
            state.tokens.insert(format!("Bearer {token}"), target_path);
            let mut issued = json!({"apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
                "spec":body["spec"],"status":{"token":token,"expirationTimestamp":
                    (Utc::now()+chrono::Duration::seconds(state.token_ttl)).to_rfc3339()}});
            if state.missing_token {
                issued["status"].as_object_mut().unwrap().remove("token");
            }
            if state.missing_expiry {
                issued["status"]
                    .as_object_mut()
                    .unwrap()
                    .remove("expirationTimestamp");
            }
            return response(201, issued);
        }
        if method == "GET" {
            return state
                .objects
                .get(&path)
                .cloned()
                .map(|v| response(200, v))
                .unwrap_or_else(|| error(404));
        }
        if method == "POST" {
            if !controller {
                return error(403);
            }
            assert!(!path.contains("clusterrolebindings"));
            let name = body["metadata"]["name"].as_str().unwrap();
            let key = format!("{path}/{name}");
            if state.objects.contains_key(&key) {
                return error(409);
            }
            let mut object = body;
            state.revision += 1;
            object["metadata"]["uid"] = json!(format!("authority-{}", state.revision));
            object["metadata"]["resourceVersion"] = json!(state.revision.to_string());
            state.objects.insert(key, object.clone());
            return response(201, object);
        }
        let status_patch = path == format!("{CR_PATH}/status");
        let key = if status_patch {
            CR_PATH.to_string()
        } else {
            path.clone()
        };
        let Some(mut object) = state.objects.get(&key).cloned() else {
            return error(404);
        };
        if method == "PATCH" {
            let target = key != CR_PATH;
            if target && controller {
                return error(403);
            }
            if let Some(rv) = body["metadata"].get("resourceVersion")
                && *rv != object["metadata"]["resourceVersion"]
            {
                return error(409);
            }
            if let Some(uid) = body["metadata"].get("uid")
                && *uid != object["metadata"]["uid"]
            {
                return error(409);
            }
            if target {
                assert_eq!(body["metadata"]["uid"], "target-uid");
                assert_eq!(body["metadata"]["resourceVersion"], "1");
            }
            merge(&mut object, &body);
            if target && dry_run && state.replace_target_after_dry_run {
                state.objects.get_mut(&key).unwrap()["metadata"]["uid"] =
                    json!("replacement-after-dry-run");
            }
            if !dry_run {
                state.revision += 1;
                object["metadata"]["resourceVersion"] = json!(state.revision.to_string());
                state.objects.insert(key, object.clone());
                if target {
                    state.writes += 1;
                }
                if status_patch
                    && state.fail_after_claim_commit
                    && object["status"]["phase"] == PHASE_APPROVED
                {
                    state.fail_after_claim_commit = false;
                    return error(500);
                }
                if status_patch
                    && state.fail_after_status_commit
                    && object["status"]["phase"] == PHASE_APPLIED
                {
                    state.fail_after_status_commit = false;
                    return error(500);
                }
            }
            return response(200, object);
        }
        if method == "DELETE" {
            if let Some(uid) = body["preconditions"].get("uid")
                && *uid != object["metadata"]["uid"]
            {
                return error(409);
            }
            if let Some(rv) = body["preconditions"].get("resourceVersion")
                && *rv != object["metadata"]["resourceVersion"]
            {
                return error(409);
            }
            // DeleteParams expresses dryRun in the JSON DeleteOptions body.
            let dry_run = dry_run || body["dryRun"] == json!(["All"]);
            if dry_run && writer_target.is_some() && state.replace_target_after_dry_run {
                state.objects.get_mut(&key).unwrap()["metadata"]["uid"] =
                    json!("replacement-after-dry-run");
            }
            if !dry_run {
                if writer_target.is_some() {
                    state.writes += 1;
                }
                if !state.held_deletions {
                    state.objects.remove(&key);
                }
            }
            return response(200, object);
        }
        error(405)
    }
}

fn action(kind: &str, workload: Option<&str>) -> KarsSREAction {
    let mut params = BTreeMap::from([
        ("namespace".into(), json!("team-a")),
        ("name".into(), json!("workload")),
        ("replicas".into(), json!(2)),
        ("container".into(), json!("app")),
        ("image".into(), json!("example/image:latest")),
    ]);
    if let Some(kind) = workload {
        params.insert("kind".into(), json!(kind));
    }
    let mut cr = KarsSREAction::new(
        "fix",
        KarsSREActionSpec {
            action: ActionSpec {
                kind: kind.into(),
                params,
            },
            approval: ApprovalSpec {
                state: APPROVAL_APPROVED.into(),
                note: None,
            },
            ..Default::default()
        },
    );
    cr.metadata.namespace = Some("kars-sre".into());
    cr.metadata.uid = Some("11111111-2222-3333-4444-555555555555".into());
    cr.metadata.resource_version = Some("1".into());
    cr
}

fn target_path(cr: &KarsSREAction) -> String {
    let scope = authority::scope(&cr.spec.action).unwrap();
    format!(
        "{}/namespaces/team-a/{}/workload{}",
        if scope.group.is_empty() {
            "/api/v1"
        } else {
            "/apis/apps/v1"
        },
        scope.resource,
        if scope.scale { "/scale" } else { "" }
    )
}

async fn setup(cr: &KarsSREAction) -> (MockServer, Cluster, Arc<Ctx>) {
    let server = MockServer::start().await;
    let scope = authority::scope(&cr.spec.action).unwrap();
    let state = State {
        token_ttl: 600,
        revision: 1,
        objects: BTreeMap::from([
            (
                NS_PATH.into(),
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"team-a","uid":"namespace-uid"}}),
            ),
            (CR_PATH.into(), serde_json::to_value(cr).unwrap()),
            (
                target_path(cr),
                json!({"apiVersion":if scope.scale {"autoscaling/v1"} else {"apps/v1"},
            "kind":if scope.scale {"Scale"} else {scope.kind},
            "metadata":{"name":"workload","namespace":"team-a","uid":"target-uid","resourceVersion":"1"},
            "spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"app","image":"old"}]}}}}),
            ),
        ]),
        ..Default::default()
    };
    let cluster = Cluster(Arc::new(Mutex::new(state)));
    Mock::given(any())
        .respond_with(cluster.clone())
        .mount(&server)
        .await;
    let mut config = kube::Config::new(server.uri().parse().unwrap());
    config.auth_info.token = Some("controller-test".into());
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(config.clone()).unwrap();
    let ctx = Arc::new(Ctx {
        client,
        connection: config,
    });
    (server, cluster, ctx)
}

fn current(cluster: &Cluster) -> KarsSREAction {
    serde_json::from_value(cluster.0.lock().unwrap().objects[CR_PATH].clone()).unwrap()
}

async fn drive(cluster: &Cluster, ctx: &Arc<Ctx>) -> Result<Action, ReconcileError> {
    reconcile(Arc::new(current(cluster)), ctx.clone()).await
}

async fn claimed(cluster: &Cluster, ctx: &Arc<Ctx>) -> KarsSREAction {
    drive(cluster, ctx).await.unwrap(); // install finalizer, no authority
    let api = Api::<KarsSREAction>::namespaced(ctx.client.clone(), "kars-sre");
    claim_execution(&api, ctx, &current(cluster)).await.unwrap()
}

fn expire(cluster: &Cluster) {
    cluster.0.lock().unwrap().objects.get_mut(CR_PATH).unwrap()["status"]["execution"]["deadline"] =
        json!((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
}

#[path = "kars_sre_action_execution_cases.rs"]
mod kars_sre_action_execution_cases;
