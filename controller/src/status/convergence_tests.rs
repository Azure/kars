// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::crd::{KarsSandboxSpec, KarsSandboxStatus};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};

fn sandbox() -> KarsSandbox {
    KarsSandbox {
        metadata: ObjectMeta {
            name: Some("demo".into()),
            namespace: Some("kars-demo".into()),
            generation: Some(7),
            uid: Some("owned-uid".into()),
            resource_version: Some("42".into()),
            ..Default::default()
        },
        spec: KarsSandboxSpec::default(),
        status: Some(KarsSandboxStatus {
            foundry_agent_id: Some("agent-to-preserve".into()),
            ..Default::default()
        }),
    }
}

fn timestamp() -> Time {
    serde_json::from_value(json!("2026-01-01T00:00:00Z")).unwrap()
}

fn extra() -> Condition {
    let mut condition = conditions::new_condition(
        conditions::TYPE_ALLOWLIST_AUTHORITATIVE,
        conditions::status::FALSE,
        conditions::reason::INLINE,
        "inline endpoints",
        Some(7),
    );
    condition.last_transition_time = timestamp();
    condition
}

fn apply_status(sb: &mut KarsSandbox, patch: Value) {
    assert_eq!(patch.as_object().unwrap().len(), 1);
    let mut status = serde_json::to_value(sb.status.as_ref().unwrap()).unwrap();
    for (key, value) in patch["status"].as_object().unwrap() {
        status[key] = value.clone();
    }
    sb.status = Some(serde_json::from_value(status).unwrap());
}

fn settled_running() -> KarsSandbox {
    let mut sb = sandbox();
    let patch = build_running_status_patch_with_extras(&sb, "kars-demo", "OpenClaw", &[extra()]);
    apply_status(&mut sb, patch);
    for condition in &mut sb.status.as_mut().unwrap().conditions {
        condition.last_transition_time = timestamp();
    }
    let mut unrelated = extra();
    unrelated.type_ = "ExternalObservation".into();
    unrelated.observed_generation = None;
    sb.status.as_mut().unwrap().conditions.push(unrelated);
    sb
}

fn reconcile_running(sb: &mut KarsSandbox, extras: &[Condition]) -> bool {
    if running_status_matches_with_extras(sb, "kars-demo", "OpenClaw", extras) {
        return false;
    }
    let patch = build_running_status_patch_with_extras(sb, "kars-demo", "OpenClaw", extras);
    apply_status(sb, patch);
    true
}

#[test]
fn running_backfills_each_missing_or_stale_condition_generation_once() {
    for type_ in [
        conditions::TYPE_READY,
        conditions::TYPE_PROGRESSING,
        conditions::TYPE_RUNTIME_READY,
        conditions::TYPE_ALLOWLIST_AUTHORITATIVE,
    ] {
        for generation in [None, Some(6), Some(8)] {
            let mut sb = settled_running();
            let current = serde_json::to_value(&sb).unwrap();
            let condition = sb
                .status
                .as_mut()
                .unwrap()
                .conditions
                .iter_mut()
                .find(|c| c.type_ == type_)
                .unwrap();
            condition.observed_generation = generation;
            assert!(
                reconcile_running(&mut sb, &[extra()]),
                "{type_}: {generation:?}"
            );
            assert_eq!(
                serde_json::to_value(&sb).unwrap(),
                current,
                "only the controller-owned generation should be backfilled"
            );
            assert!(!reconcile_running(&mut sb, &[extra()]));
            assert_eq!(serde_json::to_value(&sb).unwrap(), current);
        }
    }
}

#[test]
fn running_correct_status_is_untouched_including_messages_and_timestamps() {
    let mut sb = settled_running();
    let before = serde_json::to_value(&sb).unwrap();
    let mut desired = extra();
    desired.message = "new diagnostic text".into();
    desired.last_transition_time =
        conditions::new_condition("Ignored", "Unknown", "Ignored", "", None).last_transition_time;
    for _ in 0..3 {
        assert!(!reconcile_running(&mut sb, &[desired.clone()]));
        assert_eq!(serde_json::to_value(&sb).unwrap(), before);
    }
}

#[test]
fn running_repairs_the_api_pruned_shape_despite_current_top_level_generation() {
    let mut sb = settled_running();
    let expected = serde_json::to_value(&sb).unwrap();
    for condition in &mut sb.status.as_mut().unwrap().conditions {
        condition.observed_generation = None;
    }
    assert_eq!(sb.status.as_ref().unwrap().observed_generation, Some(7));
    assert!(reconcile_running(&mut sb, &[extra()]));
    assert_eq!(serde_json::to_value(&sb).unwrap(), expected);
    assert!(!reconcile_running(&mut sb, &[extra()]));
}

