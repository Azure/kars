// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Stateful HTTP regressions for retries across durable Kubernetes writes.

use super::tests::team;
use super::*;
use crate::kars_team::TeamCadence;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[path = "budget_interleaving_tests.rs"]
mod budget_interleavings;

const TASKS_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks";
const CMS_PATH: &str = "/api/v1/namespaces/tenant-a/configmaps";
const TEAM_STATUS_PATH: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsteams/eng/status";

#[derive(Default)]
struct Store {
    tasks: BTreeMap<String, Value>,
    cms: BTreeMap<String, Value>,
    team: Value,
    version: i64,
    fail_status_once: bool,
    fail_commons_write: bool,
}

#[derive(Clone)]
struct KubeServer(Arc<Mutex<Store>>);

fn response(code: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
}

fn failure(code: u16) -> ResponseTemplate {
    response(
        code,
        json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure",
            "reason": match code {
                404 => "NotFound", 409 => "Conflict", 422 => "Invalid", _ => "InternalError",
            }, "code": code,
        }),
    )
}

fn merge(target: &mut Value, patch: Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, value) in patch {
            if value.is_null() {
                target.as_object_mut().unwrap().remove(&key);
            } else {
                merge(
                    target
                        .as_object_mut()
                        .unwrap()
                        .entry(key)
                        .or_insert(Value::Null),
                    value,
                );
            }
        }
    } else {
        *target = patch;
    }
}

impl Respond for KubeServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut store = self.0.lock().unwrap();
        let path = request.url.path();
        let method = request.method.as_str();
        if path == TEAM_STATUS_PATH && method == "PATCH" {
            if store.fail_status_once {
                store.fail_status_once = false;
                return failure(500);
            }
            let patch: Value = serde_json::from_slice(&request.body).unwrap();
            if patch["metadata"]["resourceVersion"] != store.team["metadata"]["resourceVersion"] {
                return failure(409);
            }
            merge(&mut store.team, patch);
            store.version += 1;
            store.team["metadata"]["resourceVersion"] = json!(store.version.to_string());
            return response(200, store.team.clone());
        }
        if path == TASKS_PATH && method == "GET" {
            return response(
                200,
                json!({
                    "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTaskList",
                    "metadata": {}, "items": store.tasks.values().collect::<Vec<_>>(),
                }),
            );
        }
        let (is_task, name) = if path == TASKS_PATH || path.starts_with(&format!("{TASKS_PATH}/")) {
            (
                true,
                path.strip_prefix(TASKS_PATH)
                    .unwrap()
                    .trim_start_matches('/'),
            )
        } else if path == CMS_PATH || path.starts_with(&format!("{CMS_PATH}/")) {
            (
                false,
                path.strip_prefix(CMS_PATH).unwrap().trim_start_matches('/'),
            )
        } else {
            return failure(404);
        };
        if method == "GET" {
            return match if is_task {
                store.tasks.get(name)
            } else {
                store.cms.get(name)
            } {
                Some(value) => response(200, value.clone()),
                None => failure(404),
            };
        }
        let mut body: Value = serde_json::from_slice(&request.body).unwrap();
        if method == "POST" {
            if is_task
                && body["spec"]["objective"]
                    .as_str()
                    .is_some_and(|objective| objective.chars().count() > specs::MAX_OBJECTIVE_CHARS)
            {
                return failure(422);
            }
            let name = body["metadata"]["name"].as_str().unwrap().to_owned();
            if if is_task {
                store.tasks.contains_key(&name)
            } else {
                store.cms.contains_key(&name)
            } {
                return failure(409);
            }
            store.version += 1;
            body["metadata"]["uid"] = json!(format!("uid-{name}-{}", store.version));
            body["metadata"]["resourceVersion"] = json!(store.version.to_string());
            body["metadata"]["generation"] = json!(1);
            body["metadata"]["creationTimestamp"] = json!(Utc::now().to_rfc3339());
            if is_task {
                store.tasks.insert(name, body.clone());
            } else {
                store.cms.insert(name, body.clone());
            }
            return response(201, body);
        }
        if method == "PUT" {
            if !is_task && store.fail_commons_write {
                return failure(409);
            }
            let existing = if is_task {
                store.tasks.get(name)
            } else {
                store.cms.get(name)
            };
            let Some(existing) = existing else {
                return failure(404);
            };
            if existing["metadata"]["resourceVersion"] != body["metadata"]["resourceVersion"]
                || existing["metadata"]["uid"] != body["metadata"]["uid"]
            {
                return failure(409);
            }
            store.version += 1;
            body["metadata"]["resourceVersion"] = json!(store.version.to_string());
            if is_task {
                store.tasks.insert(name.into(), body.clone());
            } else {
                store.cms.insert(name.into(), body.clone());
            }
            return response(200, body);
        }
        if method == "DELETE" && is_task {
            let Some(existing) = store.tasks.get(name) else {
                return failure(404);
            };
            if body["preconditions"]["uid"] != existing["metadata"]["uid"]
                || body["preconditions"]["resourceVersion"]
                    != existing["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            return response(200, store.tasks.remove(name).unwrap());
        }
        failure(405)
    }
}

