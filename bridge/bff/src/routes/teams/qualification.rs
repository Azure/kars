// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    auth::Principal,
    error::{AppError, AppResult},
    kars::{cluster::Cluster, team::KarsTeam},
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::Utc;
use kube::{
    ResourceExt,
    api::{Patch, PatchParams},
};
use serde::Deserialize;
use serde_json::json;

#[path = "../../../../../shared/team_qualification.rs"]
pub(crate) mod contract;

#[cfg(test)]
#[path = "qualification_tests.rs"]
mod tests;

pub(super) fn marked(team: &KarsTeam) -> bool {
    [contract::PENDING, contract::RECORD, contract::CONSUMED]
        .iter()
        .any(|key| team.annotations().contains_key(*key))
}

#[derive(Debug, serde::Serialize)]
pub struct QualificationStatus {
    pub state: &'static str,
    pub note: &'static str,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub item_id: Option<String>,
}

pub(super) fn status(team: &KarsTeam) -> Option<QualificationStatus> {
    if !marked(team) {
        return None;
    }
    let approval = contract::record(team).ok().flatten().or_else(|| {
        team.annotations()
            .get(contract::PENDING)
            .and_then(|raw| serde_json::from_str(raw).ok())
    });
    let state = if team.annotations().contains_key(contract::CONSUMED) {
        if team.spec.paused {
            "awaiting_evidence"
        } else {
            "running"
        }
    } else if team.annotations().contains_key(contract::RECORD) {
        "requested"
    } else if approval.as_ref().is_some_and(|r| r.expires_at > Utc::now()) {
        "pending"
    } else {
        "blocked"
    };
    Some(QualificationStatus {
        state,
        note: contract::NOTE,
        expires_at: approval.as_ref().map(|r| r.expires_at),
        item_id: approval.map(|r| r.item_id).filter(|id| !id.is_empty()),
    })
}

pub(crate) fn proposal_check(
    proposal: &crate::routes::compose::ComposeTeamProposal,
    options: &crate::routes::options::Options,
    ns: &str,
) -> Result<(), String> {
    let plan = proposal
        .execution_plan
        .as_ref()
        .ok_or("Qualification proposal requires a typed execution plan")?;
    crate::routes::compose::validate_execution_plan_for_launch(plan)?;
    if proposal.budget_tokens.is_none_or(|v| v <= 0) || proposal.model.trim().is_empty() {
        return Err(
            "Qualification proposal requires an explicit provider/deployment and finite token cap"
                .into(),
        );
    }
    let roles = proposal
        .roles
        .iter()
        .map(|role| super::CreateRole {
            name: role.name.clone(),
            system_prompt: Some(role.system_prompt.clone()),
            runtime: (!role.runtime.is_empty()).then(|| role.runtime.clone()),
            model: (!role.model.is_empty()).then(|| role.model.clone()),
            skills: role.skills.clone(),
            resources: role.resources.clone(),
        })
        .collect::<Vec<_>>();
    super::validation::validate_team_model_routes_for_attempt(
        options,
        super::TeamModelRoutes {
            namespace: ns,
            total_tokens: proposal.budget_tokens,
            runtime: Some("OpenClaw"),
            model: Some(&proposal.model),
            model_fallbacks: &proposal.model_fallbacks,
            roles: &roles,
            execution_plan: plan,
            mcp_servers: &proposal.mcp_servers,
            memory: proposal.memory.as_deref(),
        },
        true,
    )
    .map_err(|e| e.to_string())
}

pub(super) fn validate_designation(body: &super::CreateTeamRequest) -> AppResult<()> {
    if !body.qualification_run {
        return Ok(());
    }
    if body
        .budget
        .as_ref()
        .and_then(|b| b.tokens)
        .is_none_or(|v| v <= 0)
        || body.runtime.as_deref().is_none_or(|v| {
            v.trim().is_empty() || crate::routes::compose::is_non_autonomous_harness(v)
        })
        || body.model.as_deref().is_none_or(|v| {
            !v.split_once("::")
                .is_some_and(|(p, d)| !p.trim().is_empty() && !d.trim().is_empty())
        })
        || body.roles.iter().any(|r| {
            r.runtime.as_deref().is_some_and(|v| {
                v.trim().is_empty() || crate::routes::compose::is_non_autonomous_harness(v)
            })
        })
    {
        return Err(AppError::BadRequest("Team qualification requires an explicit autonomous principal harness, provider/deployment and positive finite governed token cap; selected role harnesses cannot be silently replaced".into()));
    }
    if body.launch.unwrap_or(false) {
        return Err(AppError::BadRequest("Create the qualification Team paused, then explicitly approve its exact reviewed backlog item for one attempt".into()));
    }
    Ok(())
}

