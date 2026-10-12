// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bind queued work before run admission; review current output before harvest.

use super::{ANNOT_RUN_REQUESTED, ReconcileError, namespace, tasks};
use crate::kars_approval::{ApprovalAction, KarsApproval, KarsApprovalSpec, request_snapshot};
use crate::kars_task::KarsTask;
use crate::kars_team::KarsTeam;
use crate::mcp_server::LocalObjectRef;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, ResourceExt, api::PostParams};
use std::collections::BTreeMap;

#[path = "../../../shared/team_work.rs"]
pub(super) mod contract;

pub(super) enum Selection {
    Idle,
    Blocked,
    AwaitingReview,
    Item(contract::Binding, contract::TeamTask),
}

fn invalid(message: impl Into<String>) -> ReconcileError {
    ReconcileError::Invalid(format!("Team work: {}", message.into()))
}

fn persistence(error: contract::Error) -> ReconcileError {
    ReconcileError::Persistence(error.into())
}

fn owner(team: &KarsTeam) -> Result<contract::Owner, ReconcileError> {
    Ok(contract::Owner {
        namespace: namespace(team)?.into(),
        name: team.name_any(),
        uid: tasks::owner_ref(team)?.uid,
    })
}

pub(super) async fn select(client: &Client, team: &KarsTeam) -> Result<Selection, ReconcileError> {
    let owner = owner(team)?;
    let api = Api::<ConfigMap>::namespaced(client.clone(), &owner.namespace);
    let Some((cm, items)) = contract::read(&api, &owner).await.map_err(persistence)? else {
        return Ok(Selection::Idle);
    };
    if let Some(item) = contract::next_pending(&items) {
        return Ok(Selection::Item(
            contract::Binding {
                backlog_uid: cm.uid().ok_or_else(|| invalid("store has no UID"))?,
                item_id: item.id.clone(),
                request_digest: contract::request_digest(item)?,
            },
            item.clone(),
        ));
    }
    Ok(
        if items.iter().any(|item| item.status == "awaiting_review") {
            Selection::AwaitingReview
        } else if items.iter().any(|item| !contract::finished(item)) {
            Selection::Blocked
        } else {
            Selection::Idle
        },
    )
}

pub(super) fn fragment(selection: &Selection) -> Result<String, ReconcileError> {
    match selection {
        Selection::Item(_, item) => Ok(format!(
            "\nAssigned work within the charter and reviewed permissions:\n{}\n",
            serde_json::to_string(&contract::request(item))?,
        )),
        _ => Ok(String::new()),
    }
}

fn binding(task: &KarsTask) -> Result<Option<contract::Binding>, ReconcileError> {
    task.annotations()
        .get(contract::ANNOTATION)
        .map(|raw| {
            let binding: contract::Binding = serde_json::from_str(raw)?;
            if binding.backlog_uid.is_empty()
                || binding.item_id.is_empty()
                || binding.request_digest.len() != 64
            {
                return Err(invalid("invalid work binding"));
            }
            Ok(binding)
        })
        .transpose()
}

fn matches_run(item: &contract::TeamTask, task: &KarsTask) -> bool {
    item.run.as_deref() == task.metadata.name.as_deref()
        && item.run_uid.as_deref() == task.metadata.uid.as_deref()
        && item.assignment_nonce.as_ref() == task.annotations().get(ANNOT_RUN_REQUESTED)
        && item.run_uid.as_ref().is_some_and(|uid| !uid.is_empty())
        && item
            .assignment_nonce
            .as_ref()
            .is_some_and(|nonce| !nonce.is_empty())
}

pub(super) async fn claim(
    client: &Client,
    team: &KarsTeam,
    task: &KarsTask,
    allow_pending: bool,
) -> Result<(), ReconcileError> {
    let Some(binding) = binding(task)? else {
        return Ok(());
    };
    let owner = owner(team)?;
    let api = Api::<ConfigMap>::namespaced(client.clone(), &owner.namespace);
    if !tasks::owned(&task.metadata, team) {
        return Err(invalid("run is not owned by this Team"));
    }
    let uid = task
        .uid()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| invalid("run has no UID"))?;
    let nonce = task
        .annotations()
        .get(ANNOT_RUN_REQUESTED)
        .filter(|nonce| !nonce.is_empty())
        .ok_or_else(|| invalid("run has no nonce"))?;
    contract::update(&api, &owner, Some(&binding.backlog_uid), false, |items| {
        let eligible = contract::next_pending(items).map(|item| item.id.clone());
        let item = items
            .iter_mut()
            .find(|item| item.id == binding.item_id)
            .ok_or_else(|| contract::Error::Conflict("assigned work disappeared".into()))?;
        if contract::request_digest(item)? != binding.request_digest {
            return Err(contract::Error::Conflict(
                "assigned work changed before admission".into(),
            ));
        }
        if item.status == "active" && matches_run(item, task) {
            return Ok(());
        }
        if !allow_pending || item.status != "pending" || eligible.as_deref() != Some(&item.id) {
            return Err(contract::Error::Conflict(
                "work is no longer eligible for this run".into(),
            ));
        }
        item.status = "active".into();
        item.run = Some(task.name_any());
        item.run_uid = Some(uid.clone());
        item.assignment_nonce = Some(nonce.clone());
        item.stuck_since = Some(chrono::Utc::now().to_rfc3339());
        Ok(())
    })
    .await
    .map_err(persistence)
}

