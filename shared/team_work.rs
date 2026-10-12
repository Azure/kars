// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared Team backlog contract and UID-fenced, compare-and-swap persistence.

use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{Api, api::PostParams};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const ANNOTATION: &str = "kars.azure.com/team-work";
pub const TASK_UID: &str = "kars.azure.com/team-work-task-uid";
pub const RUN_NONCE: &str = "kars.azure.com/team-work-run-nonce";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub backlog_uid: String,
    pub item_id: String,
    pub request_digest: String,
}

#[derive(Serialize)]
struct WorkRequest<'a> {
    id: &'a str,
    title: &'a str,
    description: &'a str,
    depends_on: &'a [String],
    acceptance_criteria: &'a [String],
    review_required: bool,
    feedback: &'a Option<String>,
}

fn request_content(task: &TeamTask) -> WorkRequest<'_> {
    WorkRequest {
        id: &task.id,
        title: &task.title,
        description: &task.description,
        depends_on: &task.depends_on,
        acceptance_criteria: &task.acceptance_criteria,
        review_required: task.review_required,
        feedback: &task.feedback,
    }
}

pub fn request(task: &TeamTask) -> serde_json::Value {
    serde_json::json!(request_content(task))
}

pub fn request_digest(task: &TeamTask) -> Result<String, serde_json::Error> {
    // Struct field order preserves issued controller bindings independently of
    // each binary's serde_json preserve_order feature.
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &request_content(task),
    )?)))
}

