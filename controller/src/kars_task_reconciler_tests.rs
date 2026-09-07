// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_task::{KarsTaskSpec, TaskEnvelope};

fn task_with(tier: i32, authority_ceiling: i32, delegation_depth: i32) -> KarsTask {
    let mut task = KarsTask::new(
        "t",
        KarsTaskSpec {
            objective: "do the thing".into(),
            envelope: TaskEnvelope {
                tier,
                authority_ceiling,
                delegation_depth,
                ..TaskEnvelope::default()
            },
            parent_ref: None,
            execution: None,
            blueprint: None,
            display_name: None,
        },
    );
    task.metadata.namespace = Some("default".into());
    task
}

#[test]
fn valid_envelope_passes() {
    let task = task_with(3, 3, 2);
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Valid));
}

#[test]
fn root_policy_conflict_never_becomes_ready() {
    let mut task = task_with(3, 3, 2);
    task.spec.envelope.tool_policy_ref = Some(crate::mcp_server::LocalObjectRef {
        name: "read".into(),
    });
    task.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
        tool_policy: Some("write".into()),
        ..Default::default()
    });
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Invalid(_)));
}

#[test]
fn readiness_requires_current_generation_digest_and_valid_contract() {
    let mut task = task_with(3, 3, 2);
    task.metadata.generation = Some(1);
    task.status = Some(ready_status(
        None,
        Some(1),
        task.spec.envelope.digest(),
        vec![],
    ));
    assert!(task_is_ready(&task));
    task.metadata.generation = Some(2);
    assert!(!task_is_ready(&task));
    task.metadata.generation = Some(1);
    task.spec.envelope.tier = 4;
    assert!(!task_is_ready(&task));
}

#[test]
fn completeness_floor_is_not_inferred_from_resource_names() {
    let completeness = gather_completeness();
    assert!(!completeness.floor_enforced);
    assert!(!completeness.task_namespace_floor_vap);
    assert!(!completeness.exec_ban_vap);
    assert!(!completeness.posture_lock_vap);
    assert!(!completeness.default_deny_egress);
}

#[test]
fn authority_ceiling_above_tier_is_rejected() {
    let task = task_with(2, 4, 1);
    match check_envelope(&task) {
        EnvelopeCheck::Invalid(why) => assert!(why.contains("authorityCeiling")),
        EnvelopeCheck::Valid => panic!("expected rejection"),
    }
}

#[test]
fn tier_out_of_range_is_rejected() {
    let task = task_with(9, 5, 0);
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Invalid(_)));
}

#[test]
fn finalizer_roundtrip() {
    let mut task = task_with(1, 1, 0);
    assert!(!has_finalizer(&task));
    task.metadata.finalizers = Some(vec![FINALIZER.to_string(), "other/keep".to_string()]);
    assert!(has_finalizer(&task));
    let dropped = drop_finalizer(&task);
    assert_eq!(dropped, vec!["other/keep".to_string()]);
}
