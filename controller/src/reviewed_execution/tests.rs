// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[path = "lifecycle_tests.rs"]
pub(super) mod lifecycle_tests;
#[path = "scheduling_tests.rs"]
mod scheduling_tests;

#[derive(Clone, Default)]
struct Memory(Arc<Mutex<BTreeMap<String, Value>>>);
fn object_path(task: &KarsTask) -> String {
    format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/{}",
        task.name_any()
    )
}
fn map_path(name: &str) -> String {
    format!("/api/v1/namespaces/default/configmaps/{name}")
}
fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}
impl Respond for Memory {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut objects = self.0.lock().unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let method = request.method.as_str();
        let path = if method == "POST" {
            format!(
                "{}/{}",
                request.url.path(),
                body["metadata"]["name"].as_str().unwrap()
            )
        } else {
            request.url.path().trim_end_matches("/status").to_owned()
        };
        if method == "GET" && path.ends_with("/karstasks") {
            let items: Vec<_> = objects
                .values()
                .filter(|v| v["kind"] == "KarsTask")
                .cloned()
                .collect();
            return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTaskList","metadata":{},"items":items}));
        }
        let prior = objects.get(&path).cloned();
        let error = |code| {
            ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":if code == 404 {"NotFound"} else {"Conflict"},"message":format!("fixture rejection: {method} {path}"),"code":code}))
        };
        if method == "GET" {
            return prior.map_or_else(
                || error(404),
                |v| ResponseTemplate::new(200).set_body_json(v),
            );
        }
        if method == "POST" && prior.is_some() {
            return error(409);
        }
        if method != "POST" && prior.is_none() {
            return error(404);
        }
        if method == "DELETE" {
            let prior = prior.unwrap();
            if body["preconditions"]["uid"] != prior["metadata"]["uid"]
                || body["preconditions"]["resourceVersion"] != prior["metadata"]["resourceVersion"]
            {
                return error(409);
            }
            objects.remove(&path);
            return ResponseTemplate::new(200).set_body_json(prior);
        }
        if let Some(prior) = &prior {
            if body["metadata"]["resourceVersion"] != prior["metadata"]["resourceVersion"]
                || (body["metadata"].get("uid").is_some()
                    && body["metadata"]["uid"] != prior["metadata"]["uid"])
            {
                return error(409);
            }
        }
        let mut next = if method == "PATCH" {
            let mut v = prior.clone().unwrap();
            merge(&mut v, &body);
            v
        } else {
            body
        };
        if next["kind"] == "KarsTask"
            && next["spec"]["objective"]
                .as_str()
                .is_some_and(|text| !(1..=4096).contains(&text.chars().count()))
        {
            return ResponseTemplate::new(422).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","reason":"Invalid",
                "code":422,"message":"spec.objective must be 1-4096 characters"
            }));
        }
        let rv = prior
            .as_ref()
            .and_then(|v| v["metadata"]["resourceVersion"].as_str())
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap()
            + 1;
        next["metadata"]["resourceVersion"] = rv.to_string().into();
        if method == "POST" {
            next["metadata"]["uid"] =
                format!("uid-{}", next["metadata"]["name"].as_str().unwrap()).into();
            next["metadata"]["namespace"] = "default".into();
            if next["kind"] == "KarsTask" {
                next["metadata"]["generation"] = 1.into();
            }
        }
        if let Some(prior) = prior {
            if next["spec"] != prior["spec"] {
                next["metadata"]["generation"] =
                    (prior["metadata"]["generation"].as_i64().unwrap_or(0) + 1).into();
            }
        }
        objects.insert(path, next.clone());
        ResponseTemplate::new(200).set_body_json(next)
    }
}
impl Memory {
    fn put<T: Serialize>(&self, path: String, value: &T) {
        self.0
            .lock()
            .unwrap()
            .insert(path, serde_json::to_value(value).unwrap());
    }
    fn task(&self, name: &str) -> KarsTask {
        serde_json::from_value(
            self.0.lock().unwrap()
                [&format!("/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/{name}")]
                .clone(),
        )
        .unwrap()
    }
    fn ready(&self, name: &str) -> KarsTask {
        let mut task = self.task(name);
        ready(&mut task);
        self.put(object_path(&task), &task);
        task
    }
}
fn plan_value() -> Value {
    json!({"schema":"kars.execution-plan/v1","maxParallel":1,"roles":[{"name":"writer","objective":"Draft a useful report","budgetTokens":5000,"phases":[{"name":"draft","objective":"Write the reviewed handoff.md document","capabilities":["filesystem-read","filesystem-write"],"minToolCalls":1,"maxToolCalls":4,"freshContext":true}]}],"synthesis":{"objective":"Write report.md from the handoff","capabilities":["filesystem-read","filesystem-write"],"maxToolCalls":4},"deliverables":[{"name":"report.md"}]})
}
fn root() -> KarsTask {
    let mut task = KarsTask::new("report", Default::default());
    task.metadata.namespace = Some("default".into());
    task.metadata.uid = Some("root-uid".into());
    task.metadata.resource_version = Some("1".into());
    task.metadata.generation = Some(1);
    task.metadata.annotations = Some(BTreeMap::from([(RUN.into(), "run-1".into())]));
    task.spec.objective = "Prepare the reviewed report".into();
    task.spec.execution = Some(TaskExecution {
        launch: true,
        ..Default::default()
    });
    task.spec.envelope.delegation_depth = 2;
    task.spec.envelope.budget =
        Some(serde_json::from_value(json!({"scope":"GovernedInference","tokens":10000})).unwrap());
    task.spec.blueprint = Some(
        serde_json::from_value(json!({"runtime":"OpenClaw","executionPlan":plan_value()})).unwrap(),
    );
    ready(&mut task);
    task
}
fn ready(task: &mut KarsTask) {
    task.status=Some(serde_json::from_value(json!({"phase":"Ready","observedGeneration":task.metadata.generation,"envelopeDigest":task.envelope_digest(),"conditions":[{"type":"Ready","status":"True","reason":"Reconciled","message":"validated","lastTransitionTime":"2026-10-02T20:00:00Z"}]})).unwrap());
}
fn hermes_root() -> KarsTask {
    let mut task = root();
    task.spec.blueprint.as_mut().unwrap().runtime = None;
    task.spec.execution.as_mut().unwrap().runtime = Some("Hermes".into());
    ready(&mut task);
    task
}

fn seal_team_test_run(task: &mut KarsTask) {
    task.annotations_mut().insert(
        "kars.azure.com/team-request-state".into(),
        "admitted".into(),
    );
    let digest = crate::kars_team_reconciler::requests::task_digest(task).unwrap();
    task.annotations_mut()
        .insert("kars.azure.com/team-request-spec".into(), digest);
}

fn team_root(runtime: &str) -> KarsTask {
    team_root_with_roles(runtime, &["writer"])
}

fn team_root_with_roles(runtime: &str, names: &[&str]) -> KarsTask {
    let mut task = root();
    let plan = task
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    let writer = plan.roles[0].clone();
    plan.roles = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut role = writer.clone();
            role.name = (*name).into();
            role.depends_on = names[..index].iter().map(|name| (*name).into()).collect();
            role
        })
        .collect();
    plan.synthesis.max_tool_calls = plan
        .synthesis
        .max_tool_calls
        .max((plan.roles.len() + plan.deliverables.len()) as i32);
    task.spec.blueprint.as_mut().unwrap().tool_policy = Some("reviewed-policy".into());
    let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
    team.metadata.namespace = task.namespace();
    team.metadata.uid = Some("clinic-uid".into());
    team.spec.envelope = task.spec.envelope.clone();
    team.spec.envelope.delegation_depth = 3;
    team.spec.blueprint = task.spec.blueprint.clone();
    let mut blueprint = task.spec.blueprint.clone().unwrap();
    blueprint.runtime = Some(runtime.into());
    blueprint.execution_plan = None;
    blueprint.model = Some(
        serde_json::from_value(json!({"provider":"azure-foundry","deployment":"role-model"}))
            .unwrap(),
    );
    blueprint.model_fallbacks = vec![
        serde_json::from_value(json!({"provider":"backup","deployment":"role-fallback"})).unwrap(),
    ];
    team.spec.roster = names
        .iter()
        .map(|name| crate::kars_team::TeamRole {
            name: (*name).into(),
            blueprint: Some(blueprint.clone()),
            ..Default::default()
        })
        .collect();
    task.metadata.owner_references = Some(vec![
        crate::kars_team_reconciler::team_owner_ref(&team).unwrap(),
    ]);
    let name = task.name_any();
    task.annotations_mut().insert(RUN.into(), name);
    seal_team_roles(&team, &mut task).unwrap();
    seal_team_test_run(&mut task);
    ready(&mut task);
    task
}

