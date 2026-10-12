// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::task::execution_plan::{
    ExecutionPlan, REVIEWED_TASK_DELEGATION_DEPTH, REVIEWED_TEAM_DELEGATION_DEPTH,
    validate_activation, validate_reviewed_delegation_depth,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const MARKER: &str = "kars.azure.com/mission-decomposition";

#[derive(Debug, PartialEq, Eq)]
struct ReviewedPlans {
    decomposition: Option<String>,
    root: Option<ExecutionPlan>,
    roles: BTreeMap<String, ExecutionPlan>,
}

fn plan(blueprint: &Value) -> Result<Option<ExecutionPlan>, String> {
    blueprint
        .get("executionPlan")
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("Invalid reviewed execution plan: {error}"))
        })
        .transpose()
}

fn reviewed(object: &Value, active: bool) -> Result<ReviewedPlans, String> {
    let decomposition = object["metadata"]["annotations"]
        .get(MARKER)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Invalid execution-plan marker".to_string())
        })
        .transpose()?;
    let root = plan(&object["spec"]["blueprint"])?;
    validate_activation(root.as_ref(), decomposition.as_deref(), active)?;
    let depth = object["spec"]["envelope"]["delegationDepth"]
        .as_i64()
        .unwrap_or(0);
    if active && root.is_some() {
        validate_reviewed_delegation_depth(
            depth,
            if object["kind"] == "KarsTeam" {
                REVIEWED_TEAM_DELEGATION_DEPTH
            } else {
                REVIEWED_TASK_DELEGATION_DEPTH
            },
        )?;
    }
    let mut roles = BTreeMap::new();
    if let Some(roster) = object["spec"]["roster"].as_array() {
        for role in roster {
            let role_plan = plan(&role["blueprint"])?;
            let inherits_plan = role["blueprint"].is_null() && root.is_some();
            if active && (role_plan.is_some() || inherits_plan) {
                let role_depth = if role["envelope"].is_null() {
                    depth.saturating_sub(1).max(0)
                } else {
                    role["envelope"]["delegationDepth"].as_i64().unwrap_or(0)
                };
                validate_reviewed_delegation_depth(role_depth, REVIEWED_TASK_DELEGATION_DEPTH)
                    .map_err(|error| format!("roster role {}: {error}", role["name"]))?;
            }
            if let Some(plan) = role_plan {
                validate_activation(Some(&plan), None, active)?;
                let name = role["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| "Reviewed role plan requires a role name".to_string())?;
                if roles.insert(name.to_string(), plan).is_some() {
                    return Err("Duplicate reviewed role plan".into());
                }
            }
        }
    }
    Ok(ReviewedPlans {
        decomposition,
        root,
        roles,
    })
}

pub fn validate<T: Serialize>(object: &T, active: bool) -> Result<(), String> {
    let object = serde_json::to_value(object).map_err(|error| error.to_string())?;
    reviewed(&object, active).map(|_| ())
}

/// Compare canonical plans, including defaults added by Kubernetes, after each
/// package write and before subsequent credential attachment or activation.
pub fn ensure_preserved<E: Serialize, C: Serialize>(
    expected: &E,
    captured: &C,
) -> Result<(), String> {
    let expected = serde_json::to_value(expected).map_err(|error| error.to_string())?;
    let captured = serde_json::to_value(captured).map_err(|error| error.to_string())?;
    if reviewed(&expected, false)? != reviewed(&captured, false)? {
        return Err("ReviewedExecutionPlanNotPreserved: the API server changed or pruned the reviewed plan. Do not activate this package; upgrade the Core schemas and re-review it".into());
    }
    Ok(())
}

pub fn prepare_team_patch(current: &super::team::KarsTeam, spec: Value) -> Result<Value, String> {
    let fields = spec
        .as_object()
        .ok_or_else(|| "Team patch spec must be an object".to_string())?;
    let pause_only = fields.len() == 1 && fields.get("paused") == Some(&Value::Bool(true));
    let mut patch = serde_json::json!({"metadata":identity_fence(&current.metadata)?,"spec":spec});
    if patch["spec"]["blueprint"]["executionPlan"].is_object() {
        patch["metadata"]["annotations"] = serde_json::json!({MARKER:"execution-plan/v1"});
    }
    if !pause_only {
        let mut expected = serde_json::to_value(current).map_err(|error| error.to_string())?;
        json_patch::merge(&mut expected, &patch);
        validate(
            &expected,
            !expected["spec"]["paused"].as_bool().unwrap_or(false),
        )?;
    }
    Ok(patch)
}

pub fn identity_fence(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> Result<Value, String> {
    let uid = metadata
        .uid
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Captured target UID is missing".to_string())?;
    let version = metadata
        .resource_version
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Captured target resourceVersion is missing".to_string())?;
    if metadata.deletion_timestamp.is_some() {
        return Err("Captured target is being deleted".into());
    }
    Ok(serde_json::json!({"uid":uid,"resourceVersion":version}))
}

pub fn activation_fence<T: Serialize>(object: &T, active: bool) -> Result<Value, String> {
    let object = serde_json::to_value(object).map_err(|error| error.to_string())?;
    let metadata = serde_json::from_value(object["metadata"].clone())
        .map_err(|error| format!("Invalid target metadata: {error}"))?;
    let mut fence = identity_fence(&metadata)?;
    if active
        && object["kind"] == "KarsTask"
        && plan(&object["spec"]["blueprint"])?.is_some()
        && object["metadata"]["annotations"]["kars.azure.com/run-requested"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        // Core needs the first run binding before it can materialize a reviewed
        // plan. Publish it atomically with launch; preserve existing revisions.
        let nonce = super::cluster::run_nonce(
            "run",
            metadata.uid.as_deref().unwrap(),
            metadata.resource_version.as_deref().unwrap(),
        );
        fence["annotations"] = serde_json::json!({"kars.azure.com/run-requested":nonce});
    }
    Ok(fence)
}

#[cfg(test)]
#[path = "execution_plans_tests.rs"]
mod tests;
