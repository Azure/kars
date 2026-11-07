// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read the controller's reserved phase tree, not name-derived roster activity.

use anyhow::{Context, Result, ensure};
use k8s_openapi::{api::core::v1::ConfigMap, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{Resource, ResourceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[cfg(test)]
use super::run_evidence::mission_result;
use super::{MissionResultDto, revision::output_matches_task, run_evidence::live};
use super::{artifacts::build_artifact_set, router_activity};
use crate::{
    auth::Principal,
    kars::{
        cluster::Cluster,
        task::{KarsTask, execution_plan::ExecutionPlan},
    },
    routes::ownership::task_is_owned_by,
};

const PIN: &str = "kars.azure.com/reviewed-state";
const RUN: &str = "kars.azure.com/run-requested";
const TEAM_ROLES: &str = "kars.azure.com/reviewed-team-roles";
const TEAM_MEMORY: &str = "kars.azure.com/reviewed-team-memory";
const TEAM_REVISION: &str = "kars.azure.com/reviewed-team-revision";

#[derive(Debug, Serialize)]
pub struct ReviewedExecutionDto {
    pub run_nonce: String,
    pub phases: Vec<ReviewedPhaseDto>,
    pub issue: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReviewedPhaseDto {
    pub role: String,
    pub phase: String,
    pub task_name: Option<String>,
    pub task_uid: Option<String>,
    pub run_nonce: Option<String>,
    pub state: &'static str,
    pub result: Option<MissionResultDto>,
    /// Safe attempt diagnostic only; never uncommitted output or delivery evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_diagnostic: Option<String>,
    pub router_activity: Option<router_activity::RouterActivity>,
    pub(crate) stopped_activity: Option<live::LiveActivity>,
    #[serde(skip)]
    pub(crate) live_activity: Option<live::LiveActivity>,
}

#[derive(Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Pin {
    name: String,
    uid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    name: String,
    uid: String,
    spec: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    schema: String,
    root_name: String,
    root_uid: String,
    admission: Value,
    plan_digest: String,
    plan: ExecutionPlan,
    #[serde(default)]
    team_roles: Option<String>,
    #[serde(default)]
    team_memory: Option<String>,
    #[serde(default)]
    team_revision: Option<String>,
    children: BTreeMap<String, Reservation>,
}

fn digest(value: &impl Serialize) -> Result<String> {
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
    let mut value = serde_json::to_value(value)?;
    sort(&mut value);
    Ok(format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(&serde_json::to_vec(&value)?)
    ))
}

fn owned(meta: &ObjectMeta, parent: &KarsTask) -> bool {
    meta.namespace == parent.metadata.namespace
        && meta.deletion_timestamp.is_none()
        && meta.uid.as_ref().is_some_and(|uid| !uid.is_empty())
        && parent
            .controller_owner_ref(&())
            .is_some_and(|owner| meta.owner_references.as_deref() == Some(&[owner]))
}

fn parse_state(map: &ConfigMap, root: &KarsTask, pin: &Pin) -> Result<State> {
    let current_pin: Pin =
        serde_json::from_str(root.annotations().get(PIN).context("root pin missing")?)?;
    ensure!(current_pin == *pin, "root reviewed pin changed");
    ensure!(
        map.name_any() == pin.name
            && map.uid().as_deref() == Some(&pin.uid)
            && owned(&map.metadata, root),
        "reserved reviewed state identity changed"
    );
    let raw = map
        .data
        .as_ref()
        .and_then(|data| data.get("state.json"))
        .context("reviewed state has no state.json")?;
    ensure!(
        raw.len() <= 1024 * 1024,
        "reviewed state exceeds read limit"
    );
    let state: State = serde_json::from_str(raw)?;
    let plan = root
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.execution_plan.as_ref())
        .context("reviewed plan removed")?;
    ensure!(
        matches!(
            state.schema.as_str(),
            "kars.reviewed-run/v1" | "kars.reviewed-run/v2"
        ) && root.uid().as_deref() == Some(&state.root_uid)
            && state.root_name == root.name_any()
            && state.admission["state"] == "run"
            && state.admission["runNonce"].as_str()
                == root.annotations().get(RUN).map(String::as_str)
            && root
                .annotations()
                .get(RUN)
                .is_some_and(|nonce| !nonce.is_empty())
            && &state.plan == plan
            && state.plan_digest == digest(plan)?
            && state.team_roles.as_ref() == root.annotations().get(TEAM_ROLES)
            && state.team_memory.as_ref() == root.annotations().get(TEAM_MEMORY)
            && state.team_revision.as_ref() == root.annotations().get(TEAM_REVISION),
        "reviewed state does not match the current root revision and plan"
    );
    ensure!(
        state
            .plan
            .roles
            .iter()
            .map(|role| role.phases.len())
            .sum::<usize>()
            <= 64,
        "reviewed phase tree exceeds read limit"
    );
    Ok(state)
}

