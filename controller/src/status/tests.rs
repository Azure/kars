// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::crd::{KarsSandbox, KarsSandboxSpec, KarsSandboxStatus};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

fn new_sandbox(generation: Option<i64>, status: Option<KarsSandboxStatus>) -> KarsSandbox {
    KarsSandbox {
        metadata: ObjectMeta {
            name: Some("demo".into()),
            namespace: Some("kars-demo".into()),
            generation,
            ..Default::default()
        },
        spec: KarsSandboxSpec::default(),
        status,
    }
}

#[test]
fn running_patch_emits_generation_and_ready_condition() {
    let sb = new_sandbox(Some(7), None);
    let patch = build_running_status_patch(&sb, "kars-demo", "OpenClaw");
    let st = &patch["status"];
    assert_eq!(st["phase"], "Running");
    assert_eq!(st["observedGeneration"], 7);
    assert_eq!(st["runtimeKind"], "OpenClaw");
    let conds = st["conditions"].as_array().expect("conditions array");
    assert_eq!(
        conds.len(),
        3,
        "expected Ready + Progressing + RuntimeReady"
    );
    let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
    assert_eq!(ready["status"], "True");
    assert_eq!(ready["reason"], "Reconciled");
    assert_eq!(ready["observedGeneration"], 7);
    let progressing = conds
        .iter()
        .find(|c| c["type"] == "Progressing")
        .expect("Progressing");
    assert_eq!(progressing["status"], "False");
    assert_eq!(progressing["reason"], "Reconciled");
    assert_eq!(progressing["observedGeneration"], 7);
    let runtime_ready = conds
        .iter()
        .find(|c| c["type"] == "RuntimeReady")
        .expect("RuntimeReady");
    assert_eq!(runtime_ready["status"], "True");
    assert_eq!(runtime_ready["reason"], "Reconciled");
    assert!(
        runtime_ready["message"]
            .as_str()
            .unwrap_or_default()
            .contains("OpenClaw"),
        "RuntimeReady message must reference the runtime kind"
    );
}

#[test]
fn running_patch_preserves_foundry_agent_id() {
    let prior = KarsSandboxStatus {
        foundry_agent_id: Some("asst-abc".into()),
        ..Default::default()
    };
    let sb = new_sandbox(Some(3), Some(prior));
    let patch = build_running_status_patch(&sb, "kars-demo", "OpenClaw");
    assert_eq!(patch["status"]["foundryAgentId"], "asst-abc");
}

#[test]
fn running_patch_reuses_ready_transition_time() {
    let existing_ready = conditions::new_condition(
        conditions::TYPE_READY,
        conditions::status::TRUE,
        conditions::reason::RECONCILED,
        "ok",
        Some(1),
    );
    let prior_ts = existing_ready.last_transition_time.clone();
    let prior = KarsSandboxStatus {
        conditions: vec![existing_ready],
        ..Default::default()
    };
    std::thread::sleep(std::time::Duration::from_millis(5));
    let sb = new_sandbox(Some(2), Some(prior));
    let patch = build_running_status_patch(&sb, "kars-demo", "OpenClaw");
    let emitted_ts = patch["status"]["conditions"][0]["lastTransitionTime"]
        .as_str()
        .expect("timestamp must be stringified");
    // Timestamps serialize as RFC3339; ensure format unchanged == preserved.
    let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
    assert_eq!(emitted_ts, prior_ts_str.as_str().unwrap());
}

#[test]
fn running_patch_emits_null_observed_generation_when_metadata_missing() {
    let sb = new_sandbox(None, None);
    let patch = build_running_status_patch(&sb, "kars-demo", "OpenClaw");
    assert!(patch["status"]["observedGeneration"].is_null());
}

