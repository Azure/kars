// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

fn team() -> Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "metadata":{"name":"writers","namespace":"workspace","uid":"team-uid","resourceVersion":"9",
            "annotations":{MARKER:"execution-plan/v1"}},
        "spec":{"charter":"Write useful reviewed handbooks.","paused":true,
            "envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":0,
                "budget":{"tokens":2000000,"scope":"GovernedInference"}},
            "blueprint":{"executionPlan":super::super::task::execution_plan::tests::plan()},
            "roster":[]}})
}

#[test]
fn execution_plans_preserve_root_and_role_plans_and_semantic_defaults() {
    let mut expected = team();
    expected["spec"]["roster"] =
        json!([{"name":"writer","blueprint":expected["spec"]["blueprint"]}]);
    let captured: super::super::team::KarsTeam = serde_json::from_value(expected.clone()).unwrap();
    ensure_preserved(&expected, &captured).unwrap();
    let mut defaulted = serde_json::to_value(&captured).unwrap();
    for plan in [
        "/spec/blueprint/executionPlan",
        "/spec/roster/0/blueprint/executionPlan",
    ] {
        let phase = defaulted
            .pointer_mut(&format!("{plan}/roles/0/phases/0"))
            .unwrap()
            .as_object_mut()
            .unwrap();
        phase.insert("minToolCalls".into(), json!(0));
        phase.insert("freshContext".into(), json!(false));
    }
    let mut omitted = defaulted.clone();
    for plan in [
        "/spec/blueprint/executionPlan",
        "/spec/roster/0/blueprint/executionPlan",
    ] {
        let role = omitted
            .pointer_mut(&format!("{plan}/roles/0"))
            .unwrap()
            .as_object_mut()
            .unwrap();
        role.remove("dependsOn");
        let phase = role["phases"][0].as_object_mut().unwrap();
        phase.remove("minToolCalls");
        phase.remove("freshContext");
    }
    ensure_preserved(&omitted, &defaulted).unwrap();
    for path in [
        "/spec/blueprint/executionPlan",
        "/spec/roster/0/blueprint/executionPlan",
    ] {
        let mut pruned = expected.clone();
        *pruned.pointer_mut(path).unwrap() = Value::Null;
        assert!(ensure_preserved(&expected, &pruned).is_err(), "{path}");
        let mut changed = expected.clone();
        changed.pointer_mut(path).unwrap()["roles"][0]["objective"] =
            json!("A different reviewed objective.");
        assert!(ensure_preserved(&expected, &changed).is_err(), "{path}");
    }
}

#[test]
fn execution_plans_reject_missing_unsupported_and_unknown_authority() {
    let original = team();
    validate(&original, false).unwrap();
    assert!(
        validate(&original, true)
            .unwrap_err()
            .contains("TypedPlanExecutionUnavailable")
    );
    let mut missing = original.clone();
    missing["spec"]["blueprint"] = json!({});
    assert!(
        validate(&missing, false)
            .unwrap_err()
            .contains("ReviewedExecutionPlanMissing")
    );
    let mut duplicate = original.clone();
    let role = json!({"name":"writer","blueprint":original["spec"]["blueprint"]});
    duplicate["spec"]["roster"] = json!([role, role]);
    assert!(
        validate(&duplicate, false)
            .unwrap_err()
            .contains("Duplicate")
    );
    for path in [
        "",
        "/roles/0",
        "/roles/0/phases/0",
        "/roles/0/phases/0/requiredToolCalls/0",
        "/synthesis",
        "/deliverables/0",
    ] {
        let mut unknown = original.clone();
        unknown
            .pointer_mut(&format!("/spec/blueprint/executionPlan{path}"))
            .unwrap()["futureAuthority"] = json!(true);
        assert!(validate(&unknown, false).is_err(), "{path}");
    }
    for marker in [json!("future/v2"), json!(42), Value::Null] {
        let mut changed = original.clone();
        changed["metadata"]["annotations"][MARKER] = marker;
        assert!(validate(&changed, false).is_err());
    }
    missing["metadata"]["annotations"] = json!({});
    validate(&missing, true).unwrap();
    let mut role_only = missing.clone();
    role_only["spec"]["roster"] =
        json!([{"name":"writer","blueprint":original["spec"]["blueprint"]}]);
    validate(&role_only, false).unwrap();
    assert!(validate(&role_only, true).is_err());
}

#[test]
fn execution_plans_fence_effective_team_updates_and_keep_pure_pause_available() {
    let paused: super::super::team::KarsTeam = serde_json::from_value(team()).unwrap();
    let patch = prepare_team_patch(
        &paused,
        json!({"blueprint":{"model":{"provider":"azure-foundry","deployment":"reviewed-model"}}}),
    )
    .unwrap();
    assert_eq!(patch["metadata"]["uid"], "team-uid");
    assert_eq!(patch["metadata"]["resourceVersion"], "9");
    assert!(
        prepare_team_patch(&paused, json!({"paused":false}))
            .unwrap_err()
            .contains("TypedPlanExecutionUnavailable")
    );
    let mut active = paused.clone();
    active.spec.paused = false;
    assert!(
        prepare_team_patch(
            &active,
            json!({"charter":"Changed reviewed standing mandate."})
        )
        .is_err()
    );
    prepare_team_patch(&active, json!({"paused":true})).unwrap();
    prepare_team_patch(
        &active,
        json!({"paused":true,"charter":"Changed reviewed standing mandate."}),
    )
    .unwrap();
    active.spec.blueprint.as_mut().unwrap().execution_plan = None;
    prepare_team_patch(&active, json!({"paused":true})).unwrap();
    assert!(
        prepare_team_patch(
            &active,
            json!({"paused":true,"charter":"Changed reviewed standing mandate."})
        )
        .is_err()
    );
    assert!(prepare_team_patch(&paused, json!({"blueprint":{"executionPlan":null}})).is_err());
    let plan = team()["spec"]["blueprint"]["executionPlan"].clone();
    let replacement =
        prepare_team_patch(&paused, json!({"blueprint":{"executionPlan":plan}})).unwrap();
    assert_eq!(
        replacement["metadata"]["annotations"][MARKER],
        "execution-plan/v1"
    );
    active.metadata.annotations = None;
    prepare_team_patch(
        &active,
        json!({"charter":"Changed legacy standing mandate."}),
    )
    .unwrap();
}

#[test]
fn execution_plans_require_live_identity_for_every_patch_including_pause() {
    let original: super::super::team::KarsTeam = serde_json::from_value(team()).unwrap();
    let mut metadata = original.metadata.clone();
    metadata.uid = None;
    assert!(identity_fence(&metadata).is_err());
    metadata.uid = Some(String::new());
    assert!(identity_fence(&metadata).is_err());
    let mut current = original.clone();
    current.metadata.resource_version = None;
    assert!(prepare_team_patch(&current, json!({"paused":true})).is_err());
    current.metadata.resource_version = Some(String::new());
    assert!(identity_fence(&current.metadata).is_err());
    let mut deleting = original;
    deleting.metadata.deletion_timestamp =
        Some(serde_json::from_value(json!("2026-10-03T00:00:00Z")).unwrap());
    assert!(prepare_team_patch(&deleting, json!({"paused":true})).is_err());
}