fn child(
    task: &KarsTask,
    parent: &KarsTask,
    root: &KarsTask,
    reservation: &Reservation,
    pin: &Pin,
    slot: &str,
) -> Result<()> {
    let current_pin: Pin =
        serde_json::from_str(task.annotations().get(PIN).context("phase pin missing")?)?;
    let mut spec = serde_json::to_value(&task.spec)?;
    ensure!(
        reservation.spec["execution"]["launch"] == true
            || !task
                .spec
                .execution
                .as_ref()
                .is_some_and(|execution| execution.launch),
        "reviewed role budget parent cannot execute"
    );
    // Completion idles the phase without changing its reserved authority.
    if let Some(execution) = spec.get_mut("execution").and_then(Value::as_object_mut) {
        execution.insert(
            "launch".into(),
            reservation.spec["execution"]["launch"].clone(),
        );
    }
    ensure!(
        owned(&task.metadata, parent)
            && task.name_any() == reservation.name
            && task.uid().as_deref() == Some(&reservation.uid)
            && current_pin == *pin
            && task
                .spec
                .parent_ref
                .as_ref()
                .is_some_and(|reference| reference.name == parent.name_any())
            && task.annotations().get("kars.azure.com/reviewed-root") == Some(&root.name_any())
            && task
                .annotations()
                .get("kars.azure.com/reviewed-slot")
                .map(String::as_str)
                == Some(slot)
            && spec == reservation.spec,
        "reserved phase Task custody or specification changed"
    );
    let suffix = pin
        .name
        .strip_prefix("kars-reviewed-")
        .context("invalid reviewed state name")?;
    ensure!(
        task.annotations().get(RUN) == Some(&format!("plan-{suffix}-{slot}")),
        "reserved phase run nonce changed"
    );
    Ok(())
}

fn reserved_member(
    map: &ConfigMap,
    root: &KarsTask,
    task: &KarsTask,
    parent: &KarsTask,
    pin: &Pin,
) -> Result<()> {
    let state = parse_state(map, root, pin)?;
    let slot = task
        .annotations()
        .get("kars.azure.com/reviewed-slot")
        .context("reserved slot missing")?;
    let role_slot = state
        .plan
        .roles
        .iter()
        .enumerate()
        .find_map(|(r, role)| {
            let role_slot = format!("r{r}");
            (slot == &role_slot
                || role
                    .phases
                    .iter()
                    .enumerate()
                    .any(|(p, _)| slot == &format!("r{r}-p{p}")))
            .then_some(role_slot)
        })
        .context("reserved slot is outside the reviewed plan")?;
    let reservation = state
        .children
        .get(slot)
        .context("reserved member missing")?;
    if slot == &role_slot {
        ensure!(
            parent.uid() == root.uid() && parent.name_any() == root.name_any(),
            "reserved role parent changed"
        );
    } else {
        child(
            parent,
            root,
            root,
            state
                .children
                .get(&role_slot)
                .context("reserved role missing")?,
            pin,
            &role_slot,
        )?;
    }
    child(task, parent, root, reservation, pin, slot)
}

async fn verify_reserved_member(
    cluster: &Cluster,
    task: &KarsTask,
    principal: &Principal,
) -> Result<()> {
    let ns = task
        .namespace()
        .context("reserved member namespace missing")?;
    let root_name = task
        .annotations()
        .get("kars.azure.com/reviewed-root")
        .context("reserved root missing")?;
    let parent_name = &task
        .spec
        .parent_ref
        .as_ref()
        .context("reserved parent missing")?
        .name;
    let pin: Pin = serde_json::from_str(
        task.annotations()
            .get(PIN)
            .context("reserved pin missing")?,
    )?;
    let api = cluster.tasks(&ns);
    let root = api.get(root_name).await?;
    let parent = api.get(parent_name).await?;
    let map = cluster.configmaps(&ns).get(&pin.name).await?;
    for member in [task, &parent, &root] {
        ensure!(
            task_is_owned_by(member, principal),
            "reserved member access changed"
        );
    }
    reserved_member(&map, &root, task, &parent, &pin)?;
    let fresh_task = api.get(&task.name_any()).await?;
    let fresh_parent = api.get(parent_name).await?;
    let fresh_root = api.get(root_name).await?;
    let fresh_map = cluster.configmaps(&ns).get(&pin.name).await?;
    ensure!(
        fresh_map.metadata.uid == map.metadata.uid && fresh_map.data == map.data,
        "reviewed reservations changed while reading"
    );
    for member in [&fresh_task, &fresh_parent, &fresh_root] {
        ensure!(
            task_is_owned_by(member, principal),
            "reserved member access changed"
        );
    }
    reserved_member(&fresh_map, &fresh_root, &fresh_task, &fresh_parent, &pin)
}

// Only this exact runtime-generated grammar may carry numeric detail to the client.
// Truncating arbitrary provider errors would still expose credentials or workload data.
fn safe_failure_diagnostic(error: &str) -> String {
    const PREFIX: &str = "Native per-round usage verification failed: count mismatch (assistants=";
    let counts = error
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_suffix(')'))
        .and_then(|value| value.split_once(", calls="))
        .and_then(|(assistants, rest)| {
            let (calls, turns) = rest.split_once(", turns=")?;
            let count = |value: &str| {
                (value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| value.parse::<u32>().ok())
                    .flatten()
            };
            Some((count(assistants)?, count(calls)?, count(turns)?))
        });
    match counts {
        Some((assistants, calls, turns)) => {
            format!("{PREFIX}{assistants}, calls={calls}, turns={turns})")
        }
        None => "The attempt recorded a failure. Error details are withheld because they may contain sensitive data.".into(),
    }
}

