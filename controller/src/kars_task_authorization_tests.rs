// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_approval::{
    ApprovalAction, ApprovalDecision, ApprovalOutcome, KarsApproval, KarsApprovalSpec,
    KarsApprovalStatus, approval_authorizes_task, approval_binding_matches_task, evaluate,
    request_snapshot,
};

fn model() -> TaskModel {
    TaskModel {
        provider: "azure-openai".into(),
        deployment: "reviewed-model".into(),
    }
}

fn spec() -> KarsTaskSpec {
    KarsTaskSpec {
        objective: "Review the change".into(),
        envelope: TaskEnvelope {
            tier: 3,
            authority_ceiling: 2,
            delegation_depth: 1,
            ..Default::default()
        },
        blueprint: Some(TaskBlueprint {
            runtime: Some("OpenClaw".into()),
            model: Some(model()),
            instructions: Some("Report findings".into()),
            tool_policy: Some("read-only".into()),
            mcp_servers: vec!["docs".into()],
            egress: vec![TaskEgress {
                host: "api.example.com".into(),
                port: Some(443),
            }],
            isolation: Some("standard".into()),
            memory: Some("team-memory".into()),
            model_fallbacks: Vec::new(),
            credential_bindings: None,
            github_binding: None,
        }),
        ..Default::default()
    }
}