async fn setup(team: &KarsTeam) -> (MockServer, Client, Arc<Mutex<Store>>) {
    let server = MockServer::start().await;
    let store = Arc::new(Mutex::new(Store {
        team: serde_json::to_value(team).unwrap(),
        version: 100,
        ..Default::default()
    }));
    Mock::given(wiremock::matchers::any())
        .respond_with(KubeServer(store.clone()))
        .mount(&server)
        .await;
    let client = super::state_tests::client(&server).await;
    (server, client, store)
}

#[tokio::test]
async fn failed_team_status_write_reuses_the_created_cadence_run() {
    let mut team = team();
    team.spec.envelope.budget = None;
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    store.lock().unwrap().fail_status_once = true;
    let ctx = Arc::new(Ctx { client });
    assert!(reconcile(Arc::new(team), ctx.clone()).await.is_err());
    let persisted: KarsTeam = serde_json::from_value(store.lock().unwrap().team.clone()).unwrap();
    assert_eq!(
        persisted.status.as_ref().unwrap().phase.as_deref(),
        Some(PHASE_DEGRADED)
    );
    reconcile(Arc::new(persisted), ctx).await.unwrap();
    let store = store.lock().unwrap();
    assert_eq!(
        store.tasks.len(),
        2,
        "one principal and one run, not a second retry run"
    );
    assert_eq!(store.team["status"]["generatedTaskCount"], 1);
    assert_eq!(store.team["status"]["phase"], PHASE_ACTIVE);
    assert_eq!(
        store
            .tasks
            .values()
            .filter(|task| task["metadata"]["annotations"][ANNOT_TEAM_ROLE] == "taskforce")
            .count(),
        1
    );
}

#[tokio::test]
async fn invalid_team_revokes_owned_authority_but_not_a_labeled_customer() {
    let mut team = team();
    team.spec.envelope.authority_ceiling = 0;
    let (_server, client, store) = setup(&team).await;
    let mut owned = super::tests::principal(&team);
    owned.spec.execution = Some(crate::kars_task::TaskExecution {
        launch: true,
        runtime: None,
    });
    let mut customer = owned.clone();
    customer.metadata.name = Some("customer".into());
    customer.metadata.owner_references = None;
    customer.metadata.labels = Some([(ANNOT_TEAM.into(), team.name_any())].into());
    {
        let mut state = store.lock().unwrap();
        state
            .tasks
            .insert(owned.name_any(), serde_json::to_value(owned).unwrap());
        state
            .tasks
            .insert(customer.name_any(), serde_json::to_value(customer).unwrap());
    }
    assert!(
        reconcile(Arc::new(team), Arc::new(Ctx { client }))
            .await
            .is_err()
    );
    let state = store.lock().unwrap();
    assert_eq!(state.team["status"]["phase"], PHASE_DEGRADED);
    assert!(state.team["status"].get("envelopeDigest").is_none());
    assert_eq!(state.tasks.len(), 1);
    assert_eq!(state.tasks["customer"]["spec"]["execution"]["launch"], true);
}

#[tokio::test]
async fn commons_must_commit_before_retirement_and_write_conflicts_retry() {
    let mut team = team();
    team.spec.envelope.budget = None;
    let (server, client, store) = setup(&team).await;
    crate::team_commons::ensure_commons(&client, &team)
        .await
        .unwrap();
    let name = runs::cadence_name(&team).unwrap();
    let tasks_api = Api::namespaced(client.clone(), "tenant-a");
    tasks::apply_task(
        &tasks_api,
        &team,
        &name,
        specs::run_spec(&team, "").unwrap(),
        "taskforce",
    )
    .await
    .unwrap();
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&name).unwrap()["metadata"]["annotations"]["kars.azure.com/run-completed"] =
            json!(name);
        state.cms.insert(format!("kars-mission-output-{name}"), json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": { "name": format!("kars-mission-output-{name}"), "namespace": "tenant-a" },
            "data": { "status": "ok", "totalTokens": "25", "artifactCount": "0", "output": "Useful run result.", "finishedAt": Utc::now().to_rfc3339() },
        }));
        state.fail_commons_write = true;
    }
    assert!(
        runs::harvest_and_retire_runs(&client, &tasks_api, &team)
            .await
            .is_err()
    );
    assert_eq!(
        store.lock().unwrap().tasks[&name]["spec"]["execution"]["launch"],
        true
    );
    store.lock().unwrap().fail_commons_write = false;
    let stats = runs::harvest_and_retire_runs(&client, &tasks_api, &team)
        .await
        .unwrap();
    assert_eq!(stats.succeeded, 1);
    assert_eq!(
        store.lock().unwrap().tasks[&name]["spec"]["execution"]["launch"],
        false
    );
    assert_eq!(
        crate::team_commons::entry_count(&client, &team)
            .await
            .unwrap(),
        1
    );
    let requests = server.received_requests().await.unwrap();
    let last_commons_put = requests
        .iter()
        .rposition(|request| {
            request.method == "PUT" && request.url.path().contains("/configmaps/kars-commons-")
        })
        .unwrap();
    let task_put = requests
        .iter()
        .position(|request| request.method == "PUT" && request.url.path().contains("/karstasks/"))
        .unwrap();
    assert!(last_commons_put < task_put);
}