// Attempt state is display evidence, never a replacement for a committed result.
fn phase_state(
    result: Option<&MissionResultDto>,
    task: &KarsTask,
    attempt: Option<&ConfigMap>,
) -> (&'static str, Option<String>) {
    if let Some(result) = result {
        if result.reviewable {
            return ("completed", None);
        }
        if matches!(
            result.status.as_deref(),
            Some("failed" | "rejected" | "error")
        ) {
            return ("failed", None);
        }
    }
    let Some(state) = attempt.and_then(|map| {
        let uid = task.metadata.uid.as_deref().filter(|uid| !uid.is_empty())?;
        let nonce = task
            .annotations()
            .get(RUN)
            .filter(|nonce| !nonce.is_empty())?;
        if task.namespace().as_deref() != Some("kars-system")
            || task.metadata.deletion_timestamp.is_some()
            || !owned(&map.metadata, task)
            || map.name_any() != Cluster::mission_attempt_name(uid, nonce)
            || map
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return None;
        }
        let raw = map
            .data
            .as_ref()?
            .get("attempt.json")
            .filter(|raw| raw.len() <= 900 * 1024)?;
        let value = crate::mission_record::decode(raw).ok()?;
        let candidate = value.get("candidate")?;
        if value.get("version")? != 1 || candidate.get("namespace")? != "kars-system" {
            return None;
        }
        for (key, expected) in [
            ("taskName", task.name_any()),
            ("taskUid", uid.into()),
            ("runNonce", nonce.clone()),
        ] {
            if candidate.get(key)?.as_str()? != expected
                || value.get("assignment")?.get(key)?.as_str()? != expected
            {
                return None;
            }
        }
        let state = match value.get("phase")?.as_str()? {
            "uncertain" => "uncertain",
            "failed" | "rejected" => "failed",
            "accepted" | "running" if live::can_read_stopped_activity(task) => "stopped",
            "accepted" | "running"
                if task
                    .spec
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.launch) =>
            {
                "active"
            }
            "dispatching" => "pending",
            _ => "unavailable",
        };
        let diagnostic = if result.is_none() && state == "failed" {
            [value.pointer("/reply/error"), value.get("error")]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|error| !error.trim().is_empty())
                .map(safe_failure_diagnostic)
        } else {
            None
        };
        Some((state, diagnostic))
    }) else {
        return ("unavailable", None);
    };
    state
}

pub(super) type SummaryOutputs<'a> = BTreeMap<&'a str, &'a BTreeMap<String, String>>;

pub(super) async fn load(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
    summary_outputs: Option<&SummaryOutputs<'_>>,
) -> Result<Vec<ReviewedPhaseDto>> {
    let pin: Pin = serde_json::from_str(
        root.annotations()
            .get(PIN)
            .context("reviewed pin missing")?,
    )?;
    let ns = root.namespace().context("root namespace missing")?;
    let map = cluster.configmaps(&ns).get(&pin.name).await?;
    let state = parse_state(&map, root, &pin)?;
    let api = cluster.tasks(&ns);
    let mut phases = Vec::new();
    for (r, role) in state.plan.roles.iter().enumerate() {
        let role_slot = format!("r{r}");
        let role_task = match state.children.get(&role_slot) {
            Some(reservation) => {
                let task = api.get(&reservation.name).await?;
                ensure!(
                    task_is_owned_by(&task, principal),
                    "reviewed role access changed"
                );
                child(&task, root, root, reservation, &pin, &role_slot)?;
                Some(task)
            }
            None => None,
        };
        for (p, phase) in role.phases.iter().enumerate() {
            let slot = format!("r{r}-p{p}");
            let mut dto = ReviewedPhaseDto {
                role: role.name.clone(),
                phase: phase.name.clone(),
                task_name: None,
                task_uid: None,
                run_nonce: None,
                state: "pending",
                result: None,
                failure_diagnostic: None,
                router_activity: None,
                live_activity: None,
                stopped_activity: None,
            };
            if let Some(reservation) = state.children.get(&slot) {
                let parent = role_task
                    .as_ref()
                    .context("phase exists without its reserved role")?;
                let task = api.get(&reservation.name).await?;
                ensure!(
                    task_is_owned_by(&task, principal),
                    "reviewed phase access changed"
                );
                child(&task, parent, root, reservation, &pin, &slot)?;
                let output = match summary_outputs {
                    Some(outputs) => outputs
                        .get(reservation.name.as_str())
                        .map(|data| (**data).clone()),
                    None => cluster.read_mission_output(&reservation.name).await,
                }
                .filter(|output| output_matches_task(&task, output));
                let artifacts = if summary_outputs.is_none() && output.is_some() {
                    build_artifact_set(cluster, &reservation.name, output.as_ref()).await
                } else {
                    Vec::new()
                };
                let attempt = cluster
                    .read_mission_attempt(
                        &reservation.uid,
                        task.annotations().get(RUN).context("phase nonce missing")?,
                    )
                    .await
                    .ok()
                    .flatten();
                let fresh = api.get(&reservation.name).await?;
                ensure!(
                    task_is_owned_by(&fresh, principal),
                    "reviewed phase access changed"
                );
                child(&fresh, parent, root, reservation, &pin, &slot)?;
                dto.task_name = Some(reservation.name.clone());
                dto.task_uid = Some(reservation.uid.clone());
                dto.run_nonce = fresh.annotations().get(RUN).cloned();
                dto.result = output
                    .filter(|output| output_matches_task(&fresh, output))
                    .map(|output| {
                        super::run_evidence::mission_result_with_attempt(&output, attempt.as_ref())
                    })
                    .filter(|result| result.run_evidence.is_some());
                dto.router_activity =
                    router_activity::from_artifacts(&fresh, dto.result.as_ref(), &artifacts);
                (dto.state, dto.failure_diagnostic) =
                    phase_state(dto.result.as_ref(), &fresh, attempt.as_ref());
                dto.live_activity = attempt
                    .as_ref()
                    .and_then(|attempt| live::from_attempt(attempt, &fresh));
                if dto.result.is_none() && live::can_read_stopped_activity(&fresh) {
                    dto.live_activity = None;
                    dto.stopped_activity = attempt
                        .as_ref()
                        .and_then(|attempt| live::from_stopped_attempt(attempt, &fresh));
                }
            }
            phases.push(dto);
        }
        if let Some(parent) = role_task {
            let fresh = api.get(&parent.name_any()).await?;
            child(
                &fresh,
                root,
                root,
                &state.children[&role_slot],
                &pin,
                &role_slot,
            )?;
            ensure!(
                task_is_owned_by(&fresh, principal),
                "reviewed role access changed"
            );
        }
    }
    let current = cluster.configmaps(&ns).get(&pin.name).await?;
    ensure!(
        current.metadata.uid == map.metadata.uid && current.data == map.data,
        "reviewed reservations changed while reading"
    );
    let fresh = api.get(&root.name_any()).await?;
    ensure!(
        task_is_owned_by(&fresh, principal),
        "reviewed root access changed"
    );
    parse_state(&current, &fresh, &pin)?;
    Ok(phases)
}

