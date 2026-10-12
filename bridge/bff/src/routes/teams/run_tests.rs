// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::routes::teams::queries;
use axum::{
    Router,
    body::Bytes,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const TEAM: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/eng";
const TASKS: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks";
const REQUEST: &str = "kars.azure.com/run-now";
const BACKLOG: &str = "/api/v1/namespaces/work/configmaps/kars-team-tasks-eng";

fn assert_namespace_list(url: &str) {
    let uri: Uri = url.parse().unwrap();
    assert_eq!(uri.path(), TASKS);
    assert!(uri.query().is_none_or(str::is_empty));
}

fn team() -> KarsTeam {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "metadata":{"name":"eng","namespace":"work","uid":"team-uid","resourceVersion":"10",
            "annotations":{"kars.azure.com/owner-sub":"owner"}},
        "spec":{"charter":"Maintain the repository", "envelope":{"tier":2,"authorityCeiling":1},"paused":false}
    })).unwrap()
}

fn prior(team: &mut KarsTeam, request: &str) {
    team.status
        .get_or_insert_with(Default::default)
        .run_admission = Some(crate::kars::team::TeamRunAdmission {
        request: request.into(),
        task_name: "prior-run".into(),
        task_uid: "prior-uid".into(),
        authority_digest: "authority".into(),
        task_spec_digest: "spec".into(),
    });
}

fn run() -> crate::kars::task::KarsTask {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"run","namespace":"work","uid":"run-uid",
            "annotations":{"kars.azure.com/team-role":"taskforce"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
                "name":"eng","uid":"team-uid","controller":true}]},
        "spec":{"objective":"Review changes", "envelope":{"tier":1,"authorityCeiling":1},"execution":{"launch":true}}
    })).unwrap()
}

#[test]
fn request_sequence_is_canonical_deterministic_and_identity_fenced() {
    let mut team = team();
    let first = run_request_patch(&team).unwrap();
    assert_eq!(first, run_request_patch(&team).unwrap());
    assert_eq!(first["metadata"]["uid"], "team-uid");
    assert_eq!(first["metadata"]["resourceVersion"], "10");
    let request = first["metadata"]["annotations"][REQUEST].as_str().unwrap();
    let hash = request.strip_prefix("manual-1-").unwrap();
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    team.metadata.resource_version = Some("11".into());
    assert_ne!(
        run_request_patch(&team).unwrap()["metadata"]["annotations"][REQUEST],
        request
    );
    team.metadata.resource_version = Some("10".into());
    team.metadata.uid = Some("replacement-uid".into());
    assert_ne!(
        run_request_patch(&team).unwrap()["metadata"]["annotations"][REQUEST],
        request
    );
    prior(&mut team, request);
    assert!(
        run_request_patch(&team).unwrap()["metadata"]["annotations"][REQUEST]
            .as_str()
            .unwrap()
            .starts_with("manual-2-")
    );
    prior(
        &mut team,
        &format!("manual-{}-{}", i64::MAX - 1, "a".repeat(64)),
    );
    assert!(
        run_request_patch(&team).unwrap()["metadata"]["annotations"][REQUEST]
            .as_str()
            .unwrap()
            .starts_with(&format!("manual-{}-", i64::MAX))
    );
}