fn mark_ready(task: &mut KarsTask) {
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

fn approved_task() -> (KarsTask, KarsApproval) {
    let mut task = KarsTask::new("task", spec());
    task.metadata.uid = Some("task-uid".into());
    task.metadata.namespace = Some("work".into());
    task.metadata.generation = Some(1);
    mark_ready(&mut task);
    let mut approval = KarsApproval::new(
        "approval",
        KarsApprovalSpec {
            task_ref: LocalObjectRef {
                name: "task".into(),
            },
            action: ApprovalAction {
                kind: "tierRaise".into(),
                summary: "Raise this task's tier".into(),
                requested_tier: Some(4),
                ..Default::default()
            },
            decision: Some(ApprovalDecision {
                verdict: "approve".into(),
                decider: "alice".into(),
                reason: None,
            }),
            ttl: Some("PT1H".into()),
        },
    );
    approval.metadata.namespace = task.metadata.namespace.clone();
    approval.metadata.generation = Some(2);
    approval.status = Some(KarsApprovalStatus {
        phase: Some("Approved".into()),
        observed_generation: Some(2),
        bound_envelope_digest: Some(task.envelope_digest()),
        bound_task_uid: task.metadata.uid.clone(),
        bound_request: Some(request_snapshot(&approval.spec)),
        decider: Some("alice".into()),
        decided_at: Some("2026-09-07T12:00:00Z".into()),
        ..Default::default()
    });
    (task, approval)
}

#[test]
fn every_effective_blueprint_axis_changes_task_authority() {
    type SpecChange = fn(&mut KarsTaskSpec);
    let original = spec();
    let changes: &[(&str, SpecChange)] = &[
        ("runtime", |s| {
            s.blueprint.as_mut().unwrap().runtime = Some("Hermes".into())
        }),
        ("model", |s| {
            s.blueprint
                .as_mut()
                .unwrap()
                .model
                .as_mut()
                .unwrap()
                .deployment = "other-model".into()
        }),
        ("provider", |s| {
            s.blueprint
                .as_mut()
                .unwrap()
                .model
                .as_mut()
                .unwrap()
                .provider = "github-models".into()
        }),
        ("isolation", |s| {
            s.blueprint.as_mut().unwrap().isolation = Some("enhanced".into())
        }),
        ("tool", |s| {
            s.blueprint.as_mut().unwrap().tool_policy = Some("write-enabled".into())
        }),
        ("mcp", |s| {
            s.blueprint
                .as_mut()
                .unwrap()
                .mcp_servers
                .push("admin".into())
        }),
        ("memory", |s| {
            s.blueprint.as_mut().unwrap().memory = Some("other-memory".into())
        }),
        ("egress host", |s| {
            s.blueprint.as_mut().unwrap().egress[0].host = "other.example.com".into()
        }),
        ("egress port", |s| {
            s.blueprint.as_mut().unwrap().egress[0].port = None
        }),
        ("instructions", |s| {
            s.blueprint.as_mut().unwrap().instructions = Some("Publish changes".into())
        }),
        ("objective", |s| {
            s.objective = "A different objective".into()
        }),
        ("parent", |s| {
            s.parent_ref = Some(LocalObjectRef {
                name: "new-parent".into(),
            })
        }),
    ];
    for (axis, change) in changes {
        let mut changed = original.clone();
        change(&mut changed);
        assert_eq!(
            changed.envelope.digest(),
            original.envelope.digest(),
            "{axis}: lattice unchanged"
        );
        assert_ne!(
            changed.authorization_digest(),
            original.authorization_digest(),
            "{axis}"
        );
    }
}

#[test]
fn defaults_aliases_and_runtime_precedence_have_one_canonical_digest() {
    let baseline = KarsTaskSpec {
        objective: "Review".into(),
        ..Default::default()
    };
    let digest = baseline.authorization_digest_with_model(&model());
    assert_eq!(digest.len(), 71);
    assert_eq!(
        digest,
        "sha256:7089e6622e2ef5528f047701def62469360a849441ab9b285604da5f50b0c0c8"
    );
    assert_ne!(digest, baseline.envelope.digest());
    let mut explicit = baseline.clone();
    explicit.blueprint = Some(TaskBlueprint {
        runtime: Some("OpenClaw".into()),
        model: Some(model()),
        isolation: Some("standard".into()),
        instructions: Some("  ".into()),
        memory: Some(" ".into()),
        ..Default::default()
    });
    explicit.execution = Some(TaskExecution {
        launch: true,
        runtime: Some("Hermes".into()),
    });
    explicit.display_name = Some("Display only".into());
    explicit.envelope.budget = Some(TaskBudget {
        tokens: Some(0),
        usd_micros: Some(0),
        ..Default::default()
    });
    assert_eq!(explicit.authorization_digest_with_model(&model()), digest);
    explicit.blueprint.as_mut().unwrap().runtime = Some("MAF".into());
    let alias = explicit.authorization_digest_with_model(&model());
    explicit.blueprint.as_mut().unwrap().runtime = Some("MicrosoftAgentFramework".into());
    assert_eq!(explicit.authorization_digest_with_model(&model()), alias);
    explicit.blueprint.as_mut().unwrap().runtime = None;
    explicit.execution.as_mut().unwrap().runtime = Some("MAF".into());
    assert_eq!(explicit.authorization_digest_with_model(&model()), alias);
}

#[test]
fn shared_authorization_snapshot_exposes_the_exact_effective_digest_input() {
    let mut task = KarsTaskSpec {
        objective: "Review".into(),
        ..Default::default()
    };
    task.envelope.budget = Some(TaskBudget {
        tokens: Some(0),
        usd_micros: Some(0),
        ..Default::default()
    });
    let configuration = task.authorization_configuration_with_model(&model());
    assert_eq!(
        configuration,
        serde_json::json!({
            "domain": "kars.azure.com/task-authorization/v1",
            "envelope": { "tier": 1, "authorityCeiling": 1, "delegationDepth": 0 },
            "parentRef": null,
            "blueprint": {
                "runtime": "OpenClaw",
                "model": { "deployment": "reviewed-model", "provider": "azure-openai" },
                "instructions": "Your objective:\nReview",
                "isolation": "standard"
            },
            "networkPolicy": { "defaultDeny": true, "egressMode": "Strict" }
        })
    );
    assert_eq!(
        task.authorization_digest_with_model(&model()),
        "sha256:7089e6622e2ef5528f047701def62469360a849441ab9b285604da5f50b0c0c8"
    );
}

#[test]
fn effective_controller_model_defaults_are_authority_not_invisible_ambient_config() {
    let baseline = KarsTaskSpec::default();
    let different = TaskModel {
        deployment: "different".into(),
        ..model()
    };
    assert_ne!(
        baseline.authorization_digest_with_model(&model()),
        baseline.authorization_digest_with_model(&different)
    );
    let different = TaskModel {
        provider: "github-models".into(),
        ..model()
    };
    assert_ne!(
        baseline.authorization_digest_with_model(&model()),
        baseline.authorization_digest_with_model(&different)
    );
    let pinned = spec();
    assert_eq!(
        pinned.authorization_digest_with_model(&model()),
        pinned.authorization_digest_with_model(&different)
    );
    let mut blank_provider = pinned.clone();
    blank_provider
        .blueprint
        .as_mut()
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .provider
        .clear();
    assert_eq!(
        blank_provider.authorization_digest_with_model(&different),
        pinned.authorization_digest_with_model(&different)
    );
}

#[test]
fn pending_decisions_and_terminal_grants_cannot_authorize_changed_blueprints() {
    let (mut task, approval) = approved_task();
    assert!(approval_authorizes_task(&approval, &task));
    let old_digest = task.envelope_digest();
    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .egress
        .push(TaskEgress {
            host: "admin.example.com".into(),
            port: Some(443),
        });
    task.metadata.generation = Some(2);
    assert!(!approval_authorizes_task(&approval, &task));
    mark_ready(&mut task);
    assert!(crate::kars_task_reconciler::task_is_ready(&task));
    assert_ne!(task.envelope_digest(), old_digest);
    assert!(!approval_binding_matches_task(&approval, &task));
    assert!(!approval_authorizes_task(&approval, &task));
    assert_eq!(
        approval.status.as_ref().unwrap().phase.as_deref(),
        Some("Approved")
    );
    assert!(matches!(
        evaluate(
            approval.spec.decision.as_ref(),
            Some(&old_digest),
            Some(&task.envelope_digest()),
            false
        ),
        ApprovalOutcome::Stale(_)
    ));
}

#[test]
fn terminal_grants_require_current_task_uid_and_an_unchanged_decision_record() {
    let (task, approval) = approved_task();
    let mut replacement = task.clone();
    replacement.metadata.uid = Some("replacement-task".into());
    assert_eq!(replacement.envelope_digest(), task.envelope_digest());
    assert!(!approval_authorizes_task(&approval, &replacement));
    let mut changed = approval.clone();
    changed.spec.action.requested_tier = Some(5);
    assert!(!approval_authorizes_task(&changed, &task));
    let mut changed = approval.clone();
    changed.spec.decision.as_mut().unwrap().decider = "mallory".into();
    assert!(!approval_authorizes_task(&changed, &task));
    let mut changed = approval.clone();
    changed.status.as_mut().unwrap().phase = Some("Denied".into());
    assert!(!approval_authorizes_task(&changed, &task));
    let mut changed = approval;
    changed.metadata.namespace = Some("other".into());
    assert!(!approval_authorizes_task(&changed, &task));
}

#[test]
fn blueprint_drift_invalidates_parent_readiness_without_weakening_attenuation() {
    let (mut parent, _) = approved_task();
    let mut child = parent.spec.clone();
    child.envelope.tier = 2;
    child.envelope.authority_ceiling = 2;
    child.envelope.delegation_depth = 0;
    assert!(spec_attenuation_violations(&child, &parent.spec).is_empty());
    parent.spec.blueprint.as_mut().unwrap().egress.clear();
    assert!(!crate::kars_task_reconciler::task_is_ready(&parent));
    assert!(
        spec_attenuation_violations(&child, &parent.spec)
            .iter()
            .any(|violation| matches!(violation, EnvelopeViolation::EgressNotSubset { .. }))
    );
    mark_ready(&mut parent);
    assert!(crate::kars_task_reconciler::task_is_ready(&parent));
}

#[test]
fn invalid_pinned_tool_references_cannot_normalize_into_different_authority() {
    for name in ["", " read-only "] {
        let mut spec = KarsTaskSpec::default();
        spec.envelope.tool_policy_ref = Some(LocalObjectRef { name: name.into() });
        assert!(validate_execution_contract(&spec).is_err());
    }
}

#[test]
fn receipt_subject_follows_task_authority_and_refuses_stale_status() {
    use crate::kars_receipt::{PredicateCompleteness, build_statement};
    let (mut task, _) = approved_task();
    let old_status = task.status.clone().unwrap();
    let old = build_statement(
        &task,
        &old_status,
        "key",
        &[],
        PredicateCompleteness::default(),
    )
    .unwrap();
    task.spec.blueprint.as_mut().unwrap().memory = Some("different-memory".into());
    assert!(
        build_statement(
            &task,
            &old_status,
            "key",
            &[],
            PredicateCompleteness::default()
        )
        .is_none()
    );
    mark_ready(&mut task);
    let new = build_statement(
        &task,
        task.status.as_ref().unwrap(),
        "key",
        &[],
        PredicateCompleteness::default(),
    )
    .unwrap();
    assert_ne!(old.subject[0].digest.sha256, new.subject[0].digest.sha256);
    assert_eq!(
        new.subject[0].digest.sha256,
        task.envelope_digest().trim_start_matches("sha256:")
    );
}