#[tokio::test]
async fn digest_create_is_namespace_owned_and_retry_deduplicated() {
    let team = team();
    let (_server, client, store) = setup(&team).await;
    for _ in 0..2 {
        crate::team_digest::publish(&client, &team, None, "Healthy", "report", 1, 1, 25, 1)
            .await
            .unwrap();
    }
    let state = store.lock().unwrap();
    let cm = &state.cms["kars-team-digest-eng"];
    assert_eq!(cm["metadata"]["namespace"], "tenant-a");
    assert_eq!(cm["metadata"]["ownerReferences"][0]["uid"], "team-uid");
    let log: Vec<crate::team_digest::DigestEntry> =
        serde_json::from_str(cm["data"]["log.json"].as_str().unwrap()).unwrap();
    assert_eq!(log.len(), 1);
}

#[tokio::test]
async fn bounded_team_cadence_is_an_idle_plan_not_ready_to_run() {
    let mut team = team();
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    reconcile(Arc::new(team), Arc::new(Ctx { client }))
        .await
        .unwrap();
    let state = store.lock().unwrap();
    assert_eq!(state.tasks.len(), 1, "only the idle principal plan exists");
    assert_eq!(state.team["status"]["phase"], PHASE_DEGRADED);
    assert_eq!(state.team["status"]["generatedTaskCount"], 0);
    assert!(
        state.team["status"]["detail"]
            .as_str()
            .unwrap()
            .contains("UnsupportedLaunchBudget")
    );
    assert!(
        state
            .tasks
            .values()
            .all(|task| task["spec"]["execution"]["launch"] != true)
    );
}

#[tokio::test]
async fn positive_budget_existing_owned_launches_are_stopped() {
    let team = team();
    let (_server, client, store) = setup(&team).await;
    let mut principal = super::tests::principal(&team);
    principal.spec.execution = Some(crate::kars_task::TaskExecution {
        launch: true,
        runtime: None,
    });
    store.lock().unwrap().tasks.insert(
        principal.name_any(),
        serde_json::to_value(principal).unwrap(),
    );
    tasks::reconcile_revocations(&Api::namespaced(client, "tenant-a"), &team)
        .await
        .unwrap();
    assert_eq!(
        store.lock().unwrap().tasks["eng-principal"]["spec"]["execution"]["launch"],
        false
    );
}