/// A stop is not a runtime result. Release only its exact active claim without
/// an output digest; a Team pause records when cancellation is observed.
pub(super) async fn cancel_stopped(
    client: &Client,
    team: &KarsTeam,
    task: &KarsTask,
) -> Result<(), ReconcileError> {
    if task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
        || !tasks::owned(&task.metadata, team)
        || task.metadata.deletion_timestamp.is_some()
    {
        return Ok(());
    }
    let Some(binding) = binding(task)? else {
        return Ok(());
    };
    let (at, reason) = if let Some(halt) = task.annotations().get("kars.azure.com/halted") {
        let (at, reason) = halt
            .strip_prefix("halted by operator at ")
            .and_then(|value| value.split_once(": "))
            .filter(|(at, reason)| super::parse_rfc3339(at).is_some() && !reason.trim().is_empty())
            .ok_or_else(|| invalid("operator stop has no valid timestamp/reason"))?;
        (at.to_owned(), reason.to_owned())
    } else if team.spec.paused {
        (
            chrono::Utc::now().to_rfc3339(),
            "Run cancellation observed while the Team is paused; no completed output was available."
                .to_owned(),
        )
    } else {
        return Ok(());
    };
    super::requests::validate_work_run(task)?;
    let owner = owner(team)?;
    let api = Api::<ConfigMap>::namespaced(client.clone(), &owner.namespace);
    contract::update(&api, &owner, Some(&binding.backlog_uid), false, |items| {
        let item = items
            .iter_mut()
            .find(|item| item.id == binding.item_id)
            .ok_or_else(|| contract::Error::Conflict("stopped work disappeared".into()))?;
        if item.history.iter().any(|attempt| {
            Some(&attempt.run_uid) == task.metadata.uid.as_ref()
                && Some(&attempt.assignment_nonce) == task.annotations().get(ANNOT_RUN_REQUESTED)
        }) {
            return Ok(());
        }
        if item.status != "active"
            || !matches_run(item, task)
            || contract::request_digest(item)? != binding.request_digest
        {
            return Err(contract::Error::Conflict(
                "stop belongs to a different work claim".into(),
            ));
        }
        item.history.push(contract::Attempt {
            run: task.name_any(),
            run_uid: item
                .run_uid
                .clone()
                .ok_or_else(|| contract::Error::Invalid("missing run UID".into()))?,
            assignment_nonce: item
                .assignment_nonce
                .clone()
                .ok_or_else(|| contract::Error::Invalid("missing run nonce".into()))?,
            outcome: "cancelled".into(),
            finished_at: at.clone(),
            output_sha256: None,
            feedback: Some(reason.clone()),
        });
        item.status = "pending".into();
        item.run = None;
        item.run_uid = None;
        item.assignment_nonce = None;
        item.stuck_since = None;
        item.done_at = None;
        Ok(())
    })
    .await
    .map_err(persistence)
}

fn output_digest(data: &BTreeMap<String, String>) -> Result<String, ReconcileError> {
    Ok(crate::providers::signing::sha256_hex(&serde_json::to_vec(
        data,
    )?))
}

