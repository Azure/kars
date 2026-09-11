use kube::ResourceExt;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::cluster::Cluster;
use crate::kars::task::KarsTask;

pub(crate) fn task_is_owned_by(task: &KarsTask, principal: &Principal) -> bool {
    task.annotations()
        .get("kars.azure.com/owner-sub")
        .is_some_and(|subject| subject == &principal.sub)
}

pub(crate) fn output_is_owned_by(
    output: &std::collections::BTreeMap<String, String>,
    principal: &Principal,
) -> bool {
    output
        .get("ownerSub")
        .is_some_and(|subject| subject == &principal.sub)
}

pub(crate) fn principal_can_view_all(principal: &Principal) -> bool {
    principal
        .roles
        .iter()
        .any(|role| matches!(role.as_str(), "admin" | "operator"))
}

pub(crate) fn principal_can_audit_all(principal: &Principal) -> bool {
    principal_can_view_all(principal) || principal.roles.iter().any(|role| role == "auditor")
}

pub(crate) async fn require_owned_task(
    cluster: &Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<KarsTask> {
    let task = cluster
        .tasks(ns)
        .get_opt(name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .ok_or(AppError::NotFound)?;
    if !task_is_owned_by(&task, principal) {
        return Err(AppError::NotFound);
    }
    Ok(task)
}

/// Authorize access to retained task evidence after the task CR itself has been
/// garbage-collected. A live task's owner annotation always wins; only a missing
/// task may fall back to the controller-persisted output owner.
pub(crate) async fn require_owned_task_or_output(
    cluster: &Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<Option<KarsTask>> {
    if let Some(task) = cluster
        .tasks(ns)
        .get_opt(name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
    {
        if !task_is_owned_by(&task, principal) {
            return Err(AppError::NotFound);
        }
        return Ok(Some(task));
    }

    let output = cluster
        .read_mission_output(name)
        .await
        .ok_or(AppError::NotFound)?;
    if !output_is_owned_by(&output, principal) {
        return Err(AppError::NotFound);
    }
    Ok(None)
}

pub(crate) async fn require_task_evidence_access(
    cluster: &Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<Option<KarsTask>> {
    if principal_can_audit_all(principal) {
        return Ok(None);
    }
    require_owned_task_or_output(cluster, ns, name, principal).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars::task::{KarsTaskSpec, TaskEnvelope};

    fn principal(sub: &str, roles: &[&str]) -> Principal {
        Principal {
            sub: sub.to_string(),
            name: format!("{sub}@example.test"),
            roles: roles.iter().map(|role| role.to_string()).collect(),
        }
    }

    fn task(name: &str) -> KarsTask {
        KarsTask::new(
            name,
            KarsTaskSpec {
                objective: "test objective".into(),
                envelope: TaskEnvelope {
                    tier: 1,
                    budget: None,
                    tool_policy_ref: None,
                    egress_allowlist_ref: None,
                    delegation_depth: 0,
                    authority_ceiling: 1,
                },
                parent_ref: None,
                execution: None,
                blueprint: None,
                display_name: None,
                retention_ttl_seconds: None,
            },
        )
    }

    #[test]
    fn ownership_requires_exact_immutable_subject() {
        let mut owned_task = task("owned");
        owned_task
            .metadata
            .annotations
            .get_or_insert_with(Default::default)
            .insert("kars.azure.com/owner-sub".into(), "subject-a".into());

        assert!(task_is_owned_by(
            &owned_task,
            &principal("subject-a", &["user"]),
        ));
        assert!(!task_is_owned_by(
            &owned_task,
            &principal("subject-b", &["user"]),
        ));
        assert!(!task_is_owned_by(
            &task("legacy"),
            &principal("subject-a", &["user"]),
        ));
    }

    #[test]
    fn retained_output_uses_persisted_owner() {
        let output =
            std::collections::BTreeMap::from([("ownerSub".to_string(), "subject-a".to_string())]);
        assert!(output_is_owned_by(
            &output,
            &principal("subject-a", &["user"]),
        ));
        assert!(!output_is_owned_by(
            &output,
            &principal("subject-b", &["user"]),
        ));
    }

    #[test]
    fn only_operator_or_admin_can_request_cluster_aggregates() {
        assert!(!principal_can_view_all(&principal("user", &["user"])));
        assert!(principal_can_view_all(&principal(
            "operator",
            &["operator"]
        )));
        assert!(principal_can_view_all(&principal("admin", &["admin"])));
    }

    #[test]
    fn audit_evidence_is_visible_to_auditors_and_operators() {
        assert!(!principal_can_audit_all(&principal("user", &["user"])));
        assert!(principal_can_audit_all(&principal("auditor", &["auditor"])));
        assert!(principal_can_audit_all(&principal(
            "operator",
            &["operator"]
        )));
        assert!(principal_can_audit_all(&principal("admin", &["admin"])));
    }
}