fn control_state(value: &serde_json::Value) -> serde_json::Value {
    let mut metadata = value["metadata"].clone();
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.remove("resourceVersion");
        metadata.remove("managedFields");
    }
    json!({"metadata":metadata,"spec":value["spec"],
        "admission":value["status"]["runAdmission"],
        "lastGeneratedTask":value["status"]["lastGeneratedTask"],
        "lastRunAt":value["status"]["lastRunAt"],
        "generatedTaskCount":value["status"]["generatedTaskCount"].as_i64().unwrap_or(0),
        "runsSucceeded":value["status"]["runsSucceeded"].as_i64().unwrap_or(0),
        "tokensSpentTotal":value["status"]["tokensSpentTotal"].as_i64().unwrap_or(0)})
}

fn unrequested_binding(value: &serde_json::Value) -> bool {
    let annotations = &value["metadata"]["annotations"];
    value["spec"]["paused"] == true
        && value["metadata"]["uid"]
            .as_str()
            .is_some_and(|uid| !uid.is_empty())
        && value["metadata"]["deletionTimestamp"].is_null()
        && annotations[contract::PENDING] == "binding"
        && [
            contract::RECORD,
            contract::CONSUMED,
            "kars.azure.com/run-now",
        ]
        .iter()
        .all(|key| annotations[*key].is_null())
        && ["runAdmission", "lastGeneratedTask", "lastRunAt"]
            .iter()
            .all(|key| value["status"][*key].is_null())
        && ["generatedTaskCount", "runsSucceeded", "tokensSpentTotal"]
            .iter()
            .all(|key| value["status"][*key].is_null() || value["status"][*key] == 0)
}

pub(super) fn validate_creation_recovery(
    desired: &serde_json::Value,
    existing: &serde_json::Value,
) -> AppResult<()> {
    let annotation_keys = [
        "kars.azure.com/created-by",
        "kars.azure.com/owner-sub",
        "kars.azure.com/owner-name",
        "kars.azure.com/mission-decomposition",
    ];
    let mut desired_spec = desired["spec"].clone();
    // TaskBlueprint has no egressMode field: the API server prunes it and
    // task materialization always enforces Strict. No other difference is safe.
    if desired_spec["blueprint"]["egressMode"] == "Strict"
        && existing["spec"]["blueprint"]
            .as_object()
            .is_some_and(|blueprint| !blueprint.contains_key("egressMode"))
    {
        desired_spec["blueprint"]
            .as_object_mut()
            .expect("Strict egressMode requires a blueprint object")
            .remove("egressMode");
    }
    if desired["metadata"]["annotations"][contract::PENDING] != "binding"
        || !unrequested_binding(existing)
        || desired_spec != existing["spec"]
        || ["name", "namespace"]
            .iter()
            .any(|key| desired["metadata"][*key] != existing["metadata"][*key])
        || annotation_keys.iter().any(|key| {
            desired["metadata"]["annotations"][*key]
                .as_str()
                .is_none_or(str::is_empty)
                || desired["metadata"]["annotations"][*key]
                    != existing["metadata"]["annotations"][*key]
        })
    {
        return Err(AppError::Conflict("Existing Team is not this owner's identical, paused, never-requested incomplete creation; no Team was changed".into()));
    }
    Ok(())
}

async fn patch_reviewed(
    api: &kube::Api<kube::core::DynamicObject>,
    reviewed: &kube::core::DynamicObject,
    mut patch: serde_json::Value,
) -> AppResult<()> {
    let baseline = serde_json::to_value(reviewed).map_err(|e| AppError::Upstream(e.to_string()))?;
    let expected = control_state(&baseline);
    let mut current = reviewed.clone();
    for attempt in 0..3 {
        let value =
            serde_json::to_value(&current).map_err(|e| AppError::Upstream(e.to_string()))?;
        if !value["metadata"]["deletionTimestamp"].is_null() || control_state(&value) != expected {
            return Err(AppError::Conflict("Reviewed Team authority or execution changed during qualification; no request was replayed".into()));
        }
        patch["metadata"]["uid"] = json!(
            current
                .uid()
                .ok_or_else(|| AppError::Conflict("Team has no UID".into()))?
        );
        patch["metadata"]["resourceVersion"] = json!(
            current
                .resource_version()
                .ok_or_else(|| AppError::Conflict("Team has no resource version".into()))?
        );
        match api
            .patch(
                &current.name_any(),
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(kube::Error::Api(error)) if error.code == 409 && attempt < 2 => {
                // Controller status writes may race approval, but never rebase changed authority.
                current = api
                    .get(&reviewed.name_any())
                    .await
                    .map_err(|e| AppError::Upstream(e.to_string()))?;
            }
            Err(error) => return Err(AppError::Conflict(error.to_string())),
        }
    }
    unreachable!("every final patch outcome returns")
}