async fn finish_item(
    api: &Api<ConfigMap>,
    owner: &contract::Owner,
    binding: &contract::Binding,
    task: &KarsTask,
    data: &BTreeMap<String, String>,
    outcome: &str,
    feedback: Option<&str>,
) -> Result<(), ReconcileError> {
    let digest = output_digest(data)?;
    let finished = data
        .get("finishedAt")
        .filter(|value| super::parse_rfc3339(value).is_some())
        .ok_or_else(|| invalid("terminal work output has no valid finishedAt"))?;
    contract::update(api, owner, Some(&binding.backlog_uid), false, |items| {
        let item = items
            .iter_mut()
            .find(|item| item.id == binding.item_id)
            .ok_or_else(|| contract::Error::Conflict("completed work disappeared".into()))?;
        if !matches_run(item, task) || contract::request_digest(item)? != binding.request_digest {
            return Err(contract::Error::Conflict(
                "completion belongs to a different work request".into(),
            ));
        }
        let attempt = contract::Attempt {
            run: task.name_any(),
            run_uid: task
                .uid()
                .ok_or_else(|| contract::Error::Invalid("missing run UID".into()))?,
            assignment_nonce: item
                .assignment_nonce
                .clone()
                .ok_or_else(|| contract::Error::Invalid("missing run nonce".into()))?,
            outcome: outcome.into(),
            finished_at: finished.clone(),
            output_sha256: Some(digest.clone()),
            feedback: feedback.map(str::to_string),
        };
        if item.history.iter().any(|old| {
            old.run_uid == attempt.run_uid && old.assignment_nonce == attempt.assignment_nonce
        }) {
            return Err(contract::Error::Conflict(
                "work attempt already finalized".into(),
            ));
        }
        item.history.push(attempt);
        item.stuck_since = None;
        if matches!(outcome, "approved" | "completed") {
            item.status = "done".into();
            item.done_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            item.status = "pending".into();
            item.run = None;
            item.run_uid = None;
            item.assignment_nonce = None;
            item.done_at = None;
            item.feedback = feedback.map(str::to_string);
        }
        Ok(())
    })
    .await
    .map_err(persistence)
}

async fn review(
    client: &Client,
    team: &KarsTeam,
    task: &KarsTask,
    binding: &contract::Binding,
    item: &contract::TeamTask,
    data: &BTreeMap<String, String>,
) -> Result<KarsApproval, ReconcileError> {
    let owner = owner(team)?;
    let name = contract::approval_name(&owner, binding, item).map_err(persistence)?;
    let mut expected = KarsApproval::new(
        &name,
        KarsApprovalSpec {
            task_ref: LocalObjectRef {
                name: task.name_any(),
            },
            action: ApprovalAction {
                kind: "checkpoint".into(),
                summary: format!("Review '{}' for Team '{}'", item.title, team.name_any()),
                detail: Some(format!(
                    "Review only this run's retained output before it enters Team knowledge.\nWork: {}\nRun: {}\nTask UID: {}\nRevision: {}\nOutput SHA256: {}\nAcceptance criteria:\n{}",
                    item.id,
                    task.name_any(),
                    task.uid().ok_or_else(|| invalid("run has no UID"))?,
                    item.assignment_nonce
                        .as_deref()
                        .ok_or_else(|| invalid("run has no nonce"))?,
                    output_digest(data)?,
                    item.acceptance_criteria.join("\n"),
                )),
                requested_tier: None,
            },
            ttl: Some("P7D".into()),
            decision: None,
        },
    );
    expected.metadata.namespace = Some(owner.namespace.clone());
    expected.metadata.owner_references = Some(vec![tasks::owner_ref(team)?]);
    expected.metadata.labels = Some(
        [
            ("kars.azure.com/team".into(), team.name_any()),
            ("kars.azure.com/milestone".into(), item.id.clone()),
        ]
        .into(),
    );
    expected.metadata.annotations = Some(
        [
            (contract::ANNOTATION.into(), serde_json::to_string(binding)?),
            (
                contract::TASK_UID.into(),
                task.uid().ok_or_else(|| invalid("run has no UID"))?,
            ),
            (
                contract::RUN_NONCE.into(),
                item.assignment_nonce
                    .clone()
                    .ok_or_else(|| invalid("run has no nonce"))?,
            ),
        ]
        .into(),
    );
    for key in ["kars.azure.com/owner-name", "kars.azure.com/owner-sub"] {
        if let Some(value) = team.annotations().get(key) {
            expected.annotations_mut().insert(key.into(), value.clone());
        }
    }
    let approvals = Api::<KarsApproval>::namespaced(client.clone(), &owner.namespace);
    let approval = match approvals.get_opt(&name).await? {
        Some(approval) => approval,
        None => match approvals.create(&PostParams::default(), &expected).await {
            Ok(approval) => approval,
            Err(kube::Error::Api(error)) if error.code == 409 => approvals.get(&name).await?,
            Err(error) => return Err(error.into()),
        },
    };
    if !tasks::owned(&approval.metadata, team)
        || approval.metadata.deletion_timestamp.is_some()
        || request_snapshot(&approval.spec) != request_snapshot(&expected.spec)
        || [
            contract::ANNOTATION,
            contract::TASK_UID,
            contract::RUN_NONCE,
        ]
        .iter()
        .any(|key| approval.annotations().get(*key) != expected.annotations().get(*key))
    {
        return Err(invalid(
            "checkpoint ownership, request or output binding changed",
        ));
    }
    Ok(approval)
}