#[tokio::test]
async fn reviewed_team_phase_uses_sealed_roster_runtime_model_and_fallbacks() {
    for runtime in ["OpenClaw", "Hermes"] {
        let initial = team_root(runtime);
        assert_eq!(
            crate::kars_task::blueprint::effective_runtime_name(&initial.spec),
            "OpenClaw"
        );
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        let role = memory.task(&phase.spec.parent_ref.as_ref().unwrap().name);
        for task in [&role, &phase] {
            assert_eq!(
                crate::kars_task::blueprint::effective_runtime_name(&task.spec),
                runtime
            );
            let blueprint = task.spec.blueprint.as_ref().unwrap();
            assert_eq!(blueprint.model.as_ref().unwrap().deployment, "role-model");
            assert_eq!(blueprint.model_fallbacks[0].deployment, "role-fallback");
            assert_eq!(blueprint.tool_policy.as_deref(), Some("reviewed-policy"));
            assert!(blueprint.execution_plan.is_none());
        }
        assert!(grant(&client, &phase).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn changed_team_roster_cannot_retarget_a_reserved_phase() {
    let (_server, client, memory, phase) = phase_fixture_with(team_root("Hermes")).await;
    let mut root = memory.task("report");
    let mut roles: Value = serde_json::from_str(ann(&root, TEAM_ROLES).unwrap()).unwrap();
    roles["writer"]["blueprint"]["runtime"] = json!("OpenClaw");
    root.annotations_mut()
        .insert(TEAM_ROLES.into(), roles.to_string());
    memory.put(object_path(&root), &root);
    assert!(
        grant(&client, &phase).await.is_err(),
        "request seal must reject edited roster"
    );
    seal_team_test_run(&mut root);
    memory.put(object_path(&root), &root);
    assert!(
        grant(&client, &phase).await.is_err(),
        "reserved state must retain original roster"
    );
    assert_eq!(
        crate::kars_task::blueprint::effective_runtime_name(&memory.task(&phase.name_any()).spec),
        "Hermes"
    );
}

#[test]
fn reviewed_team_requires_complete_sealed_nonamplifying_roster() {
    for mutation in ["missing", "role", "runtime", "egress", "policy"] {
        let mut root = team_root("Hermes");
        let mut roles: Value = serde_json::from_str(ann(&root, TEAM_ROLES).unwrap()).unwrap();
        match mutation {
            "missing" => {
                root.annotations_mut().remove(TEAM_ROLES);
            }
            "role" => {
                roles.as_object_mut().unwrap().remove("writer");
            }
            "runtime" => roles["writer"]["blueprint"]["runtime"] = json!("unsupported"),
            "egress" => {
                roles["writer"]["blueprint"]["egress"] =
                    json!([{"host":"extra.example","port":443}])
            }
            "policy" => roles["writer"]["blueprint"]["toolPolicy"] = json!("different-policy"),
            _ => unreachable!(),
        }
        if mutation != "missing" {
            root.annotations_mut()
                .insert(TEAM_ROLES.into(), roles.to_string());
        }
        seal_team_test_run(&mut root);
        assert!(validate(&root).is_err(), "{mutation}");
    }
}
#[test]
fn reviewed_team_role_cap_cannot_raise_a_tighter_roster_allocation() {
    let mut root = team_root("Hermes");
    let mut roles: Value = serde_json::from_str(ann(&root, TEAM_ROLES).unwrap()).unwrap();
    roles["writer"]["envelope"]["budget"]["tokens"] = json!(3000);
    root.annotations_mut()
        .insert(TEAM_ROLES.into(), roles.to_string());
    seal_team_test_run(&mut root);
    let s = State {
        schema: "kars.reviewed-run/v1".into(),
        root_name: root.name_any(),
        root_uid: root.uid().unwrap(),
        admission: admission(&root).unwrap(),
        plan_digest: plan_digest(plan(&root).unwrap()).unwrap(),
        plan: plan(&root).unwrap().clone(),
        team_roles: ann(&root, TEAM_ROLES).map(str::to_owned),
        team_memory: None,
        team_revision: None,
        children: BTreeMap::new(),
    };
    let mut map = ConfigMap::default();
    map.metadata.name = Some("kars-reviewed-0123456789012345678901234567890123456789".into());
    map.metadata.uid = Some("state-uid".into());
    let role = child(&root, &root, &s, &map, 0, None).unwrap();
    assert_eq!(role.spec.envelope.budget.unwrap().tokens, Some(3000));
}

async fn fixture() -> (MockServer, Client, Memory) {
    fixture_with(root()).await
}
async fn fixture_with(root: KarsTask) -> (MockServer, Client, Memory) {
    let server = MockServer::start().await;
    let memory = Memory::default();
    memory.put(object_path(&root), &root);
    Mock::given(wiremock::matchers::path_regex(".*"))
        .respond_with(memory.clone())
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, memory)
}
async fn tick(client: &Client, memory: &Memory) -> Result<bool> {
    reconcile(client, &memory.task("report")).await
}
async fn phase_fixture() -> (MockServer, Client, Memory, KarsTask) {
    phase_fixture_with(root()).await
}
async fn phase_fixture_with(root: KarsTask) -> (MockServer, Client, Memory, KarsTask) {
    let (server, client, memory) = fixture_with(root).await;
    assert!(!tick(&client, &memory).await.unwrap());
    assert!(!tick(&client, &memory).await.unwrap());
    let root = memory.task("report");
    let (map, s) = load(&client, &root).await.unwrap();
    let role = memory.ready(&s.children["r0"].name);
    assert!(!tick(&client, &memory).await.unwrap());
    let (_, s) = load(&client, &root).await.unwrap();
    let phase = memory.task(&s.children["r0-p0"].name);
    assert!(!launched(&role));
    assert!(!launched(&phase));
    assert_eq!(
        phase.spec.parent_ref.as_ref().unwrap().name,
        role.name_any()
    );
    assert_eq!(
        phase.spec.envelope.budget.as_ref().unwrap().tokens,
        Some(5000)
    );
    assert_eq!(s.children["r0-p0"].spec["execution"]["launch"], true);
    assert!(!tick(&client, &memory).await.unwrap());
    let phase = memory.ready(&phase.name_any());
    assert!(launched(&phase));
    assert_eq!(map.uid(), load(&client, &root).await.unwrap().0.uid());
    (server, client, memory, phase)
}
#[tokio::test]
async fn native_single_agent_reserves_only_root_and_grants_explicit_principal_authority() {
    for mut task in [root(), hermes_root()] {
        let plan = task
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        plan.roles.clear();
        plan.synthesis.execution_mode = Some("native".into());
        plan.synthesis.capabilities = vec!["filesystem-write".into(), "network".into()];
        task.spec.envelope.delegation_depth = 0;
        ready(&mut task);
        let (_server, client, memory) = fixture_with(task).await;
        assert!(!tick(&client, &memory).await.unwrap());
        assert!(tick(&client, &memory).await.unwrap());
        let root = memory.task("report");
        let (_, state) = load(&client, &root).await.unwrap();
        assert!(state.children.is_empty());
        let admitted = grant(&client, &root).await.unwrap().unwrap();
        assert_eq!(admitted.inputs, json!([]));
        assert_eq!(
            admitted.contract["reviewedPhase"]["executionMode"],
            "native"
        );
        assert_eq!(
            admitted.contract["reviewedPhase"]["capabilities"],
            json!(["filesystem-write", "network"])
        );
        assert_eq!(admitted.contract["requiredOutputs"], json!(["report.md"]));
        let tasks = memory
            .0
            .lock()
            .unwrap()
            .values()
            .filter(|v| v["kind"] == "KarsTask")
            .count();
        assert_eq!(tasks, 1);
    }
}

#[tokio::test]
async fn creates_inactive_reserved_children_then_admits_exact_phase() {
    let (_server, client, memory, phase) = phase_fixture().await;
    assert!(crate::kars_task_reconciler::task_is_ready(&phase));
    let admitted = grant(&client, &phase).await.unwrap().unwrap();
    assert_eq!(admitted.contract["version"], 5);
    assert_eq!(admitted.contract["peerContext"]["slot"], "r0-p0");
    assert_eq!(
        admitted.contract["peerContext"]["peers"][0]["agentName"],
        phase.name_any()
    );
    assert_eq!(admitted.contract["requiredOutputs"], json!([HANDOFF]));
    assert_eq!(admitted.inputs, json!([]));
    assert!(!tick(&client, &memory).await.unwrap());
}

#[tokio::test]
async fn verified_peer_context_is_required_by_completion_without_changing_legacy_runs() {
    let (_server, client, memory, phase) = phase_fixture().await;
    let (map, s) = load(&client, &memory.task("report")).await.unwrap();
    let peers = peer_context(&s, &map, "r0-p0").unwrap();
    let (_, record) = complete(&client, &memory, &phase).await;
    let reviewed = &s.plan.roles[0].phases[0];
    evidence::success(&record, &phase, reviewed, Some(&peers)).unwrap();
    assert!(evidence::success(&record, &phase, reviewed, None).is_err());
    for part in ["candidate", "assignment", "reply"] {
        for change in ["omit", "root", "role", "slot"] {
            let mut altered = record.clone();
            match change {
                "omit" => {
                    altered[part].as_object_mut().unwrap().remove("peerContext");
                }
                "root" => altered[part]["peerContext"]["rootUid"] = "other-root".into(),
                "role" => altered[part]["peerContext"]["peers"][0]["role"] = "other-role".into(),
                _ => altered[part]["peerContext"]["slot"] = "synthesis".into(),
            }
            assert!(
                evidence::success(&altered, &phase, reviewed, Some(&peers)).is_err(),
                "{part}/{change}"
            );
        }
    }
    let mut legacy = s.clone();
    legacy.schema = "kars.reviewed-run/v1".into();
    assert!(peer_context(&legacy, &map, "r0-p0").is_none());
    assert_eq!(
        peer_contract(reviewed, &[], vec![HANDOFF.into()], None).unwrap()["version"],
        4
    );
}

#[tokio::test]
async fn complete_revision_context_uses_bound_objectives_above_the_task_schema_limit() {
    for initial in [root(), hermes_root()] {
        for length in [4096, 4097, 24000] {
            let mut initial = initial.clone();
            let plan = plan(&initial).unwrap();
            let suffix = format!(
                "\n\nRole: {}\n\nPhase: {}",
                plan.roles[0].objective, plan.roles[0].phases[0].objective
            );
            let context = "é".repeat(length - suffix.chars().count());
            let annotations = initial.metadata.annotations.as_mut().unwrap();
            annotations.insert(RUN.into(), "rev-complete-files".into());
            annotations.insert(OBJECTIVE_NONCE.into(), "rev-complete-files".into());
            annotations.insert(OBJECTIVE_B64.into(), STANDARD.encode(&context));
            annotations.insert(OBJECTIVE_DIGEST.into(), digest(&context));
            let (_server, client, memory, phase) = phase_fixture_with(initial).await;
            assert!(phase.spec.objective.chars().count() <= 4096);
            assert_eq!(
                crate::mission_delivery::admission::objective(&phase, ann(&phase, RUN).unwrap())
                    .unwrap(),
                format!("{context}{suffix}")
            );
            assert!(grant(&client, &phase).await.unwrap().is_some());
            assert_eq!(ann(&phase, OBJECTIVE_NONCE).is_some(), length > 4096);
            for key in [OBJECTIVE_NONCE, OBJECTIVE_B64, OBJECTIVE_DIGEST] {
                let mut altered = phase.clone();
                altered
                    .metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(key.into(), "changed".into());
                memory.put(object_path(&altered), &altered);
                assert!(grant(&client, &altered).await.is_err(), "{key}");
            }
        }
    }
}

#[tokio::test]
async fn restores_missing_reviewed_user_owners_without_replacing_reserved_work() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        let before_spec = serde_json::to_value(&phase.spec).unwrap();
        let mut root = memory.task("report");
        root.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/owner-sub".into(), "customer".into());
        memory.put(object_path(&root), &root);
        let state_before = load(&client, &root).await.unwrap().0;
        for _ in 0..3 {
            assert!(!tick(&client, &memory).await.unwrap());
        }
        let restored = memory.task(&phase.name_any());
        let role = memory.task(&restored.spec.parent_ref.as_ref().unwrap().name);
        assert_eq!(ann(&role, "kars.azure.com/owner-sub"), Some("customer"));
        assert_eq!(ann(&restored, "kars.azure.com/owner-sub"), Some("customer"));
        assert_eq!(restored.metadata.uid, phase.metadata.uid);
        assert_eq!(serde_json::to_value(&restored.spec).unwrap(), before_spec);
        assert_eq!(
            load(&client, &memory.task("report")).await.unwrap().0,
            state_before
        );
        assert!(grant(&client, &restored).await.unwrap().is_some());

        let mut foreign = restored.clone();
        foreign
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/owner-sub".into(), "another-customer".into());
        memory.put(object_path(&foreign), &foreign);
        assert!(tick(&client, &memory).await.is_err());
        assert!(grant(&client, &foreign).await.is_err());
        assert_eq!(
            ann(
                &memory.task(&foreign.name_any()),
                "kars.azure.com/owner-sub"
            ),
            Some("another-customer")
        );
    }
}