#[tokio::test]
async fn five_entry_history_recovers_cadence_after_oversized_objective_rejection() {
    let mut team = team();
    team.spec.charter = "c".repeat(160);
    team.spec.envelope.budget = None;
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    team.status = Some(KarsTeamStatus {
        phase: Some(PHASE_DEGRADED.into()),
        detail: Some("spec.objective must be 1-4096 characters".into()),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    crate::team_commons::ensure_commons(&client, &team)
        .await
        .unwrap();
    for n in 0..5 {
        crate::team_commons::record_entry(
            &client,
            &team,
            &format!("00000000-0000-4000-8000-{n:012}"),
            &team.spec.charter,
            &format!("engineering-run-{n:032x}"),
            &format!("engineering-run-{n:032x}"),
            &"f".repeat(400),
        )
        .await
        .unwrap();
    }

    let name = runs::cadence_name(&team).unwrap();
    let all_history = crate::team_commons::prior_knowledge(&client, &team, usize::MAX)
        .await
        .unwrap();
    let mut legacy_run = specs::run_spec(&team, "").unwrap();
    legacy_run.objective.push_str(&all_history);
    assert!(legacy_run.objective.chars().count() > specs::MAX_OBJECTIVE_CHARS);
    let task_api = Api::namespaced(client.clone(), "tenant-a");
    let error = tasks::apply_task(&task_api, &team, &name, legacy_run, "taskforce")
        .await
        .unwrap_err();
    assert!(matches!(error, ReconcileError::Kube(kube::Error::Api(error)) if error.code == 422));

    reconcile(
        Arc::new(team.clone()),
        Arc::new(Ctx {
            client: client.clone(),
        }),
    )
    .await
    .unwrap();
    {
        let state = store.lock().unwrap();
        assert_eq!(state.team["status"]["phase"], PHASE_ACTIVE);
        assert_eq!(state.team["status"]["generatedTaskCount"], 1);
        assert_eq!(state.team["status"]["lastGeneratedTask"], name);
        let objective = state.tasks[&name]["spec"]["objective"].as_str().unwrap();
        assert!(objective.chars().count() <= specs::MAX_OBJECTIVE_CHARS);
        assert!(objective.starts_with(&specs::run_spec(&team, "").unwrap().objective));
        let entries: Vec<Value> = objective
            .lines()
            .filter(|line| line.starts_with('{'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(!entries.is_empty() && entries.len() < 5);
        assert!(objective.ends_with("--- END UNTRUSTED REFERENCE DATA ---\n"));
        assert_eq!(
            state.tasks.len(),
            2,
            "one principal and one admitted cadence run"
        );
    }
    assert_eq!(
        crate::team_commons::entry_count(&client, &team)
            .await
            .unwrap(),
        5
    );
}

#[tokio::test]
async fn composed_budget_mcp_seat_preserves_only_authorized_credential_rebind_holds() {
    for (governed, paused, pending, narrowed, launch) in [
        (true, false, true, false, true),
        (false, false, true, false, false),
        (true, true, true, false, false),
        (true, false, false, false, false),
        (true, false, true, true, false),
    ] {
        let bindings = |key: &str| {
            json!({
                "grant": {"name": "workspace", "uid": "grant-uid"},
                "sources": [{"scope": "workspace",
                    "source": {"name": "kars-credential-input-workspace", "uid": "source-uid"},
                    "keys": [key]}]
            })
        };
        let mut team = team();
        if governed {
            team.spec.envelope.budget.as_mut().unwrap().scope =
                Some(crate::inference_budget_contract::BudgetScope::GovernedInference);
        }
        team.spec.blueprint = Some(
            serde_json::from_value(json!({
                "model": {"provider": "azure-openai", "deployment": "model"},
                "toolPolicy": "governed-tools", "mcpServers": ["everything"],
                "credentialBindings": bindings("SLACK_BOT_TOKEN")
            }))
            .unwrap(),
        );
        let mut principal = super::tests::principal(&team);
        principal
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .credential_bindings =
            Some(serde_json::from_value(bindings("TELEGRAM_BOT_TOKEN")).unwrap());
        principal.spec.execution = Some(crate::kars_task::TaskExecution {
            launch: true,
            runtime: None,
        });
        if pending {
            principal.annotations_mut().insert(
                crate::kars_task_reconciler::rebind::PENDING.into(),
                "true".into(),
            );
        }
        team.spec.paused = paused;
        if narrowed {
            team.spec.envelope.authority_ceiling -= 1;
        }
        let uid = principal.uid();
        let name = principal.name_any();
        let (_server, client, store) = setup(&team).await;
        store
            .lock()
            .unwrap()
            .tasks
            .insert(name.clone(), serde_json::to_value(&principal).unwrap());
        tasks::reconcile_revocations(&Api::namespaced(client, "tenant-a"), &team)
            .await
            .unwrap();
        let state = store.lock().unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[&name]["metadata"]["uid"], json!(uid));
        assert_eq!(
            state.tasks[&name]["spec"]["execution"]["launch"], launch,
            "governed={governed}, paused={paused}, pending={pending}, narrowed={narrowed}"
        );
        assert_eq!(
            state.tasks[&name]["spec"]["blueprint"]["mcpServers"],
            json!(["everything"])
        );
        assert_eq!(
            state.tasks[&name]["spec"]["blueprint"]["credentialBindings"],
            bindings("TELEGRAM_BOT_TOKEN")
        );
    }
}

#[tokio::test]
async fn zero_history_allowance_still_checks_store_ownership_and_integrity() {
    let team = team();
    let (_server, client, store) = setup(&team).await;
    crate::team_commons::ensure_commons(&client, &team)
        .await
        .unwrap();
    assert!(
        crate::team_commons::prior_knowledge(&client, &team, 0)
            .await
            .unwrap()
            .is_empty()
    );
    let name = crate::team_commons::commons_cm_name(&team.commons_name());
    store.lock().unwrap().cms.get_mut(&name).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
        json!("foreign");
    assert!(
        crate::team_commons::prior_knowledge(&client, &team, 0)
            .await
            .is_err()
    );
    {
        let mut state = store.lock().unwrap();
        let cm = state.cms.get_mut(&name).unwrap();
        cm["metadata"]["ownerReferences"][0]["uid"] = json!(team.metadata.uid);
        cm["data"]["index.json"] = json!("malformed");
    }
    assert!(
        crate::team_commons::prior_knowledge(&client, &team, 0)
            .await
            .is_err()
    );
}