/// Reuse the reserved-tree custody checks without loading terminal output/artifact bodies.
pub(crate) async fn read_live(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
) -> Result<Vec<ReviewedPhaseDto>> {
    if !root.annotations().contains_key(PIN) {
        return Ok(Vec::new());
    }
    if root
        .annotations()
        .contains_key("kars.azure.com/reviewed-root")
        || root
            .annotations()
            .contains_key("kars.azure.com/reviewed-slot")
    {
        verify_reserved_member(cluster, root, principal).await?;
        return Ok(Vec::new());
    }
    load(cluster, root, principal, Some(&BTreeMap::new())).await
}

pub(crate) async fn read(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
) -> Option<ReviewedExecutionDto> {
    root.annotations().get(PIN)?;
    let run_nonce = root.annotations().get(RUN).cloned().unwrap_or_default();
    let result = if root
        .annotations()
        .contains_key("kars.azure.com/reviewed-root")
        || root
            .annotations()
            .contains_key("kars.azure.com/reviewed-slot")
    {
        match verify_reserved_member(cluster, root, principal).await {
            Ok(()) => return None,
            Err(error) => Err(error),
        }
    } else {
        load(cluster, root, principal, None).await
    };
    Some(match result {
        Ok(phases) => ReviewedExecutionDto {
            run_nonce,
            phases,
            issue: None,
        },
        Err(error) => {
            tracing::warn!(task = %root.name_any(), %error, "Reviewed execution evidence unavailable");
            ReviewedExecutionDto {
                run_nonce,
                phases: Vec::new(),
                issue: Some(
                    "Reviewed phase evidence is unavailable or changed; refresh the run.".into(),
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn phase_state(
        result: Option<&MissionResultDto>,
        task: &KarsTask,
        attempt: Option<&ConfigMap>,
    ) -> &'static str {
        let (state, diagnostic) = super::phase_state(result, task, attempt);
        if state != "failed" || result.is_some() {
            assert!(diagnostic.is_none());
        }
        state
    }

    fn root() -> KarsTask {
        serde_json::from_value(json!({
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
            "metadata": {"name": "root", "namespace": "kars-system", "uid": "root-uid"},
            "spec": {"objective": "Write a checklist",
                "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 2},
                "blueprint": {"executionPlan": {
                    "schema": "kars.execution-plan/v1", "maxParallel": 1,
                    "roles": [{"name": "writer", "objective": "Write", "dependsOn": [],
                        "phases": [{"name": "draft", "objective": "Draft", "capabilities": [],
                            "minToolCalls": 0, "maxToolCalls": 6, "freshContext": true}]}],
                    "synthesis": {"objective": "Collect", "capabilities": [], "maxToolCalls": 6},
                    "deliverables": [{"name": "checklist.md"}]
                }}
            }
        }))
        .unwrap()
    }

    fn fixture() -> (KarsTask, ConfigMap, Pin) {
        let mut root = root();
        let pin = Pin {
            name: "kars-reviewed-test".into(),
            uid: "map-uid".into(),
        };
        root.annotations_mut().insert(
            PIN.into(),
            json!({"name": pin.name, "uid": pin.uid}).to_string(),
        );
        root.annotations_mut().insert(RUN.into(), "root-run".into());
        let plan = root
            .spec
            .blueprint
            .as_ref()
            .unwrap()
            .execution_plan
            .as_ref()
            .unwrap();
        let state = json!({"schema": "kars.reviewed-run/v2", "rootName": "root", "rootUid": "root-uid",
                        "admission": {"state": "run", "runNonce": "root-run"},
                        "planDigest": digest(plan).unwrap(), "plan": plan, "children": {}});
        let map = ConfigMap {
            metadata: ObjectMeta {
                name: Some(pin.name.clone()),
                namespace: root.namespace(),
                uid: Some(pin.uid.clone()),
                owner_references: Some(vec![root.controller_owner_ref(&()).unwrap()]),
                ..Default::default()
            },
            data: Some(BTreeMap::from([("state.json".into(), state.to_string())])),
            ..Default::default()
        };
        (root, map, pin)
    }

    fn attempt_fixture(phase: &str) -> (KarsTask, ConfigMap) {
        let (mut task, _, _) = fixture();
        task.spec.execution = Some(crate::kars::task::TaskExecution {
            launch: true,
            runtime: None,
        });
        let identity = json!({"namespace": "kars-system", "taskName": "root",
            "taskUid": "root-uid", "runNonce": "root-run"});
        let map = ConfigMap {
            metadata: ObjectMeta {
                name: Some(Cluster::mission_attempt_name("root-uid", "root-run")),
                namespace: task.namespace(),
                uid: Some("attempt-uid".into()),
                resource_version: Some("1".into()),
                owner_references: Some(vec![task.controller_owner_ref(&()).unwrap()]),
                ..Default::default()
            },
            data: Some(BTreeMap::from([(
                "attempt.json".into(),
                json!({
                    "version": 1, "candidate": identity, "assignment": identity,
                    "phase": phase, "error": "Timed out waiting for message (293777ms)",
                    "reply": {"status": "running"}
                })
                .to_string(),
            )])),
            ..Default::default()
        };
        (task, map)
    }

    #[test]
    fn reviewed_phase_attempt_state_overrides_launch_and_last_running_reply() {
        for (phase, expected) in [
            ("uncertain", "uncertain"),
            ("failed", "failed"),
            ("rejected", "failed"),
            ("running", "active"),
            ("accepted", "active"),
            ("dispatching", "pending"),
            ("succeeded", "unavailable"),
        ] {
            let (mut task, map) = attempt_fixture(phase);
            assert_eq!(phase_state(None, &task, Some(&map)), expected, "{phase}");
            task.spec.execution.as_mut().unwrap().launch = false;
            let inactive = if expected == "active" {
                "stopped"
            } else {
                expected
            };
            assert_eq!(
                phase_state(None, &task, Some(&map)),
                inactive,
                "stopped {phase}"
            );
        }
        let (mut task, map) = attempt_fixture("uncertain");
        task.spec.execution.as_mut().unwrap().launch = false;
        assert_eq!(phase_state(None, &task, Some(&map)), "uncertain");
        assert_eq!(phase_state(None, &task, None), "unavailable");
    }

    #[test]
    fn reviewed_phase_attempt_requires_current_custody_and_revision() {
        let (task, map) = attempt_fixture("failed");
        assert!(super::phase_state(None, &task, Some(&map)).1.is_some());
        for path in [
            "/candidate/taskUid",
            "/candidate/runNonce",
            "/candidate/taskName",
            "/candidate/namespace",
            "/assignment/taskUid",
            "/assignment/runNonce",
            "/assignment/taskName",
        ] {
            let mut changed = map.clone();
            let mut value: Value =
                serde_json::from_str(&changed.data.as_ref().unwrap()["attempt.json"]).unwrap();
            *value.pointer_mut(path).unwrap() = json!("different");
            changed
                .data
                .as_mut()
                .unwrap()
                .insert("attempt.json".into(), value.to_string());
            assert_eq!(
                phase_state(None, &task, Some(&changed)),
                "unavailable",
                "{path}"
            );
        }
        let mut changed = map.clone();
        changed.metadata.owner_references = None;
        assert_eq!(phase_state(None, &task, Some(&changed)), "unavailable");
        changed = map.clone();
        changed.metadata.name = Some("wrong-attempt".into());
        assert_eq!(phase_state(None, &task, Some(&changed)), "unavailable");
        changed = map.clone();
        changed.metadata.resource_version = None;
        assert_eq!(phase_state(None, &task, Some(&changed)), "unavailable");
        changed = map.clone();
        changed.metadata.deletion_timestamp =
            Some(serde_json::from_value(json!("2026-10-15T15:00:00Z")).unwrap());
        assert_eq!(phase_state(None, &task, Some(&changed)), "unavailable");
        changed = map.clone();
        changed
            .data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), "not JSON".into());
        assert_eq!(phase_state(None, &task, Some(&changed)), "unavailable");
        let mut replaced = task.clone();
        replaced.metadata.uid = Some("replacement".into());
        assert_eq!(phase_state(None, &replaced, Some(&map)), "unavailable");
        replaced = task.clone();
        replaced
            .annotations_mut()
            .insert(RUN.into(), "new-run".into());
        assert_eq!(phase_state(None, &replaced, Some(&map)), "unavailable");
    }

    #[test]
    fn reviewed_phase_failure_diagnostic_is_safe_and_never_a_result() {
        let message = "Native per-round usage verification failed: count mismatch (assistants=18, calls=15, turns=15)";
        let (task, mut map) = attempt_fixture("failed");
        let mut value: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["attempt.json"]).unwrap();
        value["reply"] = json!({"status": "failed", "error": message,
            "output": "private workload output", "artifacts": [{"content": "private artifact"}]});
        value["error"] = json!("private provider response");
        map.data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), value.to_string());
        let (state, diagnostic) = super::phase_state(None, &task, Some(&map));
        assert_eq!(state, "failed");
        assert_eq!(diagnostic.as_deref(), Some(message));
        let phase = ReviewedPhaseDto {
            role: "builder".into(),
            phase: "build".into(),
            task_name: task.metadata.name.clone(),
            task_uid: task.metadata.uid.clone(),
            run_nonce: task.annotations().get(RUN).cloned(),
            state,
            result: None,
            failure_diagnostic: diagnostic,
            router_activity: None,
            live_activity: None,
            stopped_activity: None,
        };
        let serialized = serde_json::to_value(phase).unwrap();
        assert_eq!(serialized["failure_diagnostic"], message);
        assert!(serialized["result"].is_null());
        assert!(serialized.get("artifacts").is_none());
        assert!(!serialized.to_string().contains("private"));

        for phase in [
            "rejected",
            "running",
            "accepted",
            "dispatching",
            "uncertain",
            "succeeded",
        ] {
            value["phase"] = json!(phase);
            map.data
                .as_mut()
                .unwrap()
                .insert("attempt.json".into(), value.to_string());
            let (_, diagnostic) = super::phase_state(None, &task, Some(&map));
            assert_eq!(
                diagnostic.as_deref(),
                (phase == "rejected").then_some(message)
            );
        }
        value["phase"] = json!("failed");
        value["reply"]["error"] = Value::Null;
        value["error"] = json!(message);
        map.data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), value.to_string());
        assert_eq!(
            super::phase_state(None, &task, Some(&map)).1.as_deref(),
            Some(message)
        );
        value["error"] = json!("  ");
        map.data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), value.to_string());
        assert!(super::phase_state(None, &task, Some(&map)).1.is_none());
    }

    #[test]
    fn reviewed_phase_failure_diagnostic_withholds_arbitrary_error_content() {
        let message = "Native per-round usage verification failed: count mismatch (assistants=18, calls=15, turns=15)";
        let withheld = safe_failure_diagnostic("provider credentials and private data");
        for error in [
            "Authorization: Bearer secret".to_string(),
            "<script>secret</script>".to_string(),
            format!("{message}\nprivate workload data"),
            format!("prefix {message}"),
            message.replace("18", "secret"),
            message.replace("18", "4294967296"),
            message.replace("18", "+18"),
            message.replace("18", ""),
            message.replace("18", "١٨"),
            "private".repeat(100_000),
        ] {
            assert_eq!(safe_failure_diagnostic(&error), withheld);
        }
        assert!(withheld.len() <= 256);
        assert_eq!(safe_failure_diagnostic(message), message);
        let boundary = message.replace("18", "4294967295");
        assert_eq!(safe_failure_diagnostic(&boundary), boundary);
        assert!(safe_failure_diagnostic(&boundary).len() <= 256);
    }

    #[test]
    fn reviewed_phase_failure_diagnostic_rejects_invalid_attempt_metadata_and_version() {
        let (task, map) = attempt_fixture("failed");
        for (key, replacement) in [
            ("uid", json!(null)),
            ("uid", json!("")),
            ("namespace", json!("other")),
            ("resourceVersion", json!("")),
            ("ownerReferences", json!([])),
        ] {
            let mut changed = serde_json::to_value(&map).unwrap();
            changed["metadata"][key] = replacement;
            let changed: ConfigMap = serde_json::from_value(changed).unwrap();
            assert_eq!(
                super::phase_state(None, &task, Some(&changed)),
                ("unavailable", None)
            );
        }
        for raw in [
            " ".repeat(900 * 1024 + 1),
            "{}".into(),
            map.data.as_ref().unwrap()["attempt.json"].replace("\"version\":1", "\"version\":2"),
        ] {
            let mut changed = map.clone();
            changed
                .data
                .as_mut()
                .unwrap()
                .insert("attempt.json".into(), raw);
            assert_eq!(
                super::phase_state(None, &task, Some(&changed)),
                ("unavailable", None)
            );
        }
        for (key, replacement) in [
            ("namespace", json!("other")),
            ("uid", json!("")),
            ("deletionTimestamp", json!("2026-10-15T15:00:00Z")),
        ] {
            let mut changed = serde_json::to_value(&task).unwrap();
            changed["metadata"][key] = replacement;
            let changed: KarsTask = serde_json::from_value(changed).unwrap();
            assert_eq!(
                super::phase_state(None, &changed, Some(&map)),
                ("unavailable", None)
            );
        }
    }

    #[test]
    fn reviewed_phase_retained_results_take_priority_over_attempt_snapshots() {
        let (task, map) = attempt_fixture("failed");
        let mut result = mission_result(&BTreeMap::from([("status".into(), "failed".into())]));
        assert_eq!(phase_state(Some(&result), &task, Some(&map)), "failed");
        result.reviewable = true;
        result.status = Some("ok".into());
        result.output = "Last valid builder completion".into();
        assert_eq!(phase_state(Some(&result), &task, Some(&map)), "completed");
        assert_eq!(result.output, "Last valid builder completion");
    }

    #[test]
    fn reviewed_projection_reads_current_and_retained_controller_state_versions() {
        let (root, mut map, pin) = fixture();
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        for schema in ["kars.reviewed-run/v1", "kars.reviewed-run/v2"] {
            state["schema"] = schema.into();
            map.data
                .as_mut()
                .unwrap()
                .insert("state.json".into(), state.to_string());
            assert_eq!(parse_state(&map, &root, &pin).unwrap().plan.roles.len(), 1);
        }
    }

    #[test]
    fn reviewed_projection_requires_pinned_owner_root_revision_and_plan() {
        let (root, map, pin) = fixture();
        assert_eq!(parse_state(&map, &root, &pin).unwrap().plan.roles.len(), 1);
        for key in ["schema", "rootName", "rootUid", "planDigest"] {
            let mut changed = map.clone();
            let mut value: Value =
                serde_json::from_str(&changed.data.as_ref().unwrap()["state.json"]).unwrap();
            value[key] = "changed".into();
            changed
                .data
                .as_mut()
                .unwrap()
                .insert("state.json".into(), value.to_string());
            assert!(parse_state(&changed, &root, &pin).is_err(), "{key}");
        }
        let mut changed = root.clone();
        changed
            .annotations_mut()
            .insert(RUN.into(), "new-run".into());
        assert!(parse_state(&map, &changed, &pin).is_err());
        changed = root.clone();
        changed.metadata.uid = Some("new-uid".into());
        assert!(parse_state(&map, &changed, &pin).is_err());
        let mut changed = map.clone();
        changed.metadata.uid = Some("new-uid".into());
        assert!(parse_state(&changed, &root, &pin).is_err());
        changed = map.clone();
        changed.metadata.owner_references = None;
        assert!(parse_state(&changed, &root, &pin).is_err());
    }

    #[test]
    fn reviewed_projection_accepts_only_the_exact_sealed_team_memory_binding() {
        let (mut root, mut map, pin) = fixture();
        let memory = json!({"schema": "kars.team-memory/v1", "documents": []}).to_string();
        root.annotations_mut()
            .insert(TEAM_MEMORY.into(), memory.clone());
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        state["teamMemory"] = memory.clone().into();
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert_eq!(
            parse_state(&map, &root, &pin).unwrap().team_memory,
            Some(memory.clone())
        );
        root.annotations_mut()
            .insert(TEAM_MEMORY.into(), "changed".into());
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().remove(TEAM_MEMORY);
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().insert(TEAM_MEMORY.into(), memory);
        state.as_object_mut().unwrap().remove("teamMemory");
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert!(parse_state(&map, &root, &pin).is_err());
    }

    #[test]
    fn reviewed_projection_serializes_stopped_progress_without_live_or_result_authority() {
        let (task, attempt, _) = live::tests::fixture();
        let mut value = serde_json::to_value(task).unwrap();
        value["spec"]["execution"] = serde_json::json!({"launch":false});
        let task: KarsTask = serde_json::from_value(value).unwrap();
        assert_eq!(phase_state(None, &task, Some(&attempt)), "stopped");
        let phase = ReviewedPhaseDto {
            role: "researcher".into(),
            phase: "research".into(),
            task_name: task.metadata.name.clone(),
            task_uid: task.metadata.uid.clone(),
            run_nonce: task.annotations().get(RUN).cloned(),
            state: "stopped",
            result: None,
            failure_diagnostic: None,
            router_activity: None,
            live_activity: None,
            stopped_activity: live::from_stopped_attempt(&attempt, &task),
        };
        let value = serde_json::to_value(phase).unwrap();
        assert_eq!(value["stopped_activity"]["run_nonce"], "run-1");
        assert!(value["result"].is_null());
        assert!(value["router_activity"].is_null());
        assert!(value.get("live_activity").is_none());
        assert!(value.get("failure_diagnostic").is_none());
        assert!(value["stopped_activity"].get("source").is_none());
    }

    #[test]
    fn reviewed_projection_accepts_only_the_exact_sealed_team_revision_binding() {
        let (mut root, mut map, pin) = fixture();
        let revision = json!({"documents": [{"name": "revision-0"}]}).to_string();
        root.annotations_mut()
            .insert(TEAM_REVISION.into(), revision.clone());
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        state["teamRevision"] = revision.clone().into();
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert_eq!(
            parse_state(&map, &root, &pin).unwrap().team_revision,
            Some(revision.clone())
        );
        root.annotations_mut()
            .insert(TEAM_REVISION.into(), "changed".into());
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().remove(TEAM_REVISION);
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut()
            .insert(TEAM_REVISION.into(), revision);
        state.as_object_mut().unwrap().remove("teamRevision");
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert!(parse_state(&map, &root, &pin).is_err());
    }

    #[test]
    fn reviewed_projection_requires_reserved_child_spec_uid_slot_and_nonce() {
        let (root, _, pin) = fixture();
        let mut task = root.clone();
        task.metadata.name = Some("phase".into());
        task.metadata.uid = Some("phase-uid".into());
        task.metadata.owner_references = Some(vec![root.controller_owner_ref(&()).unwrap()]);
        task.spec.parent_ref = Some(crate::kars::task::LocalObjectRef {
            name: root.name_any(),
        });
        task.spec.blueprint = None;
        task.spec.execution = Some(crate::kars::task::TaskExecution {
            launch: true,
            runtime: None,
        });
        task.annotations_mut()
            .insert("kars.azure.com/reviewed-root".into(), "root".into());
        task.annotations_mut()
            .insert("kars.azure.com/reviewed-slot".into(), "r0-p0".into());
        task.annotations_mut()
            .insert(RUN.into(), "plan-test-r0-p0".into());
        let reservation = Reservation {
            name: task.name_any(),
            uid: task.uid().unwrap(),
            spec: serde_json::to_value(&task.spec).unwrap(),
        };
        assert!(child(&task, &root, &root, &reservation, &pin, "r0-p0").is_ok());
        task.spec.execution.as_mut().unwrap().launch = false;
        assert!(child(&task, &root, &root, &reservation, &pin, "r0-p0").is_ok());
        for key in [
            RUN,
            PIN,
            "kars.azure.com/reviewed-root",
            "kars.azure.com/reviewed-slot",
        ] {
            let mut changed = task.clone();
            changed
                .annotations_mut()
                .insert(key.into(), "changed".into());
            assert!(
                child(&changed, &root, &root, &reservation, &pin, "r0-p0").is_err(),
                "{key}"
            );
        }
        let mut changed = task.clone();
        changed.metadata.uid = Some("replacement".into());
        assert!(child(&changed, &root, &root, &reservation, &pin, "r0-p0").is_err());
        changed = task.clone();
        changed.spec.objective = "different work".into();
        assert!(child(&changed, &root, &root, &reservation, &pin, "r0-p0").is_err());
    }

    #[test]
    fn specialist_pin_resolves_through_reserved_role_and_root_custody() {
        let (root, mut map, pin) = fixture();
        let member = |name: &str, slot: &str, parent: &KarsTask, launch: bool| {
            let mut task = root.clone();
            task.metadata.name = Some(name.into());
            task.metadata.uid = Some(format!("{name}-uid"));
            task.metadata.owner_references = Some(vec![parent.controller_owner_ref(&()).unwrap()]);
            task.spec.parent_ref = Some(crate::kars::task::LocalObjectRef {
                name: parent.name_any(),
            });
            task.spec.blueprint = None;
            task.spec.execution = Some(crate::kars::task::TaskExecution {
                launch,
                runtime: None,
            });
            task.annotations_mut()
                .insert("kars.azure.com/reviewed-root".into(), root.name_any());
            task.annotations_mut()
                .insert("kars.azure.com/reviewed-slot".into(), slot.into());
            task.annotations_mut()
                .insert(RUN.into(), format!("plan-test-{slot}"));
            task
        };
        let role = member("role", "r0", &root, false);
        let phase = member("phase", "r0-p0", &role, true);
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        for (slot, task) in [("r0", &role), ("r0-p0", &phase)] {
            state["children"][slot] =
                json!({"name":task.name_any(),"uid":task.uid().unwrap(),"spec":task.spec});
        }
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert!(parse_state(&map, &phase, &pin).is_err());
        assert!(reserved_member(&map, &root, &role, &root, &pin).is_ok());
        assert!(reserved_member(&map, &root, &phase, &role, &pin).is_ok());
        let mut stopped = phase.clone();
        stopped.spec.execution.as_mut().unwrap().launch = false;
        assert!(reserved_member(&map, &root, &stopped, &role, &pin).is_ok());
        for part in [
            "uid", "owner", "parent", "nonce", "pin", "slot", "root", "spec",
        ] {
            let mut changed = phase.clone();
            match part {
                "uid" => changed.metadata.uid = Some("replaced".into()),
                "owner" => {
                    changed.metadata.owner_references =
                        Some(vec![root.controller_owner_ref(&()).unwrap()])
                }
                "parent" => changed.spec.parent_ref.as_mut().unwrap().name = root.name_any(),
                "nonce" => {
                    changed
                        .annotations_mut()
                        .insert(RUN.into(), "other-run".into());
                }
                "pin" => {
                    changed.annotations_mut().insert(
                        PIN.into(),
                        json!({"name":pin.name,"uid":"other-map"}).to_string(),
                    );
                }
                "slot" => {
                    changed
                        .annotations_mut()
                        .insert("kars.azure.com/reviewed-slot".into(), "r9-p9".into());
                }
                "root" => {
                    changed
                        .annotations_mut()
                        .insert("kars.azure.com/reviewed-root".into(), "other-root".into());
                }
                "spec" => changed.spec.objective = "other-work".into(),
                _ => unreachable!(),
            }
            assert!(
                reserved_member(&map, &root, &changed, &role, &pin).is_err(),
                "{part}"
            );
        }
        let mut changed_role = role.clone();
        changed_role.metadata.uid = Some("replaced-role".into());
        assert!(reserved_member(&map, &root, &phase, &changed_role, &pin).is_err());
        changed_role = role.clone();
        changed_role.spec.execution.as_mut().unwrap().launch = true;
        assert!(reserved_member(&map, &root, &phase, &changed_role, &pin).is_err());
        let mut changed_root = root.clone();
        changed_root.metadata.uid = Some("replaced-root".into());
        assert!(reserved_member(&map, &changed_root, &phase, &role, &pin).is_err());
        let mut changed_map = map.clone();
        changed_map.metadata.uid = Some("replaced-map".into());
        assert!(reserved_member(&changed_map, &root, &phase, &role, &pin).is_err());
    }

    #[test]
    fn reviewed_team_projection_preserves_the_sealed_roster_binding() {
        let (mut root, mut map, pin) = fixture();
        let roles = json!({
            "writer": {
                "blueprint": {"runtime": "Hermes"},
                "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 1}
            }
        })
        .to_string();
        root.annotations_mut()
            .insert(TEAM_ROLES.into(), roles.clone());
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        state["teamRoles"] = roles.clone().into();
        map.data
            .as_mut()
            .unwrap()
            .insert("state.json".into(), state.to_string());
        assert_eq!(
            parse_state(&map, &root, &pin).unwrap().team_roles,
            Some(roles)
        );

        root.annotations_mut()
            .insert(TEAM_ROLES.into(), "{}".into());
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().remove(TEAM_ROLES);
        assert!(parse_state(&map, &root, &pin).is_err());

        let (mut root, map, pin) = fixture();
        root.annotations_mut()
            .insert(TEAM_ROLES.into(), "{}".into());
        assert!(parse_state(&map, &root, &pin).is_err());
    }
}