pub(super) async fn bind_created(
    cluster: &Cluster,
    ns: &str,
    team: &kube::core::DynamicObject,
    principal: &Principal,
) -> AppResult<()> {
    let client = cluster.qualification_client();
    let api = contract::api(client.clone(), ns, "KarsTeam");
    // Credential attachment precedes binding; capture its completed package, not the CREATE response.
    let current = api
        .get(&team.name_any())
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if current.uid() != team.uid() || current.annotations() != team.annotations() {
        return Err(AppError::Conflict(
            "Created Team identity or approval changed before binding".into(),
        ));
    }
    let team = &current;
    let value = serde_json::to_value(team).map_err(|e| AppError::Upstream(e.to_string()))?;
    if !unrequested_binding(&value) {
        return Err(AppError::Conflict(
            "Qualification Team changed during creation; it must remain paused".into(),
        ));
    }
    let now = Utc::now();
    let record = contract::Record {
        approved_by: principal.name.clone(),
        approved_by_sub: principal.sub.clone(),
        approved_at: now,
        expires_at: now + chrono::Duration::minutes(45),
        team_uid: team
            .uid()
            .ok_or_else(|| AppError::Conflict("Team has no UID".into()))?,
        package_sha256: contract::package(&value),
        resources: contract::resources(client, ns, &value)
            .await
            .map_err(AppError::BadRequest)?,
        backlog_uid: String::new(),
        item_id: String::new(),
        work_sha256: String::new(),
        request: String::new(),
    };
    patch_reviewed(&api, team, json!({"metadata":{
        "annotations":{(contract::PENDING):serde_json::to_string(&record).map_err(|e| AppError::Upstream(e.to_string()))?}
    }})).await
}

pub(super) fn has_active_attempt(team: &KarsTeam, tasks: &[crate::kars::task::KarsTask]) -> bool {
    let mut owned = tasks
        .iter()
        .filter(|task| super::lifecycle::owned_run(&task.metadata, team))
        .filter_map(ResourceExt::uid)
        .collect::<std::collections::BTreeSet<_>>();
    owned.extend(team.annotations().get(contract::CONSUMED).cloned());
    loop {
        let before = owned.len();
        for task in tasks {
            if task.namespace() == team.namespace()
                && task
                    .metadata
                    .owner_references
                    .as_ref()
                    .is_some_and(|owners| {
                        owners.iter().any(|owner| {
                            owner.controller == Some(true)
                                && owner.api_version == "kars.azure.com/v1alpha1"
                                && owner.kind == "KarsTask"
                                && owned.contains(&owner.uid)
                        })
                    })
            {
                owned.extend(task.uid());
            }
        }
        if owned.len() == before {
            break;
        }
    }
    tasks.iter().any(|task| {
        (super::lifecycle::owned_run(&task.metadata, team)
            || task.uid().is_some_and(|uid| owned.contains(&uid)))
            && task
                .spec
                .execution
                .as_ref()
                .is_some_and(|execution| execution.launch)
    })
}

