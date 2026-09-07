// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_approval::{ApprovalDecision, KarsApproval, KarsApprovalSpec, request_snapshot};
use crate::kars_task::{
    KarsTaskStatus, TaskBlueprint, TaskBudget, TaskEgress, TaskEnvelope, TaskModel,
};
use crate::kars_team::{KarsTeamSpec, TeamCadence, TeamRole};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

pub(super) fn team() -> KarsTeam {
    let mut team = KarsTeam::new(
        "eng",
        KarsTeamSpec {
            charter: "Keep the repository healthy".into(),
            envelope: TaskEnvelope {
                tier: 4,
                authority_ceiling: 3,
                delegation_depth: 2,
                budget: Some(TaskBudget {
                    tokens: Some(1_000),
                    usd_micros: Some(2_000),
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    team.metadata.namespace = Some("tenant-a".into());
    team.metadata.uid = Some("team-uid".into());
    team.metadata.generation = Some(1);
    team.metadata.resource_version = Some("10".into());
    team.metadata.finalizers = Some(vec![FINALIZER.into()]);
    team
}

pub(super) fn principal(team: &KarsTeam) -> KarsTask {
    let mut task = KarsTask::new(&specs::principal_name(team), specs::principal_spec(team));
    task.spec
        .blueprint
        .get_or_insert_with(Default::default)
        .model
        .get_or_insert_with(|| TaskModel {
            provider: "azure-openai".into(),
            deployment: "reviewed-model".into(),
        });
    task.metadata.namespace = team.metadata.namespace.clone();
    task.metadata.uid = Some("principal-uid".into());
    task.metadata.generation = Some(2);
    task.metadata.resource_version = Some("20".into());
    task.metadata.owner_references = Some(vec![tasks::owner_ref(team).unwrap()]);
    task.metadata.annotations = Some([(ANNOT_TEAM_ROLE.into(), "principal".into())].into());
    task.status = Some(KarsTaskStatus {
        phase: Some("Ready".into()),
        observed_generation: task.metadata.generation,
        envelope_digest: Some(task.envelope_digest()),
        conditions: Some(vec![Condition {
            type_: "Ready".into(),
            status: "True".into(),
            reason: "Validated".into(),
            message: "Validated".into(),
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
            observed_generation: task.metadata.generation,
        }]),
        ..Default::default()
    });
    task
}

pub(super) fn approved(team: &KarsTeam, principal: &KarsTask) -> KarsApproval {
    let now = Utc::now();
    let name = promotion::ticket_name(team, principal, 5).unwrap();
    let mut approval = KarsApproval::new(
        &name,
        KarsApprovalSpec {
            task_ref: LocalObjectRef {
                name: principal.name_any(),
            },
            action: crate::kars_approval::ApprovalAction {
                kind: "tierRaise".into(),
                requested_tier: Some(5),
                ..Default::default()
            },
            ttl: Some("PT1H".into()),
            decision: Some(ApprovalDecision {
                verdict: "approve".into(),
                decider: "human@example.com".into(),
                reason: None,
            }),
        },
    );
    approval.metadata.namespace = team.metadata.namespace.clone();
    approval.metadata.owner_references = Some(vec![tasks::owner_ref(team).unwrap()]);
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.generation = Some(2);
    approval.metadata.resource_version = Some("30".into());
    approval.metadata.annotations = Some(
        [
            (
                "kars.azure.com/promotion-principal-uid".into(),
                principal.metadata.uid.clone().unwrap(),
            ),
            (
                "kars.azure.com/promotion-principal-digest".into(),
                principal.envelope_digest(),
            ),
        ]
        .into(),
    );
    approval.status = Some(crate::kars_approval::KarsApprovalStatus {
        phase: Some("Approved".into()),
        observed_generation: approval.metadata.generation,
        bound_task_uid: principal.metadata.uid.clone(),
        bound_envelope_digest: Some(principal.envelope_digest()),
        bound_request: Some(request_snapshot(&approval.spec)),
        requested_at: Some((now - chrono::Duration::minutes(30)).to_rfc3339()),
        decided_at: Some((now - chrono::Duration::minutes(1)).to_rfc3339()),
        expires_at: Some((now + chrono::Duration::minutes(30)).to_rfc3339()),
        decider: Some("human@example.com".into()),
        ..Default::default()
    });
    approval
}

#[test]
fn default_member_clamps_actual_tier_to_parent_ceiling() {
    let mut parent = team().spec.envelope;
    parent.authority_ceiling = 1;
    let child = specs::default_member_envelope(&parent);
    assert_eq!(child.tier, 1);
    assert_eq!(child.authority_ceiling, 1);
    assert!(child.delegation_depth < parent.delegation_depth);
    assert!(child.attenuation_violations(&parent).is_empty());
    parent.tier = 1;
    assert_eq!(specs::default_member_envelope(&parent).tier, 1);
}

#[test]
fn zero_depth_passive_team_is_valid_but_members_and_cadence_are_not() {
    let mut team = team();
    team.spec.envelope.delegation_depth = 0;
    assert!(team.validation_errors().is_empty());
    team.spec.roster.push(TeamRole {
        name: "reader".into(),
        ..Default::default()
    });
    assert!(!team.validation_errors().is_empty());
    team.spec.roster.clear();
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    assert!(!team.validation_errors().is_empty());
}

#[test]
fn generated_children_validate_blueprint_policy_and_egress() {
    let mut team = team();
    team.spec.blueprint = Some(TaskBlueprint {
        tool_policy: Some("read-only".into()),
        ..Default::default()
    });
    team.spec.roster = vec![TeamRole {
        name: "reader".into(),
        blueprint: Some(TaskBlueprint {
            tool_policy: Some("write-all".into()),
            egress: vec![TaskEgress {
                host: "unapproved.example".into(),
                port: Some(443),
            }],
            ..Default::default()
        }),
        ..Default::default()
    }];
    let errors = team.validation_errors().join("; ");
    assert!(errors.contains("reader"));
    assert!(errors.contains("unapproved.example"));
    team.spec.roster[0].blueprint = Some(TaskBlueprint {
        tool_policy: Some("read-only".into()),
        ..Default::default()
    });
    assert!(team.validation_errors().is_empty());
}

#[test]
fn negative_budgets_invalid_ceiling_and_sanitized_reserved_roles_rejected() {
    for name in ["principal", "run-42", "", "!!!"] {
        let mut team = team();
        team.spec.roster.push(TeamRole {
            name: name.into(),
            ..Default::default()
        });
        assert!(!team.validation_errors().is_empty(), "{name}");
    }
    let mut team = team();
    team.spec.roster = ["Bugfix Engineer", "bugfix-engineer"]
        .into_iter()
        .map(|name| TeamRole {
            name: name.into(),
            ..Default::default()
        })
        .collect();
    assert!(!team.validation_errors().is_empty());
    team.spec.roster.clear();
    team.spec.envelope.authority_ceiling = 0;
    assert!(!team.validation_errors().is_empty());
    team.spec.envelope.authority_ceiling = 3;
    team.spec.envelope.budget.as_mut().unwrap().usd_micros = Some(-1);
    assert!(!team.validation_errors().is_empty());
}

#[test]
fn explicit_empty_egress_does_not_inherit_team_egress() {
    let mut team = team();
    team.spec.blueprint = Some(TaskBlueprint {
        runtime: Some("Hermes".into()),
        isolation: Some("confidential".into()),
        egress: vec![TaskEgress {
            host: "api.example".into(),
            port: None,
        }],
        ..Default::default()
    });
    let mut role = TeamRole {
        name: "reader".into(),
        ..Default::default()
    };
    assert_eq!(
        specs::member_blueprint(&team, &role)
            .unwrap()
            .runtime
            .as_deref(),
        Some("Hermes")
    );
    role.blueprint = Some(TaskBlueprint::default());
    assert!(
        specs::member_blueprint(&team, &role)
            .unwrap()
            .egress
            .is_empty()
    );
}

#[test]
fn promotion_rejects_checkpoint_wrong_task_uid_digest_owner_and_decision() {
    let mut team = team();
    team.spec.requested_tier = Some(5);
    let principal = principal(&team);
    let good = approved(&team, &principal);
    assert!(promotion::authorized(&team, &principal, &good, 5));
    let mut bad = good.clone();
    bad.spec.action.kind = "checkpoint".into();
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.status.as_mut().unwrap().bound_task_uid = Some("recreated-principal".into());
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.status.as_mut().unwrap().bound_envelope_digest = Some("old-authority".into());
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.metadata.owner_references = None;
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.metadata.namespace = Some("tenant-b".into());
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.spec.decision.as_mut().unwrap().verdict = "deny".into();
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
    bad = good.clone();
    bad.status.as_mut().unwrap().decider = Some("different-human".into());
    assert!(!promotion::authorized(&team, &principal, &bad, 5));
}

#[test]
fn promotion_requires_current_readiness_and_single_transition_identity() {
    let mut team = team();
    team.spec.requested_tier = Some(5);
    let mut principal = principal(&team);
    let approval = approved(&team, &principal);
    principal.metadata.generation = Some(3);
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
    principal.metadata.generation = Some(2);
    let first = promotion::ticket_name(&team, &principal, 5).unwrap();
    team.metadata.generation = Some(2);
    assert_ne!(first, promotion::ticket_name(&team, &principal, 5).unwrap());
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
    team.metadata.generation = Some(1);
    team.metadata.annotations = Some(
        [(
            "kars.azure.com/consumed-promotion".into(),
            "approval-uid".into(),
        )]
        .into(),
    );
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
}

#[test]
fn promotion_ttl_limits_first_decision_not_terminal_consumption() {
    let mut team = team();
    team.spec.requested_tier = Some(5);
    let principal = principal(&team);
    let mut approval = approved(&team, &principal);
    let now = Utc::now();
    let status = approval.status.as_mut().unwrap();
    status.requested_at = Some((now - chrono::Duration::hours(3)).to_rfc3339());
    status.decided_at = Some((now - chrono::Duration::minutes(150)).to_rfc3339());
    status.expires_at = Some((now - chrono::Duration::hours(2)).to_rfc3339());
    assert!(promotion::authorized(&team, &principal, &approval, 5));
    approval.status.as_mut().unwrap().decided_at = Some(now.to_rfc3339());
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
}

#[test]
fn blueprint_only_authority_drift_invalidates_promotion_and_ticket() {
    let mut team = team();
    team.spec.requested_tier = Some(5);
    let mut principal = principal(&team);
    let approval = approved(&team, &principal);
    let ticket = promotion::ticket_name(&team, &principal, 5).unwrap();
    principal.spec.blueprint.as_mut().unwrap().isolation = Some("confidential".into());
    principal.metadata.generation = Some(3);
    principal.status.as_mut().unwrap().observed_generation = Some(3);
    assert!(promotion::ticket_name(&team, &principal, 5).is_err());
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
    let digest = principal.envelope_digest();
    let status = principal.status.as_mut().unwrap();
    status.observed_generation = Some(3);
    status.envelope_digest = Some(digest);
    assert_ne!(
        ticket,
        promotion::ticket_name(&team, &principal, 5).unwrap()
    );
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
}

#[test]
fn cadence_slot_survives_failed_status_write_and_separates_generations() {
    let mut team = team();
    let name = runs::cadence_name(&team).unwrap();
    assert_eq!(name, runs::cadence_name(&team).unwrap());
    team.metadata.generation = Some(2);
    assert_ne!(name, runs::cadence_name(&team).unwrap());
    team.metadata.generation = Some(1);
    team.status = Some(KarsTeamStatus {
        generated_task_count: 1,
        ..Default::default()
    });
    assert_ne!(name, runs::cadence_name(&team).unwrap());
}

#[test]
fn only_finite_positive_budgets_block_execution_not_planning() {
    let mut team = team();
    assert!(specs::has_positive_budget(&team.spec.envelope));
    assert!(team.validation_errors().is_empty());
    team.spec.envelope.budget = Some(TaskBudget {
        tokens: Some(0),
        usd_micros: None,
    });
    assert!(!specs::has_positive_budget(&team.spec.envelope));
    team.spec.envelope.budget.as_mut().unwrap().usd_micros = Some(1);
    assert!(specs::has_positive_budget(&team.spec.envelope));
    team.spec.envelope.budget = None;
    assert!(!specs::has_positive_budget(&team.spec.envelope));
}