/// Called only after the existing run/output UID, nonce and completion fences.
/// `true` authorizes commons harvesting, not a tool or external action.
pub(super) async fn harvest(
    client: &Client,
    team: &KarsTeam,
    task: &KarsTask,
    data: &BTreeMap<String, String>,
    successful: bool,
) -> Result<bool, ReconcileError> {
    let Some(binding) = binding(task)? else {
        return Ok(true);
    };
    super::requests::validate_work_run(task)?;
    let owner = owner(team)?;
    let api = Api::<ConfigMap>::namespaced(client.clone(), &owner.namespace);
    let (cm, items) = contract::read(&api, &owner)
        .await
        .map_err(persistence)?
        .ok_or_else(|| invalid("bound work store disappeared"))?;
    if cm.uid().as_deref() != Some(&binding.backlog_uid) {
        return Err(invalid("bound work store UID changed"));
    }
    let item = items
        .iter()
        .find(|item| item.id == binding.item_id)
        .ok_or_else(|| invalid("bound work item disappeared"))?;
    if let Some(attempt) = item.history.iter().find(|attempt| {
        Some(&attempt.run_uid) == task.metadata.uid.as_ref()
            && Some(&attempt.assignment_nonce) == task.annotations().get(ANNOT_RUN_REQUESTED)
    }) {
        if attempt.run != task.name_any()
            || attempt.output_sha256.as_deref() != Some(output_digest(data)?.as_str())
        {
            return Err(invalid("retained terminal work evidence changed"));
        }

        return Ok(matches!(attempt.outcome.as_str(), "approved" | "completed"));
    }
    if !matches_run(item, task) || contract::request_digest(item)? != binding.request_digest {
        return Err(invalid("terminal result has no matching work claim"));
    }
    if !successful {
        finish_item(
            &api,
            &owner,
            &binding,
            task,
            data,
            "failed",
            Some("Previous run did not deliver successful output; inspect its retained records."),
        )
        .await?;
        return Ok(false);
    }
    if !item.review_required {
        finish_item(&api, &owner, &binding, task, data, "completed", None).await?;
        return Ok(true);
    }
    contract::update(&api, &owner, Some(&binding.backlog_uid), false, |items| {
        let current = items
            .iter_mut()
            .find(|current| current.id == binding.item_id)
            .ok_or_else(|| contract::Error::Conflict("reviewed work disappeared".into()))?;
        if !matches_run(current, task)
            || contract::request_digest(current)? != binding.request_digest
            || !matches!(current.status.as_str(), "active" | "awaiting_review")
        {
            return Err(contract::Error::Conflict(
                "reviewed work claim changed".into(),
            ));
        }
        current.status = "awaiting_review".into();
        Ok(())
    })
    .await
    .map_err(persistence)?;
    // Retire execution before creating the checkpoint. Otherwise its first
    // authority binding can race the generation change caused by idling.
    if task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
    {
        return Ok(false);
    }
    let approval = review(client, team, task, &binding, item, data).await?;
    let Some(status) = approval.status.as_ref() else {
        return Ok(false);
    };
    let phase = status.phase.as_deref();
    if matches!(phase, Some("Approved" | "Denied"))
        && !crate::kars_approval::approval_binding_matches_task(&approval, task)
    {
        return Err(invalid(
            "checkpoint decision lacks its exact Task/request binding",
        ));
    }
    match phase {
        Some("Approved") => {
            finish_item(
                &api,
                &owner,
                &binding,
                task,
                data,
                "approved",
                approval
                    .spec
                    .decision
                    .as_ref()
                    .and_then(|decision| decision.reason.as_deref()),
            )
            .await?;
            Ok(true)
        }
        Some("Denied" | "Expired" | "Stale") => {
            let feedback = approval.spec.decision.as_ref().and_then(|decision| decision.reason.as_deref())
                .unwrap_or("Previous checkpoint was not approved; inspect its retained decision before revising.");
            finish_item(
                &api,
                &owner,
                &binding,
                task,
                data,
                "changes_requested",
                Some(feedback),
            )
            .await?;
            Ok(false)
        }
        _ => Ok(false),
    }
}