pub(super) fn release_record(
    team: &KarsTeam,
    expected_uid: Option<&str>,
) -> AppResult<contract::Record> {
    if expected_uid.is_none_or(str::is_empty)
        || team.uid().as_deref() != expected_uid
        || !team.spec.paused
        || team.annotations().contains_key(contract::PENDING)
        || !team.annotations().contains_key(contract::CONSUMED)
        || team.annotations().contains_key("kars.azure.com/run-now")
    {
        return Err(AppError::Conflict(
            "Evidence review requires the consumed, paused original Team with no pending request"
                .into(),
        ));
    }
    let approval = contract::record(team)
        .map_err(AppError::Conflict)?
        .ok_or_else(|| AppError::Conflict("Consumed qualification lacks its approval".into()))?;
    if !team
        .status
        .as_ref()
        .and_then(|status| status.run_admission.as_ref())
        .is_some_and(|admission| {
            admission.request == approval.request
                && team.annotations().get(contract::CONSUMED) == Some(&admission.task_uid)
        })
    {
        return Err(AppError::Conflict(
            "Qualification consumption lacks its matching durable admission".into(),
        ));
    }
    Ok(approval)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationAttemptRequest {
    pub team_uid: String,
    pub expected: super::TeamTaskDto,
}

/// Approval is for this exact reviewed backlog item, not a generic Run or Resume.
pub async fn qualify_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<QualificationAttemptRequest>,
) -> AppResult<Json<serde_json::Value>> {
    crate::routes::qualification_runs::authorize(&principal, true)?;
    let cluster = crate::routes::tasks::require_cluster(&state)?;
    let team = super::require_owned_team(cluster, &ns, &name, &principal).await?;
    if !team.spec.paused
        || team.uid().as_deref() != Some(body.team_uid.as_str())
        || team.annotations().contains_key(contract::RECORD)
        || team.annotations().contains_key(contract::CONSUMED)
        || team.annotations().contains_key("kars.azure.com/run-now")
        || team
            .status
            .as_ref()
            .and_then(|s| s.run_admission.as_ref())
            .is_some()
    {
        return Err(AppError::Conflict("Qualification requires the original paused, never-requested Team; an attempt cannot be replayed".into()));
    }
    let mut record: contract::Record =
        serde_json::from_str(team.annotations().get(contract::PENDING).ok_or_else(|| {
            AppError::BadRequest("This Team was not explicitly designated for qualification".into())
        })?)
        .map_err(|_| {
            AppError::Conflict(
                "Qualification package was not successfully bound at creation".into(),
            )
        })?;
    let client = cluster.qualification_client();
    let raw = contract::api(client.clone(), &ns, "KarsTeam")
        .get(&name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let value = serde_json::to_value(&raw).map_err(|e| AppError::Upstream(e.to_string()))?;
    contract::validate_package(&record, &value, Utc::now()).map_err(AppError::Conflict)?;
    let current: KarsTeam =
        serde_json::from_value(value.clone()).map_err(|e| AppError::Upstream(e.to_string()))?;
    if control_state(&json!(current)) != control_state(&json!(team))
        || contract::resources(client.clone(), &ns, &value)
            .await
            .map_err(AppError::Conflict)?
            != record.resources
    {
        return Err(AppError::Conflict(
            "Reviewed Team or resource package changed; create a newly reviewed attempt".into(),
        ));
    }
    let owner = super::work_owner(&team)?;
    let (backlog, items) = super::work::read(&cluster.configmaps(&ns), &owner)
        .await
        .map_err(super::work_error)?
        .ok_or_else(|| {
            AppError::BadRequest("Qualification requires one reviewed backlog item".into())
        })?;
    if super::work::next_pending(&items) != Some(&body.expected) {
        return Err(AppError::Conflict(
            "Reviewed work changed, is not pending, or is not the next eligible item".into(),
        ));
    }
    super::validate_work_intent(&team, &body.expected)?;
    super::backlog::validate_revision_plan(&team, &items)?;
    crate::kars::execution_plans::validate(&team, true).map_err(AppError::BadRequest)?;
    crate::routes::validate::require_team_budget_contracts(cluster, &team).await?;
    crate::routes::budgets::enforce_launch_budget(cluster, &ns, &principal.name).await?;
    let mut patch = super::lifecycle::run_request_patch(&team)?;
    record.approved_by = principal.name;
    record.approved_by_sub = principal.sub;
    record.backlog_uid = backlog
        .uid()
        .ok_or_else(|| AppError::Conflict("Backlog has no UID".into()))?;
    record.item_id = body.expected.id.clone();
    record.work_sha256 = super::work::request_digest(&body.expected)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    record.request = patch["metadata"]["annotations"]["kars.azure.com/run-now"]
        .as_str()
        .unwrap_or_default()
        .into();
    patch["metadata"]["annotations"][contract::RECORD] =
        json!(serde_json::to_string(&record).map_err(|e| AppError::Upstream(e.to_string()))?);
    patch["metadata"]["annotations"][contract::PENDING] = serde_json::Value::Null;
    patch["spec"] = json!({"paused":false});
    patch_reviewed(&contract::api(client, &ns, "KarsTeam"), &raw, patch).await?;
    Ok(Json(
        json!({"requested":true,"request":record.request,"qualification_run":true,"note":contract::NOTE}),
    ))
}
