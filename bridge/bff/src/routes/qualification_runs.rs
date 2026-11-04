// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use chrono::{DateTime, Utc};
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::routes::tasks::BlueprintDto;

const ANNOTATION: &str = "kars.azure.com/qualification-run";
pub(crate) const NOTE: &str = "Operator-approved qualification run: prior runtime/provider and selected MCP, memory or approved-skill execution evidence is advisory for this exact reviewed package. Current resource identities, specifications and versions are bound at approval and checked before admission. This run is NOT proof of qualification. Authentication, authorization, live resource readiness, credential authority, isolation, policies, approvals and finite budgets remain enforced; promotion requires separate evidence review.";

#[path = "qualification_resources.rs"]
mod resources;
pub(crate) use resources::{admit_resource, check_resources, resource_bindings};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationRun {
    pub approved_by: String,
    pub approved_by_sub: String,
    pub approved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub package_sha256: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub resource_bindings: std::collections::BTreeMap<String, String>,
}

pub(crate) fn authorize(principal: &Principal, requested: bool) -> AppResult<()> {
    if requested {
        crate::auth::require_operator(principal)?;
        match std::env::var("BRIDGE_ROUTE_QUALIFICATION_MODE") {
            Ok(mode) if mode == "required" => {}
            Err(std::env::VarError::NotPresent) => {}
            _ => return Err(AppError::BadRequest(
                "Per-run qualification requires required mode; it cannot use global validation mode".into(),
            )),
        }
    }
    Ok(())
}

pub(crate) fn validate_package(bp: &BlueprintDto, tokens: Option<i64>) -> Result<(), String> {
    if tokens.is_none_or(|tokens| tokens <= 0)
        || bp
            .runtime
            .as_deref()
            .is_none_or(|runtime| runtime.trim().is_empty())
        || bp.model.as_ref().is_none_or(|model| {
            model.provider.trim().is_empty() || model.deployment.trim().is_empty()
        })
    {
        return Err("Qualification runs require an explicit harness, provider/deployment and positive finite token budget".into());
    }
    if !matches!(bp.egress_mode.as_deref(), None | Some("strict" | "Strict")) {
        return Err("Qualification Missions require strict egress; the Mission controller does not support learning mode".into());
    }
    Ok(())
}

// Evaluate evidence first so a designation never hides malformed configuration.
pub(crate) fn admit(
    evidence: Result<bool, String>,
    qualification_run: bool,
) -> Result<bool, String> {
    evidence.map(|admitted| admitted || qualification_run)
}

fn package_sha256(task: &KarsTask) -> String {
    let mut blueprint = task.spec.blueprint.clone();
    if let Some(blueprint) = blueprint.as_mut() {
        // Credentials are UID-bound by the existing post-CREATE grant process.
        // Its generated binding is not an editable part of the reviewed package.
        blueprint.credential_bindings = None;
        // Missions enforce Strict in Core; the Task CRD prunes this redundant
        // UI field. Bind the equivalent default without ignoring other modes.
        if matches!(blueprint.egress_mode.as_deref(), Some("strict" | "Strict")) {
            blueprint.egress_mode = None;
        }
    }
    let package = serde_json::json!({
        "namespace": task.namespace(), "name": task.name_any(),
        "objective": task.spec.objective, "parent": task.spec.parent_ref,
        "envelope": task.spec.envelope, "blueprint": blueprint,
        "execution_runtime": task.spec.execution.as_ref().and_then(|execution| execution.runtime.as_deref()),
    });
    format!(
        "sha256:{:x}",
        Sha256::digest(package.to_string().as_bytes())
    )
}

pub(crate) fn stamp(
    task: &mut KarsTask,
    principal: &Principal,
    resource_bindings: std::collections::BTreeMap<String, String>,
) -> AppResult<()> {
    authorize(principal, true)?;
    let approved_at = Utc::now();
    let record = QualificationRun {
        approved_by: principal.name.clone(),
        approved_by_sub: principal.sub.clone(),
        approved_at,
        expires_at: approved_at + chrono::Duration::minutes(45),
        package_sha256: package_sha256(task),
        resource_bindings,
    };
    task.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert(
            ANNOTATION.into(),
            serde_json::to_string(&record)
                .map_err(|error| AppError::Upstream(error.to_string()))?,
        );
    Ok(())
}

pub(crate) fn record(task: &KarsTask) -> AppResult<Option<QualificationRun>> {
    task.annotations()
        .get(ANNOTATION)
        .map(|raw| {
            serde_json::from_str(raw).map_err(|_| {
                AppError::Conflict("Stored qualification authorization is malformed".into())
            })
        })
        .transpose()
}

fn validate_binding(
    task: &KarsTask,
    record: &QualificationRun,
    now: DateTime<Utc>,
) -> AppResult<()> {
    if record.approved_by_sub.is_empty()
        || record.approved_by.is_empty()
        || record.approved_at > now
        || record.expires_at <= now
        || record.expires_at <= record.approved_at
        || record.expires_at - record.approved_at > chrono::Duration::minutes(45)
        || record.package_sha256 != package_sha256(task)
    {
        return Err(AppError::Conflict("Qualification admission expired or the reviewed package changed; create a fresh explicitly reviewed qualification Mission".into()));
    }
    Ok(())
}

pub(crate) fn authorize_stored(task: &KarsTask, principal: &Principal) -> AppResult<bool> {
    let Some(record) = record(task)? else {
        return Ok(false);
    };
    authorize(principal, true)?;
    validate_binding(task, &record, Utc::now())?;
    Ok(true)
}

pub(crate) fn authorize_run(task: &KarsTask, principal: &Principal) -> AppResult<bool> {
    let Some(record) = record(task)? else {
        return Ok(false);
    };
    authorize(principal, true)?;
    if let Some(requested) = task
        .annotations()
        .get("kars.azure.com/run-requested")
        .filter(|nonce| !nonce.is_empty())
    {
        if task.annotations().get("kars.azure.com/run-completed") == Some(requested) {
            return Err(AppError::Conflict("This qualification run has completed; create a fresh reviewed Mission for another attempt".into()));
        }
        // A pending request is reused by Core, not new admission. Expiry must
        // not interrupt or replace that durable request.
        validate_binding(task, &record, record.approved_at)?;
        return Ok(true);
    }
    validate_binding(task, &record, Utc::now())?;
    Ok(true)
}

pub(crate) fn forbid_inherited_run(task: &KarsTask) -> AppResult<()> {
    if task.annotations().contains_key(ANNOTATION) {
        return Err(AppError::BadRequest("Qualification authorization cannot be inherited by replicas or revisions; create a fresh explicitly reviewed qualification Mission".into()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "qualification_runs_tests.rs"]
mod tests;