pub fn approval_name(owner: &Owner, binding: &Binding, task: &TeamTask) -> Result<String, Error> {
    let (Some(run_uid), Some(nonce)) = (&task.run_uid, &task.assignment_nonce) else {
        return Err(Error::Invalid("review has no immutable run binding".into()));
    };
    let identity = serde_json::to_vec(&(
        &owner.uid,
        &binding.backlog_uid,
        &binding.item_id,
        run_uid,
        nonce,
    ))?;
    Ok(format!(
        "checkpoint-{}",
        &hex::encode(Sha256::digest(identity))[..32]
    ))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub review_required: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Attempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub run: String,
    pub run_uid: String,
    pub assignment_nonce: String,
    pub outcome: String,
    pub finished_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Owner {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Kube(#[from] kube::Error),
    #[error("invalid Team backlog JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Team backlog: {0}")]
    Invalid(String),
    #[error("Team backlog changed: {0}")]
    Conflict(String),
}

impl Owner {
    pub fn configmap_name(&self) -> String {
        format!("kars-team-tasks-{}", self.name)
    }

    fn reference(&self) -> OwnerReference {
        OwnerReference {
            api_version: "kars.azure.com/v1alpha1".into(),
            kind: "KarsTeam".into(),
            name: self.name.clone(),
            uid: self.uid.clone(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    pub fn owns(&self, cm: &ConfigMap) -> bool {
        !self.uid.is_empty()
            && !self.name.is_empty()
            && !self.namespace.is_empty()
            && cm.metadata.name.as_deref() == Some(self.configmap_name().as_str())
            && cm.metadata.namespace.as_deref() == Some(self.namespace.as_str())
            && cm.metadata.deletion_timestamp.is_none()
            && cm.metadata.uid.as_ref().is_some_and(|uid| !uid.is_empty())
            && cm
                .metadata
                .resource_version
                .as_ref()
                .is_some_and(|rv| !rv.is_empty())
            && cm.metadata.owner_references.as_ref().is_some_and(|owners| {
                owners.len() == 1
                    && owners[0].api_version == "kars.azure.com/v1alpha1"
                    && owners[0].kind == "KarsTeam"
                    && owners[0].name == self.name
                    && owners[0].uid == self.uid
                    && owners[0].controller == Some(true)
            })
    }
}

pub fn parse(raw: &str) -> Result<Vec<TeamTask>, Error> {
    let tasks: Vec<TeamTask> = serde_json::from_str(raw)?;
    validate(&tasks)?;
    Ok(tasks)
}

pub fn finished(task: &TeamTask) -> bool {
    matches!(task.status.as_str(), "done" | "retired")
}

pub fn validate(tasks: &[TeamTask]) -> Result<(), Error> {
    let mut ids = BTreeSet::new();
    for task in tasks {
        if task.id.is_empty() || task.title.trim().is_empty() || !ids.insert(&task.id) {
            return Err(Error::Invalid(
                "missing or duplicate work identity/title".into(),
            ));
        }
        if !matches!(
            task.status.as_str(),
            "pending" | "active" | "awaiting_review" | "done" | "retired"
        ) {
            return Err(Error::Invalid(format!("unknown status for '{}'", task.id)));
        }
        let binding = [&task.run, &task.run_uid, &task.assignment_nonce];
        let unassigned = matches!(task.status.as_str(), "pending" | "retired");
        if unassigned && binding.iter().any(|value| value.is_some())
            || !unassigned
                && binding
                    .iter()
                    .any(|value| value.as_ref().is_none_or(String::is_empty))
        {
            return Err(Error::Invalid(format!(
                "incomplete run binding for '{}'",
                task.id
            )));
        }
        if task.status == "retired"
            && task
                .done_at
                .as_deref()
                .is_none_or(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
        {
            return Err(Error::Invalid(format!(
                "source retirement has no observation timestamp for '{}'",
                task.id
            )));
        }
        if task.status == "awaiting_review" && !task.review_required {
            return Err(Error::Invalid(format!(
                "unrequested review for '{}'",
                task.id
            )));
        }
        let mut attempts = BTreeSet::new();
        for attempt in &task.history {
            if attempt.run.is_empty()
                || attempt.run_uid.is_empty()
                || attempt.assignment_nonce.is_empty()
                || !matches!(
                    attempt.outcome.as_str(),
                    "failed" | "changes_requested" | "approved" | "completed" | "cancelled"
                )
                || if attempt.outcome == "cancelled" {
                    attempt.output_sha256.is_some()
                        || attempt
                            .feedback
                            .as_ref()
                            .is_none_or(|reason| reason.trim().is_empty())
                } else {
                    attempt.output_sha256.as_ref().is_none_or(|hash| {
                        hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                }
                || chrono::DateTime::parse_from_rfc3339(&attempt.finished_at).is_err()
                || !attempts.insert((&attempt.run_uid, &attempt.assignment_nonce))
            {
                return Err(Error::Invalid(format!(
                    "invalid attempt history for '{}'",
                    task.id
                )));
            }
        }
    }
    for task in tasks {
        if task
            .depends_on
            .iter()
            .any(|id| id == &task.id || !ids.contains(id))
        {
            return Err(Error::Invalid(format!(
                "invalid dependency for '{}'",
                task.id
            )));
        }
    }
    if tasks
        .iter()
        .filter(|task| matches!(task.status.as_str(), "active" | "awaiting_review"))
        .count()
        > 1
    {
        return Err(Error::Invalid("multiple active work assignments".into()));
    }
    let mut resolved = BTreeSet::new();
    loop {
        let before = resolved.len();
        for task in tasks {
            if task.depends_on.iter().all(|id| resolved.contains(id)) {
                resolved.insert(task.id.clone());
            }
        }
        if resolved.len() == tasks.len() {
            return Ok(());
        }
        if resolved.len() == before {
            return Err(Error::Invalid("cyclic work dependencies".into()));
        }
    }
}

pub fn next_pending(tasks: &[TeamTask]) -> Option<&TeamTask> {
    if tasks
        .iter()
        .any(|task| matches!(task.status.as_str(), "active" | "awaiting_review"))
    {
        return None;
    }
    tasks.iter().find(|task| {
        task.status == "pending"
            && task.depends_on.iter().all(|id| {
                tasks
                    .iter()
                    .any(|dependency| &dependency.id == id && finished(dependency))
            })
    })
}

pub async fn read(
    api: &Api<ConfigMap>,
    owner: &Owner,
) -> Result<Option<(ConfigMap, Vec<TeamTask>)>, Error> {
    let Some(cm) = api.get_opt(&owner.configmap_name()).await? else {
        return Ok(None);
    };
    if !owner.owns(&cm) {
        return Err(Error::Invalid(
            "store is not exclusively owned by the current Team UID; refusing implicit adoption"
                .into(),
        ));
    }
    let raw = cm
        .data
        .as_ref()
        .and_then(|data| data.get("tasks.json"))
        .ok_or_else(|| Error::Invalid("existing store has no tasks.json".into()))?;
    let items = parse(raw)?;
    Ok(Some((cm, items)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_digest_preserves_issued_controller_binding_across_json_features() {
        let item: super::TeamTask = serde_json::from_str(r#"{
            "id":"workflow-complete",
            "title":"Complete three-role checklist workflow",
            "description":"Coordinator plans the workflow, checklist-writer drafts the checklist, reviewer issues corrections, and principal synthesis exports the final markdown checklist for human review.",
            "depends_on":[],
            "acceptance_criteria":[
                "All three roles complete in sequence",
                "The exported checklist is one page in practical checklist form",
                "The content includes voluntary consent and withdrawal, accessible participation, a two-week pilot, exactly three risks and mitigations, measurable criteria, and a retrospective",
                "No personal data is included",
                "Human review is required before any further use"
            ],
            "review_required":true,"status":"pending"
        }"#).unwrap();
        assert_eq!(
            super::request_digest(&item).unwrap(),
            "fc85dbf484b88e645dccacac28c4ce7b00eb0bfbe47b40017be0d7eb3f414a0c"
        );
    }

    #[test]
    fn source_retirement_is_timestamped_unassigned_and_not_run_completion() {
        let raw = r#"[{"id":"retired","title":"Source withdrawn","status":"retired","done_at":"2026-10-08T00:00:00Z"},{"id":"next","title":"Next","status":"pending","depends_on":["retired"]}]"#;
        let mut tasks = super::parse(raw).unwrap();
        assert!(super::finished(&tasks[0]));
        assert_eq!(super::next_pending(&tasks).unwrap().id, "next");
        assert!(tasks[0].history.is_empty());
        tasks[0].done_at = None;
        assert!(super::validate(&tasks).is_err());
        tasks[0].done_at = Some("2026-10-08T00:00:00Z".into());
        tasks[0].run = Some("unproved-run".into());
        assert!(super::validate(&tasks).is_err());
        tasks[0].run = None;
        tasks[0].status = "done".into();
        assert!(super::validate(&tasks).is_err());
    }

    use super::*;
    use serde_json::json;

    fn pending(id: &str) -> TeamTask {
        TeamTask {
            id: id.into(),
            title: id.into(),
            status: "pending".into(),
            ..Default::default()
        }
    }

    #[test]
    fn malformed_unknown_partial_and_cyclic_work_is_not_an_empty_queue() {
        for raw in [
            "null",
            "{}",
            "not json",
            r#"[{"id":"a","title":"A","status":"pending","extra":true}]"#,
            r#"[{"id":"a","title":"A","status":"done"}]"#,
            r#"[{"id":"a","title":"A","status":"pending","run":"run"}]"#,
            r#"[{"id":"a","title":"A","status":"pending","depends_on":["missing"]}]"#,
            r#"[{"id":"a","title":"A","status":"pending","depends_on":["b"]},{"id":"b","title":"B","status":"pending","depends_on":["a"]}]"#,
        ] {
            assert!(parse(raw).is_err(), "{raw}");
        }
        assert!(parse("[]").unwrap().is_empty());
        assert!(validate(&[pending("a"), pending("a")]).is_err());
    }

    #[test]
    fn selection_respects_dependency_order_and_exclusive_review_barrier() {
        let mut dependent = pending("dependent");
        dependent.depends_on.push("first".into());
        let first = pending("first");
        let mut queue = vec![dependent, first];
        assert_eq!(next_pending(&queue).unwrap().id, "first");
        for status in ["active", "awaiting_review"] {
            queue[1].status = status.into();
            assert!(next_pending(&queue).is_none());
        }
        queue[1].status = "done".into();
        assert_eq!(next_pending(&queue).unwrap().id, "dependent");
    }

    #[test]
    fn custody_requires_exact_exclusive_team_uid_namespace_and_live_store() {
        let owner = Owner {
            namespace: "work".into(),
            name: "eng".into(),
            uid: "team-uid".into(),
        };
        let raw = json!({"metadata": {
            "name":owner.configmap_name(),"namespace":"work","uid":"cm-uid","resourceVersion":"1",
            "ownerReferences":[owner.reference()]
        }});
        assert!(owner.owns(&serde_json::from_value(raw.clone()).unwrap()));
        for (path, value) in [
            ("/metadata/name", json!("other")),
            ("/metadata/namespace", json!("other")),
            ("/metadata/uid", json!("")),
            ("/metadata/resourceVersion", json!("")),
            ("/metadata/ownerReferences", json!([])),
            (
                "/metadata/ownerReferences",
                json!([owner.reference(), owner.reference()]),
            ),
            ("/metadata/ownerReferences/0/uid", json!("old-team")),
            ("/metadata/ownerReferences/0/name", json!("other")),
            ("/metadata/ownerReferences/0/kind", json!("KarsTask")),
            ("/metadata/ownerReferences/0/controller", json!(false)),
        ] {
            let mut changed = raw.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                !owner.owns(&serde_json::from_value(changed).unwrap()),
                "{path}"
            );
        }
    }

    #[test]
    fn request_hash_ignores_progress_but_binds_each_intent_and_revision_feedback() {
        let item = pending("a");
        let digest = request_digest(&item).unwrap();
        let mut assigned = item.clone();
        assigned.status = "active".into();
        assigned.run = Some("run".into());
        assigned.run_uid = Some("run-uid".into());
        assigned.assignment_nonce = Some("nonce".into());
        assert_eq!(request_digest(&assigned).unwrap(), digest);
        for field in [
            "title",
            "description",
            "feedback",
            "criteria",
            "review",
            "dependency",
        ] {
            let mut changed = assigned.clone();
            match field {
                "title" => changed.title = "Other".into(),
                "description" => changed.description = "Other".into(),
                "feedback" => changed.feedback = Some("Revise".into()),
                "criteria" => changed.acceptance_criteria.push("Other".into()),
                "review" => changed.review_required = true,
                _ => changed.depends_on.push("Other".into()),
            }
            assert_ne!(request_digest(&changed).unwrap(), digest, "{field}");
        }
        let owner = Owner {
            namespace: "work".into(),
            name: "eng".into(),
            uid: "team-uid".into(),
        };
        let binding = Binding {
            backlog_uid: "cm-uid".into(),
            item_id: "a".into(),
            request_digest: digest,
        };
        let name = approval_name(&owner, &binding, &assigned).unwrap();
        assert_eq!(name, approval_name(&owner, &binding, &assigned).unwrap());
        assigned.run_uid = Some("next-run".into());
        assert_ne!(name, approval_name(&owner, &binding, &assigned).unwrap());
        assert!(approval_name(&owner, &binding, &item).is_err());
    }

    #[test]
    fn history_requires_real_identity_terminal_outcome_hash_timestamp_and_unique_attempt() {
        let mut item = pending("a");
        let attempt = Attempt {
            run: "run".into(),
            run_uid: "uid".into(),
            assignment_nonce: "nonce".into(),
            outcome: "failed".into(),
            finished_at: "2026-10-08T22:00:00Z".into(),
            output_sha256: Some("a".repeat(64)),
            feedback: None,
        };
        item.history.push(attempt.clone());
        validate(&[item.clone()]).unwrap();
        for field in [
            "run",
            "uid",
            "nonce",
            "outcome",
            "hash",
            "timestamp",
            "duplicate",
        ] {
            let mut changed = item.clone();
            match field {
                "run" => changed.history[0].run.clear(),
                "uid" => changed.history[0].run_uid.clear(),
                "nonce" => changed.history[0].assignment_nonce.clear(),
                "outcome" => changed.history[0].outcome = "active".into(),
                "hash" => changed.history[0].output_sha256 = Some("z".repeat(64)),
                "timestamp" => changed.history[0].finished_at = "unknown".into(),
                _ => changed.history.push(attempt.clone()),
            }
            assert!(validate(&[changed]).is_err(), "{field}");
        }
    }
}
#[cfg(test)]
#[test]
fn cancellation_history_requires_a_recorded_reason_and_no_fabricated_output_digest() {
    let mut item = TeamTask {
        id: "work".into(),
        title: "Work".into(),
        status: "pending".into(),
        history: vec![Attempt {
            run: "run".into(),
            run_uid: "run-uid".into(),
            assignment_nonce: "nonce".into(),
            outcome: "cancelled".into(),
            finished_at: "2026-10-09T01:14:00Z".into(),
            output_sha256: None,
            feedback: Some("Operator stopped this attempt".into()),
        }],
        ..Default::default()
    };
    validate(&[item.clone()]).unwrap();
    item.history[0].output_sha256 = Some("a".repeat(64));
    assert!(validate(&[item.clone()]).is_err());
    item.history[0].output_sha256 = None;
    item.history[0].feedback = None;
    assert!(validate(&[item.clone()]).is_err());
    item.history[0].outcome = "completed".into();
    assert!(validate(&[item]).is_err());
}

pub async fn update<T>(
    api: &Api<ConfigMap>,
    owner: &Owner,
    expected_uid: Option<&str>,
    create: bool,
    change: impl Fn(&mut Vec<TeamTask>) -> Result<T, Error>,
) -> Result<T, Error> {
    if owner.uid.is_empty() || owner.name.is_empty() || owner.namespace.is_empty() {
        return Err(Error::Invalid("missing Team identity".into()));
    }
    let mut bound_uid = expected_uid.map(str::to_owned);
    for _ in 0..8 {
        let current = read(api, owner).await?;
        let existed = current.is_some();
        let (mut cm, mut items) = match current {
            Some(value) => value,
            None if create && bound_uid.is_none() => {
                let mut cm = ConfigMap::default();
                cm.metadata.name = Some(owner.configmap_name());
                cm.metadata.namespace = Some(owner.namespace.clone());
                cm.metadata.owner_references = Some(vec![owner.reference()]);
                cm.metadata.labels =
                    Some([("kars.azure.com/team-tasks".into(), owner.name.clone())].into());
                (cm, Vec::new())
            }
            None => return Err(Error::Conflict("bound store disappeared".into())),
        };
        if bound_uid
            .as_deref()
            .is_some_and(|uid| cm.metadata.uid.as_deref() != Some(uid))
        {
            return Err(Error::Conflict("bound store UID changed".into()));
        }
        if existed && bound_uid.is_none() {
            bound_uid = cm.metadata.uid.clone();
        }
        let original = items.clone();
        let result = change(&mut items)?;
        validate(&items)?;
        if existed && original == items {
            return Ok(result);
        }
        cm.data
            .get_or_insert_with(BTreeMap::new)
            .insert("tasks.json".into(), serde_json::to_string(&items)?);
        let write = if existed {
            api.replace(&owner.configmap_name(), &PostParams::default(), &cm)
                .await
        } else {
            api.create(&PostParams::default(), &cm).await
        };
        match write {
            Ok(_) => return Ok(result),
            Err(kube::Error::Api(error)) if error.code == 409 => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Conflict(
        "compare-and-swap retry limit reached".into(),
    ))
}
