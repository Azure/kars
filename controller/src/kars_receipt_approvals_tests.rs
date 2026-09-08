// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_approval::{
    ApprovalAction, ApprovalDecision, KarsApprovalSpec, KarsApprovalStatus,
    approval_authorizes_task,
};
use crate::kars_task::{KarsTaskSpec, KarsTaskStatus, TaskBlueprint, TaskModel};
use crate::mcp_server::LocalObjectRef;

fn ready(task: &mut KarsTask) {
    task.status = Some(KarsTaskStatus {
        phase: Some("Ready".into()),
        observed_generation: task.metadata.generation,
        envelope_digest: Some(task.envelope_digest()),
        conditions: Some(vec![crate::status::conditions::new_condition(
            "Ready",
            "True",
            "Reconciled",
            "validated",
            task.metadata.generation,
        )]),
        ..Default::default()
    });
}

fn fixture() -> (KarsTask, KarsApproval) {
    let mut task = KarsTask::new(
        "task",
        KarsTaskSpec {
            objective: "Review a change".into(),
            blueprint: Some(TaskBlueprint {
                model: Some(TaskModel {
                    provider: "azure-openai".into(),
                    deployment: "reviewed-model".into(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    task.metadata.uid = Some("task-uid".into());
    task.metadata.namespace = Some("work".into());
    task.metadata.generation = Some(1);
    ready(&mut task);
    let mut approval = KarsApproval::new(
        "promote",
        KarsApprovalSpec {
            task_ref: LocalObjectRef {
                name: "task".into(),
            },
            action: ApprovalAction {
                kind: "tierRaise".into(),
                summary: "Permit tier 2".into(),
                requested_tier: Some(2),
                ..Default::default()
            },
            ttl: Some("PT1H".into()),
            decision: Some(ApprovalDecision {
                verdict: "approve".into(),
                decider: "alice".into(),
                reason: None,
            }),
        },
    );
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.namespace = Some("work".into());
    approval.metadata.generation = Some(2);
    approval.status = Some(KarsApprovalStatus {
        phase: Some("Approved".into()),
        observed_generation: Some(2),
        bound_task_uid: task.metadata.uid.clone(),
        bound_envelope_digest: Some(task.envelope_digest()),
        bound_request: Some(request_snapshot(&approval.spec)),
        requested_at: Some("2026-09-07T08:00:00Z".into()),
        decided_at: Some("2026-09-07T08:15:00Z".into()),
        expires_at: Some("2026-09-07T09:00:00Z".into()),
        decider: Some("alice".into()),
        ..Default::default()
    });
    (task, approval)
}

#[test]
fn d0_decision_survives_d1_without_becoming_a_current_grant() {
    use crate::kars_receipt::{PredicateCompleteness, build_statement, canonical_json};
    let (mut task, approval) = fixture();
    assert!(approval_authorizes_task(&approval, &task));
    let d0 = task.envelope_digest();
    task.spec.envelope.tier = 2;
    task.metadata.generation = Some(2);
    ready(&mut task);
    let d1 = task.envelope_digest();
    assert_ne!(d0, d1);
    assert!(!approval_authorizes_task(&approval, &task));
    let history = historical_approval_facts(&task, std::slice::from_ref(&approval));
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].task_uid, "task-uid");
    assert_eq!(history[0].task_name, "task");
    assert_eq!(history[0].task_namespace, "work");
    assert_eq!(history[0].approval_uid, "approval-uid");
    assert_eq!(history[0].bound_envelope_digest, d0);
    assert_eq!(history[0].bound_request, request_snapshot(&approval.spec));
    assert_eq!(history[0].evidence_scope, "historicalDecision");
    assert!(!history[0].authorizes_current_task);
    assert!(!history[0].consumption_attested);
    let mut statement = build_statement(
        &task,
        task.status.as_ref().unwrap(),
        "key",
        &[],
        PredicateCompleteness::default(),
    )
    .unwrap();
    statement.predicate.approval_history = history;
    let signed_payload: serde_json::Value =
        serde_json::from_slice(&canonical_json(&statement)).unwrap();
    assert_eq!(
        signed_payload["subject"][0]["digest"]["sha256"],
        d1.trim_start_matches("sha256:")
    );
    assert_eq!(
        signed_payload["predicate"]["approvalHistory"][0]["boundEnvelopeDigest"],
        d0
    );
    assert!(signed_payload["predicate"].get("approvals").is_none());
    assert_eq!(
        approval.status.as_ref().unwrap().phase.as_deref(),
        Some("Approved")
    );
}

#[test]
fn history_never_claims_an_unconsumed_approval_caused_a_transition() {
    let (task, mut approval) = fixture();
    let before = historical_approval_facts(&task, std::slice::from_ref(&approval));
    assert!(!before[0].authorizes_current_task);
    assert!(!before[0].consumption_attested);
    approval.metadata.annotations = Some(std::collections::BTreeMap::from([(
        "kars.azure.com/consumed-by".into(),
        "untrusted-annotation".into(),
    )]));
    let after = historical_approval_facts(&task, &[approval]);
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[test]
fn historical_records_reject_task_or_request_rebinding() {
    type Change = fn(&mut KarsApproval);
    let (task, approval) = fixture();
    let changes: &[Change] = &[
        |a| a.status.as_mut().unwrap().bound_task_uid = Some("replacement-task".into()),
        |a| a.status.as_mut().unwrap().bound_task_uid = None,
        |a| a.spec.task_ref.name = "another-task".into(),
        |a| a.metadata.namespace = Some("other".into()),
        |a| a.metadata.uid = None,
        |a| a.metadata.name = None,
        |a| a.spec.action.requested_tier = Some(5),
        |a| a.status.as_mut().unwrap().bound_request = None,
        |a| a.status.as_mut().unwrap().bound_envelope_digest = None,
        |a| a.status.as_mut().unwrap().bound_envelope_digest = Some("invalid-digest".into()),
    ];
    for change in changes {
        let mut invalid = approval.clone();
        change(&mut invalid);
        assert!(historical_approval_facts(&task, &[invalid]).is_empty());
    }
    let mut replacement = task.clone();
    replacement.metadata.uid = Some("replacement-task".into());
    assert!(historical_approval_facts(&replacement, std::slice::from_ref(&approval)).is_empty());
    replacement.metadata.uid = None;
    assert!(historical_approval_facts(&replacement, std::slice::from_ref(&approval)).is_empty());
}

#[test]
fn history_requires_a_coherent_terminal_decision_and_timely_recording() {
    type Change = fn(&mut KarsApproval);
    let (task, approval) = fixture();
    let changes: &[Change] = &[
        |a| a.status.as_mut().unwrap().phase = Some("Pending".into()),
        |a| a.status.as_mut().unwrap().phase = Some("Expired".into()),
        |a| a.status.as_mut().unwrap().phase = Some("Stale".into()),
        |a| a.spec.decision = None,
        |a| a.spec.decision.as_mut().unwrap().verdict = "deny".into(),
        |a| a.spec.decision.as_mut().unwrap().decider = "mallory".into(),
        |a| {
            a.spec.decision.as_mut().unwrap().decider = " ".into();
            a.status.as_mut().unwrap().decider = Some(" ".into());
        },
        |a| a.status.as_mut().unwrap().decider = None,
        |a| a.status.as_mut().unwrap().decided_at = None,
        |a| a.status.as_mut().unwrap().decided_at = Some("not-a-time".into()),
        |a| a.status.as_mut().unwrap().decided_at = Some("2026-09-07T07:59:00Z".into()),
        |a| a.status.as_mut().unwrap().decided_at = Some("2026-09-07T09:00:00Z".into()),
        |a| a.status.as_mut().unwrap().requested_at = None,
        |a| a.status.as_mut().unwrap().expires_at = None,
        |a| a.status.as_mut().unwrap().observed_generation = Some(1),
        |a| a.metadata.generation = None,
    ];
    for change in changes {
        let mut invalid = approval.clone();
        change(&mut invalid);
        assert!(historical_approval_facts(&task, std::slice::from_ref(&invalid)).is_empty());
        assert!(decision_facts(&[invalid]).is_empty());
    }
}

#[test]
fn approved_and_denied_history_is_deterministic_and_retains_old_deadlines() {
    let (task, mut approved) = fixture();
    approved.metadata.name = Some("alpha".into());
    let mut denied = approved.clone();
    denied.metadata.name = Some("zebra".into());
    denied.metadata.uid = Some("denial-uid".into());
    denied.spec.decision.as_mut().unwrap().verdict = "deny".into();
    denied.status.as_mut().unwrap().phase = Some("Denied".into());
    let history = historical_approval_facts(&task, &[denied, approved]);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].decision.name, "alpha");
    assert_eq!(history[1].decision.verdict, "deny");
    assert_eq!(history[0].expires_at, "2026-09-07T09:00:00Z");
    assert!(history.iter().all(|fact| !fact.consumption_attested));
}
