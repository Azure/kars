// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reviewed execution uses the ordinary Task authority, budget and sandbox lifecycles.
//! A role is a governed budget parent; each phase is a fresh executable child.
use crate::{
    kars_task::{KarsTask, TaskExecution},
    task_execution_plan::{ExecutionPhase, ExecutionPlan, ExecutionRequiredToolCall},
};
use anyhow::{Result, bail, ensure};
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

pub(crate) const MAX_INPUT_BYTES: usize = 192 * 1024;
const MAX_ASSIGNMENT_INPUT_BYTES: usize = 384 * 1024;
pub(crate) const MAX_RECALL_FILES: usize = 8;
// Eight final-phase producers, plus sealed memory and revision documents.
const MAX_ASSIGNMENT_INPUTS: usize = 8 * 16 + MAX_RECALL_FILES + 16;

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
pub(crate) const TEAM_REVISION: &str = "kars.azure.com/reviewed-team-revision";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_revision: Option<String>,
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
pub(crate) use retention::{retained_failed_team_documents, retained_team_run};
mod team_roles;
pub(crate) use team_roles::seal as seal_team_roles;

use crate::team_memory::{Document as Input, Source};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct InputReference {
    name: String,
    source: Source,
    sha256: String,
}
impl From<&Input> for InputReference {
    fn from(input: &Input) -> Self {
        Self {
            name: input.name.clone(),
            source: input.source.clone(),
            sha256: input.sha256.clone(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionRole {
    name: String,
    phases: Vec<String>,
}

pub(crate) fn revision_roles(plan: &ExecutionPlan) -> Vec<RevisionRole> {
    plan.roles
        .iter()
        .map(|role| RevisionRole {
            name: role.name.clone(),
            phases: role.phases.iter().map(|phase| phase.name.clone()).collect(),
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionManifest {
    schema: String,
    inputs: Vec<InputReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    diagnostic_roles: Vec<RevisionRole>,
}
const REVISION_MANIFEST_SCHEMA: &str = "kars.team-revision-inputs/v1";

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
/// A scheduling hint only; retirement still requires live authority and handoff checks.
pub(crate) fn phase_completion_recorded(task: &KarsTask) -> bool {
    ann(task, ROOT).is_some_and(|root| !root.is_empty())
        && ann(task, RUN)
            .filter(|nonce| !nonce.is_empty())
            .is_some_and(|nonce| ann(task, COMPLETE) == Some(nonce))
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
#[derive(Debug, thiserror::Error)]
#[error("reviewed Task changed during reconciliation")]
pub(crate) struct TaskSnapshotChanged;

async fn current(client: &Client, task: &KarsTask) -> Result<()> {
    let fresh = tasks(client, task).get(&task.name_any()).await?;
    live(&fresh.metadata)?;
    ensure!(
        fresh.metadata.uid == task.metadata.uid,
        "reviewed Task changed during reconciliation"
    );
    ensure!(
        fresh.metadata.resource_version == task.metadata.resource_version,
        TaskSnapshotChanged
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
        if p.roles.is_empty() {
            0
        } else {
            crate::task_execution_plan::REVIEWED_TASK_DELEGATION_DEPTH
        },
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
        matches!(
            s.schema.as_str(),
            "kars.reviewed-run/v1" | "kars.reviewed-run/v2"
        ) && Some(&s.root_uid) == root.metadata.uid.as_ref()
            && s.root_name == root.name_any()
            && s.admission == admission(root)?
            && &s.plan == p
            && s.plan_digest == plan_digest(p)?
            && s.team_roles.as_deref() == ann(root, TEAM_ROLES)
            && s.team_memory.as_deref() == ann(root, TEAM_MEMORY)
            && s.team_revision.as_deref() == ann(root, TEAM_REVISION)
            && map.name_any() == state_name(root, &s.admission),
        "reviewed state authority changed"
    );
    Ok(s)
}
async fn load(client: &Client, root: &KarsTask) -> Result<(ConfigMap, State)> {
    validate(root)?;
    ensure!(
        launched(root)
            && !crate::kars_task_reconciler::halt::halted(root)
            && crate::kars_task_reconciler::task_is_ready(root),
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
    ensure!(
        !ann(root, RUN)
            .filter(|nonce| !nonce.is_empty())
            .is_some_and(|nonce| ann(root, COMPLETE) == Some(nonce)),
        "CompletedReviewedRun: a completed run cannot reserve new reviewed phases; request a new revision before relaunching"
    );
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
            schema: "kars.reviewed-run/v2".into(),
            root_name: root.name_any(),
            root_uid: root.uid().unwrap(),
            admission: a,
            plan_digest: plan_digest(&p)?,
            plan: p,
            team_roles: ann(root, TEAM_ROLES).map(str::to_owned),
            team_memory: ann(root, TEAM_MEMORY).map(str::to_owned),
            team_revision: ann(root, TEAM_REVISION).map(str::to_owned),
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
    ensure!(
        !crate::kars_task_reconciler::halt::halted(actual)
            && !crate::kars_task_reconciler::halt::halted(parent),
        "reviewed Task halted; refusing reactivation or synthesis"
    );
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
    peers: Option<&Value>,
) -> Result<Option<Input>> {
    Ok(phase_inputs(client, task, phase, name, peers)
        .await?
        .and_then(|inputs| {
            inputs
                .into_iter()
                .find(|input| input.source.artifact_name == HANDOFF)
        }))
}

async fn phase_inputs(
    client: &Client,
    task: &KarsTask,
    phase: &ExecutionPhase,
    name: String,
    peers: Option<&Value>,
) -> Result<Option<Vec<Input>>> {
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
    let record = crate::mission_record::decode(encoded)?;
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
    evidence::success(&record, task, phase, peers)?;
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
    Ok(Some(aliased_phase_documents(&record, source, name)?))
}

fn aliased_phase_documents(record: &Value, source: Source, name: String) -> Result<Vec<Input>> {
    let prefix = name.strip_suffix(".md").unwrap_or(&name);
    let mut result = vec![];
    let mut documents: Vec<_> = record["reply"]["artifacts"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing phase artifacts"))?
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "response.md" | "kars-router-observations.json"
            )
        })
        .collect();
    // JSON preserves runtime insertion order; the dispatcher validates lexical aliases.
    documents.sort_by_key(|(name, _)| *name);
    for (index, (artifact_name, content)) in documents.into_iter().enumerate() {
        let content = content
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid phase artifact content"))?;
        let mut source = source.clone();
        source.artifact_name = artifact_name.clone();
        let input_name = if artifact_name == HANDOFF {
            name.clone()
        } else {
            let mut readable = format!("{prefix}--{index}-{artifact_name}");
            readable.truncate(128);
            readable
        };
        result.push(Input {
            name: input_name,
            source,
            sha256: digest(content),
            content: content.into(),
        });
    }
    Ok(result)
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
    let historical = retained_base_inputs(client, root).await?;
    let mut peers = Vec::new();
    for &(r, p) in sources {
        let Some(task) = existing(client, root, map, s, r, p).await? else {
            return Ok(None);
        };
        let Some(phase_inputs) = phase_inputs(
            client,
            &task,
            &s.plan.roles[r].phases[p],
            format!("r{r}-p{p}.md"),
            peer_context(s, map, &slot(r, Some(p))).as_ref(),
        )
        .await?
        else {
            return Ok(None);
        };
        peers.extend(phase_inputs);
    }
    let mut result = route_revision_inputs(root, historical, &peers)?;
    result.extend(peers);
    result.sort_by(|a, b| a.name.cmp(&b.name));
    ensure!(
        result.len() <= MAX_ASSIGNMENT_INPUTS
            && serde_json::to_vec(&result)?.len() <= MAX_ASSIGNMENT_INPUT_BYTES,
        "reviewed retained inputs exceed the Mission transport capacity"
    );
    Ok(Some(result))
}

pub(crate) fn seal_team_revision(
    root: &mut KarsTask,
    documents: &[Input],
    diagnostic_roles: &[RevisionRole],
) -> Result<()> {
    if !documents.is_empty() {
        validate_revision_documents(root, documents)?;
        let raw = if documents
            .iter()
            .any(|d| d.name.starts_with("revision-unaccepted-"))
        {
            let manifest = RevisionManifest {
                schema: REVISION_MANIFEST_SCHEMA.into(),
                inputs: documents.iter().map(InputReference::from).collect(),
                diagnostic_roles: diagnostic_roles.to_vec(),
            };
            validate_revision_roles(&manifest)?;
            serde_json::to_string(&manifest)?
        } else {
            // Successful rejected deliveries retain their established inline format.
            let raw = serde_json::to_string(documents)?;
            validate_revision(root, &raw)?;
            raw
        };
        ensure!(
            raw.len() <= MAX_INPUT_BYTES,
            "sealed revision exceeds annotation capacity"
        );
        root.annotations_mut().insert(TEAM_REVISION.into(), raw);
    }
    Ok(())
}

fn validate_revision(root: &KarsTask, raw: &str) -> Result<Vec<Input>> {
    ensure!(
        raw.len() <= MAX_INPUT_BYTES,
        "sealed revision exceeds Mission input capacity"
    );
    let documents: Vec<Input> = serde_json::from_str(raw)?;
    ensure!(
        documents.len() <= 16,
        "invalid inline revision document count"
    );
    validate_revision_documents(root, &documents)?;
    Ok(documents)
}

fn validate_revision_documents(root: &KarsTask, documents: &[Input]) -> Result<()> {
    ensure!(
        serde_json::to_vec(documents)?.len() <= MAX_ASSIGNMENT_INPUT_BYTES,
        "revision documents exceed Mission transport capacity"
    );
    for document in documents {
        ensure!(
            !document.content.trim().is_empty()
                && document.content.len() <= 65536
                && document.sha256 == digest(&document.content),
            "sealed revision content changed"
        );
    }
    validate_revision_sources(
        root,
        &documents
            .iter()
            .map(InputReference::from)
            .collect::<Vec<_>>(),
    )
}

fn validate_revision_sources(root: &KarsTask, documents: &[InputReference]) -> Result<()> {
    ensure!(
        !documents.is_empty() && documents.len() <= MAX_ASSIGNMENT_INPUTS,
        "invalid revision document count"
    );
    let mut producers = BTreeMap::<String, &Source>::new();
    let mut names = std::collections::BTreeSet::new();
    let mut diagnostics_started = false;
    for (index, document) in documents.iter().enumerate() {
        let source = &document.source;
        let group = if let Some(alias) = document.name.strip_prefix("revision-unaccepted-") {
            diagnostics_started = true;
            ensure!(
                document.name.len() <= 128
                    && document
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
                "invalid diagnostic revision alias"
            );
            let group = alias
                .split_once("--")
                .filter(|(_, suffix)| !suffix.is_empty())
                .map(|(group, _)| group)
                .or_else(|| alias.strip_suffix(".md"))
                .ok_or_else(|| anyhow::anyhow!("invalid diagnostic phase alias"))?;
            let (role, phase) = group
                .strip_prefix('r')
                .and_then(|s| s.split_once("-p"))
                .ok_or_else(|| anyhow::anyhow!("invalid diagnostic phase alias"))?;
            for number in [role, phase] {
                ensure!(
                    number
                        .parse::<usize>()
                        .ok()
                        .is_some_and(|n| n.to_string() == number),
                    "invalid diagnostic phase index"
                );
            }
            group.to_owned()
        } else {
            ensure!(
                !diagnostics_started && document.name == format!("revision-{index}"),
                "invalid rejected-delivery revision alias"
            );
            "delivery".into()
        };
        let original = *producers.entry(group).or_insert(source);
        ensure!(
            names.insert(document.name.clone())
                && document
                    .sha256
                    .strip_prefix("sha256:")
                    .is_some_and(|hash| hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                && !["response.md", "kars-router-observations.json"]
                    .contains(&source.artifact_name.as_str())
                && source.namespace == root.namespace().unwrap_or_default()
                && source.namespace == original.namespace
                && source.task_name == original.task_name
                && source.task_uid == original.task_uid
                && source.run_nonce == original.run_nonce
                && source.assignment_id == original.assignment_id
                && source.sandbox_uid == original.sandbox_uid
                && source.pod_uid == original.pod_uid
                && source.agent_did == original.agent_did
                && [
                    &source.task_name,
                    &source.task_uid,
                    &source.run_nonce,
                    &source.assignment_id,
                    &source.sandbox_uid,
                    &source.pod_uid,
                    &source.agent_did,
                    &source.artifact_name
                ]
                .iter()
                .all(|value| !value.trim().is_empty() && value.len() <= 253),
            "sealed revision content or provenance changed"
        );
    }
    Ok(())
}

fn revision_manifest(root: &KarsTask) -> Result<Option<RevisionManifest>> {
    let Some(raw) = ann(root, TEAM_REVISION) else {
        return Ok(None);
    };
    ensure!(
        raw.len() <= MAX_INPUT_BYTES,
        "sealed revision exceeds annotation capacity"
    );
    if raw.trim_start().starts_with('[') {
        return Ok(None);
    }
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    Ok(Some(parse_revision_manifest(root, raw)?))
}

fn parse_revision_manifest(root: &KarsTask, raw: &str) -> Result<RevisionManifest> {
    ensure!(
        raw.len() <= MAX_INPUT_BYTES,
        "sealed revision exceeds annotation capacity"
    );
    let manifest: RevisionManifest = serde_json::from_str(raw)?;
    ensure!(
        manifest.schema == REVISION_MANIFEST_SCHEMA,
        "unknown revision manifest schema"
    );
    validate_revision_sources(root, &manifest.inputs)?;
    validate_revision_roles(&manifest)?;
    Ok(manifest)
}

pub(crate) fn staged_team_input_count(root: &KarsTask) -> Result<usize> {
    // Admission has verified the reservation seal, but the Task is not admitted yet.
    let mut count = match ann(root, TEAM_MEMORY) {
        Some(raw) => serde_json::from_str::<Vec<Input>>(raw)?.len(),
        None => 0,
    };
    if let Some(raw) = ann(root, TEAM_REVISION) {
        count += if raw.trim_start().starts_with('[') {
            validate_revision(root, raw)?.len()
        } else {
            parse_revision_manifest(root, raw)?.inputs.len()
        };
    }
    Ok(count)
}

fn revision_references(root: &KarsTask) -> Result<Option<Vec<InputReference>>> {
    Ok(revision_manifest(root)?.map(|manifest| manifest.inputs))
}

fn input_coordinates(name: &str) -> Option<(usize, usize)> {
    let alias = name.strip_prefix("revision-unaccepted-").unwrap_or(name);
    let phase = alias
        .split_once("--")
        .map(|(phase, _)| phase)
        .or_else(|| alias.strip_suffix(".md"))?;
    let (r, p) = phase.strip_prefix('r')?.split_once("-p")?;
    Some((r.parse().ok()?, p.parse().ok()?))
}

fn validate_revision_roles(manifest: &RevisionManifest) -> Result<()> {
    let roles = &manifest.diagnostic_roles;
    ensure!(roles.len() <= 8, "excessive diagnostic role routing");
    let valid_name = |name: &str| !name.is_empty() && name.len() <= 128;
    ensure!(
        roles.iter().all(|role| valid_name(&role.name)
            && !role.phases.is_empty()
            && role.phases.len() <= 8
            && role.phases.iter().all(|name| valid_name(name))
            && role
                .phases
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == role.phases.len())
            && roles
                .iter()
                .map(|role| &role.name)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == roles.len(),
        "invalid diagnostic role routing"
    );
    if !roles.is_empty() {
        for input in &manifest.inputs {
            if input.name.starts_with("revision-unaccepted-") {
                let (r, p) = input_coordinates(&input.name)
                    .ok_or_else(|| anyhow::anyhow!("invalid diagnostic routing coordinates"))?;
                ensure!(
                    roles
                        .get(r)
                        .is_some_and(|role| p.checked_add(1) == Some(role.phases.len())),
                    "diagnostic routing is not a retained final phase"
                );
            }
        }
    }
    Ok(())
}

fn selected_revision_references(
    root: &KarsTask,
    peers: &[InputReference],
) -> Result<Option<Vec<InputReference>>> {
    let Some(manifest) = revision_manifest(root)? else {
        return Ok(None);
    };
    let current =
        plan(root).ok_or_else(|| anyhow::anyhow!("revision routing requires a reviewed plan"))?;
    let RevisionManifest {
        inputs,
        diagnostic_roles,
        ..
    } = manifest;
    Ok(Some(
        inputs
            .into_iter()
            .filter(|historical| {
                if !historical.name.starts_with("revision-unaccepted-") {
                    return true;
                }
                let Some((r, p)) = input_coordinates(&historical.name) else {
                    return true;
                };
                let Some(role) = diagnostic_roles.get(r) else {
                    return true;
                };
                !peers.iter().any(|peer| {
                    let Some((r, p_current)) = input_coordinates(&peer.name) else {
                        return false;
                    };
                    current.roles.get(r).is_some_and(|current_role| {
                        current_role.name == role.name
                            && current_role
                                .phases
                                .get(p_current)
                                .is_some_and(|phase| phase.name == role.phases[p])
                            && peer.source.artifact_name == historical.source.artifact_name
                    })
                })
            })
            .collect(),
    ))
}

fn route_revision_inputs(
    root: &KarsTask,
    historical: Vec<Input>,
    peers: &[Input],
) -> Result<Vec<Input>> {
    let Some(selected) = selected_revision_references(
        root,
        &peers.iter().map(InputReference::from).collect::<Vec<_>>(),
    )?
    else {
        return Ok(historical);
    };
    Ok(historical
        .into_iter()
        .filter(|input| {
            !input.name.starts_with("revision-")
                || selected
                    .iter()
                    .any(|reference| *reference == InputReference::from(input))
        })
        .collect())
}

async fn retained_base_inputs(client: &Client, root: &KarsTask) -> Result<Vec<Input>> {
    let Some(references) = revision_references(root)? else {
        return base_inputs(root);
    };
    let mut documents = memory_inputs(root)?;
    documents.extend(retention::referenced_documents(client, root, &references).await?);
    ensure!(
        documents.len() <= MAX_ASSIGNMENT_INPUTS
            && serde_json::to_vec(&documents)?.len() <= MAX_ASSIGNMENT_INPUT_BYTES,
        "reviewed retained inputs exceed the Mission transport capacity"
    );
    Ok(documents)
}

fn record_base_inputs(root: &KarsTask, actual: &[Input], peers: &[Input]) -> Result<Vec<Input>> {
    let Some(references) = selected_revision_references(
        root,
        &peers.iter().map(InputReference::from).collect::<Vec<_>>(),
    )?
    else {
        return base_inputs(root);
    };
    // The canonical attempt already retains the resolved bytes. Compare them to
    // the original seal rather than recursively reopening older revision graphs.
    let documents = references
        .iter()
        .map(|reference| {
            actual
                .iter()
                .find(|document| InputReference::from(*document) == *reference)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("retained revision input differs from its seal"))
        })
        .collect::<Result<Vec<_>>>()?;
    if !documents.is_empty() {
        validate_revision_documents(root, &documents)?;
    }
    let mut result = memory_inputs(root)?;
    result.extend(documents);
    Ok(result)
}

fn base_inputs(root: &KarsTask) -> Result<Vec<Input>> {
    let mut documents = memory_inputs(root)?;
    if let Some(raw) = ann(root, TEAM_REVISION) {
        crate::kars_team_reconciler::requests::validate_work_run(root)?;
        documents.extend(validate_revision(root, raw)?);
    }
    Ok(documents)
}

fn memory_inputs(root: &KarsTask) -> Result<Vec<Input>> {
    let Some(raw) = ann(root, TEAM_MEMORY) else {
        return Ok(vec![]);
    };
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    ensure!(
        raw.len() <= MAX_INPUT_BYTES,
        "sealed Team memory exceeds Mission input capacity"
    );
    let inputs: Vec<Input> = serde_json::from_str(raw)?;
    ensure!(
        !inputs.is_empty() && inputs.len() <= MAX_RECALL_FILES,
        "sealed Team memory exceeds its input count"
    );
    let mut names = std::collections::BTreeSet::new();
    for input in &inputs {
        ensure!(
            input.name.starts_with("memory-")
                && input.name.len() <= 128
                && input
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                && names.insert(&input.name)
                && input.sha256 == digest(&input.content)
                && input.source.namespace == root.namespace().unwrap_or_default(),
            "sealed Team memory identity or content is invalid"
        );
    }
    Ok(inputs)
}

/// Verify failed/rejected publication separately from successful handoff custody.
/// Uncertain, in-flight, incomplete or mismatched evidence never retires compute.
async fn failed_phase_evidence(
    client: &Client,
    task: &KarsTask,
    phase: &ExecutionPhase,
    peers: Option<&Value>,
) -> Result<Option<Value>> {
    Ok(
        failed_evidence(client, task, phase, vec![HANDOFF.into()], peers)
            .await?
            .map(|(record, _)| record),
    )
}

async fn failed_evidence(
    client: &Client,
    task: &KarsTask,
    phase: &ExecutionPhase,
    required: Vec<String>,
    peers: Option<&Value>,
) -> Result<Option<(Value, ConfigMap)>> {
    let nonce = ann(task, RUN).ok_or_else(|| anyhow::anyhow!("phase run missing"))?;
    if ann(task, COMPLETE) != Some(nonce) {
        return Ok(None);
    }
    let key = digest(serde_json::to_vec(&json!([task.metadata.uid, nonce]))?)[7..47].to_string();
    let api = maps(client, task);
    let Some(attempt) = api.get_opt(&format!("kars-mission-attempt-{key}")).await? else {
        return Ok(None);
    };
    evidence::owned(&attempt, task)?;
    let encoded = attempt
        .data
        .as_ref()
        .and_then(|data| data.get("attempt.json"))
        .filter(|encoded| encoded.len() <= 900 * 1024)
        .ok_or_else(|| anyhow::anyhow!("missing or oversized phase attempt"))?;
    let record = crate::mission_record::decode(encoded)?;
    if !matches!(record["phase"].as_str(), Some("failed" | "rejected")) {
        return Ok(None);
    }
    evidence::failure_with_outputs(&record, task, phase, required, peers)?;
    let output = api.get(&format!("kars-mission-output-{key}")).await?;
    let artifacts = api.get(&format!("kars-mission-artifacts-{key}")).await?;
    evidence::canonical(&output, task, &key, "output")?;
    evidence::canonical(&artifacts, task, &key, "artifacts")?;
    evidence::projection(&record, &output, &artifacts)?;
    for prior in [&attempt, &output, &artifacts] {
        ensure!(
            api.get(&prior.name_any()).await? == *prior,
            "terminal evidence changed during retirement"
        );
    }
    current(client, task).await?;
    Ok(Some((record, output)))
}

fn failure_summary(task: &KarsTask, record: &Value) -> Result<BTreeMap<String, String>> {
    Ok(BTreeMap::from([
        ("status".into(), "failed".into()),
        (
            "finishedAt".into(),
            record["updatedAt"].as_str().unwrap().into(),
        ),
        (
            "error".into(),
            record["reply"]["error"].as_str().unwrap_or("").into(),
        ),
        ("sourceTask".into(), task.name_any()),
        ("sourceTaskUid".into(), task.uid().unwrap()),
        ("sourceRunNonce".into(), ann(task, RUN).unwrap().into()),
        (
            "sourceAttemptSha256".into(),
            digest(serde_json::to_vec(record)?),
        ),
    ]))
}

/// A failed specialist cannot produce principal synthesis. Return its verified
/// terminal provenance for ordinary Team work retirement, not a synthetic reply.
pub(crate) async fn failed_team_run(
    client: &Client,
    team: &crate::kars_team::KarsTeam,
    root: &KarsTask,
    retained_digest: Option<&str>,
) -> Result<Option<BTreeMap<String, String>>> {
    let Some(p) = plan(root) else { return Ok(None) };
    if !launched(root) || ann(root, PIN).is_none() {
        return Ok(None);
    }
    live(&root.metadata)?;
    ensure!(
        ann(root, ROOT).is_none()
            && root.namespace() == team.namespace()
            && root.metadata.owner_references.as_ref()
                == Some(&vec![crate::kars_team_reconciler::team_owner_ref(team)?]),
        "failed reviewed run is outside exact Team custody"
    );
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    validate(root)?;
    let (map, s) = load(client, root).await?;
    for (r, role) in p.roles.iter().enumerate() {
        for (index, phase) in role.phases.iter().enumerate() {
            let Some(task) = existing(client, root, &map, &s, r, index).await? else {
                continue;
            };
            if !phase_completion_recorded(&task) {
                continue;
            }
            let reviewed = grant(client, &task)
                .await?
                .ok_or_else(|| anyhow::anyhow!("failed reviewed phase grant missing"))?;
            let Some(record) =
                failed_phase_evidence(client, &task, phase, reviewed.contract.get("peerContext"))
                    .await?
            else {
                continue;
            };
            ensure!(
                maps(client, root).get(&map.name_any()).await? == map,
                "reviewed reservations changed during failure harvest"
            );
            current(client, root).await?;
            let data = failure_summary(&task, &record)?;
            // A parallel phase can fail after history was written but before the
            // root was idled. Reuse that exact checkpoint, never change its source.
            let candidate_digest =
                crate::providers::signing::sha256_hex(&serde_json::to_vec(&data)?);
            if retained_digest.is_none_or(|expected| candidate_digest == expected) {
                return Ok(Some(data));
            }
        }
    }
    ensure!(
        retained_digest.is_none(),
        "retained reviewed failure evidence changed or disappeared"
    );
    Ok(None)
}

/// Retire verified terminal phase compute or a stopped root's compute, retaining publication records.
pub(crate) async fn phase_compute_retirable(client: &Client, task: &KarsTask) -> Result<bool> {
    let Some(name) = ann(task, ROOT) else {
        return Ok(false);
    };
    let api = tasks(client, task);
    let root = api.get(name).await?;
    if launched(&root) {
        if !phase_completion_recorded(task) {
            return Ok(false);
        }
        let reviewed = grant(client, task)
            .await?
            .ok_or_else(|| anyhow::anyhow!("completed reviewed phase grant missing"))?;
        let phase: ExecutionPhase =
            serde_json::from_value(reviewed.contract["reviewedPhase"].clone())?;
        let peers = reviewed.contract.get("peerContext");
        if failed_phase_evidence(client, task, &phase, peers)
            .await?
            .is_some()
        {
            return Ok(true);
        }
        return Ok(handoff(client, task, &phase, "check.md".into(), peers)
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

async fn principal_dispatch_progress(client: &Client, task: &KarsTask) -> Result<()> {
    let nonce = ann(task, RUN).ok_or_else(|| anyhow::anyhow!("principal run missing"))?;
    let key = digest(serde_json::to_vec(&json!([task.metadata.uid, nonce]))?);
    let name = format!("kars-mission-attempt-{}", &key[7..47]);
    let Some(attempt) = maps(client, task).get_opt(&name).await? else {
        return Ok(());
    };
    evidence::owned(&attempt, task)?;
    let encoded = attempt
        .data
        .as_ref()
        .and_then(|data| data.get("attempt.json"))
        .ok_or_else(|| anyhow::anyhow!("missing principal attempt"))?;
    let record = crate::mission_record::decode(encoded)?;
    ensure!(
        attempt.name_any() == name
            && record["version"] == 1
            && record["candidate"]["namespace"].as_str() == task.namespace().as_deref()
            && ["candidate", "assignment"].iter().all(|field| {
                record[field]["taskUid"].as_str() == task.metadata.uid.as_deref()
                    && record[field]["taskName"].as_str() == Some(task.name_any().as_str())
                    && record[field]["runNonce"].as_str() == Some(nonce)
            }),
        "principal attempt does not match the current Task and run"
    );
    ensure!(
        record["phase"] != "uncertain",
        "Final assembly needs attention: Kars could not confirm the outcome of its recorded attempt. Completed role work is retained. Inspect the attempt before starting a fresh run; this attempt will not be sent again."
    );
    ensure!(
        matches!(
            record["phase"].as_str(),
            Some("dispatching" | "accepted" | "running" | "succeeded" | "failed" | "rejected")
        ),
        "unsupported principal attempt state"
    );
    Ok(())
}

/// True permits the ordinary materializer; false leaves the root governed but waiting.
pub(crate) async fn reconcile(client: &Client, task: &KarsTask) -> Result<bool> {
    ensure!(
        !crate::kars_task_reconciler::halt::halted(task),
        "reviewed Task halted"
    );
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
    let (mut map, mut s) = load(client, task).await?;
    let mut done = vec![false; p.roles.len()];
    for (r, role) in p.roles.iter().enumerate() {
        if let Some(last) = existing(client, task, &map, &s, r, role.phases.len() - 1).await? {
            done[r] = handoff(
                client,
                &last,
                &role.phases[role.phases.len() - 1],
                "check.md".into(),
                peer_context(&s, &map, &slot(r, Some(role.phases.len() - 1))).as_ref(),
            )
            .await?
            .is_some();
        }
    }
    let mut phase_hold = None;
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
        let active = done
            .iter()
            .enumerate()
            .filter(|(index, completed)| {
                !**completed && s.children.contains_key(&slot(*index, None))
            })
            .count();
        if !s.children.contains_key(&slot(r, None)) && active >= p.max_parallel as usize {
            continue;
        }
        let Some(parent) = ensure_child(client, task, task, &map, &s, r, None).await? else {
            // Refresh the reservation CAS and capacity before advancing independent roles.
            current(client, task).await?;
            (map, s) = load(client, task).await?;
            continue;
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
                current(client, task).await?;
                (map, s) = load(client, task).await?;
                break;
            };
            if handoff(
                client,
                &child,
                &role.phases[phase],
                "check.md".into(),
                peer_context(&s, &map, &slot(r, Some(phase))).as_ref(),
            )
            .await?
            .is_none()
            {
                if let Some(status) = child.status.as_ref().filter(|status| {
                    child.metadata.generation.is_some()
                        && status.observed_generation == child.metadata.generation
                }) && status.execution_phase.as_deref() == Some("Degraded")
                {
                    phase_hold.get_or_insert_with(|| {
                            format!(
                                "ReviewedPhaseHeld: role '{}' phase '{}' (task '{}'): {}",
                                role.name,
                                role.phases[phase].name,
                                child.name_any(),
                                status.execution_detail.as_deref().unwrap_or(
                                    "execution is degraded; inspect the phase before starting a fresh run"
                                )
                            )
                        });
                }
                break;
            }
        }
    }
    if let Some(detail) = phase_hold {
        bail!(detail);
    }
    if done.iter().all(|v| *v) {
        principal_dispatch_progress(client, task).await?;
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
    required_tool_calls: &'a [ExecutionRequiredToolCall],
    min_tool_calls: i32,
    max_tool_calls: i32,
    fresh_context: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_mode: Option<&'a str>,
}
fn peer_context(s: &State, map: &ConfigMap, own_slot: &str) -> Option<Value> {
    if s.schema != "kars.reviewed-run/v2" {
        return None;
    }
    let name = map.name_any();
    let peers: Vec<_> = s
        .plan
        .roles
        .iter()
        .enumerate()
        .flat_map(|(r, role)| {
            let name = &name;
            role.phases.iter().enumerate().map(move |(p, phase)| {
                let key = slot(r, Some(p));
                json!({"role":role.name, "phase":phase.name, "slot":key,
                "agentName":format!("rp-{}-{key}", &name[14..34])})
            })
        })
        .collect();
    Some(json!({"rootUid":s.root_uid, "stateName":name, "slot":own_slot, "peers":peers}))
}

fn peer_contract(
    phase: &ExecutionPhase,
    inputs: &[Input],
    required: Vec<String>,
    peers: Option<&Value>,
) -> Result<Value> {
    let mut value = contract(phase, inputs, required)?;
    if let Some(peers) = peers {
        value["version"] = json!(5);
        value["peerContext"] = peers.clone();
    }
    Ok(value)
}

fn contract(phase: &ExecutionPhase, inputs: &[Input], mut required: Vec<String>) -> Result<Value> {
    let mut caps: Vec<_> = phase.capabilities.iter().map(String::as_str).collect();
    caps.sort();
    required.sort();
    let canonical = CanonicalPhase {
        name: &phase.name,
        objective: &phase.objective,
        capabilities: caps,
        required_tool_calls: &phase.required_tool_calls,
        min_tool_calls: phase.min_tool_calls,
        max_tool_calls: phase.max_tool_calls,
        fresh_context: phase.fresh_context,
        execution_mode: phase.execution_mode.as_deref(),
    };
    let mut value = json!({"version":4,"reviewedPhase":canonical,"phaseDigest":digest(serde_json::to_vec(&canonical)?),"requiredOutputs":required});
    if !inputs.is_empty() {
        ensure!(
            phase
                .capabilities
                .iter()
                .any(|cap| cap == "filesystem-read")
                && phase.max_tool_calls >= 1,
            "ReviewedInputCapabilityRequired: phase '{}' has retained inputs but cannot read them; review its filesystem-read capability and tool allowance before starting a fresh run",
            phase.name
        );
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
                execution_mode: s.plan.synthesis.execution_mode.clone(),
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
    let contract = peer_contract(
        &phase,
        &inputs,
        required,
        peer_context(&s, &map, &selected).as_ref(),
    )?;
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
#[cfg(test)]
pub(crate) use tests::lifecycle_tests::{
    completed_team_fixture, failed_revision_team_fixture, failed_team_fixture,
};
