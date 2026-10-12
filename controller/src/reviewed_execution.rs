// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reviewed execution uses the ordinary Task authority, budget and sandbox lifecycles.
//! A role is a governed budget parent; each phase is a fresh executable child.
use crate::{
    kars_task::{KarsTask, TaskExecution},
    task_execution_plan::{ExecutionPhase, ExecutionPlan},
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use k8s_openapi::{api::core::v1::ConfigMap, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const PIN: &str = "kars.azure.com/reviewed-state";
const ROOT: &str = "kars.azure.com/reviewed-root";
const SLOT: &str = "kars.azure.com/reviewed-slot";
const RUN: &str = "kars.azure.com/run-requested";
const COMPLETE: &str = "kars.azure.com/run-completed";
const HANDOFF: &str = "handoff.md";
const OBJECTIVE_NONCE: &str = "kars.azure.com/run-objective-nonce";
const OBJECTIVE_B64: &str = "kars.azure.com/run-objective-b64";
const OBJECTIVE_DIGEST: &str = "kars.azure.com/run-objective-digest";
pub(crate) const TEAM_ROLES: &str = "kars.azure.com/reviewed-team-roles";
pub(crate) const TEAM_MEMORY: &str = "kars.azure.com/reviewed-team-memory";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    name: String,
    uid: String,
    spec: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    schema: String,
    root_name: String,
    root_uid: String,
    admission: Value,
    plan_digest: String,
    plan: ExecutionPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_roles: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_memory: Option<String>,
    children: BTreeMap<String, Reservation>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Pin {
    name: String,
    uid: String,
}
mod evidence;
mod retention;
pub(crate) use retention::retained_team_run;
mod team_roles;
pub(crate) use team_roles::seal as seal_team_roles;

use crate::team_memory::{Document as Input, Source};
#[derive(Clone, Serialize, PartialEq)]
pub(crate) struct Grant {
    pub contract: Value,
    pub inputs: Value,
    pub authority: Value,
}

fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn plan_digest(plan: &ExecutionPlan) -> Result<String> {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.values_mut().for_each(sort);
                map.sort_keys();
            }
            Value::Array(values) => values.iter_mut().for_each(sort),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(plan)?;
    sort(&mut value);
    Ok(digest(serde_json::to_vec(&value)?))
}
fn ann<'a>(task: &'a KarsTask, key: &str) -> Option<&'a str> {
    task.annotations().get(key).map(String::as_str)
}
fn launched(task: &KarsTask) -> bool {
    task.spec.execution.as_ref().is_some_and(|e| e.launch)
}
fn plan(task: &KarsTask) -> Option<&ExecutionPlan> {
    task.spec.blueprint.as_ref()?.execution_plan.as_ref()
}
fn tasks(client: &Client, task: &KarsTask) -> Api<KarsTask> {
    Api::namespaced(client.clone(), &task.namespace().unwrap_or_default())
}
fn maps(client: &Client, task: &KarsTask) -> Api<ConfigMap> {
    Api::namespaced(client.clone(), &task.namespace().unwrap_or_default())
}
fn live(meta: &ObjectMeta) -> Result<()> {
    ensure!(
        meta.uid.as_ref().is_some_and(|s| !s.is_empty())
            && meta.resource_version.is_some()
            && meta.deletion_timestamp.is_none(),
        "reviewed resource identity missing or deleting"
    );
    Ok(())
}
fn owner(meta: &ObjectMeta, task: &KarsTask) -> Result<()> {
    live(meta)?;
    ensure!(
        meta.namespace == task.metadata.namespace
            && meta.owner_references.as_ref()
                == Some(&vec![
                    task.controller_owner_ref(&())
                        .ok_or_else(|| anyhow::anyhow!("missing Task UID"))?
                ]),
        "reviewed resource outside exact Task custody"
    );
    Ok(())
}
async fn current(client: &Client, task: &KarsTask) -> Result<()> {
    let fresh = tasks(client, task).get(&task.name_any()).await?;
    live(&fresh.metadata)?;
    ensure!(
        fresh.metadata.uid == task.metadata.uid
            && fresh.metadata.resource_version == task.metadata.resource_version,
        "reviewed Task changed during reconciliation"
    );
    Ok(())
}
fn admission(task: &KarsTask) -> Result<Value> {
    let a = crate::mission_delivery::admission::for_task(task)?;
    ensure!(
        a["state"] == "run",
        "reviewed execution requires a bound run nonce"
    );
    Ok(a)
}