#[tokio::test]
async fn new_reviewed_children_inherit_the_user_owner_at_creation() {
    let mut initial = root();
    initial
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("kars.azure.com/owner-sub".into(), "customer".into());
    let (_server, _client, memory, phase) = phase_fixture_with(initial).await;
    assert_eq!(ann(&phase, "kars.azure.com/owner-sub"), Some("customer"));
    let role = memory.task(&phase.spec.parent_ref.as_ref().unwrap().name);
    assert_eq!(ann(&role, "kars.azure.com/owner-sub"), Some("customer"));
}
#[tokio::test]
async fn restart_never_recreates_missing_reserved_child_or_state() {
    let (_server, client, memory, phase) = phase_fixture().await;
    memory.0.lock().unwrap().remove(&object_path(&phase));
    assert!(tick(&client, &memory).await.is_err());
    assert!(!memory.0.lock().unwrap().contains_key(&object_path(&phase)));
    let pin: Pin = serde_json::from_str(ann(&memory.task("report"), PIN).unwrap()).unwrap();
    memory.0.lock().unwrap().remove(&map_path(&pin.name));
    assert!(tick(&client, &memory).await.is_err());
    assert!(!memory.0.lock().unwrap().contains_key(&map_path(&pin.name)));
}
#[tokio::test]
async fn rejects_reserved_uid_and_runtime_replacement() {
    let (_server, client, memory, phase) = phase_fixture().await;
    let mut changed = phase.clone();
    changed.metadata.uid = Some("replacement".into());
    memory.put(object_path(&changed), &changed);
    assert!(grant(&client, &changed).await.is_err());
    changed = phase;
    changed.spec.execution.as_mut().unwrap().runtime = Some("Hermes".into());
    memory.put(object_path(&changed), &changed);
    assert!(grant(&client, &changed).await.is_err());
}
#[tokio::test]
async fn revision_invalidates_old_phase_without_replay() {
    let (_server, client, memory, phase) = phase_fixture().await;
    let mut root = memory.task("report");
    root.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(RUN.into(), "run-2".into());
    root.metadata.resource_version = Some("100".into());
    memory.put(object_path(&root), &root);
    assert!(grant(&client, &phase).await.is_err());
    assert!(!tick(&client, &memory).await.unwrap());
    assert!(grant(&client, &phase).await.is_err());
    assert_eq!(memory.task(&phase.name_any()).uid(), phase.uid());
}
fn evidence_map(task: &KarsTask, name: &str, data: BTreeMap<String, String>) -> ConfigMap {
    let mut owner = task.controller_owner_ref(&()).unwrap();
    owner.block_owner_deletion = None;
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: task.namespace(),
            uid: Some(format!("uid-{name}")),
            resource_version: Some("1".into()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}
async fn complete(client: &Client, memory: &Memory, task: &KarsTask) -> (String, Value) {
    complete_with_tool_count(client, memory, task, 1).await
}

async fn complete_with_tool_count(
    client: &Client,
    memory: &Memory,
    task: &KarsTask,
    tool_calls: u64,
) -> (String, Value) {
    complete_with_artifacts(client, memory, task, tool_calls, BTreeMap::new()).await
}

async fn complete_with_artifacts(
    client: &Client,
    memory: &Memory,
    task: &KarsTask,
    tool_calls: u64,
    extra_artifacts: BTreeMap<String, String>,
) -> (String, Value) {
    let g = grant(client, task).await.unwrap().unwrap();
    let content =
        crate::mission_delivery::admission::objective(task, ann(task, RUN).unwrap()).unwrap();
    let mut candidate = json!({"namespace":"default","taskName":task.name_any(),"taskUid":task.uid(),"runNonce":ann(task,RUN),
        "sandboxUid":"sandbox-uid","podUid":"pod-uid","agentDid":format!("did:mesh:{}","a".repeat(32)),
        "dispatcherDid":format!("did:mesh:{}","b".repeat(32)),"agentName":task.spec.display_name.clone().unwrap_or_else(|| task.name_any()),"content":content,
        "reviewedPhase":g.contract["reviewedPhase"],"requiredOutputs":g.contract["requiredOutputs"]});
    if let Some(peers) = g.contract.get("peerContext") {
        candidate["peerContext"] = peers.clone();
    }
    if !g.inputs.as_array().unwrap().is_empty() {
        let root = ann(task, ROOT)
            .map(|name| memory.task(name))
            .unwrap_or_else(|| task.clone());
        let memories = retained_base_inputs(client, &root).await.unwrap();
        let mut retained = g.inputs.as_array().unwrap().clone();
        for input in &mut retained {
            input["content"] = if let Some(document) = memories
                .iter()
                .find(|document| input["name"] == document.name)
            {
                document.content.clone().into()
            } else {
                let source = &input["source"];
                let key = digest(
                    serde_json::to_vec(&json!([source["taskUid"], source["runNonce"]])).unwrap(),
                );
                let path = map_path(&format!("kars-mission-artifacts-{}", &key[7..47]));
                memory.0.lock().unwrap()[&path]["data"][source["artifactName"].as_str().unwrap()]
                    .clone()
            };
        }
        candidate["inputArtifacts"] = retained.into();
    }
    let runtime = crate::kars_task::blueprint::effective_runtime_name(&task.spec);
    if runtime != "OpenClaw" {
        candidate["runtime"] = runtime.into();
    }
    let mut assignment = candidate.clone();
    for key in ["namespace", "agentName", "runtime"] {
        assignment.as_object_mut().unwrap().remove(key);
    }
    merge(&mut assignment, &g.contract);
    merge(
        &mut assignment,
        &json!({"type":"mission:assign","assignmentId":"assignment-1","bootId":"boot-1","artifactFormat":"text-v1"}),
    );
    if runtime == "OpenClaw" {
        assignment["activityFormat"] = "router-observations-v1".into();
    }
    let evidence = json!({"model":"fixture-model","rounds":1,"usage":{"promptTokens":20,"completionTokens":30,"totalTokens":50},
        "phase":{"name":g.contract["reviewedPhase"]["name"],"minToolCalls":g.contract["reviewedPhase"]["minToolCalls"],
            "maxToolCalls":g.contract["reviewedPhase"]["maxToolCalls"],"attemptedToolCalls":tool_calls,"successfulToolCalls":tool_calls}});
    let mut reply = assignment.clone();
    for key in ["inputArtifacts", "content"] {
        reply.as_object_mut().unwrap().remove(key);
    }
    let mut artifacts: serde_json::Map<String, Value> = g.contract["requiredOutputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| {
            (
                name.as_str().unwrap().into(),
                "Reviewed handoff — 日本語".into(),
            )
        })
        .collect();
    artifacts.extend(
        extra_artifacts
            .into_iter()
            .map(|(name, content)| (name, content.into())),
    );
    merge(
        &mut reply,
        &json!({"type":"mission:reply","status":"succeeded","output":"Completed measured work","artifacts":artifacts,"evidence":evidence}),
    );
    let stamp = "2026-10-02T20:00:00.000Z";
    let record = json!({"version":1,"phase":"succeeded","ownerSession":"dispatcher-1","candidate":candidate,"assignment":assignment,
        "contentDigest":digest(&content),"startedAt":stamp,"updatedAt":stamp,"reply":reply,"events":[{"at":stamp,"status":"succeeded","evidence":evidence}]});
    let key = digest(serde_json::to_vec(&json!([task.uid(), ann(task, RUN)])).unwrap())[7..47]
        .to_string();
    let attempt = evidence_map(
        task,
        &format!("kars-mission-attempt-{key}"),
        BTreeMap::from([("attempt.json".into(), record.to_string())]),
    );
    memory.put(map_path(&attempt.name_any()), &attempt);
    let mut files: BTreeMap<String, String> =
        serde_json::from_value(reply["artifacts"].clone()).unwrap();
    files.insert(
        "response.md".into(),
        reply["output"].as_str().unwrap().into(),
    );
    let mut output = BTreeMap::new();
    for field in [
        "taskName",
        "taskUid",
        "agentName",
        "agentDid",
        "dispatcherDid",
        "sandboxUid",
        "podUid",
    ] {
        output.insert(field.into(), candidate[field].as_str().unwrap().into());
    }
    for (field, value) in [
        ("assignmentNonce", ann(task, RUN).unwrap()),
        ("assignmentId", "assignment-1"),
        ("runtimeBootId", "boot-1"),
        ("output", "Completed measured work"),
        ("startedAt", stamp),
        ("finishedAt", stamp),
        ("model", "fixture-model"),
        ("totalTokens", "50"),
        ("usageKnown", "true"),
        ("status", "ok"),
        ("error", ""),
    ] {
        output.insert(field.into(), value.into());
    }
    output.insert("artifactCount".into(), files.len().to_string());
    output.insert("evidence.json".into(), reply.to_string());
    for (kind, data) in [("output", output), ("artifacts", files)] {
        let mut map = evidence_map(task, &format!("kars-mission-{kind}-{key}"), data);
        map.immutable = Some(true);
        map.metadata.labels = Some(BTreeMap::from([(
            format!("kars.azure.com/mission-{kind}"),
            key.clone(),
        )]));
        map.metadata.annotations = Some(
            [
                ("evidence-key", key.clone()),
                ("evidence-role", "canonical".into()),
                ("principal-name", task.name_any()),
                ("task-uid", task.uid().unwrap()),
                ("run-nonce", ann(task, RUN).unwrap().into()),
            ]
            .into_iter()
            .map(|(k, v)| (format!("kars.azure.com/mission-{k}"), v))
            .collect(),
        );
        memory.put(map_path(&map.name_any()), &map);
    }
    let nonce = ann(task, RUN).unwrap().to_string();
    let mut task = task.clone();
    task.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(COMPLETE.into(), nonce);
    memory.put(object_path(&task), &task);
    (key, record)
}
#[tokio::test]
async fn verified_phase_compute_can_retire_without_losing_synthesis_inputs_or_task_authority() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        assert!(!phase_compute_retirable(&client, &phase).await.unwrap());
        complete(&client, &memory, &phase).await;
        let completed = memory.task(&phase.name_any());
        let before = memory.0.lock().unwrap().clone();
        assert!(phase_compute_retirable(&client, &completed).await.unwrap());
        assert_eq!(*memory.0.lock().unwrap(), before);

        let mut retired = completed.clone();
        let status = retired.status.as_mut().unwrap();
        status.execution_phase = Some("Idle".into());
        status.sandbox_ref = None;
        memory.put(object_path(&retired), &retired);
        assert_eq!(
            serde_json::to_value(&retired.spec).unwrap(),
            serde_json::to_value(&completed.spec).unwrap()
        );
        assert!(launched(&retired));
        assert!(phase_compute_retirable(&client, &retired).await.unwrap());
        assert!(tick(&client, &memory).await.unwrap());
        let synthesis = grant(&client, &memory.task("report"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            synthesis.inputs[0]["source"]["taskUid"],
            phase.uid().unwrap()
        );
        assert_eq!(
            synthesis.inputs[0]["sha256"],
            digest("Reviewed handoff — 日本語")
        );
    }
}

#[tokio::test]
async fn stopped_root_still_retires_only_its_owned_phase_compute() {
    let (_server, client, memory, phase) = phase_fixture().await;
    let mut root = memory.task("report");
    root.spec.execution.as_mut().unwrap().launch = false;
    memory.put(object_path(&root), &root);
    assert!(phase_compute_retirable(&client, &phase).await.unwrap());
    assert!(!phase_compute_retirable(&client, &root).await.unwrap());
    let mut foreign = phase;
    foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "foreign-parent".into();
    memory.put(object_path(&foreign), &foreign);
    assert!(phase_compute_retirable(&client, &foreign).await.is_err());
}

#[tokio::test]
async fn immutable_success_unlocks_synthesis_with_typed_required_outputs() {
    let (_server, client, memory, phase) = phase_fixture().await;
    assert!(grant(&client, &memory.task("report")).await.is_err());
    complete(&client, &memory, &phase).await;
    assert!(tick(&client, &memory).await.unwrap());
    let g = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(g.contract["requiredOutputs"], json!(["report.md"]));
    assert_eq!(g.inputs[0]["name"], "r0-p0.md");
    assert_eq!(g.inputs[0]["source"]["taskUid"], phase.uid().unwrap());
    assert_eq!(g.inputs[0]["sha256"], digest("Reviewed handoff — 日本語"));
    assert!(g.inputs[0].get("content").is_none());
    assert!(g.contract.get("inputDigest").is_some());
    // A restarted reconciler reuses success, without creating or activating another Task.
    let before = memory.0.lock().unwrap().clone();
    assert!(tick(&client, &memory).await.unwrap());
    assert_eq!(*memory.0.lock().unwrap(), before);
}

#[tokio::test]
async fn uncertain_principal_holds_completed_roles_without_replay_or_evidence_changes() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        complete(&client, &memory, &phase).await;
        assert!(tick(&client, &memory).await.unwrap());
        let principal = memory.task("report");
        let binding = json!({"namespace": principal.namespace(), "taskName": principal.name_any(),
            "taskUid": principal.uid(), "runNonce": ann(&principal, RUN)});
        let key =
            digest(serde_json::to_vec(&json!([principal.uid(), ann(&principal, RUN)])).unwrap());
        let name = format!("kars-mission-attempt-{}", &key[7..47]);
        for state in [
            "dispatching",
            "accepted",
            "running",
            "succeeded",
            "failed",
            "rejected",
            "uncertain",
        ] {
            let record = json!({"version": 1, "phase": state, "candidate": binding,
                "assignment": binding, "reply": {"status": "running"}});
            memory.put(
                map_path(&name),
                &evidence_map(
                    &principal,
                    &name,
                    BTreeMap::from([("attempt.json".into(), record.to_string())]),
                ),
            );
            let before = memory.0.lock().unwrap().clone();
            if state == "uncertain" {
                let error = tick(&client, &memory).await.unwrap_err().to_string();
                assert!(error.contains("Final assembly needs attention"), "{error}");
                let mut status = principal.status.clone().unwrap();
                crate::kars_task_reconciler::reconcile_execution(
                    &client,
                    "default",
                    &principal,
                    &mut status,
                    false,
                )
                .await;
                assert_eq!(status.execution_phase.as_deref(), Some("Degraded"));
                assert!(
                    status
                        .execution_detail
                        .as_deref()
                        .unwrap()
                        .starts_with("reviewed execution held: Final assembly needs attention")
                );
            } else {
                assert!(tick(&client, &memory).await.unwrap(), "{state}");
            }
            assert_eq!(*memory.0.lock().unwrap(), before);
        }
    }
}

#[tokio::test]
async fn principal_attempt_progress_requires_current_identity_and_custody() {
    let (_server, client, memory, phase) = phase_fixture().await;
    complete(&client, &memory, &phase).await;
    assert!(tick(&client, &memory).await.unwrap());
    let principal = memory.task("report");
    let binding = json!({"namespace": principal.namespace(), "taskName": principal.name_any(),
        "taskUid": principal.uid(), "runNonce": ann(&principal, RUN)});
    let record =
        json!({"version": 1, "phase": "running", "candidate": binding, "assignment": binding});
    let key = digest(serde_json::to_vec(&json!([principal.uid(), ann(&principal, RUN)])).unwrap());
    let name = format!("kars-mission-attempt-{}", &key[7..47]);
    for pointer in [
        "/candidate/taskUid",
        "/candidate/taskName",
        "/candidate/runNonce",
        "/candidate/namespace",
        "/assignment/taskUid",
        "/assignment/taskName",
        "/assignment/runNonce",
        "/version",
        "/phase",
    ] {
        let mut changed = record.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("replaced");
        memory.put(
            map_path(&name),
            &evidence_map(
                &principal,
                &name,
                BTreeMap::from([("attempt.json".into(), changed.to_string())]),
            ),
        );
        assert!(tick(&client, &memory).await.is_err(), "{pointer}");
    }
    let map = serde_json::to_value(evidence_map(
        &principal,
        &name,
        BTreeMap::from([("attempt.json".into(), record.to_string())]),
    ))
    .unwrap();
    for pointer in [
        "/metadata/name",
        "/metadata/namespace",
        "/metadata/ownerReferences/0/uid",
        "/metadata/uid",
        "/metadata/resourceVersion",
    ] {
        let mut changed = map.clone();
        *changed.pointer_mut(pointer).unwrap() = Value::Null;
        memory.put(map_path(&name), &changed);
        assert!(tick(&client, &memory).await.is_err(), "{pointer}");
    }
}

