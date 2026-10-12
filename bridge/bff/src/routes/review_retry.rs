// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ReviewEntry, ReviewRequest, ReviewStateDto, review_state};
use crate::{
    auth::Principal,
    error::{AppError, AppResult},
    kars::{cluster::Cluster, task::KarsTask},
};
use kube::ResourceExt;
use std::collections::BTreeMap;

fn candidate(task: &KarsTask, data: &BTreeMap<String, String>) -> Option<String> {
    let nonce = data
        .get("requestedRevisionNonce")
        .filter(|s| !s.is_empty())?;
    let annotations = task.annotations();
    (task.namespace().as_deref() == Some("kars-system")
        && task.metadata.deletion_timestamp.is_none()
        && task
            .uid()
            .filter(|s| !s.is_empty())
            .is_some_and(|uid| data.get("taskUid") == Some(&uid))
        && task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| !execution.launch)
        && data.get("status").map(String::as_str) == Some("changes_requested")
        && data.get("redrivePending").map(String::as_str) == Some("true")
        && annotations.get("kars.azure.com/run-requested") == Some(nonce)
        && annotations.get("kars.azure.com/run-completed") != Some(nonce)
        && annotations.get("kars.azure.com/run-objective-nonce") == Some(nonce))
    .then(|| nonce.clone())
}

pub(super) async fn eligible(
    cluster: &Cluster,
    task: &KarsTask,
    principal: &Principal,
    data: &BTreeMap<String, String>,
) -> Option<String> {
    super::require_mission_review(task).ok()?;
    let nonce = candidate(task, data)?;
    let proof = crate::routes::tasks::read_reviewed_execution(cluster, task, principal).await?;
    (proof.issue.is_none()
        && proof.run_nonce == nonce
        && proof.phases.iter().any(|phase| phase.state == "failed"))
    .then_some(nonce)
}

pub(super) async fn request(
    cluster: &Cluster,
    task: &KarsTask,
    principal: &Principal,
    body: &ReviewRequest,
) -> AppResult<ReviewStateDto> {
    if body
        .comment
        .as_deref()
        .is_some_and(|comment| !comment.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "retry preserves the existing feedback; it does not accept new instructions".into(),
        ));
    }
    let prior = cluster
        .review_snapshot("kars-system", &task.name_any())
        .await?
        .ok_or_else(|| AppError::Conflict("the revision review record is unavailable".into()))?;
    let mut next = prior
        .data
        .clone()
        .ok_or_else(|| AppError::Conflict("the revision review record is empty".into()))?;
    if eligible(cluster, task, principal, &next).await.as_deref()
        != Some(body.assignment_nonce.as_str())
    {
        return Err(AppError::Conflict(
            "retry requires this stopped revision and verified failed phase evidence; refresh before retrying".into(),
        ));
    }
    let revision = next
        .get("revision")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n >= 0)
        .ok_or_else(|| AppError::Conflict("the review revision is invalid".into()))?;
    let mut history: Vec<ReviewEntry> = serde_json::from_str(
        next.get("history")
            .ok_or_else(|| AppError::Conflict("the review history is missing".into()))?,
    )
    .map_err(|_| AppError::Conflict("the review history is invalid".into()))?;
    history.push(ReviewEntry {
        decision: "retry_revision".into(),
        comment: Some("Retry requested after a verified phase failure. Complete documents and feedback retained; launch remains stopped.".into()),
        reviewer: principal.name.clone(),
        decided_at: chrono::Utc::now().to_rfc3339(),
        revision,
        attested: true,
        assignment_nonce: Some(body.assignment_nonce.clone()),
    });
    let encoded_history =
        serde_json::to_string(&history).map_err(|error| AppError::Internal(error.into()))?;
    let nonce = cluster
        .retry_stopped_revision(task, &body.assignment_nonce)
        .await?;
    next.insert("requestedRevisionNonce".into(), nonce);
    next.insert("history".into(), encoded_history);
    // As with a new feedback revision, a journal failure is reported without
    // replaying or undoing the separately committed, still-stopped request.
    cluster
        .write_review(task, Some(&prior), next.clone())
        .await?;
    Ok(review_state(task, &next, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (KarsTask, BTreeMap<String, String>) {
        let task = serde_json::from_value(json!({
            "metadata":{"name":"briefing","namespace":"kars-system","uid":"uid","annotations":{
                "kars.azure.com/run-requested":"revision",
                "kars.azure.com/run-completed":"previous",
                "kars.azure.com/run-objective-nonce":"revision"}},
            "spec":{"objective":"Briefing","execution":{"launch":false},
                "envelope":{"tier":1,"authorityCeiling":1}}
        }))
        .unwrap();
        let data = [
            ("taskUid", "uid"),
            ("requestedRevisionNonce", "revision"),
            ("status", "changes_requested"),
            ("redrivePending", "true"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        (task, data)
    }

    #[test]
    fn retry_candidate_requires_stopped_bound_pending_revision() {
        let (task, data) = fixture();
        assert_eq!(candidate(&task, &data).as_deref(), Some("revision"));
        for (key, value) in [
            ("taskUid", "other"),
            ("requestedRevisionNonce", "stale"),
            ("status", "approved"),
            ("redrivePending", "false"),
        ] {
            let mut changed = data.clone();
            changed.insert(key.into(), value.into());
            assert!(candidate(&task, &changed).is_none());
        }
        let mut running = task.clone();
        running.spec.execution.as_mut().unwrap().launch = true;
        assert!(candidate(&running, &data).is_none());
        for key in [
            "kars.azure.com/run-requested",
            "kars.azure.com/run-objective-nonce",
        ] {
            let mut changed = task.clone();
            changed.metadata.annotations.as_mut().unwrap().remove(key);
            assert!(candidate(&changed, &data).is_none());
        }
        let mut completed = task;
        completed
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-completed".into(), "revision".into());
        assert!(candidate(&completed, &data).is_none());
    }
}