#[test]
fn running_generation_backfill_does_not_churn_extra_timestamps() {
    let mut sb = settled_running();
    let before = serde_json::to_value(&sb).unwrap();
    sb.status.as_mut().unwrap().conditions[0].observed_generation = None;
    let mut desired = extra();
    desired.last_transition_time =
        conditions::new_condition("Ignored", "Unknown", "Ignored", "", None).last_transition_time;
    assert!(reconcile_running(&mut sb, &[desired.clone()]));
    assert_eq!(serde_json::to_value(&sb).unwrap(), before);
    assert!(!reconcile_running(&mut sb, &[desired]));
}

#[test]
fn extras_keep_authoritative_generations_and_last_writer_semantics() {
    let mut sb = settled_running();
    let mut earlier = extra();
    earlier.status = "True".into();
    let mut desired = extra();
    desired.observed_generation = Some(6);
    assert!(reconcile_running(
        &mut sb,
        &[earlier.clone(), desired.clone()]
    ));
    assert!(!reconcile_running(&mut sb, &[earlier, desired]));
    let condition = conditions::find(
        &sb.status.as_ref().unwrap().conditions,
        conditions::TYPE_ALLOWLIST_AUTHORITATIVE,
    )
    .unwrap();
    assert_eq!(
        condition.observed_generation,
        Some(6),
        "do not invent extra freshness"
    );
}

#[test]
fn explicit_standard_condition_overrides_do_not_create_a_reconcile_loop() {
    let mut sb = settled_running();
    let mut desired = extra();
    desired.type_ = conditions::TYPE_READY.into();
    desired.status = conditions::status::FALSE.into();
    desired.observed_generation = Some(6);
    assert!(reconcile_running(&mut sb, &[extra(), desired.clone()]));
    assert!(!reconcile_running(&mut sb, &[extra(), desired]));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn real_extra_transition_is_preserved_and_then_settles() {
    let mut sb = settled_running();
    let mut desired = extra();
    desired.status = "True".into();
    desired.reason = conditions::reason::VERIFIED.into();
    desired.last_transition_time =
        conditions::new_condition("Ignored", "Unknown", "Ignored", "", None).last_transition_time;
    assert!(reconcile_running(&mut sb, &[desired.clone()]));
    let condition = conditions::find(
        &sb.status.as_ref().unwrap().conditions,
        conditions::TYPE_ALLOWLIST_AUTHORITATIVE,
    )
    .unwrap();
    assert_eq!(condition.last_transition_time, desired.last_transition_time);
    assert!(!reconcile_running(&mut sb, &[desired]));
}

#[test]
fn recovery_retires_controller_degraded_condition_not_external_observations() {
    let mut sb = settled_running();
    sb.status
        .as_mut()
        .unwrap()
        .conditions
        .push(conditions::new_condition(
            conditions::TYPE_DEGRADED,
            "True",
            "SpecInvalid",
            "prior failure",
            Some(6),
        ));
    sb.status.as_mut().unwrap().conditions[0].observed_generation = None;
    assert!(reconcile_running(&mut sb, &[extra()]));
    let conditions = &sb.status.as_ref().unwrap().conditions;
    assert!(conditions::find(conditions, conditions::TYPE_DEGRADED).is_none());
    assert!(conditions::find(conditions, "ExternalObservation").is_some());
}

#[test]
fn overlay_and_unsupported_generation_repairs_preserve_transitions_and_settle() {
    for overlay in [true, false] {
        let build = |sb: &KarsSandbox| {
            if overlay {
                build_overlay_status_patch(sb, "kars-demo", "upstream", "OpenClaw")
            } else {
                build_runtime_unsupported_status_patch(sb, "BYO", "adapter unavailable")
            }
        };
        let matches = |sb: &KarsSandbox| {
            if overlay {
                overlay_status_matches(sb, "kars-demo", "upstream", "OpenClaw")
            } else {
                runtime_unsupported_status_matches(sb, "BYO")
            }
        };
        for index in 0..4 {
            for generation in [None, Some(6)] {
                let mut sb = settled_running();
                let patch = build(&sb);
                apply_status(&mut sb, patch);
                assert!(matches(&sb));
                let before = serde_json::to_value(&sb).unwrap();
                sb.status.as_mut().unwrap().conditions[index].observed_generation = generation;
                assert!(!matches(&sb));
                let patch = build(&sb);
                apply_status(&mut sb, patch);
                assert!(matches(&sb));
                assert_eq!(serde_json::to_value(&sb).unwrap(), before);
            }
        }
    }
}