pub(crate) fn validate(task: &KarsTask) -> Result<()> {
    let Some(p) = plan(task) else { return Ok(()) };
    if !launched(task) {
        return Ok(());
    }
    crate::task_execution_plan::validate_filesystem_execution(p).map_err(anyhow::Error::msg)?;
    ensure!(
        matches!(
            crate::kars_task::blueprint::effective_runtime_name(&task.spec),
            "OpenClaw" | "Hermes"
        ),
        "TypedPlanExecutionUnavailable: reviewed execution requires an installed OpenClaw or Hermes adapter"
    );
    crate::task_execution_plan::validate_reviewed_delegation_depth(
        i64::from(task.spec.envelope.delegation_depth),
        crate::task_execution_plan::REVIEWED_TASK_DELEGATION_DEPTH,
    )
    .map_err(anyhow::Error::msg)?;
    let budget = task.spec.envelope.budget.as_ref();
    ensure!(
        budget
            .and_then(|b| b.scope.as_ref())
            .is_some_and(|s| serde_json::to_value(s).ok() == Some(json!("GovernedInference"))),
        "reviewed execution requires the shared GovernedInference budget lineage"
    );
    ensure!(
        budget
            .and_then(|b| b.tokens)
            .is_none_or(|n| (0..=9_007_199_254_740_991).contains(&n)),
        "TypedPlanExecutionUnavailable: reviewed root budget requires an exact nonnegative wire integer"
    );
    if let Some(cap) = budget.and_then(|b| b.tokens).filter(|n| *n > 0) {
        ensure!(
            p.roles
                .iter()
                .all(|r| r.budget_tokens.is_none_or(|n| n <= cap)),
            "reviewed role cap exceeds its parent envelope"
        );
    }
    admission(task)?;
    team_roles::validate(task)?;
    Ok(())
}
fn state_name(root: &KarsTask, a: &Value) -> String {
    let hash = digest(
        serde_json::to_vec(&json!([
            root.metadata.uid,
            a["taskGeneration"],
            a["authorizationDigest"],
            a["runNonce"],
            a["objectiveDigest"]
        ]))
        .unwrap(),
    );
    format!("kars-reviewed-{}", &hash[7..47])
}
fn state(map: &ConfigMap, root: &KarsTask) -> Result<State> {
    owner(&map.metadata, root)?;
    let s: State = serde_json::from_str(
        map.data
            .as_ref()
            .and_then(|d| d.get("state.json"))
            .ok_or_else(|| anyhow::anyhow!("missing reviewed state"))?,
    )?;
    let p = plan(root).ok_or_else(|| anyhow::anyhow!("reviewed plan removed"))?;
    ensure!(
        s.schema == "kars.reviewed-run/v1"
            && Some(&s.root_uid) == root.metadata.uid.as_ref()
            && s.root_name == root.name_any()
            && s.admission == admission(root)?
            && &s.plan == p
            && s.plan_digest == plan_digest(p)?
            && s.team_roles.as_deref() == ann(root, TEAM_ROLES)
            && s.team_memory.as_deref() == ann(root, TEAM_MEMORY)
            && map.name_any() == state_name(root, &s.admission),
        "reviewed state authority changed"
    );
    Ok(s)
}
async fn load(client: &Client, root: &KarsTask) -> Result<(ConfigMap, State)> {
    validate(root)?;
    ensure!(
        launched(root) && crate::kars_task_reconciler::task_is_ready(root),
        "reviewed root no longer Ready/active"
    );
    let pin: Pin = serde_json::from_str(
        ann(root, PIN).ok_or_else(|| anyhow::anyhow!("reviewed run not reserved"))?,
    )?;
    let map = maps(client, root).get(&pin.name).await?;
    ensure!(
        map.metadata.uid.as_ref() == Some(&pin.uid),
        "reserved reviewed state replaced"
    );
    let s = state(&map, root)?;
    Ok((map, s))
}
async fn reserve_state(client: &Client, root: &KarsTask) -> Result<bool> {
    let a = admission(root)?;
    let name = state_name(root, &a);
    if let Some(pin) = ann(root, PIN) {
        let pin: Pin = serde_json::from_str(pin)?;
        if pin.name == name {
            load(client, root).await?;
            return Ok(true);
        }
    }
    let api = maps(client, root);
    let map = if let Some(map) = api.get_opt(&name).await? {
        let s = state(&map, root)?;
        ensure!(
            s.children.is_empty(),
            "cannot adopt an unpinned progressed reviewed state"
        );
        map
    } else {
        let p = plan(root).unwrap().clone();
        let s = State {
            schema: "kars.reviewed-run/v1".into(),
            root_name: root.name_any(),
            root_uid: root.uid().unwrap(),
            admission: a,
            plan_digest: plan_digest(&p)?,
            plan: p,
            team_roles: ann(root, TEAM_ROLES).map(str::to_owned),
            team_memory: ann(root, TEAM_MEMORY).map(str::to_owned),
            children: BTreeMap::new(),
        };
        current(client, root).await?;
        api.create(
            &PostParams::default(),
            &ConfigMap {
                metadata: ObjectMeta {
                    name: Some(name.clone()),
                    namespace: root.namespace(),
                    owner_references: Some(vec![root.controller_owner_ref(&()).unwrap()]),
                    ..Default::default()
                },
                data: Some(BTreeMap::from([(
                    "state.json".into(),
                    serde_json::to_string(&s)?,
                )])),
                ..Default::default()
            },
        )
        .await?
    };
    owner(&map.metadata, root)?;
    current(client, root).await?;
    let pin = Pin {
        name,
        uid: map.uid().unwrap(),
    };
    tasks(client, root).patch(&root.name_any(), &PatchParams::default(), &Patch::Merge(json!({"metadata": {
        "uid": root.metadata.uid, "resourceVersion": root.metadata.resource_version, "annotations": {PIN: serde_json::to_string(&pin)?}
    }}))).await?;
    Ok(false)
}
fn slot(r: usize, p: Option<usize>) -> String {
    p.map_or_else(|| format!("r{r}"), |p| format!("r{r}-p{p}"))
}
fn child(
    root: &KarsTask,
    parent: &KarsTask,
    s: &State,
    map: &ConfigMap,
    r: usize,
    p: Option<usize>,
) -> Result<KarsTask> {
    let role = &s.plan.roles[r];
    let key = slot(r, p);
    let mut spec = parent.spec.clone();
    spec.parent_ref = Some(crate::mcp_server::LocalObjectRef {
        name: parent.name_any(),
    });
    spec.envelope.tier = parent.spec.envelope.authority_ceiling;
    spec.envelope.delegation_depth -= 1;
    if p.is_none() {
        team_roles::apply(root, r, &mut spec)?;
    }
    if p.is_none()
        && let Some(cap) = role.budget_tokens
    {
        let cap = spec
            .envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.tokens)
            .filter(|limit| *limit > 0)
            .map_or(cap, |limit| limit.min(cap));
        spec.envelope
            .budget
            .get_or_insert_with(Default::default)
            .tokens = Some(cap);
    }
    spec.blueprint.as_mut().unwrap().execution_plan = None;
    let objective = crate::mission_delivery::admission::objective(
        root,
        ann(root, RUN).ok_or_else(|| anyhow::anyhow!("missing root run"))?,
    )?;
    let phase_objective = p.map(|i| {
        format!(
            "{}\n\nRole: {}\n\nPhase: {}",
            objective, role.objective, role.phases[i].objective
        )
    });
    let bound_objective = phase_objective
        .as_ref()
        .filter(|text| text.chars().count() > 4096);
    spec.objective = if bound_objective.is_some() {
        format!(
            "Role: {}\n\nPhase: {}",
            role.objective,
            role.phases[p.unwrap()].objective
        )
    } else {
        phase_objective
            .clone()
            .unwrap_or_else(|| role.objective.clone())
    };
    spec.display_name = Some(p.map_or_else(
        || role.name.clone(),
        |i| format!("{} / {}", role.name, role.phases[i].name),
    ));
    spec.execution = Some(TaskExecution {
        launch: p.is_some(),
        runtime: if p.is_none() && ann(root, TEAM_ROLES).is_some() {
            None
        } else {
            parent
                .spec
                .execution
                .as_ref()
                .and_then(|e| e.runtime.clone())
        },
        ..Default::default()
    });
    ensure!(
        crate::kars_task::spec_attenuation_violations(&spec, &parent.spec).is_empty(),
        "reviewed child amplifies parent authority"
    );
    let name = format!("rp-{}-{key}", &map.name_any()[14..34]);
    let mut task = KarsTask::new(&name, spec);
    task.metadata.namespace = root.namespace();
    task.metadata.owner_references = Some(vec![parent.controller_owner_ref(&()).unwrap()]);
    task.metadata.annotations = Some(BTreeMap::from([
        (ROOT.into(), root.name_any()),
        (
            PIN.into(),
            serde_json::to_string(&Pin {
                name: map.name_any(),
                uid: map.uid().unwrap(),
            })?,
        ),
        (SLOT.into(), key.clone()),
        (RUN.into(), format!("plan-{}-{key}", &map.name_any()[14..])),
    ]));
    if let Some(objective) = bound_objective {
        let annotations = task.metadata.annotations.as_mut().unwrap();
        annotations.insert(OBJECTIVE_NONCE.into(), annotations[RUN].clone());
        annotations.insert(OBJECTIVE_B64.into(), STANDARD.encode(objective));
        annotations.insert(OBJECTIVE_DIGEST.into(), digest(objective));
    }
    task.inherit_user_owner(parent)
        .map_err(anyhow::Error::msg)?;
    Ok(task)
}
fn check_child(
    actual: &KarsTask,
    expected: &KarsTask,
    parent: &KarsTask,
    reservation: Option<&Reservation>,
) -> Result<()> {
    owner(&actual.metadata, parent)?;
    actual
        .clone()
        .inherit_user_owner(parent)
        .map_err(anyhow::Error::msg)?;
    let mut spec = actual.spec.clone();
    if let (Some(actual), Some(expected)) = (&mut spec.execution, &expected.spec.execution) {
        ensure!(
            expected.launch || !actual.launch,
            "role budget parent cannot execute"
        );
        actual.launch = expected.launch;
    }
    ensure!(
        actual.name_any() == expected.name_any()
            && serde_json::to_value(&spec)? == serde_json::to_value(&expected.spec)?
            && [
                ROOT,
                PIN,
                SLOT,
                RUN,
                OBJECTIVE_NONCE,
                OBJECTIVE_B64,
                OBJECTIVE_DIGEST
            ]
            .iter()
            .all(|key| ann(actual, key) == ann(expected, key)),
        "reviewed child specification changed"
    );
    if let Some(r) = reservation {
        ensure!(
            Some(&r.uid) == actual.metadata.uid.as_ref()
                && r.name == actual.name_any()
                && r.spec == serde_json::to_value(&expected.spec)?,
            "reserved child UID/spec changed"
        );
    } else {
        ensure!(
            !launched(actual)
                && ann(actual, "kars.azure.com/run-ack").is_none()
                && ann(actual, COMPLETE).is_none(),
            "unreserved child already accepted or active"
        );
    }
    Ok(())
}
async fn ensure_child(
    client: &Client,
    root: &KarsTask,
    parent: &KarsTask,
    map: &ConfigMap,
    s: &State,
    r: usize,
    p: Option<usize>,
) -> Result<Option<KarsTask>> {
    let expected = child(root, parent, s, map, r, p)?;
    let key = slot(r, p);
    let reservation = s.children.get(&key);
    let api = tasks(client, root);
    let actual = api.get_opt(&expected.name_any()).await?;
    ensure!(
        reservation.is_none() || actual.is_some(),
        "reserved reviewed child missing; refusing recreation"
    );
    let mut actual = match actual {
        Some(actual) => actual,
        None => {
            let mut inactive = expected.clone();
            inactive.spec.execution.as_mut().unwrap().launch = false;
            current(client, root).await?;
            current(client, parent).await?;
            api.create(&PostParams::default(), &inactive).await?
        }
    };
    check_child(&actual, &expected, parent, reservation)?;
    if actual
        .inherit_user_owner(parent)
        .map_err(anyhow::Error::msg)?
    {
        current(client, parent).await?;
        current(client, root).await?;
        api.replace(&actual.name_any(), &PostParams::default(), &actual)
            .await?;
        return Ok(None);
    }
    if reservation.is_none() {
        let mut next = s.clone();
        next.children.insert(
            key,
            Reservation {
                name: actual.name_any(),
                uid: actual.uid().unwrap(),
                spec: serde_json::to_value(&expected.spec)?,
            },
        );
        let mut next_map = map.clone();
        next_map.data = Some(BTreeMap::from([(
            "state.json".into(),
            serde_json::to_string(&next)?,
        )]));
        current(client, &actual).await?;
        current(client, parent).await?;
        current(client, root).await?;
        maps(client, root)
            .replace(&map.name_any(), &PostParams::default(), &next_map)
            .await?;
        return Ok(None);
    }
    if p.is_some() && !launched(&actual) {
        ensure!(
            crate::kars_task_reconciler::task_is_ready(parent),
            "reviewed parent is not Ready"
        );
        ensure!(
            maps(client, root).get(&map.name_any()).await? == *map,
            "reviewed state changed before activation"
        );
        current(client, parent).await?;
        current(client, root).await?;
        api.patch(&actual.name_any(), &PatchParams::default(), &Patch::Merge(json!({"metadata": {"uid": actual.metadata.uid, "resourceVersion": actual.metadata.resource_version}, "spec": {"execution": {"launch": true}}}))).await?;
        return Ok(None);
    }
    Ok(Some(actual))
}
async fn existing(
    client: &Client,
    root: &KarsTask,
    map: &ConfigMap,
    s: &State,
    r: usize,
    p: usize,
) -> Result<Option<KarsTask>> {
    let Some(role) = s.children.get(&slot(r, None)) else {
        return Ok(None);
    };
    let parent = tasks(client, root).get(&role.name).await?;
    check_child(
        &parent,
        &child(root, root, s, map, r, None)?,
        root,
        Some(role),
    )?;
    let Some(reserved) = s.children.get(&slot(r, Some(p))) else {
        return Ok(None);
    };
    ensure!(
        crate::kars_task_reconciler::task_is_ready(&parent),
        "reviewed role authority no longer Ready"
    );
    let task = tasks(client, root).get(&reserved.name).await?;
    check_child(
        &task,
        &child(root, &parent, s, map, r, Some(p))?,
        &parent,
        Some(reserved),
    )?;
    Ok(Some(task))
}

