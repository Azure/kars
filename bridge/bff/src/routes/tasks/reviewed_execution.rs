// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read the controller's reserved phase tree, not name-derived roster activity.

use anyhow::{Context, Result, ensure};
use k8s_openapi::{api::core::v1::ConfigMap, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{Resource, ResourceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::{MissionResultDto, revision::output_matches_task, run_evidence::mission_result};
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
    pub router_activity: Option<router_activity::RouterActivity>,
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
        state.schema == "kars.reviewed-run/v1"
            && root.uid().as_deref() == Some(&state.root_uid)
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
            && state.team_memory.as_ref() == root.annotations().get(TEAM_MEMORY),
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

async fn load(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
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
                router_activity: None,
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
                let output = cluster
                    .read_mission_output(&reservation.name)
                    .await
                    .filter(|output| output_matches_task(&task, output));
                let artifacts = if output.is_some() {
                    build_artifact_set(cluster, &reservation.name, output.as_ref()).await
                } else {
                    Vec::new()
                };
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
                    .map(|output| mission_result(&output))
                    .filter(|result| result.run_evidence.is_some());
                dto.router_activity =
                    router_activity::from_artifacts(&fresh, dto.result.as_ref(), &artifacts);
                dto.state = match dto.result.as_ref() {
                    Some(result) if result.reviewable => "completed",
                    Some(result)
                        if matches!(
                            result.status.as_deref(),
                            Some("failed" | "rejected" | "error")
                        ) =>
                    {
                        "failed"
                    }
                    _ if fresh
                        .spec
                        .execution
                        .as_ref()
                        .is_some_and(|execution| execution.launch) =>
                    {
                        "active"
                    }
                    _ => "unavailable",
                };
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

pub(crate) async fn read(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
) -> Option<ReviewedExecutionDto> {
    root.annotations().get(PIN)?;
    let run_nonce = root.annotations().get(RUN).cloned().unwrap_or_default();
    Some(match load(cluster, root, principal).await {
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
        let state = json!({"schema": "kars.reviewed-run/v1", "rootName": "root", "rootUid": "root-uid",
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
        root.annotations_mut().insert(TEAM_MEMORY.into(), memory.clone());
        let mut state: Value =
            serde_json::from_str(&map.data.as_ref().unwrap()["state.json"]).unwrap();
        state["teamMemory"] = memory.clone().into();
        map.data.as_mut().unwrap().insert("state.json".into(), state.to_string());
        assert_eq!(
            parse_state(&map, &root, &pin).unwrap().team_memory,
            Some(memory.clone())
        );
        root.annotations_mut().insert(TEAM_MEMORY.into(), "changed".into());
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().remove(TEAM_MEMORY);
        assert!(parse_state(&map, &root, &pin).is_err());
        root.annotations_mut().insert(TEAM_MEMORY.into(), memory);
        state.as_object_mut().unwrap().remove("teamMemory");
        map.data.as_mut().unwrap().insert("state.json".into(), state.to_string());
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