#[tokio::test]
async fn completed_root_cannot_reserve_replayed_phases_after_stop_and_relaunch() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        complete(&client, &memory, &phase).await;
        assert!(tick(&client, &memory).await.unwrap());
        let finished = memory.task("report");
        complete(&client, &memory, &finished).await;
        let before = memory.0.lock().unwrap().clone();
        assert!(tick(&client, &memory).await.unwrap());
        assert_eq!(*memory.0.lock().unwrap(), before);

        let mut restored = memory.task("report");
        restored.metadata.generation = Some(restored.metadata.generation.unwrap() + 2);
        memory.put(object_path(&restored), &restored);
        let before = memory.0.lock().unwrap().clone();
        assert!(!tick(&client, &memory).await.unwrap());
        assert_eq!(*memory.0.lock().unwrap(), before);

        ready(&mut restored);
        assert!(crate::kars_task_reconciler::task_is_ready(&restored));
        memory.put(object_path(&restored), &restored);
        let before = memory.0.lock().unwrap().clone();
        assert!(
            tick(&client, &memory)
                .await
                .unwrap_err()
                .to_string()
                .contains("CompletedReviewedRun")
        );
        assert_eq!(*memory.0.lock().unwrap(), before);

        restored
            .annotations_mut()
            .insert(RUN.into(), "explicit-next-revision".into());
        memory.put(object_path(&restored), &restored);
        assert!(!tick(&client, &memory).await.unwrap());
        assert_ne!(ann(&memory.task("report"), PIN), ann(&restored, PIN));
    }
}

fn revision_document() -> Input {
    serde_json::from_value(json!({"name":"revision-0","source":{
        "namespace":"default","taskName":"prior-run","taskUid":"prior-uid",
        "sandboxUid":"prior-sandbox","podUid":"prior-pod","runNonce":"prior-nonce",
        "assignmentId":"prior-assignment","agentDid":format!("did:mesh:{}", "d".repeat(32)),
        "artifactName":"report.md"},"sha256":digest("Retained findings"),"content":"Retained findings"})).unwrap()
}

#[test]
fn retained_inputs_require_reviewed_read_capability_before_projection() {
    let fixture: Value =
        serde_json::from_str(include_str!("phase-projection-fixture.json")).unwrap();
    let mut phase: ExecutionPhase =
        serde_json::from_value(fixture["reviewedPhase"].clone()).unwrap();
    let inputs = vec![revision_document()];
    for mode in [None, Some("native".to_owned())] {
        phase.execution_mode = mode;
        for (capabilities, allowance, accepted) in [
            (vec!["filesystem-write"], 6, false),
            (vec!["filesystem-read", "filesystem-write"], 0, false),
            (vec!["filesystem-read", "filesystem-write"], 1, true),
        ] {
            phase.capabilities = capabilities.into_iter().map(str::to_owned).collect();
            phase.max_tool_calls = allowance;
            let result = contract(&phase, &inputs, vec![HANDOFF.into()]);
            assert_eq!(result.is_ok(), accepted);
            if let Err(error) = result {
                assert!(
                    error
                        .to_string()
                        .contains("ReviewedInputCapabilityRequired")
                );
                assert!(error.to_string().contains(&phase.name));
            }
            assert!(contract(&phase, &[], vec![HANDOFF.into()]).is_ok());
        }
    }
}

#[test]
fn staged_revision_input_count_preserves_manifest_validation_and_memory() {
    for runtime in ["OpenClaw", "Hermes"] {
        let mut root = revision_package_root(runtime);
        let documents = revision_package_documents();
        let plan = root
            .spec
            .blueprint
            .as_ref()
            .unwrap()
            .execution_plan
            .as_ref()
            .unwrap();
        let roles = revision_roles(plan);
        seal_team_revision(&mut root, &documents, &roles).unwrap();
        root.annotations_mut()
            .insert("kars.azure.com/team-request-state".into(), "staged".into());
        assert!(
            revision_manifest(&root).is_err(),
            "staging must not permit execution-time input use"
        );
        assert_eq!(staged_team_input_count(&root).unwrap(), documents.len());
        root.annotations_mut().insert(
            TEAM_MEMORY.into(),
            serde_json::to_string(&vec![revision_document()]).unwrap(),
        );
        assert_eq!(staged_team_input_count(&root).unwrap(), documents.len() + 1);
        let manifest: Value = serde_json::from_str(ann(&root, TEAM_REVISION).unwrap()).unwrap();
        for fault in ["schema", "namespace", "routing", "unknown-field"] {
            let mut changed = manifest.clone();
            match fault {
                "schema" => changed["schema"] = json!("unknown"),
                "namespace" => changed["inputs"][0]["source"]["namespace"] = json!("other-tenant"),
                "routing" => changed["diagnosticRoles"][0]["phases"] = json!([]),
                "unknown-field" => changed["unexpected"] = json!(true),
                _ => unreachable!(),
            }
            let mut invalid = root.clone();
            invalid
                .annotations_mut()
                .insert(TEAM_REVISION.into(), changed.to_string());
            assert!(
                staged_team_input_count(&invalid).is_err(),
                "{runtime}: {fault}"
            );
        }
    }
}

#[test]
fn revision_snapshot_checks_content_provenance_and_existing_capacity() {
    let mut root = team_root("OpenClaw");
    let before = crate::kars_team_reconciler::requests::task_digest(&root).unwrap();
    seal_team_revision(&mut root, &[], &[]).unwrap();
    assert_eq!(
        crate::kars_team_reconciler::requests::task_digest(&root).unwrap(),
        before
    );
    let valid = revision_document();
    for mutation in [
        "hash",
        "name",
        "namespace",
        "empty",
        "size",
        "source",
        "capacity",
    ] {
        let mut docs = vec![valid.clone()];
        match mutation {
            "hash" => docs[0].sha256 = digest("Changed"),
            "name" => docs[0].name = "memory-0".into(),
            "namespace" => docs[0].source.namespace = "other-tenant".into(),
            "empty" => {
                docs[0].content.clear();
                docs[0].sha256 = digest("");
            }
            "size" => {
                docs[0].content = "x".repeat(65537);
                docs[0].sha256 = digest(&docs[0].content);
            }
            "source" => {
                let mut second = valid.clone();
                second.name = "revision-1".into();
                second.source.task_uid = "another-run".into();
                docs.push(second);
            }
            "capacity" => {
                docs = (0..3)
                    .map(|index| {
                        let mut doc = valid.clone();
                        doc.name = format!("revision-{index}");
                        doc.content = "x".repeat(65536);
                        doc.sha256 = digest(&doc.content);
                        doc
                    })
                    .collect();
            }
            _ => unreachable!(),
        }
        assert!(
            seal_team_revision(&mut root, &docs, &[]).is_err(),
            "{mutation}"
        );
        assert!(!root.annotations().contains_key(TEAM_REVISION));
    }
    seal_team_revision(&mut root, &[valid], &[]).unwrap();
    assert_ne!(
        crate::kars_team_reconciler::requests::task_digest(&root).unwrap(),
        before
    );
    assert!(
        base_inputs(&root).is_err(),
        "new inputs require a fresh admission seal"
    );
    seal_team_test_run(&mut root);
    assert_eq!(base_inputs(&root).unwrap().len(), 1);
}

#[test]
fn revision_diagnostic_snapshot_keeps_phase_producers_separate_and_sealed() {
    let mut root = team_root("OpenClaw");
    let delivery = revision_document();
    let mut builder = delivery.clone();
    builder.name = "revision-unaccepted-r0-p0--1-package.txt".into();
    builder.source.task_uid = "builder-uid".into();
    builder.source.artifact_name = "package.txt".into();
    let mut qa = builder.clone();
    qa.name = "revision-unaccepted-r1-p0--1-package.txt".into();
    qa.source.task_uid = "qa-uid".into();
    let valid = vec![delivery, builder, qa];
    for mutation in [
        "duplicate",
        "same-phase-producer",
        "hash",
        "reserved-name",
        "path",
        "phase",
        "order",
    ] {
        let mut docs = valid.clone();
        match mutation {
            "duplicate" => docs[2].name = docs[1].name.clone(),
            "same-phase-producer" => {
                docs[2].name = "revision-unaccepted-r0-p0--2-package.txt".into()
            }
            "hash" => docs[2].sha256 = digest("changed"),
            "reserved-name" => docs[2].name = "kars-python-executions.json".into(),
            "path" => docs[2].name = "revision-unaccepted-r1-p0--1-../package.txt".into(),
            "phase" => docs[2].name = "revision-unaccepted-r01-p0--1-package.txt".into(),
            "order" => docs.swap(0, 1),
            _ => unreachable!(),
        }
        assert!(
            seal_team_revision(&mut root, &docs, &[]).is_err(),
            "{mutation}"
        );
        assert!(!root.annotations().contains_key(TEAM_REVISION));
    }
    seal_team_revision(&mut root, &valid, &[]).unwrap();
    assert!(
        revision_references(&root).is_err(),
        "diagnostics require fresh work admission"
    );
    seal_team_test_run(&mut root);
    assert!(
        revision_references(&root).unwrap().unwrap()
            == valid.iter().map(InputReference::from).collect::<Vec<_>>()
    );
    assert_eq!(record_base_inputs(&root, &valid, &[]).unwrap(), valid);
}

fn revision_package_exports() -> Vec<BTreeMap<String, String>> {
    // Exact observed UTF-8 sizes and JSON-escaping overhead, without source bodies.
    let groups: &[&[(&str, usize, usize)]] = &[
        &[
            ("benchmarks.json", 6196, 715),
            ("test_server.py", 14522, 290),
            ("app.js", 5685, 52),
            ("index.html", 3615, 71),
            ("server.py", 45629, 1679),
            ("README.md", 12109, 83),
            ("execution-report.md", 9141, 68),
            ("execution-results.json", 9500, 637),
        ],
        &[
            ("benchmark-evidence.json", 15944, 1534),
            ("benchmark-handoff.md", 8654, 71),
            ("handoff.md", 8654, 71),
        ],
        &[
            ("lifecycle-evidence.json", 10911, 1193),
            ("lifecycle-handoff.md", 6554, 41),
            ("handoff.md", 6554, 41),
        ],
        &[
            ("README.md", 2264, 41),
            ("server.py", 45629, 1679),
            ("test_server.py", 14522, 290),
            ("benchmarks.json", 6196, 715),
            ("index.html", 3615, 71),
            ("app.js", 5685, 52),
            ("handoff.md", 4201, 46),
        ],
        &[
            ("kars-python-executions.json", 11018, 803),
            ("qa_bootstrap.py", 1031, 21),
            ("qa_integration.py", 6672, 92),
            ("qa_live.py", 2080, 35),
            ("execution-results.json", 4430, 331),
            ("execution-report.md", 6387, 40),
            ("handoff.md", 3899, 28),
        ],
    ];
    groups
        .iter()
        .map(|files| {
            files
                .iter()
                .map(|(name, bytes, escaped)| {
                    let content = "\n".repeat(*escaped) + &"x".repeat(*bytes - *escaped);
                    ((*name).into(), content)
                })
                .collect()
        })
        .collect()
}

fn revision_package_root(runtime: &str) -> KarsTask {
    let mut root = team_root_with_roles(runtime, &["builder", "reviewer", "packager", "qa"]);
    let plan = root
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    plan.roles[1].depends_on.clear();
    let builder = plan.roles[2].name.clone();
    plan.roles[3].depends_on = vec![builder];
    for role in &mut plan.roles {
        role.phases[0].execution_mode = Some("native".into());
        role.phases[0].max_tool_calls = 35;
    }
    plan.synthesis.execution_mode = Some("native".into());
    plan.synthesis.max_tool_calls = 35;
    plan.deliverables = revision_package_exports()[0]
        .keys()
        .map(|name| serde_json::from_value(json!({"name":name})).unwrap())
        .collect();
    root
}

fn revision_package_documents() -> Vec<Input> {
    let mut documents = Vec::new();
    for (group, files) in revision_package_exports().into_iter().enumerate() {
        for (index, (name, content)) in files.into_iter().enumerate() {
            let mut document = revision_document();
            document.name = if group == 0 {
                format!("revision-{index}")
            } else if name == HANDOFF {
                format!("revision-unaccepted-r{}-p0.md", group - 1)
            } else {
                format!("revision-unaccepted-r{}-p0--{index}-{name}", group - 1)
            };
            document.source.task_name = format!("retained-producer-{group}");
            document.source.task_uid = format!("producer-{group}-uid");
            document.source.artifact_name = name;
            document.content = content;
            document.sha256 = digest(&document.content);
            documents.push(document);
        }
    }
    documents
}