#[test]
fn malformed_exhausted_or_unfenced_requests_fail_closed() {
    let hash = "a".repeat(64);
    for request in [
        String::new(),
        "2026-10-03T00:00:00Z".into(),
        format!("manual-0-{hash}"),
        format!("manual-01-{hash}"),
        format!("manual-+1-{hash}"),
        format!("manual--1-{hash}"),
        format!("manual-1-{}", "A".repeat(64)),
        format!("manual-1-{}", "g".repeat(64)),
        format!("manual-1-{hash}a"),
        format!("manual-{}-{hash}", i64::MAX),
        format!("manual-18446744073709551616-{hash}"),
    ] {
        let mut team = team();
        prior(&mut team, &request);
        assert!(
            matches!(run_request_patch(&team), Err(AppError::Conflict(_))),
            "{request}"
        );
    }
    for mutation in [
        "uid-missing",
        "uid-empty",
        "rv-missing",
        "rv-empty",
        "deleting",
    ] {
        let mut team = team();
        match mutation {
            "uid-missing" => team.metadata.uid = None,
            "uid-empty" => team.metadata.uid = Some(String::new()),
            "rv-missing" => team.metadata.resource_version = None,
            "rv-empty" => team.metadata.resource_version = Some(String::new()),
            "deleting" => {
                team.metadata.deletion_timestamp =
                    Some(serde_json::from_value(json!("2026-10-03T00:00:00Z")).unwrap())
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(run_request_patch(&team), Err(AppError::Conflict(_))),
            "{mutation}"
        );
    }
}

#[test]
fn run_ownership_uses_namespace_and_one_exact_controller_not_labels() {
    assert!(owned_run(&run().metadata, &team()));
    for mutation in [
        "namespace",
        "team-namespace",
        "empty-namespace",
        "missing-uid",
        "empty-uid",
        "label-only",
        "api",
        "kind",
        "name",
        "uid",
        "controller",
        "multiple",
    ] {
        let mut team = team();
        let mut run = run();
        run.labels_mut()
            .insert("kars.azure.com/team".into(), "eng".into());
        match mutation {
            "namespace" => run.metadata.namespace = Some("another".into()),
            "team-namespace" => team.metadata.namespace = None,
            "empty-namespace" => {
                team.metadata.namespace = Some(String::new());
                run.metadata.namespace = Some(String::new());
            }
            "missing-uid" => team.metadata.uid = None,
            "empty-uid" => {
                team.metadata.uid = Some(String::new());
                run.metadata.owner_references.as_mut().unwrap()[0]
                    .uid
                    .clear();
            }
            "label-only" => run.metadata.owner_references = None,
            _ => {
                let owners = run.metadata.owner_references.as_mut().unwrap();
                match mutation {
                    "api" => owners[0].api_version = "foreign/v1".into(),
                    "kind" => owners[0].kind = "KarsTask".into(),
                    "name" => owners[0].name = "another".into(),
                    "uid" => owners[0].uid = "another".into(),
                    "controller" => owners[0].controller = Some(false),
                    "multiple" => owners.push(owners[0].clone()),
                    _ => unreachable!(),
                }
            }
        }
        assert!(!owned_run(&run.metadata, &team), "{mutation}");
    }
}

#[test]
fn retained_runs_use_creation_times_not_random_identifiers_and_keep_custody_fences() {
    let make = |name: &str, timestamp: Option<&str>| {
        let mut task = run();
        task.metadata.name = Some(name.into());
        task.metadata.uid = Some(format!("uid-{name}"));
        task.metadata.creation_timestamp =
            timestamp.map(|value| serde_json::from_value(json!(value)).unwrap());
        task
    };
    let latest = make("run-012345", Some("2026-10-09T02:08:11Z"));
    let older = make("run-ffffff", Some("2026-10-09T01:08:11Z"));
    let unknown = make("run-zzzzzz", None);
    let mut foreign = make("run-foreign", Some("2026-10-09T03:08:11Z"));
    foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "another-team".into();
    let mut member = make("run-member", Some("2026-10-09T03:08:11Z"));
    member
        .annotations_mut()
        .insert("kars.azure.com/team-role".into(), "principal".into());
    for tasks in [
        vec![older.clone(), foreign.clone(), latest.clone(), member.clone(), unknown.clone()],
        vec![unknown, member, latest, foreign, older],
    ] {
        assert_eq!(
            queries::generated_runs_newest_first(&team(), tasks),
            ["run-012345", "run-ffffff", "run-zzzzzz"]
        );
    }
    let same_time = vec![
        make("run-a", Some("2026-10-09T02:08:11Z")),
        make("run-b", Some("2026-10-09T04:08:11+02:00")),
    ];
    assert_eq!(
        queries::generated_runs_newest_first(&team(), same_time),
        ["run-b", "run-a"]
    );
    assert!(queries::generated_runs_newest_first(&team(), Vec::new()).is_empty());
}

#[derive(Default)]
struct Store {
    team: Value,
    tasks: Vec<Value>,
    calls: Vec<(Method, String, Value)>,
    replacement: Option<Value>,
    failure: Option<u16>,
    backlog: Option<Value>,
    backlog_replacement: Option<Value>,
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn failure(code: u16) -> Response {
    (
        StatusCode::from_u16(code).unwrap(),
        Json(json!({"apiVersion":"v1","kind":"Status",
        "status":"Failure","reason":if code == 404 { "NotFound" } else { "Fixture" },
        "message":"Request rejected","code":code})),
    )
        .into_response()
}

async fn handle(
    State(store): State<Arc<Mutex<Store>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let mut state = store.lock().unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state
        .calls
        .push((method.clone(), uri.to_string(), body.clone()));
    if method == Method::GET && uri.path() == TEAM {
        return Json(state.team.clone()).into_response();
    }
    if method == Method::GET && uri.path() == TASKS {
        return Json(
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTaskList",
            "metadata":{},"items":state.tasks}),
        )
        .into_response();
    }
    if method == Method::GET && uri.path() == BACKLOG {
        return state
            .backlog
            .as_ref()
            .map(|backlog| Json(backlog.clone()).into_response())
            .unwrap_or_else(|| failure(404));
    }
    if method == Method::PUT && uri.path() == BACKLOG {
        if let Some(replacement) = state.backlog_replacement.take() {
            state.backlog = Some(replacement);
            return failure(409);
        }
        let Some(current) = &state.backlog else {
            return failure(404);
        };
        if body["metadata"]["uid"] != current["metadata"]["uid"]
            || body["metadata"]["resourceVersion"] != current["metadata"]["resourceVersion"]
        {
            return failure(409);
        }
        state.backlog = Some(body.clone());
        return Json(body).into_response();
    }
    if method == Method::PATCH && uri.path() == TEAM {
        if let Some(replacement) = state.replacement.take() {
            state.team = replacement;
        }
        if let Some(code) = state.failure {
            return failure(code);
        }
        if body["metadata"]["uid"] != state.team["metadata"]["uid"]
            || body["metadata"]["resourceVersion"] != state.team["metadata"]["resourceVersion"]
        {
            return failure(409);
        }
        state.team["metadata"]["annotations"][REQUEST] =
            body["metadata"]["annotations"][REQUEST].clone();
        state.team["metadata"]["resourceVersion"] = json!("11");
        return Json(state.team.clone()).into_response();
    }
    failure(405)
}