async fn handoff(
    client: &Client,
    task: &KarsTask,
    phase: &ExecutionPhase,
    name: String,
) -> Result<Option<Input>> {
    let nonce = ann(task, RUN).ok_or_else(|| anyhow::anyhow!("phase run missing"))?;
    let key = digest(serde_json::to_vec(&json!([task.metadata.uid, nonce]))?)[7..47].to_string();
    let api = maps(client, task);
    let attempt = api.get_opt(&format!("kars-mission-attempt-{key}")).await?;
    let Some(attempt) = attempt else {
        return Ok(None);
    };
    evidence::owned(&attempt, task)?;
    let encoded = attempt
        .data
        .as_ref()
        .and_then(|d| d.get("attempt.json"))
        .filter(|s| s.len() <= 900 * 1024)
        .ok_or_else(|| anyhow::anyhow!("missing or oversized phase attempt"))?;
    let record: Value = serde_json::from_str(encoded)?;
    ensure!(
        !matches!(
            record["phase"].as_str(),
            Some("failed" | "rejected" | "uncertain")
        ),
        "reviewed phase failed or ambiguous; synthesis prohibited"
    );
    if record["phase"] != "succeeded" || ann(task, COMPLETE) != Some(nonce) {
        return Ok(None);
    }
    ensure!(
        crate::kars_task_reconciler::task_is_ready(task) && launched(task),
        "completed phase authority is not Ready"
    );
    evidence::success(&record, task, phase)?;
    let c = &record["candidate"];
    let a = &record["assignment"];
    let reply = &record["reply"];
    let output = api.get(&format!("kars-mission-output-{key}")).await?;
    let artifacts = api.get(&format!("kars-mission-artifacts-{key}")).await?;
    evidence::canonical(&output, task, &key, "output")?;
    evidence::canonical(&artifacts, task, &key, "artifacts")?;
    evidence::projection(&record, &output, &artifacts)?;
    let content = artifacts
        .data
        .as_ref()
        .and_then(|d| d.get(HANDOFF))
        .ok_or_else(|| anyhow::anyhow!("missing required handoff.md"))?
        .clone();
    ensure!(
        !content.trim().is_empty()
            && content.len() <= 65536
            && reply["artifacts"][HANDOFF] == content,
        "invalid immutable handoff"
    );
    let field = |key: &str| -> Result<String> {
        Ok(c[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing source {key}"))?
            .into())
    };
    let source = Source {
        namespace: task.namespace().unwrap(),
        task_name: task.name_any(),
        task_uid: task.uid().unwrap(),
        sandbox_uid: field("sandboxUid")?,
        pod_uid: field("podUid")?,
        run_nonce: nonce.into(),
        assignment_id: a["assignmentId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing assignment ID"))?
            .into(),
        agent_did: field("agentDid")?,
        artifact_name: HANDOFF.into(),
    };
    for prior in [&artifacts, &output, &attempt] {
        let fresh = api.get(&prior.name_any()).await?;
        ensure!(
            fresh == *prior,
            "reviewed input evidence changed during selection"
        );
    }
    current(client, task).await?;
    Ok(Some(Input {
        name,
        source,
        sha256: digest(&content),
        content,
    }))
}
fn dependencies(s: &State, r: usize, p: usize) -> Vec<(usize, usize)> {
    if p > 0 {
        return vec![(r, p - 1)];
    }
    s.plan.roles[r]
        .depends_on
        .iter()
        .map(|name| {
            let i = s
                .plan
                .roles
                .iter()
                .position(|role| &role.name == name)
                .unwrap();
            (i, s.plan.roles[i].phases.len() - 1)
        })
        .collect()
}
async fn inputs(
    client: &Client,
    root: &KarsTask,
    map: &ConfigMap,
    s: &State,
    sources: &[(usize, usize)],
) -> Result<Option<Vec<Input>>> {
    let mut result = memory_inputs(root)?;
    for &(r, p) in sources {
        let Some(task) = existing(client, root, map, s, r, p).await? else {
            return Ok(None);
        };
        let Some(input) = handoff(
            client,
            &task,
            &s.plan.roles[r].phases[p],
            format!("r{r}-p{p}.md"),
        )
        .await?
        else {
            return Ok(None);
        };
        result.push(input);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    ensure!(
        result.len() <= 16 && serde_json::to_vec(&result)?.len() <= 65536,
        "reviewed retained inputs exceed the installed 64KiB limit"
    );
    Ok(Some(result))
}

fn memory_inputs(root: &KarsTask) -> Result<Vec<Input>> {
    let Some(raw) = ann(root, TEAM_MEMORY) else { return Ok(vec![]) };
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    ensure!(raw.len() <= 32768, "sealed Team memory exceeds 32KiB");
    let inputs: Vec<Input> = serde_json::from_str(raw)?;
    ensure!(!inputs.is_empty() && inputs.len() <= 8, "sealed Team memory exceeds its input count");
    let mut names = std::collections::BTreeSet::new();
    for input in &inputs {
        ensure!(
            input.name.starts_with("memory-")
                && input.name.len() <= 128
                && input.name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                && names.insert(&input.name)
                && input.sha256 == digest(&input.content)
                && input.source.namespace == root.namespace().unwrap_or_default(),
            "sealed Team memory identity or content is invalid"
        );
    }
    Ok(inputs)
}

/// Retire completed phase compute or a stopped root's compute, retaining publication records.
pub(crate) async fn phase_compute_retirable(client: &Client, task: &KarsTask) -> Result<bool> {
    let Some(name) = ann(task, ROOT) else {
        return Ok(false);
    };
    let api = tasks(client, task);
    let root = api.get(name).await?;
    if launched(&root) {
        let Some(nonce) = ann(task, RUN) else {
            return Ok(false);
        };
        if ann(task, COMPLETE) != Some(nonce) {
            return Ok(false);
        }
        let reviewed = grant(client, task)
            .await?
            .ok_or_else(|| anyhow::anyhow!("completed reviewed phase grant missing"))?;
        let phase: ExecutionPhase =
            serde_json::from_value(reviewed.contract["reviewedPhase"].clone())?;
        return Ok(handoff(client, task, &phase, "check.md".into())
            .await?
            .is_some());
    }
    let parent_name = task
        .spec
        .parent_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("reviewed phase parent missing"))?;
    let parent = api.get(&parent_name.name).await?;
    owner(&task.metadata, &parent)?;
    owner(&parent.metadata, &root)?;
    ensure!(
        ann(&parent, ROOT) == Some(name),
        "reviewed role root changed"
    );
    current(client, &root).await?;
    current(client, &parent).await?;
    current(client, task).await?;
    Ok(true)
}