#[test]
fn revision_manifest_routes_only_matching_current_exports_without_erasing_history() {
    let original = revision_package_root("OpenClaw");
    let documents = revision_package_documents();
    let roles = revision_roles(&plan(&original).unwrap());
    let historical = documents
        .iter()
        .find(|input| input.name == "revision-unaccepted-r0-p0.md")
        .unwrap();
    for case in [
        "matching",
        "reordered",
        "role",
        "phase",
        "filename",
        "legacy",
    ] {
        let mut root = original.clone();
        let mut peer = historical.clone();
        peer.name = "r0-p0.md".into();
        peer.source.task_name = "current-specialist".into();
        peer.source.task_uid = "current-specialist-uid".into();
        peer.content = "Fresh independently reviewed handoff".into();
        peer.sha256 = digest(&peer.content);
        let plan = root
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        match case {
            "reordered" => {
                plan.roles.swap(0, 1);
                peer.name = "r1-p0.md".into();
            }
            "role" => plan.roles[0].name = "different-role".into(),
            "phase" => plan.roles[0].phases[0].name = "different-phase".into(),
            "filename" => peer.source.artifact_name = "different.md".into(),
            _ => (),
        }
        seal_team_revision(
            &mut root,
            &documents,
            if case == "legacy" { &[] } else { &roles },
        )
        .unwrap();
        seal_team_test_run(&mut root);
        let selected = route_revision_inputs(&root, documents.clone(), &[peer.clone()]).unwrap();
        let superseded = matches!(case, "matching" | "reordered");
        assert_eq!(selected.len(), if superseded { 27 } else { 28 }, "{case}");
        assert_eq!(selected.contains(historical), !superseded, "{case}");
        assert_eq!(
            selected
                .iter()
                .filter(|input| !input.name.starts_with("revision-unaccepted-"))
                .count(),
            8
        );
        assert_eq!(revision_references(&root).unwrap().unwrap().len(), 28);
        assert_eq!(
            record_base_inputs(&root, &selected, &[peer]).unwrap(),
            selected
        );
    }
    let diagnostics = documents[8..].to_vec();
    let peers = diagnostics
        .iter()
        .cloned()
        .map(|mut input| {
            input.name = input
                .name
                .strip_prefix("revision-unaccepted-")
                .unwrap()
                .into();
            input.source.task_name = format!("current-{}", input.source.task_name);
            input.source.task_uid = format!("current-{}", input.source.task_uid);
            input
        })
        .collect::<Vec<_>>();
    let mut root = original;
    seal_team_revision(&mut root, &diagnostics, &roles).unwrap();
    seal_team_test_run(&mut root);
    assert!(
        route_revision_inputs(&root, diagnostics, &peers)
            .unwrap()
            .is_empty()
    );
    assert!(
        record_base_inputs(&root, &peers, &peers)
            .unwrap()
            .is_empty()
    );
    assert_eq!(revision_references(&root).unwrap().unwrap().len(), 20);
}

#[test]
fn revision_manifest_keeps_28_documents_with_existing_annotation_and_transport_bounds() {
    let mut root = team_root("OpenClaw");
    let documents = revision_package_documents();
    assert_eq!(documents.len(), 28);
    assert_eq!(
        documents.iter().map(|d| d.content.len()).sum::<usize>(),
        281297
    );
    let bytes = serde_json::to_vec(&documents).unwrap().len();
    assert!(bytes > MAX_INPUT_BYTES && bytes < MAX_ASSIGNMENT_INPUT_BYTES);
    seal_team_revision(&mut root, &documents, &[]).unwrap();
    assert!(ann(&root, TEAM_REVISION).unwrap().len() < MAX_INPUT_BYTES);
    assert!(!ann(&root, TEAM_REVISION).unwrap().contains("\"content\""));
    seal_team_test_run(&mut root);
    assert_eq!(revision_references(&root).unwrap().unwrap().len(), 28);
    assert_eq!(
        record_base_inputs(&root, &documents, &[]).unwrap(),
        documents
    );
    for mutation in [
        "count",
        "file",
        "bytes",
        "escaped-bytes",
        "missing",
        "hash",
        "source",
        "annotation",
        "schema",
        "inline-count",
    ] {
        let mut altered = root.clone();
        let mut inputs = documents.clone();
        match mutation {
            "count" => {
                inputs = (0..=MAX_ASSIGNMENT_INPUTS)
                    .map(|i| {
                        let mut document = documents[8].clone();
                        document.name = format!("revision-unaccepted-r{i}-p0.md");
                        document.content = "x".into();
                        document.sha256 = digest("x");
                        document
                    })
                    .collect();
            }
            "file" => {
                inputs[0].content = "x".repeat(65537);
                inputs[0].sha256 = digest(&inputs[0].content);
            }
            "bytes" | "escaped-bytes" => {
                for document in &mut inputs {
                    document.content = if mutation == "bytes" {
                        "x".repeat(65536)
                    } else {
                        "\"".repeat(8192)
                    };
                    document.sha256 = digest(&document.content);
                }
            }
            "missing" => {
                inputs.pop();
            }
            "hash" => inputs[9].sha256 = digest("different"),
            "source" => inputs[9].source.pod_uid = "replacement".into(),
            "annotation" => {
                let raw = format!(
                    "{}{}",
                    ann(&root, TEAM_REVISION).unwrap(),
                    " ".repeat(MAX_INPUT_BYTES)
                );
                altered.annotations_mut().insert(TEAM_REVISION.into(), raw);
            }
            "schema" => {
                let raw = ann(&root, TEAM_REVISION)
                    .unwrap()
                    .replace(REVISION_MANIFEST_SCHEMA, "unknown");
                altered.annotations_mut().insert(TEAM_REVISION.into(), raw);
            }
            "inline-count" => {
                altered.annotations_mut().insert(
                    TEAM_REVISION.into(),
                    serde_json::to_string(&inputs).unwrap(),
                );
            }
            _ => unreachable!(),
        }
        if ["count", "file", "bytes", "escaped-bytes"].contains(&mutation) {
            assert!(
                seal_team_revision(&mut altered, &inputs, &[]).is_err(),
                "{mutation}"
            );
            assert_eq!(ann(&altered, TEAM_REVISION), ann(&root, TEAM_REVISION));
        } else {
            seal_team_test_run(&mut altered);
            assert!(
                record_base_inputs(&altered, &inputs, &[]).is_err(),
                "{mutation}"
            );
        }
    }
}

fn continuity_inputs(bytes: usize) -> Vec<Input> {
    let mut inputs: Vec<Input> = [29571, 13205, 19474, 43332, 17511, 5728, 4869]
        .into_iter()
        .enumerate()
        .map(|(index, size)| {
            let mut document = revision_document();
            document.name = format!("memory-0-document-{index}.md");
            document.source.artifact_name = format!("retained-{index}.md");
            document.content = "x".repeat(size);
            document.sha256 = digest(&document.content);
            document
        })
        .collect();
    let padding = bytes
        .checked_sub(serde_json::to_vec(&inputs).unwrap().len())
        .unwrap();
    // Spread padding across files so the 64KiB per-file contract stays intact.
    let mut remaining = padding;
    for input in &mut inputs {
        let amount = remaining.min(65536 - input.content.len());
        input.content.push_str(&"x".repeat(amount));
        input.sha256 = digest(&input.content);
        remaining -= amount;
    }
    assert_eq!(remaining, 0);
    assert_eq!(serde_json::to_vec(&inputs).unwrap().len(), bytes);
    inputs
}

#[test]
fn memory_snapshot_preserves_mission_capacity_and_admission_seal() {
    let mut root = team_root("OpenClaw");
    for (size, accepted) in [
        (143430, true),
        (evidence::MAX_BASE_BYTES, true),
        (evidence::MAX_BASE_BYTES + 1, false),
    ] {
        let inputs = continuity_inputs(size);
        root.annotations_mut()
            .insert(TEAM_MEMORY.into(), serde_json::to_string(&inputs).unwrap());
        assert!(
            memory_inputs(&root).is_err(),
            "changed memory requires fresh admission"
        );
        seal_team_test_run(&mut root);
        let result = memory_inputs(&root);
        assert_eq!(result.is_ok(), accepted, "{size}");
        if accepted {
            assert_eq!(result.unwrap(), inputs);
        }
    }
}