async fn fixture() -> (crate::kars::cluster::Cluster, Arc<Mutex<Store>>, Server) {
    let state = Arc::new(Mutex::new(Store {
        team: serde_json::to_value(team()).unwrap(),
        ..Default::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(handle).with_state(state.clone());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client =
        kube::Client::try_from(kube::Config::new(format!("http://{addr}").parse().unwrap()))
            .unwrap();
    (
        crate::kars::cluster::Cluster::for_test_client(client),
        state,
        server,
    )
}

fn principal() -> Principal {
    Principal {
        sub: "owner".into(),
        name: "Owner".into(),
        roles: vec!["user".into()],
    }
}

fn backlog() -> Value {
    json!({
        "apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":"kars-team-tasks-eng","namespace":"work","uid":"backlog-uid",
            "resourceVersion":"10","ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1",
                "kind":"KarsTeam","name":"eng","uid":"team-uid","controller":true}]},
        "data":{"tasks.json":r#"[{"id":"a","title":"A","status":"pending"}]"#,"retained":"value"}
    })
}

#[tokio::test]
async fn backlog_errors_and_uid_replacement_never_become_an_empty_queue_or_a_new_write() {
    use crate::routes::teams::{read_tasks, work, work_owner};
    for mutation in [
        "malformed",
        "missing-data",
        "unowned",
        "foreign-owner",
        "replaced",
    ] {
        let (cluster, state, _server) = fixture().await;
        let mut stored = backlog();
        match mutation {
            "malformed" => stored["data"]["tasks.json"] = json!("not json"),
            "missing-data" => {
                stored["data"].as_object_mut().unwrap().remove("tasks.json");
            }
            "unowned" => {
                stored["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("ownerReferences");
            }
            "foreign-owner" => stored["metadata"]["ownerReferences"][0]["uid"] = json!("other"),
            _ => {}
        }
        state.lock().unwrap().backlog = Some(stored.clone());
        if mutation == "replaced" {
            let mut replacement = stored.clone();
            replacement["metadata"]["uid"] = json!("replacement-uid");
            replacement["metadata"]["resourceVersion"] = json!("11");
            state.lock().unwrap().backlog_replacement = Some(replacement.clone());
            let owner = work_owner(&team()).unwrap();
            let result = work::update(&cluster.configmaps("work"), &owner, None, true, |items| {
                items[0].title = "Changed".into();
                Ok(())
            })
            .await;
            assert!(
                matches!(result, Err(work::Error::Conflict(_))),
                "{result:?}"
            );
            let state = state.lock().unwrap();
            assert_eq!(state.backlog.as_ref(), Some(&replacement));
            assert_eq!(
                state
                    .calls
                    .iter()
                    .filter(|(method, _, _)| *method == Method::PUT)
                    .count(),
                1
            );
        } else {
            assert!(read_tasks(&cluster, &team()).await.is_err(), "{mutation}");
            let state = state.lock().unwrap();
            assert_eq!(state.backlog.as_ref(), Some(&stored));
            assert!(
                state
                    .calls
                    .iter()
                    .all(|(method, _, _)| *method == Method::GET)
            );
        }
    }
}

#[tokio::test]
async fn request_records_only_fenced_intent_and_repeated_calls_preserve_it() {
    let (cluster, state, _server) = fixture().await;
    let result = request_team_run(&cluster, "work", "eng", &principal())
        .await
        .unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(result["triggered"], true);
        assert!(
            result["note"]
                .as_str()
                .unwrap()
                .contains("not yet executed")
        );
        assert_eq!(
            result["request"],
            state.team["metadata"]["annotations"][REQUEST]
        );
        assert_eq!(state.calls.len(), 4);
        assert_eq!(state.calls[1].0, Method::GET);
        assert_eq!(state.calls[1].1, BACKLOG);
        assert_namespace_list(&state.calls[2].1);
        assert_eq!(state.calls[3].0, Method::PATCH);
        assert_eq!(state.calls[3].2, run_request_patch(&team()).unwrap());
        assert!(state.tasks.is_empty());
    }
    let saved = state.lock().unwrap().team.clone();
    assert!(matches!(
        request_team_run(&cluster, "work", "eng", &principal()).await,
        Err(AppError::BadRequest(_))
    ));
    assert_eq!(state.lock().unwrap().team, saved);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _, _)| *method != Method::GET)
            .count(),
        1
    );
}