/// True permits the ordinary materializer; false leaves the root governed but waiting.
pub(crate) async fn reconcile(client: &Client, task: &KarsTask) -> Result<bool> {
    if ann(task, ROOT).is_some() {
        grant(client, task).await?;
        return Ok(true);
    }
    let Some(p) = plan(task) else { return Ok(true) };
    validate(task)?;
    // First commit ordinary governance/budget readiness; children cannot precede it.
    if !crate::kars_task_reconciler::task_is_ready(task) {
        return Ok(false);
    }
    if !reserve_state(client, task).await? {
        return Ok(false);
    }
    let (map, s) = load(client, task).await?;
    let mut done = vec![false; p.roles.len()];
    let mut active = 0;
    for (r, role) in p.roles.iter().enumerate() {
        if let Some(last) = existing(client, task, &map, &s, r, role.phases.len() - 1).await? {
            done[r] = handoff(
                client,
                &last,
                &role.phases[role.phases.len() - 1],
                "check.md".into(),
            )
            .await?
            .is_some();
        }
        if !done[r] && s.children.contains_key(&slot(r, None)) {
            active += 1;
        }
    }
    for (r, role) in p.roles.iter().enumerate() {
        if done[r] {
            continue;
        }
        if role
            .depends_on
            .iter()
            .any(|name| !done[p.roles.iter().position(|v| &v.name == name).unwrap()])
        {
            continue;
        }
        if !s.children.contains_key(&slot(r, None)) && active >= p.max_parallel {
            continue;
        }
        let Some(parent) = ensure_child(client, task, task, &map, &s, r, None).await? else {
            return Ok(false);
        };
        if !crate::kars_task_reconciler::task_is_ready(&parent) {
            continue;
        }
        for phase in 0..role.phases.len() {
            if inputs(client, task, &map, &s, &dependencies(&s, r, phase))
                .await?
                .is_none()
            {
                break;
            }
            let Some(child) = ensure_child(client, task, &parent, &map, &s, r, Some(phase)).await?
            else {
                return Ok(false);
            };
            if handoff(client, &child, &role.phases[phase], "check.md".into())
                .await?
                .is_none()
            {
                break;
            }
        }
    }
    if done.iter().all(|v| *v) {
        grant(client, task).await?;
        return Ok(true);
    }
    Ok(false)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalPhase<'a> {
    name: &'a str,
    objective: &'a str,
    capabilities: Vec<&'a str>,
    required_tool_calls: Vec<Value>,
    min_tool_calls: i32,
    max_tool_calls: i32,
    fresh_context: bool,
}
fn contract(phase: &ExecutionPhase, inputs: &[Input], mut required: Vec<String>) -> Result<Value> {
    let mut caps: Vec<_> = phase.capabilities.iter().map(String::as_str).collect();
    caps.sort();
    required.sort();
    let canonical = CanonicalPhase {
        name: &phase.name,
        objective: &phase.objective,
        capabilities: caps,
        required_tool_calls: vec![],
        min_tool_calls: phase.min_tool_calls,
        max_tool_calls: phase.max_tool_calls,
        fresh_context: phase.fresh_context,
    };
    let mut value = json!({"version":4,"reviewedPhase":canonical,"phaseDigest":digest(serde_json::to_vec(&canonical)?),"requiredOutputs":required});
    if !inputs.is_empty() {
        #[derive(Serialize)]
        struct Bundle<'a> {
            schema: &'static str,
            inputs: &'a [Input],
        }
        value["inputDigest"] = digest(serde_json::to_vec(&Bundle {
            schema: "kars.mission-inputs/v1",
            inputs,
        })?)
        .into();
    }
    Ok(value)
}