#[tokio::test]
async fn full_approved_memory_reaches_phase_and_synthesis_with_new_peer_exports() {
    for runtime in ["OpenClaw", "Hermes"] {
        let mut root = team_root(runtime);
        let inputs = continuity_inputs(143430);
        root.annotations_mut()
            .insert(TEAM_MEMORY.into(), serde_json::to_string(&inputs).unwrap());
        seal_team_test_run(&mut root);
        let (_server, client, memory, phase) = phase_fixture_with(root).await;
        let references = |inputs: &[Input]| {
            inputs
                .iter()
                .map(|input| {
                    let mut reference = serde_json::to_value(input).unwrap();
                    reference.as_object_mut().unwrap().remove("content");
                    reference
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            grant(&client, &phase).await.unwrap().unwrap().inputs,
            json!(references(&inputs))
        );
        complete_with_artifacts(
            &client,
            &memory,
            &phase,
            1,
            BTreeMap::from([
                ("agenda.md".into(), "Decision agenda\n".repeat(800)),
                ("questions.md".into(), "Evidence question\n".repeat(400)),
            ]),
        )
        .await;
        assert!(tick(&client, &memory).await.unwrap());
        let root = memory.task("report");
        let grant = grant(&client, &root).await.unwrap().unwrap();
        let combined = grant.inputs.as_array().unwrap();
        assert_eq!(combined.len(), 10);
        for reference in references(&inputs) {
            assert!(
                combined.contains(&reference),
                "{runtime}: full retained provenance lost"
            );
        }
        let (_, record) = complete(&client, &memory, &root).await;
        let actual: Vec<Input> =
            serde_json::from_value(record["candidate"]["inputArtifacts"].clone()).unwrap();
        for input in inputs {
            assert!(actual.contains(&input), "{runtime}: retained bytes changed");
        }
        assert!(serde_json::to_vec(&actual).unwrap().len() < evidence::MAX_BASE_BYTES);
        assert!(memory.task("report").annotations().contains_key(COMPLETE));
    }
}

#[tokio::test]
async fn revision_snapshot_cannot_change_after_phase_admission_even_if_root_resealed() {
    let mut root = team_root("OpenClaw");
    seal_team_revision(&mut root, &[revision_document()], &[]).unwrap();
    seal_team_test_run(&mut root);
    let (_server, client, memory, phase) = phase_fixture_with(root).await;
    let mut root = memory.task("report");
    let mut replacement = revision_document();
    replacement.content = "Changed retained findings".into();
    replacement.sha256 = digest(&replacement.content);
    seal_team_revision(&mut root, &[replacement], &[]).unwrap();
    seal_team_test_run(&mut root);
    memory.put(object_path(&root), &root);
    assert!(grant(&client, &phase).await.is_err());
}

#[tokio::test]
async fn completed_team_documents_survive_retirement_and_task_gc_with_full_peer_provenance() {
    for (runtime, input_kind) in ["OpenClaw", "Hermes"]
        .into_iter()
        .flat_map(|runtime| [TEAM_MEMORY, TEAM_REVISION].map(|kind| (runtime, kind)))
    {
        let mut initial = team_root(runtime);
        let prior: Input = serde_json::from_value(json!({
            "name":if input_kind == TEAM_REVISION { "revision-0" } else { "memory-0-document-0.md" },
            "source":{"namespace":"default","taskName":"previous-run","taskUid":"previous-uid",
                "sandboxUid":"previous-sandbox","podUid":"previous-pod","runNonce":"previous-nonce",
                "assignmentId":"previous-assignment","agentDid":format!("did:mesh:{}","c".repeat(32)),
                "artifactName":"decision-log.md"},
            "sha256":digest("Keep the complete clinic decision, including 日本語."),
            "content":"Keep the complete clinic decision, including 日本語."
        })).unwrap();
        initial.annotations_mut().insert(
            input_kind.into(),
            serde_json::to_string(&vec![prior.clone()]).unwrap(),
        );
        seal_team_test_run(&mut initial);
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        let mut reference = serde_json::to_value(&prior).unwrap();
        reference.as_object_mut().unwrap().remove("content");
        assert_eq!(
            grant(&client, &phase).await.unwrap().unwrap().inputs[0],
            reference
        );
        complete_with_artifacts(
            &client,
            &memory,
            &phase,
            1,
            BTreeMap::from([
                (
                    "capacity-plan.md".into(),
                    "Reviewed actual draft — 日本語".into(),
                ),
                (
                    "station-plan.csv".into(),
                    "station,jobs,notes\n1,14,\"quoted, note\"\n".into(),
                ),
                ("kars-router-observations.json".into(), "{}".into()),
            ]),
        )
        .await;
        assert!(tick(&client, &memory).await.unwrap());
        let root = memory.task("report");
        let inputs = grant(&client, &root).await.unwrap().unwrap().inputs;
        assert_eq!(inputs.as_array().unwrap().len(), 4);
        assert!(
            inputs
                .as_array()
                .unwrap()
                .iter()
                .any(|input| input["name"] == "r0-p0--2-station-plan.csv"
                    && input["source"]["taskUid"] == phase.uid().unwrap())
        );
        complete(&client, &memory, &root).await;
        let mut root = memory.task("report");
        let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
        team.metadata.namespace = Some("default".into());
        team.metadata.uid = Some("clinic-uid".into());
        let archive = retained_team_run(&client, &team, &root)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(archive.documents[0].name, "report.md");
        assert_eq!(archive.documents[0].content, "Reviewed handoff — 日本語");
        assert_eq!(archive.handoffs[0].source.task_uid, phase.uid().unwrap());
        assert_eq!(archive.handoffs[0].source.artifact_name, "handoff.md");

        root.spec.execution.as_mut().unwrap().launch = false;
        root.metadata.generation = Some(root.metadata.generation.unwrap() + 1);
        memory.put(object_path(&root), &root);
        assert!(grant(&client, &root).await.is_err());
        assert_eq!(
            retained_team_run(&client, &team, &root).await.unwrap(),
            Some(archive.clone())
        );
        let mut changed = root.clone();
        changed.spec.objective.push_str(" changed after admission");
        memory.put(object_path(&changed), &changed);
        assert!(retained_team_run(&client, &team, &changed).await.is_err());
        memory.put(object_path(&root), &root);
        let mut wrong_team = team.clone();
        wrong_team.metadata.uid = Some("replacement-team".into());
        assert!(
            retained_team_run(&client, &wrong_team, &root)
                .await
                .is_err()
        );
        for block_owner_deletion in [None, Some(false)] {
            let mut changed = root.clone();
            changed.metadata.owner_references.as_mut().unwrap()[0].block_owner_deletion =
                block_owner_deletion;
            memory.put(object_path(&changed), &changed);
            assert!(retained_team_run(&client, &team, &changed).await.is_err());
        }
        memory.put(object_path(&root), &root);

        crate::team_commons::ensure_commons(&client, &team)
            .await
            .unwrap();
        crate::team_commons::record_run(
            &client,
            &team,
            "run-one",
            "Clinic decision",
            "report",
            "report",
            &archive.output,
            Some(&archive),
        )
        .await
        .unwrap();
        let commons = crate::team_commons::commons_cm_name(&team.commons_name());
        memory
            .0
            .lock()
            .unwrap()
            .retain(|key, _| key == &map_path(&commons));
        assert_eq!(
            crate::team_commons::retained_archive(&client, &team, "run-one")
                .await
                .unwrap(),
            Some(archive.clone())
        );
        let remembered = crate::team_commons::prior_documents(&client, &team)
            .await
            .unwrap();
        assert_eq!(remembered.len(), 2);
        assert_eq!(remembered[0].content, archive.documents[0].content);
        assert_eq!(remembered[1].content, archive.handoffs[0].content);
        assert_eq!(remembered[1].source, archive.handoffs[0].source);
    }
}
#[tokio::test]
async fn execution_only_hermes_survives_role_phase_reservations_and_synthesis() {
    let (_server, client, memory, phase) = phase_fixture_with(hermes_root()).await;
    let root = memory.task("report");
    let (_, state) = load(&client, &root).await.unwrap();
    for slot in ["r0", "r0-p0"] {
        let child = memory.task(&state.children[slot].name);
        assert_eq!(
            child.spec.execution.as_ref().unwrap().runtime.as_deref(),
            Some("Hermes")
        );
        assert!(child.spec.blueprint.as_ref().unwrap().runtime.is_none());
        assert_eq!(state.children[slot].spec["execution"]["runtime"], "Hermes");
        assert_eq!(
            crate::kars_task::blueprint::effective_runtime_name(&child.spec),
            "Hermes"
        );
    }
    let (_, record) = complete(&client, &memory, &phase).await;
    assert_eq!(record["candidate"]["runtime"], "Hermes");
    for field in ["assignment", "reply"] {
        assert!(record[field].get("activityFormat").is_none());
        assert!(record[field].get("runtime").is_none());
    }
    assert!(tick(&client, &memory).await.unwrap());
    let g = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(g.contract["requiredOutputs"], json!(["report.md"]));
    assert_eq!(g.inputs[0]["sha256"], digest("Reviewed handoff — 日本語"));
    let before = memory.0.lock().unwrap().clone();
    assert!(tick(&client, &memory).await.unwrap());
    assert_eq!(*memory.0.lock().unwrap(), before);
}

#[tokio::test]
async fn hermes_synthesis_accepts_matching_negotiated_router_activity() {
    let (_server, client, memory, phase) = phase_fixture_with(hermes_root()).await;
    let (key, mut record) = complete(&client, &memory, &phase).await;
    for field in ["assignment", "reply"] {
        record[field]["activityFormat"] = "router-observations-v1".into();
    }
    {
        let mut objects = memory.0.lock().unwrap();
        objects
            .get_mut(&map_path(&format!("kars-mission-attempt-{key}")))
            .unwrap()["data"]["attempt.json"] = record.to_string().into();
        objects
            .get_mut(&map_path(&format!("kars-mission-output-{key}")))
            .unwrap()["data"]["evidence.json"] = record["reply"].to_string().into();
    }
    assert!(tick(&client, &memory).await.unwrap());
    let synthesis = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        synthesis.inputs[0]["sha256"],
        digest("Reviewed handoff — 日本語")
    );
}

#[tokio::test]
async fn hermes_synthesis_rejects_other_runtime_or_mismatched_router_activity_evidence() {
    for damage in [
        "missing runtime",
        "wrong runtime",
        "unsolicited activity",
        "missing reply activity",
        "unsupported activity",
    ] {
        let (_server, client, memory, phase) = phase_fixture_with(hermes_root()).await;
        let (key, mut record) = complete(&client, &memory, &phase).await;
        match damage {
            "missing runtime" => {
                record["candidate"]
                    .as_object_mut()
                    .unwrap()
                    .remove("runtime");
            }
            "wrong runtime" => record["candidate"]["runtime"] = "OpenClaw".into(),
            "unsolicited activity" => {
                record["reply"]["activityFormat"] = "router-observations-v1".into();
            }
            "missing reply activity" => {
                record["assignment"]["activityFormat"] = "router-observations-v1".into();
            }
            _ => {
                record["assignment"]["activityFormat"] = "unknown-observations-v2".into();
                record["reply"]["activityFormat"] = "unknown-observations-v2".into();
            }
        }
        let path = map_path(&format!("kars-mission-attempt-{key}"));
        let mut map: ConfigMap =
            serde_json::from_value(memory.0.lock().unwrap()[&path].clone()).unwrap();
        map.data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), record.to_string());
        memory.put(path, &map);
        assert!(tick(&client, &memory).await.is_err(), "{damage}");
        assert!(
            grant(&client, &memory.task("report")).await.is_err(),
            "{damage}"
        );
    }
}

#[tokio::test]
async fn required_fetch_calls_survive_reviewed_grant_projection_for_both_runtimes() {
    let expected: Value =
        serde_json::from_str(include_str!("phase-projection-fixture.json")).unwrap();
    let mut reviewed: ExecutionPhase =
        serde_json::from_value(expected["reviewedPhase"].clone()).unwrap();
    reviewed.capabilities.reverse();
    assert_eq!(
        contract(&reviewed, &[], vec![HANDOFF.into()]).unwrap(),
        expected
    );
    for mut initial in [root(), hermes_root()] {
        initial
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap()
            .roles[0]
            .phases[0] = reviewed.clone();
        ready(&mut initial);
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        let admitted = grant(&client, &phase).await.unwrap().unwrap();
        let (map, state) = load(&client, &memory.task("report")).await.unwrap();
        let mut current = expected.clone();
        current["version"] = json!(5);
        current["peerContext"] = peer_context(&state, &map, "r0-p0").unwrap();
        assert_eq!(admitted.contract, current);
    }
    let mut changed = reviewed.clone();
    changed.required_tool_calls[0]
        .arguments
        .insert("url".into(), "https://www.example.com/other/".into());
    assert_ne!(
        contract(&changed, &[], vec![HANDOFF.into()]).unwrap()["phaseDigest"],
        expected["phaseDigest"]
    );
    changed = reviewed;
    changed.required_tool_calls.reverse();
    assert_ne!(
        contract(&changed, &[], vec![HANDOFF.into()]).unwrap()["phaseDigest"],
        expected["phaseDigest"]
    );
}