#[tokio::test]
async fn paused_pending_and_nonowner_requests_make_no_mutations() {
    for case in ["paused", "pending", "empty-pending", "nonowner"] {
        let (cluster, state, _server) = fixture().await;
        let mut caller = principal();
        {
            let mut state = state.lock().unwrap();
            match case {
                "paused" => state.team["spec"]["paused"] = json!(true),
                "pending" => state.team["metadata"]["annotations"][REQUEST] = json!("legacy"),
                "empty-pending" => state.team["metadata"]["annotations"][REQUEST] = json!(""),
                "nonowner" => {
                    caller.sub = "another".into();
                    caller.roles = vec!["operator".into()];
                }
                _ => unreachable!(),
            }
        }
        let saved = state.lock().unwrap().team.clone();
        let result = request_team_run(&cluster, "work", "eng", &caller).await;
        if case == "nonowner" {
            assert!(matches!(result, Err(AppError::NotFound)));
        } else {
            assert!(
                matches!(result, Err(AppError::BadRequest(_))),
                "{case}: {result:?}"
            );
        }
        let state = state.lock().unwrap();
        assert_eq!(state.team, saved);
        assert_eq!(state.calls.len(), 1);
        assert_eq!(state.calls[0].0, Method::GET);
    }
}

#[tokio::test]
async fn namespace_wide_owned_active_runs_block_but_impostors_and_idle_tasks_do_not() {
    for case in [
        "owned-unlabelled",
        "foreign-labelled",
        "label-only",
        "inactive",
        "principal",
    ] {
        let (cluster, state, _server) = fixture().await;
        let mut task = run();
        match case {
            "owned-unlabelled" => {}
            "foreign-labelled" => {
                task.labels_mut()
                    .insert("kars.azure.com/team".into(), "eng".into());
                task.metadata.owner_references.as_mut().unwrap()[0].uid = "another".into();
            }
            "label-only" => {
                task.labels_mut()
                    .insert("kars.azure.com/team".into(), "eng".into());
                task.metadata.owner_references = None;
            }
            "inactive" => task.spec.execution.as_mut().unwrap().launch = false,
            "principal" => {
                task.annotations_mut()
                    .insert("kars.azure.com/team-role".into(), "principal".into());
            }
            _ => unreachable!(),
        }
        state
            .lock()
            .unwrap()
            .tasks
            .push(serde_json::to_value(task).unwrap());
        let result = request_team_run(&cluster, "work", "eng", &principal()).await;
        if case == "owned-unlabelled" {
            assert!(matches!(result, Err(AppError::BadRequest(_))));
        } else {
            assert!(result.is_ok(), "{case}: {result:?}");
        }
        let state = state.lock().unwrap();
        assert_namespace_list(&state.calls[2].1);
        assert_eq!(
            state
                .calls
                .iter()
                .filter(|(method, _, _)| *method != Method::GET)
                .count(),
            usize::from(case != "owned-unlabelled")
        );
    }
}