/// Re-derived before workload projection, publication and child materialization.
/// Locator annotations alone never authorize a phase or an input.
pub(crate) async fn grant(client: &Client, task: &KarsTask) -> Result<Option<Grant>> {
    if plan(task).is_none() && ann(task, ROOT).is_none() {
        return Ok(None);
    }
    let root = if let Some(name) = ann(task, ROOT) {
        tasks(client, task).get(name).await?
    } else {
        task.clone()
    };
    let (map, s) = load(client, &root).await?;
    let (phase, sources, required, selected) = if ann(task, ROOT).is_some() {
        let key = ann(task, SLOT).ok_or_else(|| anyhow::anyhow!("missing reviewed slot"))?;
        let mut selected = None;
        for (r, role) in s.plan.roles.iter().enumerate() {
            for p in 0..role.phases.len() {
                if slot(r, Some(p)) == key {
                    selected = Some((r, p));
                }
            }
        }
        let (r, p) =
            selected.ok_or_else(|| anyhow::anyhow!("role budget parent cannot execute"))?;
        let actual = existing(client, &root, &map, &s, r, p)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unreserved phase"))?;
        ensure!(
            actual.metadata.uid == task.metadata.uid
                && actual.metadata.resource_version == task.metadata.resource_version
                && launched(&actual),
            "phase authority changed"
        );
        (
            s.plan.roles[r].phases[p].clone(),
            dependencies(&s, r, p),
            vec![HANDOFF.into()],
            key.to_string(),
        )
    } else {
        (
            ExecutionPhase {
                name: "synthesis".into(),
                objective: s.plan.synthesis.objective.clone(),
                capabilities: s.plan.synthesis.capabilities.clone(),
                required_tool_calls: vec![],
                min_tool_calls: 0,
                max_tool_calls: s.plan.synthesis.max_tool_calls,
                fresh_context: true,
            },
            s.plan
                .roles
                .iter()
                .enumerate()
                .map(|(r, role)| (r, role.phases.len() - 1))
                .collect(),
            s.plan.deliverables.iter().map(|d| d.name.clone()).collect(),
            "synthesis".into(),
        )
    };
    let inputs = inputs(client, &root, &map, &s, &sources)
        .await?
        .ok_or_else(|| anyhow::anyhow!("reviewed dependencies not successful"))?;
    let contract = contract(&phase, &inputs, required)?;
    let references: Vec<_> = inputs
        .iter()
        .map(|i| json!({"name":i.name,"source":i.source,"sha256":i.sha256}))
        .collect();
    let fresh = maps(client, &root).get(&map.name_any()).await?;
    ensure!(
        fresh == map,
        "reviewed reservations changed during admission"
    );
    current(client, &root).await?;
    current(client, task).await?;
    Ok(Some(Grant {
        contract,
        inputs: json!(references),
        authority: json!({"rootName":root.name_any(),"rootUid":root.metadata.uid,"stateName":map.name_any(),"stateUid":map.metadata.uid,"slot":selected}),
    }))
}

#[cfg(test)]
mod tests;