#[tokio::test]
async fn native_authority_survives_reviewed_grant_projection_for_both_runtimes() {
    let mut expected: Value =
        serde_json::from_str(include_str!("phase-projection-fixture.json")).unwrap();
    expected["reviewedPhase"]["executionMode"] = json!("native");
    expected["reviewedPhase"]["requiredToolCalls"] = json!([]);
    // Independently computed by the TypeScript missionPhaseDigest producer.
    expected["phaseDigest"] =
        json!("sha256:e67d6a318edfa7deed04ba8119bde4ef5b945983c39661e94eacf0f5133f7bff");
    let reviewed: ExecutionPhase =
        serde_json::from_value(expected["reviewedPhase"].clone()).unwrap();
    assert_eq!(
        contract(&reviewed, &[], vec![HANDOFF.into()]).unwrap(),
        expected
    );
    for mut initial in [root(), hermes_root()] {
        let plan = initial
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        plan.roles[0].phases[0] = reviewed.clone();
        plan.synthesis.execution_mode = Some("native".into());
        ready(&mut initial);
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        let admitted = grant(&client, &phase).await.unwrap().unwrap();
        let (map, state) = load(&client, &memory.task("report")).await.unwrap();
        let mut current = expected.clone();
        current["version"] = json!(5);
        current["peerContext"] = peer_context(&state, &map, "r0-p0").unwrap();
        assert_eq!(admitted.contract, current);
        complete_with_tool_count(&client, &memory, &phase, 3).await;
        assert!(tick(&client, &memory).await.unwrap());
        let synthesis = grant(&client, &memory.task("report"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            synthesis.contract["reviewedPhase"]["executionMode"],
            "native"
        );
    }
}

#[tokio::test]
async fn cross_language_reviewed_custody_fixture() {
    assert_custody_fixture(root(), "protocol-fixture.json").await;
}
#[tokio::test]
async fn cross_language_hermes_reviewed_custody_fixture() {
    assert_custody_fixture(hermes_root(), "protocol-hermes-fixture.json").await;
}
async fn assert_custody_fixture(root: KarsTask, filename: &str) {
    let (_server, client, memory, phase) = phase_fixture_with(root).await;
    // Retained v1 fixtures must remain byte-for-byte compatible with v4 evidence.
    let (mut map, mut state) = load(&client, &memory.task("report")).await.unwrap();
    state.schema = "kars.reviewed-run/v1".into();
    map.data
        .as_mut()
        .unwrap()
        .insert("state.json".into(), serde_json::to_string(&state).unwrap());
    memory.put(map_path(&map.name_any()), &map);
    let phase_grant = grant(&client, &phase).await.unwrap().unwrap();
    complete_with_artifacts(
        &client,
        &memory,
        &phase,
        1,
        BTreeMap::from([
            ("capacity-plan.md".into(), "Original draft — 日本語".into()),
            (
                "station-plan.csv".into(),
                "station,jobs,notes\n1,14,\"quoted, note\"\n".into(),
            ),
            (
                format!("{}.md", "a".repeat(125)),
                "Long filename retains source identity".into(),
            ),
        ]),
    )
    .await;
    assert!(tick(&client, &memory).await.unwrap());
    let synthesis = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    let fixture = json!({"phaseName":phase.name_any(),"phaseGrant":phase_grant,"synthesisGrant":synthesis,"resources":memory.0.lock().unwrap().clone()});
    let updating = std::env::var("UPDATE_REVIEWED_FIXTURE").as_deref() == Ok("1");
    let directory = if updating {
        std::env::var_os("REVIEWED_FIXTURE_DIR").map(std::path::PathBuf::from)
    } else {
        None
    }
    .unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/reviewed_execution")
    });
    let path = directory.join(filename);
    if updating {
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        fixture, expected,
        "controller and mesh consume the same reviewed custody fixture"
    );
}
#[tokio::test]
async fn failed_partial_output_matches_publication_without_permitting_synthesis() {
    for initial in [root(), hermes_root()] {
        for (status, partial) in [
            ("failed", None),
            ("failed", Some("")),
            (
                "failed",
                Some("Plan only — required exports were not written."),
            ),
            ("rejected", Some("Unexecuted response")),
        ] {
            let (_server, client, memory, phase) = phase_fixture_with(initial.clone()).await;
            let (key, mut record) = complete(&client, &memory, &phase).await;
            record["phase"] = status.into();
            record["reply"]["status"] = status.into();
            record["reply"]["error"] = "Required exports missing".into();
            record["reply"].as_object_mut().unwrap().remove("artifacts");
            record["reply"].as_object_mut().unwrap().remove("output");
            if let Some(partial) = partial {
                record["reply"]["output"] = partial.into();
            }
            record["events"][0]["status"] = status.into();
            let output_path = map_path(&format!("kars-mission-output-{key}"));
            let artifacts_path = map_path(&format!("kars-mission-artifacts-{key}"));
            let mut output: ConfigMap =
                serde_json::from_value(memory.0.lock().unwrap()[&output_path].clone()).unwrap();
            let mut artifacts: ConfigMap =
                serde_json::from_value(memory.0.lock().unwrap()[&artifacts_path].clone()).unwrap();
            let data = output.data.as_mut().unwrap();
            for (field, value) in [
                ("status", status),
                ("error", "Required exports missing"),
                (
                    "output",
                    if status == "failed" {
                        partial.unwrap_or("")
                    } else {
                        ""
                    },
                ),
                ("artifactCount", "0"),
            ] {
                data.insert(field.into(), value.into());
            }
            data.insert("evidence.json".into(), record["reply"].to_string());
            artifacts.data = Some(BTreeMap::new());
            evidence::projection(&record, &output, &artifacts).unwrap();
            memory.put(output_path.clone(), &output);
            memory.put(artifacts_path, &artifacts);
            memory.put(
                map_path(&format!("kars-mission-attempt-{key}")),
                &evidence_map(
                    &phase,
                    &format!("kars-mission-attempt-{key}"),
                    BTreeMap::from([("attempt.json".into(), record.to_string())]),
                ),
            );
            assert!(
                phase_compute_retirable(&client, &memory.task(&phase.name_any()))
                    .await
                    .unwrap()
            );
            assert!(tick(&client, &memory).await.is_err());
            assert!(grant(&client, &memory.task("report")).await.is_err());
            output
                .data
                .as_mut()
                .unwrap()
                .insert("output".into(), "Forged response".into());
            assert!(evidence::projection(&record, &output, &artifacts).is_err());
            memory.put(output_path, &output);
            assert!(
                phase_compute_retirable(&client, &memory.task(&phase.name_any()))
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn failed_or_ambiguous_child_never_permits_synthesis() {
    for status in ["failed", "rejected", "uncertain"] {
        let (_server, client, memory, phase) = phase_fixture().await;
        let (key, mut record) = complete(&client, &memory, &phase).await;
        record["phase"] = status.into();
        memory.put(
            map_path(&format!("kars-mission-attempt-{key}")),
            &evidence_map(
                &phase,
                &format!("kars-mission-attempt-{key}"),
                BTreeMap::from([("attempt.json".into(), record.to_string())]),
            ),
        );
        assert!(tick(&client, &memory).await.is_err());
        assert!(grant(&client, &memory.task("report")).await.is_err());
        assert!(
            phase_compute_retirable(&client, &memory.task(&phase.name_any()))
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn rejects_missing_handoff_forged_evidence_or_noncanonical_projection() {
    for damage in [
        "handoff",
        "owner",
        "immutable",
        "bytes",
        "identity",
        "tool-limit",
        "activity",
        "journal",
        "completion",
    ] {
        let (_server, client, memory, phase) = phase_fixture().await;
        let (key, mut record) = complete(&client, &memory, &phase).await;
        match damage {
            "handoff" => {
                record["reply"]["artifacts"]
                    .as_object_mut()
                    .unwrap()
                    .remove(HANDOFF);
            }
            "identity" => record["reply"]["assignmentId"] = "foreign-assignment".into(),
            "tool-limit" => record["reply"]["evidence"]["phase"]["attemptedToolCalls"] = 99.into(),
            "activity" => {
                record["assignment"]
                    .as_object_mut()
                    .unwrap()
                    .remove("activityFormat");
                record["reply"]
                    .as_object_mut()
                    .unwrap()
                    .remove("activityFormat");
            }
            "journal" => record["events"][0]["evidence"]["rounds"] = 0.into(),
            "completion" => {
                let mut task = memory.task(&phase.name_any());
                task.metadata.annotations.as_mut().unwrap().remove(COMPLETE);
                memory.put(object_path(&task), &task);
            }
            other => {
                let mut objects = memory.0.lock().unwrap();
                let map = objects
                    .get_mut(&map_path(&format!("kars-mission-artifacts-{key}")))
                    .unwrap();
                match other {
                    "owner" => map["metadata"]["ownerReferences"][0]["uid"] = "foreign-uid".into(),
                    "immutable" => map["immutable"] = false.into(),
                    _ => map["data"][HANDOFF] = "Forged bytes".into(),
                }
            }
        }
        memory.put(
            map_path(&format!("kars-mission-attempt-{key}")),
            &evidence_map(
                &phase,
                &format!("kars-mission-attempt-{key}"),
                BTreeMap::from([("attempt.json".into(), record.to_string())]),
            ),
        );
        assert!(!tick(&client, &memory).await.unwrap_or(false), "{damage}");
        assert!(
            !matches!(
                phase_compute_retirable(&client, &memory.task(&phase.name_any())).await,
                Ok(true)
            ),
            "{damage}"
        );
        assert!(
            grant(&client, &memory.task("report")).await.is_err(),
            "{damage}"
        );
    }
}
async fn drive_phase(client: &Client, memory: &Memory, key: &str) -> KarsTask {
    for _ in 0..20 {
        tick(client, memory).await.unwrap();
        let (_, s) = load(client, &memory.task("report")).await.unwrap();
        for reservation in s.children.values() {
            memory.ready(&reservation.name);
        }
        if let Some(r) = s.children.get(key) {
            let task = memory.task(&r.name);
            if launched(&task) {
                return task;
            }
        }
    }
    panic!("phase {key} not admitted");
}
#[tokio::test]
async fn dependency_phases_and_parallel_bound_are_connected() {
    let (_server, client, memory) = fixture().await;
    let mut root = memory.task("report");
    let p = root
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    let mut reviewer = p.roles[0].clone();
    reviewer.name = "reviewer".into();
    reviewer.depends_on = vec!["writer".into()];
    let mut edit = p.roles[0].phases[0].clone();
    edit.name = "edit".into();
    p.roles[0].phases.push(edit);
    p.roles.push(reviewer);
    ready(&mut root);
    memory.put(object_path(&root), &root);
    let first = drive_phase(&client, &memory, "r0-p0").await;
    let (_, s) = load(&client, &memory.task("report")).await.unwrap();
    assert!(!s.children.contains_key("r1"));
    assert!(!s.children.contains_key("r0-p1"));
    complete(&client, &memory, &first).await;
    let second = drive_phase(&client, &memory, "r0-p1").await;
    let g = grant(&client, &second).await.unwrap().unwrap();
    assert_eq!(g.inputs[0]["source"]["taskUid"], first.uid().unwrap());
    complete(&client, &memory, &second).await;
    let review = drive_phase(&client, &memory, "r1-p0").await;
    let g = grant(&client, &review).await.unwrap().unwrap();
    assert_eq!(g.inputs[0]["source"]["taskUid"], second.uid().unwrap());
    complete(&client, &memory, &review).await;
    assert!(tick(&client, &memory).await.unwrap());
    assert_eq!(
        grant(&client, &memory.task("report"))
            .await
            .unwrap()
            .unwrap()
            .inputs
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn dependent_review_and_synthesis_receive_actual_exported_files_with_custody() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory) = fixture().await;
        let mut root = initial;
        let plan = root
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        let mut reviewer = plan.roles[0].clone();
        reviewer.name = "reviewer".into();
        reviewer.depends_on = vec!["writer".into()];
        plan.roles.push(reviewer);
        ready(&mut root);
        memory.put(object_path(&root), &root);
        let writer = drive_phase(&client, &memory, "r0-p0").await;
        let csv = "station,jobs,notes\n1,14,\"quoted, note\"\n";
        let long_name = format!("{}.md", "a".repeat(125));
        let (key, record) = complete_with_artifacts(
            &client,
            &memory,
            &writer,
            1,
            BTreeMap::from([
                ("capacity-plan.md".into(), "Original draft — 日本語".into()),
                ("station-plan.csv".into(), csv.into()),
                (
                    long_name.clone(),
                    "Long filename remains pinned in source".into(),
                ),
                ("kars-router-observations.json".into(), "{}".into()),
            ]),
        )
        .await;
        assert_eq!(
            record["reply"]["artifacts"]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap(),
            "handoff.md"
        );
        let review = drive_phase(&client, &memory, "r1-p0").await;
        let g = grant(&client, &review).await.unwrap().unwrap();
        let references = g.inputs.as_array().unwrap();
        assert_eq!(references.len(), 4);
        assert!(
            references
                .iter()
                .all(|input| input["name"].as_str().unwrap().len() <= 128
                    && input["source"]["taskUid"] == writer.uid().unwrap()
                    && input.get("content").is_none())
        );
        let input = references
            .iter()
            .find(|input| input["source"]["artifactName"] == "station-plan.csv")
            .unwrap();
        assert_eq!(input["sha256"], digest(csv));
        assert_eq!(input["name"], "r0-p0--3-station-plan.csv");
        assert!(
            references
                .iter()
                .any(|input| input["name"] == "r0-p0--1-capacity-plan.md")
        );
        assert!(
            references
                .iter()
                .any(|input| input["source"]["artifactName"] == long_name)
        );
        assert!(references.iter().any(|input| input["name"] == "r0-p0.md"));

        let path = map_path(&format!("kars-mission-artifacts-{key}"));
        let saved = memory.0.lock().unwrap()[&path].clone();
        let mut damaged = saved.clone();
        damaged["data"]["station-plan.csv"] = "replaced bytes".into();
        memory.put(path.clone(), &damaged);
        assert!(grant(&client, &review).await.is_err());
        memory.put(path, &saved);

        let (_, record) = complete_with_artifacts(
            &client,
            &memory,
            &review,
            1,
            BTreeMap::from([(
                "review.md".into(),
                "Inspected exact CSV; keep valid quoting".into(),
            )]),
        )
        .await;
        let supplied = record["assignment"]["inputArtifacts"].as_array().unwrap();
        assert_eq!(
            supplied
                .iter()
                .find(|input| input["source"]["artifactName"] == "station-plan.csv")
                .unwrap()["content"],
            csv
        );
        assert!(tick(&client, &memory).await.unwrap());
        let synthesis = grant(&client, &memory.task("report"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(synthesis.inputs.as_array().unwrap().len(), 6);
        assert!(
            synthesis
                .inputs
                .as_array()
                .unwrap()
                .iter()
                .any(|input| input["source"]["artifactName"] == "review.md"
                    && input["source"]["taskUid"] == review.uid().unwrap())
        );
        assert!(synthesis.inputs.as_array().unwrap().iter().any(
            |input| input["source"]["artifactName"] == "station-plan.csv"
                && input["sha256"] == digest(csv)
        ));
    }
}

#[tokio::test]
async fn multi_role_exports_are_not_limited_to_one_producers_file_count() {
    for runtime in ["OpenClaw", "Hermes"] {
        for names in [
            vec!["builder", "tester", "researcher", "validator"],
            vec![
                "one", "two", "three", "four", "five", "six", "seven", "eight",
            ],
        ] {
            let (_server, client, memory) = fixture().await;
            let mut root = team_root_with_roles(runtime, &names);
            let plan = root
                .spec
                .blueprint
                .as_mut()
                .unwrap()
                .execution_plan
                .as_mut()
                .unwrap();
            let calls = i32::try_from(names.len() + 1).unwrap();
            plan.synthesis.max_tool_calls = calls;
            for role in &mut plan.roles {
                for phase in &mut role.phases {
                    phase.max_tool_calls = calls;
                }
            }
            seal_team_test_run(&mut root);
            ready(&mut root);
            memory.put(object_path(&root), &root);
            let mut expected = Vec::new();
            for r in 0..names.len() {
                let task = drive_phase(&client, &memory, &format!("r{r}-p0")).await;
                let documents: BTreeMap<String, String> = (0..6)
                    .map(|i| {
                        (
                            format!("file-{i}.md"),
                            format!("Role {r}, file {i}: exact bytes\r\n✓"),
                        )
                    })
                    .collect();
                complete_with_artifacts(&client, &memory, &task, 1, documents.clone()).await;
                for (name, content) in documents {
                    expected.push((task.uid().unwrap(), name, content));
                }
            }
            assert!(tick(&client, &memory).await.unwrap());
            let root = memory.task("report");
            let (_, record) = complete(&client, &memory, &root).await;
            let actual: Vec<Input> =
                serde_json::from_value(record["candidate"]["inputArtifacts"].clone()).unwrap();
            assert_eq!(actual.len(), names.len() * 7);
            assert!(serde_json::to_vec(&actual).unwrap().len() < MAX_ASSIGNMENT_INPUT_BYTES);
            for (uid, name, content) in expected {
                assert!(actual.iter().any(|input| input.source.task_uid == uid
                    && input.source.artifact_name == name
                    && input.content == content
                    && input.sha256 == digest(&content)));
            }
        }
    }
}

#[tokio::test]
async fn full_memory_and_role_outputs_reach_synthesis_with_bounded_capacity() {
    for runtime in ["OpenClaw", "Hermes"] {
        for overflow in [false, true] {
            let (_server, client, memory) = fixture().await;
            let mut root = team_root_with_roles(runtime, &["writer", "reviewer"]);
            let retained = continuity_inputs(143430);
            root.annotations_mut().insert(
                TEAM_MEMORY.into(),
                serde_json::to_string(&retained).unwrap(),
            );
            seal_team_test_run(&mut root);
            memory.put(object_path(&root), &root);
            let writer = drive_phase(&client, &memory, "r0-p0").await;
            let documents = if overflow {
                BTreeMap::from([
                    ("research-draft.md".into(), "x".repeat(64000)),
                    ("research-sources.json".into(), "x".repeat(64000)),
                ])
            } else {
                BTreeMap::from([
                    ("research-draft.md".into(), "x".repeat(31553)),
                    ("research-sources.json".into(), "x".repeat(26140)),
                    ("research-notes.md".into(), "x".repeat(10785)),
                ])
            };
            complete_with_artifacts(&client, &memory, &writer, 1, documents.clone()).await;
            let review = drive_phase(&client, &memory, "r1-p0").await;
            let (_, record) = complete_with_artifacts(
                &client,
                &memory,
                &review,
                1,
                BTreeMap::from([
                    ("review.md".into(), "x".repeat(64000)),
                    ("review-sources.json".into(), "x".repeat(64000)),
                ]),
            )
            .await;
            let supplied = record["assignment"]["inputArtifacts"].as_array().unwrap();
            for (name, content) in documents {
                let input = supplied
                    .iter()
                    .find(|item| item["source"]["artifactName"] == name)
                    .unwrap();
                assert_eq!(input["content"], content);
                assert_eq!(input["sha256"], digest(&content));
                assert_eq!(input["source"]["taskUid"], writer.uid().unwrap());
            }
            if overflow {
                assert!(tick(&client, &memory).await.is_err());
                assert!(grant(&client, &memory.task("report")).await.is_err());
            } else {
                assert!(tick(&client, &memory).await.unwrap());
                let root = memory.task("report");
                let (_, record) = complete(&client, &memory, &root).await;
                let actual: Vec<Input> =
                    serde_json::from_value(record["candidate"]["inputArtifacts"].clone()).unwrap();
                let bytes = serde_json::to_vec(&actual).unwrap().len();
                assert!(bytes > MAX_INPUT_BYTES && bytes <= MAX_ASSIGNMENT_INPUT_BYTES);
                for input in retained {
                    assert!(
                        actual.contains(&input),
                        "{runtime}: retained bytes or custody changed"
                    );
                }
                assert!(
                    actual
                        .iter()
                        .any(|input| input.source.task_uid == writer.uid().unwrap()
                            && input.source.artifact_name == "research-draft.md")
                );
                assert!(
                    actual
                        .iter()
                        .any(|input| input.source.task_uid == review.uid().unwrap()
                            && input.source.artifact_name == "review.md"
                            && input.content.len() == 64000)
                );
            }
        }
    }
}

#[tokio::test]
async fn complete_exported_files_above_64kib_reach_synthesis_with_custody() {
    let (_server, client, memory, phase) = phase_fixture().await;
    complete_with_artifacts(
        &client,
        &memory,
        &phase,
        1,
        BTreeMap::from([("large.md".into(), "x".repeat(65536))]),
    )
    .await;
    assert!(tick(&client, &memory).await.unwrap());
    let synthesis = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    let inputs = synthesis.inputs.as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    let document = inputs
        .iter()
        .find(|input| input["source"]["artifactName"] == "large.md")
        .unwrap();
    assert_eq!(document["sha256"], digest(&"x".repeat(65536)));
    assert_eq!(document["source"]["taskUid"], phase.uid().unwrap());
    assert!(document.get("content").is_none());
}

#[tokio::test]
async fn independent_roles_never_exceed_reviewed_parallel_upper_bound() {
    for parallel in [1, 2] {
        let (_server, client, memory) = fixture().await;
        let mut root = memory.task("report");
        let p = root
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        let mut other = p.roles[0].clone();
        other.name = "other".into();
        p.roles.push(other);
        p.max_parallel = parallel;
        ready(&mut root);
        memory.put(object_path(&root), &root);
        drive_phase(&client, &memory, "r0-p0").await;
        for _ in 0..5 {
            tick(&client, &memory).await.unwrap();
        }
        let (_, s) = load(&client, &memory.task("report")).await.unwrap();
        assert_eq!(s.children.contains_key("r1"), parallel == 2);
    }
}
#[tokio::test]
async fn scheduled_phases_share_verified_lineage_and_the_existing_role_budget_ledger() {
    use crate::{
        inference_budget::binding::limits,
        inference_budget_contract::{
            AttemptCommand, BudgetError, ExecutionIdentity, ReserveRequest, ResourceIdentity,
            RootIdentity, RootKind, TaskAuthority,
            ledger::Ledger,
            tariffs::{ModelContract, Operation, OutputField},
        },
        task_identity::{LeafReadiness, resolve},
    };
    let (_server, client, memory) = fixture().await;
    memory.put(
        "/api/v1/namespaces/default".into(),
        &json!({"apiVersion":"v1","kind":"Namespace",
        "metadata":{"name":"default","uid":"workspace-uid","resourceVersion":"1"}}),
    );
    let mut root = memory.task("report");
    let plan = root
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    let mut edit = plan.roles[0].phases[0].clone();
    edit.name = "edit".into();
    plan.roles[0].phases.push(edit);
    ready(&mut root);
    memory.put(object_path(&root), &root);
    let first = drive_phase(&client, &memory, "r0-p0").await;
    complete(&client, &memory, &first).await;
    let second = drive_phase(&client, &memory, "r0-p1").await;
    let first_lineage = resolve(&client, &first, LeafReadiness::RequireReady)
        .await
        .unwrap();
    let second_lineage = resolve(&client, &second, LeafReadiness::RequireReady)
        .await
        .unwrap();
    assert_eq!(first_lineage.nodes.len(), 3);
    assert_eq!(second_lineage.nodes.len(), 3);
    let role_uid = first_lineage.nodes[1].pin.task.uid.clone();
    assert_eq!(second_lineage.nodes[1].pin.task.uid, role_uid);
    let resource = |name: &str, uid: &str| ResourceIdentity {
        namespace: "default".into(),
        name: name.into(),
        uid: uid.into(),
    };
    let mut ledger = Ledger::new(
        "account-uid".into(),
        RootIdentity {
            kind: RootKind::KarsTask,
            resource: resource("report", "root-uid"),
            workspace_uid: first_lineage.workspace_uid.clone(),
            cluster_uid: "cluster-uid".into(),
        },
        limits(&root.spec.envelope).unwrap(),
    )
    .unwrap();
    for node in first_lineage
        .nodes
        .iter()
        .chain(second_lineage.nodes.iter())
    {
        assert_eq!(node.pin.root_task_uid, "root-uid");
        ledger = ledger
            .register_task(TaskAuthority {
                task: resource(&node.pin.task.name, &node.pin.task.uid),
                parent_uid: node.pin.parent_task_uid.clone(),
                root_task_uid: node.pin.root_task_uid.clone(),
                authorization_digest: node.authorization_digest.clone(),
                effective_authorization: node.effective_authorization.clone(),
                limits: limits(&node.task.spec.envelope).unwrap(),
            })
            .unwrap()
            .next;
    }
    let contract = ModelContract {
        id: "fixture-model".into(),
        version: "v1".into(),
        valid_until: "2030-01-01T00:00:00Z".into(),
        provider_id: "fixture-provider".into(),
        endpoint: "https://provider.example".into(),
        model: "fixture-model".into(),
        operation: Operation::ChatCompletions,
        output_field: OutputField::Tokens,
        maximum_input_tokens: 1000,
        maximum_output_tokens: 2000,
        maximum_wire_bytes: 4096,
        output_bound_includes_reasoning: true,
        maximum_price: None,
    };
    let (_, quote) = contract
        .normalize(
            br#"{"model":"fixture-model","messages":[{"role":"user","content":"report"}]}"#,
            100,
            false,
        )
        .unwrap();
    let request = |task: &KarsTask, index: usize| ReserveRequest {
        account_uid: "account-uid".into(),
        identity: ExecutionIdentity {
            task_uid: task.uid().unwrap(),
            authorization_digest: task.envelope_digest(),
            sandbox: resource(&format!("sandbox-{index}"), &format!("sandbox-{index}")),
            runtime_namespace_uid: format!("runtime-{index}"),
            pod_name: format!("pod-{index}"),
            pod_uid: format!("pod-{index}"),
        },
        sequence: 1,
        wire_digest: format!("sha256:{}", "a".repeat(64)),
        quote: quote.clone().into(),
    };
    let first_request = request(&first, 0);
    ledger = ledger
        .register_session(first_request.identity.clone())
        .unwrap()
        .next;
    let reserved = ledger.reserve(&first_request, 100).unwrap();
    ledger = reserved
        .next
        .begin_dispatch(
            &AttemptCommand {
                account_uid: first_request.account_uid.clone(),
                key: reserved.value.key,
                identity: first_request.identity.clone(),
                wire_digest: first_request.wire_digest.clone(),
            },
            101,
        )
        .unwrap()
        .next;
    ledger = ledger
        .close_session(&first_request.identity.pod_uid)
        .unwrap()
        .next;
    ledger = serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
    for uid in ["root-uid", &role_uid, first.uid().as_deref().unwrap()] {
        assert_eq!(ledger.nodes[uid].meters.uncertain.tokens, 3000);
    }
    assert_eq!(ledger.nodes[&role_uid].authority.limits.tokens, Some(5000));
    assert_eq!(ledger.limits.tokens, Some(10000));
    let second_request = request(&second, 1);
    ledger = ledger
        .register_session(second_request.identity.clone())
        .unwrap()
        .next;
    assert!(matches!(
        ledger.reserve(&second_request, 102),
        Err(BudgetError::Exhausted)
    ));
    assert_eq!(
        ledger.nodes[&second.uid().unwrap()]
            .meters
            .total()
            .unwrap()
            .tokens,
        0
    );
    assert_eq!(ledger.meters.total().unwrap().tokens, 3000);
    let synthesis = request(&root, 2);
    ledger = ledger
        .register_session(synthesis.identity.clone())
        .unwrap()
        .next;
    ledger = ledger.reserve(&synthesis, 102).unwrap().next;
    assert_eq!(ledger.meters.total().unwrap().tokens, 6000);
    assert_eq!(ledger.nodes[&role_uid].meters.total().unwrap().tokens, 3000);
}

#[test]
fn unsupported_full_plan_and_runtime_remain_closed() {
    let mut task = root();
    assert!(validate(&task).is_ok());
    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .roles[0]
        .phases[0]
        .capabilities
        .push("shell".into());
    assert!(validate(&task).is_err());
    task = root();
    task.spec.blueprint.as_mut().unwrap().runtime = Some("MicrosoftAgentFramework".into());
    assert!(validate(&task).is_err());
    for (tokens, accepted) in [
        (0, true),
        (9_007_199_254_740_991, true),
        (-1, false),
        (9_007_199_254_740_992, false),
    ] {
        task = root();
        task.spec.envelope.budget.as_mut().unwrap().tokens = Some(tokens);
        assert_eq!(validate(&task).is_ok(), accepted, "root cap {tokens}");
    }
}