#[tokio::test]
async fn concurrent_team_changes_and_api_rejections_never_overwrite_current_state() {
    for case in ["replacement", "pause", "new-request", "422", "503"] {
        let (cluster, state, _server) = fixture().await;
        let mut expected = state.lock().unwrap().team.clone();
        {
            let mut state = state.lock().unwrap();
            match case {
                "replacement" => expected["metadata"]["uid"] = json!("replacement-uid"),
                "pause" => expected["spec"]["paused"] = json!(true),
                "new-request" => {
                    expected["metadata"]["annotations"][REQUEST] = json!("newer-request")
                }
                "422" => state.failure = Some(422),
                "503" => state.failure = Some(503),
                _ => unreachable!(),
            }
            if !matches!(case, "422" | "503") {
                expected["metadata"]["resourceVersion"] = json!("12");
                state.replacement = Some(expected.clone());
            }
        }
        let result = request_team_run(&cluster, "work", "eng", &principal()).await;
        if case == "503" {
            assert!(matches!(result, Err(AppError::Upstream(_))));
        } else {
            assert!(
                matches!(result, Err(AppError::Conflict(_))),
                "{case}: {result:?}"
            );
        }
        let state = state.lock().unwrap();
        assert_eq!(state.team, expected);
        assert_eq!(state.calls.len(), 4);
        assert!(state.tasks.is_empty());
    }
}