#[test]
fn running_status_matches_returns_false_when_status_missing() {
    let sb = new_sandbox(Some(1), None);
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_false_when_phase_differs() {
    let prior = KarsSandboxStatus {
        phase: Some("Pending".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::RECONCILED,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_false_when_namespace_differs() {
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-other".into()),
        observed_generation: Some(1),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::RECONCILED,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_false_when_generation_stale() {
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::RECONCILED,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(2), Some(prior));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_false_when_ready_false() {
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::FALSE,
            conditions::reason::FAILED,
            "boom",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_true_for_settled_status() {
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        runtime_kind: Some("OpenClaw".into()),
        conditions: vec![
            conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_PROGRESSING,
                conditions::status::FALSE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_RUNTIME_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            ),
        ],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn running_status_matches_returns_false_when_progressing_missing() {
    // Phase 2 S7.B regression: pre-S7.B controllers wrote
    // [Ready=True, RuntimeReady=True] without Progressing. After
    // upgrade, that prior shape must be considered stale so the
    // first reconcile back-fills the new Progressing condition
    // instead of being short-circuited as a no-op.
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        runtime_kind: Some("OpenClaw".into()),
        conditions: vec![
            conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_RUNTIME_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            ),
        ],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!running_status_matches(&sb, "kars-demo", "OpenClaw"));
}

#[test]
fn degraded_patch_stamps_degraded_true_and_ready_false() {
    let sb = new_sandbox(Some(9), None);
    let patch = build_degraded_status_patch(
        &sb,
        conditions::reason::SPEC_INVALID,
        "empty inference.model",
    );
    let st = &patch["status"];
    assert_eq!(st["phase"], "Degraded");
    assert_eq!(st["observedGeneration"], 9);
    let conds = st["conditions"].as_array().expect("conditions array");
    assert_eq!(conds.len(), 3);
    let degraded = conds
        .iter()
        .find(|c| c["type"] == "Degraded")
        .expect("Degraded cond");
    assert_eq!(degraded["status"], "True");
    assert_eq!(degraded["reason"], "SpecInvalid");
    assert_eq!(degraded["observedGeneration"], 9);
    let ready = conds
        .iter()
        .find(|c| c["type"] == "Ready")
        .expect("Ready cond");
    assert_eq!(ready["status"], "False");
    assert_eq!(ready["reason"], "SpecInvalid");
    assert_eq!(ready["observedGeneration"], 9);
    let progressing = conds
        .iter()
        .find(|c| c["type"] == "Progressing")
        .expect("Progressing cond");
    assert_eq!(progressing["status"], "False");
    assert_eq!(progressing["reason"], "SpecInvalid");
    assert_eq!(progressing["observedGeneration"], 9);
}

#[test]
fn degraded_patch_preserves_transition_time_on_repeat() {
    let prior_degraded = conditions::new_condition(
        conditions::TYPE_DEGRADED,
        conditions::status::TRUE,
        conditions::reason::SPEC_INVALID,
        "bad spec",
        Some(1),
    );
    let prior_ts = prior_degraded.last_transition_time.clone();
    let prior = KarsSandboxStatus {
        conditions: vec![prior_degraded],
        ..Default::default()
    };
    std::thread::sleep(std::time::Duration::from_millis(5));
    let sb = new_sandbox(Some(2), Some(prior));
    let patch =
        build_degraded_status_patch(&sb, conditions::reason::SPEC_INVALID, "still bad spec");
    let degraded_ts = patch["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Degraded")
        .unwrap()["lastTransitionTime"]
        .as_str()
        .unwrap()
        .to_string();
    let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
    assert_eq!(degraded_ts, prior_ts_str.as_str().unwrap());
}

#[test]
fn degraded_patch_handles_missing_generation() {
    let sb = new_sandbox(None, None);
    let patch = build_degraded_status_patch(&sb, conditions::reason::SPEC_INVALID, "no generation");
    assert!(patch["status"]["observedGeneration"].is_null());
    let degraded = patch["status"]["conditions"][0].clone();
    assert!(degraded["observedGeneration"].is_null());
}

// ── OverlayMode (Phase 2 S8) status helpers ──

#[test]
fn overlay_patch_emits_overlay_phase_and_three_conditions() {
    let sb = new_sandbox(Some(4), None);
    let patch = build_overlay_status_patch(&sb, "kars-demo", "upstream-1", "OpenClaw");
    let st = &patch["status"];
    assert_eq!(st["phase"], "Overlay");
    assert_eq!(st["namespace"], "kars-demo");
    assert_eq!(st["sandboxPod"], "upstream/upstream-1");
    assert_eq!(st["observedGeneration"], 4);
    assert_eq!(st["runtimeKind"], "OpenClaw");
    let conds = st["conditions"].as_array().expect("conditions array");
    assert_eq!(
        conds.len(),
        4,
        "expected Ready+Progressing+Suspended+RuntimeReady"
    );
    let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
    assert_eq!(ready["status"], "True");
    assert_eq!(ready["reason"], "OverlayMode");
    let progressing = conds
        .iter()
        .find(|c| c["type"] == "Progressing")
        .expect("Progressing");
    assert_eq!(progressing["status"], "False");
    assert_eq!(progressing["reason"], "OverlayMode");
    let suspended = conds
        .iter()
        .find(|c| c["type"] == "Suspended")
        .expect("Suspended");
    assert_eq!(suspended["status"], "True");
    assert_eq!(suspended["reason"], "OverlayMode");
    assert!(
        suspended["message"]
            .as_str()
            .unwrap_or_default()
            .contains("upstream-1"),
        "Suspended message must reference the upstream CR name"
    );
    let runtime_ready = conds
        .iter()
        .find(|c| c["type"] == "RuntimeReady")
        .expect("RuntimeReady");
    assert_eq!(runtime_ready["status"], "False");
    assert_eq!(runtime_ready["reason"], "OverlayMode");
}

#[test]
fn overlay_status_matches_rejects_when_status_missing() {
    let sb = new_sandbox(Some(1), None);
    assert!(!overlay_status_matches(&sb, "kars-demo", "u1", "OpenClaw"));
}

#[test]
fn overlay_status_matches_rejects_when_phase_is_running() {
    let prior = KarsSandboxStatus {
        phase: Some("Running".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        sandbox_pod: Some("upstream/u1".into()),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::OVERLAY_MODE,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!overlay_status_matches(&sb, "kars-demo", "u1", "OpenClaw"));
}

#[test]
fn overlay_status_matches_rejects_when_upstream_ref_differs() {
    let prior = KarsSandboxStatus {
        phase: Some("Overlay".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        sandbox_pod: Some("upstream/old-name".into()),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::OVERLAY_MODE,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!overlay_status_matches(
        &sb,
        "kars-demo",
        "new-name",
        "OpenClaw"
    ));
}

#[test]
fn overlay_status_matches_rejects_when_generation_stale() {
    let prior = KarsSandboxStatus {
        phase: Some("Overlay".into()),
        namespace: Some("kars-demo".into()),
        observed_generation: Some(1),
        sandbox_pod: Some("upstream/u1".into()),
        conditions: vec![conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::OVERLAY_MODE,
            "ok",
            Some(1),
        )],
        ..Default::default()
    };
    let sb = new_sandbox(Some(2), Some(prior));
    assert!(!overlay_status_matches(&sb, "kars-demo", "u1", "OpenClaw"));
}

#[test]
fn overlay_status_matches_returns_true_for_settled_overlay_status() {
    let mut sb = new_sandbox(Some(1), None);
    let patch = build_overlay_status_patch(&sb, "kars-demo", "u1", "OpenClaw");
    sb.status = Some(serde_json::from_value(patch["status"].clone()).unwrap());
    assert!(overlay_status_matches(&sb, "kars-demo", "u1", "OpenClaw"));
}

#[test]
fn overlay_patch_preserves_ready_transition_time_on_repeat() {
    let existing_ready = conditions::new_condition(
        conditions::TYPE_READY,
        conditions::status::TRUE,
        conditions::reason::OVERLAY_MODE,
        "overlay",
        Some(1),
    );
    let prior_ts = existing_ready.last_transition_time.clone();
    let prior = KarsSandboxStatus {
        conditions: vec![existing_ready],
        ..Default::default()
    };
    std::thread::sleep(std::time::Duration::from_millis(5));
    let sb = new_sandbox(Some(2), Some(prior));
    let patch = build_overlay_status_patch(&sb, "kars-demo", "u1", "OpenClaw");
    let ready = patch["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Ready")
        .unwrap()
        .clone();
    let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
    assert_eq!(
        ready["lastTransitionTime"].as_str().unwrap(),
        prior_ts_str.as_str().unwrap()
    );
}

// ── S10.A1: AdapterMissing (runtime unsupported) status helpers ──

#[test]
fn runtime_unsupported_patch_stamps_three_conditions_and_runtime_kind() {
    let sb = new_sandbox(Some(5), None);
    let patch = build_runtime_unsupported_status_patch(
        &sb,
        "OpenAIAgents",
        "no adapter wired in this build",
    );
    let st = &patch["status"];
    assert_eq!(st["phase"], "Degraded");
    assert_eq!(st["observedGeneration"], 5);
    assert_eq!(st["runtimeKind"], "OpenAIAgents");
    let conds = st["conditions"].as_array().expect("conditions array");
    assert_eq!(
        conds.len(),
        4,
        "expected Degraded+Ready+RuntimeReady+Progressing"
    );
    let degraded = conds
        .iter()
        .find(|c| c["type"] == "Degraded")
        .expect("Degraded");
    assert_eq!(degraded["status"], "True");
    assert_eq!(degraded["reason"], "AdapterMissing");
    let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
    assert_eq!(ready["status"], "False");
    assert_eq!(ready["reason"], "AdapterMissing");
    let runtime_ready = conds
        .iter()
        .find(|c| c["type"] == "RuntimeReady")
        .expect("RuntimeReady");
    assert_eq!(runtime_ready["status"], "False");
    assert_eq!(runtime_ready["reason"], "AdapterMissing");
    let progressing = conds
        .iter()
        .find(|c| c["type"] == "Progressing")
        .expect("Progressing");
    assert_eq!(progressing["status"], "False");
    assert_eq!(progressing["reason"], "AdapterMissing");
}

#[test]
fn runtime_unsupported_status_matches_rejects_when_status_missing() {
    let sb = new_sandbox(Some(1), None);
    assert!(!runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
}

#[test]
fn runtime_unsupported_status_matches_rejects_when_runtime_kind_differs() {
    let prior = KarsSandboxStatus {
        phase: Some("Degraded".into()),
        observed_generation: Some(1),
        runtime_kind: Some("MicrosoftAgentFramework".into()),
        conditions: vec![
            conditions::new_condition(
                conditions::TYPE_DEGRADED,
                conditions::status::TRUE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::FALSE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_RUNTIME_READY,
                conditions::status::FALSE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
        ],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(!runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
}

#[test]
fn runtime_unsupported_status_matches_returns_true_for_settled_status() {
    let prior = KarsSandboxStatus {
        phase: Some("Degraded".into()),
        observed_generation: Some(1),
        runtime_kind: Some("OpenAIAgents".into()),
        conditions: vec![
            conditions::new_condition(
                conditions::TYPE_DEGRADED,
                conditions::status::TRUE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::FALSE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_RUNTIME_READY,
                conditions::status::FALSE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
            conditions::new_condition(
                conditions::TYPE_PROGRESSING,
                conditions::status::FALSE,
                conditions::reason::ADAPTER_MISSING,
                "x",
                Some(1),
            ),
        ],
        ..Default::default()
    };
    let sb = new_sandbox(Some(1), Some(prior));
    assert!(runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
}

#[test]
fn runtime_unsupported_patch_preserves_transition_time_on_repeat() {
    let prior_degraded = conditions::new_condition(
        conditions::TYPE_DEGRADED,
        conditions::status::TRUE,
        conditions::reason::ADAPTER_MISSING,
        "no adapter",
        Some(1),
    );
    let prior_ts = prior_degraded.last_transition_time.clone();
    let prior = KarsSandboxStatus {
        conditions: vec![prior_degraded],
        ..Default::default()
    };
    std::thread::sleep(std::time::Duration::from_millis(5));
    let sb = new_sandbox(Some(2), Some(prior));
    let patch = build_runtime_unsupported_status_patch(&sb, "OpenAIAgents", "still no adapter");
    let degraded_ts = patch["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Degraded")
        .unwrap()["lastTransitionTime"]
        .as_str()
        .unwrap()
        .to_string();
    let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
    assert_eq!(degraded_ts, prior_ts_str.as_str().unwrap());
}
