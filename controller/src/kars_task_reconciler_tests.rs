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
