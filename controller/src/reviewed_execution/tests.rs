// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

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
            request.url.path().to_owned()
        };
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
    let mut task = root();
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
    team.spec.roster = vec![crate::kars_team::TeamRole {
        name: "writer".into(),
        blueprint: Some(blueprint),
        ..Default::default()
    }];
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
async fn creates_inactive_reserved_children_then_admits_exact_phase() {
    let (_server, client, memory, phase) = phase_fixture().await;
    assert!(crate::kars_task_reconciler::task_is_ready(&phase));
    let admitted = grant(&client, &phase).await.unwrap().unwrap();
    assert_eq!(admitted.contract["version"], 4);
    assert_eq!(admitted.contract["requiredOutputs"], json!([HANDOFF]));
    assert_eq!(admitted.inputs, json!([]));
    assert!(!tick(&client, &memory).await.unwrap());
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
    let g = grant(client, task).await.unwrap().unwrap();
    let content =
        crate::mission_delivery::admission::objective(task, ann(task, RUN).unwrap()).unwrap();
    let mut candidate = json!({"namespace":"default","taskName":task.name_any(),"taskUid":task.uid(),"runNonce":ann(task,RUN),
        "sandboxUid":"sandbox-uid","podUid":"pod-uid","agentDid":format!("did:mesh:{}","a".repeat(32)),
        "dispatcherDid":format!("did:mesh:{}","b".repeat(32)),"agentName":task.spec.display_name.clone().unwrap_or_else(|| task.name_any()),"content":content,
        "reviewedPhase":g.contract["reviewedPhase"],"requiredOutputs":g.contract["requiredOutputs"]});
    if !g.inputs.as_array().unwrap().is_empty() {
        let root = ann(task, ROOT)
            .map(|name| memory.task(name))
            .unwrap_or_else(|| task.clone());
        let memory = memory_inputs(&root).unwrap();
        let mut retained = g.inputs.as_array().unwrap().clone();
        for input in &mut retained {
            input["content"] = memory
                .iter()
                .find(|document| input["name"] == document.name)
                .map(|document| document.content.as_str())
                .unwrap_or("Reviewed handoff — 日本語")
                .into();
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
            "maxToolCalls":g.contract["reviewedPhase"]["maxToolCalls"],"attemptedToolCalls":1,"successfulToolCalls":1}});
    let mut reply = assignment.clone();
    for key in ["inputArtifacts", "content"] {
        reply.as_object_mut().unwrap().remove(key);
    }
    let artifacts: BTreeMap<String, String> = g.contract["requiredOutputs"]
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
async fn completed_team_documents_survive_retirement_and_task_gc_with_full_peer_provenance() {
    for runtime in ["OpenClaw", "Hermes"] {
        let mut initial = team_root(runtime);
        let prior: Input = serde_json::from_value(json!({
            "name":"memory-0-document-0.md",
            "source":{"namespace":"default","taskName":"previous-run","taskUid":"previous-uid",
                "sandboxUid":"previous-sandbox","podUid":"previous-pod","runNonce":"previous-nonce",
                "assignmentId":"previous-assignment","agentDid":format!("did:mesh:{}","c".repeat(32)),
                "artifactName":"decision-log.md"},
            "sha256":digest("Keep the complete clinic decision, including 日本語."),
            "content":"Keep the complete clinic decision, including 日本語."
        })).unwrap();
        initial.annotations_mut().insert(
            TEAM_MEMORY.into(),
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
        complete(&client, &memory, &phase).await;
        assert!(tick(&client, &memory).await.unwrap());
        let root = memory.task("report");
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
async fn cross_language_reviewed_custody_fixture() {
    assert_custody_fixture(root(), "protocol-fixture.json").await;
}
#[tokio::test]
async fn cross_language_hermes_reviewed_custody_fixture() {
    assert_custody_fixture(hermes_root(), "protocol-hermes-fixture.json").await;
}
async fn assert_custody_fixture(root: KarsTask, filename: &str) {
    let (_server, client, memory, phase) = phase_fixture_with(root).await;
    let phase_grant = grant(&client, &phase).await.unwrap().unwrap();
    complete(&client, &memory, &phase).await;
    assert!(tick(&client, &memory).await.unwrap());
    let synthesis = grant(&client, &memory.task("report"))
        .await
        .unwrap()
        .unwrap();
    let fixture = json!({"phaseName":phase.name_any(),"phaseGrant":phase_grant,"synthesisGrant":synthesis,"resources":memory.0.lock().unwrap().clone()});
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/reviewed_execution")
        .join(filename);
    if std::env::var("UPDATE_REVIEWED_FIXTURE").as_deref() == Ok("1") {
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
        quote: quote.clone(),
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
